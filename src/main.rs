//! localgp_ogp_ingest driver — processes exactly one layer per run.
//!
//! Usage:
//!   localgp_ogp_ingest [config.toml] --tag NAME --provenance-link URL --code-version URL --layer T-B [--no-ensemble]
//!
//! `--tag`, `--provenance-link`, `--code-version`, `--layer` are REQUIRED (one run = one layer). Each
//! may also be given via env (`OHC_TAG`, `OHC_PROVENANCE_LINK`, `OHC_CODE_VERSION`, `OHC_LAYER`); CLI
//! wins. The time axis is autodetected
//! from the mapping files present in `dir_mean` (and, with the ensemble, `dir_ensemble`): every whole
//! calendar year found, validated for gaps (a missing month, or a mean/ensemble mismatch, is a hard
//! error). `--tag` is the run identifier: it names the output
//! store (`ohc_<tag>_<Ymin>_<Ymax>_plev<layer>.zarr`, the years being the discovered data span) and is
//! written to the store's `provenance_tag` attr
//! (whitespace-stripped, never lowercased — must match the provenance record char-for-char).
//! `--provenance-link` points at that record (this run's documentation) → `provenance_link` attr;
//! `--code-version` links the exact localgp_ogp_ingest code (a commit/release URL) → `localgp_ingest_code_version`.
//! The store also carries `localgp_ingest_run_config` (the whole resolved config, cold-serialized) and
//! `localgp_ingest_run_facts` (the discovered axis, layer, ensemble size, grid) as compact JSON-string
//! attrs — this step's local provenance, namespaced so downstream steps roll it forward untouched.
//! `--no-ensemble` (or `OHC_NO_ENSEMBLE`) ingests the mean only — skips the LocalCondSim files
//! and omits `field_ensemble` from the store (for mean-only products, or incomplete CondSim sets).
//! Static constants + paths come from `config.toml`, or from the defaults + path env vars
//! (`OHC_DIR_MEAN`, `OHC_DIR_ENSEMBLE`, `OHC_DIR_OUT`, `OHC_ETOPO`, `OHC_BASINMASK`) when no
//! config is given. `--dir_mean`, `--dir_ensemble`, `--dir_out` override those directories on the
//! command line (CLI wins over both config and env) — for munging paths per shell submission.
//! The config may omit those three dirs entirely (they default to `.`) and rely on the flags.
//!
//! Examples:
//!   localgp_ogp_ingest --layer 15_20
//!   localgp_ogp_ingest config.toml --layer 300_700
//!
//! To process many layers, run one invocation per layer (e.g. a scheduler job array).

use anyhow::{bail, Context, Result};
use std::env;
use std::path::PathBuf;
use std::time::Instant;

use ohc_ingest::config::{parse_layer, LayerSpec, RunConfig, Slice};
use ohc_ingest::{basinmask, grid as gridmod, ingest, masks, ncread, zarrwrite, GridDef};

struct Cli {
    config_path: Option<String>,
    tag: Option<String>,
    provenance_link: Option<String>,
    code_version: Option<String>,
    layer: Option<LayerSpec>,
    no_ensemble: bool,
    dir_mean: Option<PathBuf>,
    dir_ensemble: Option<PathBuf>,
    dir_out: Option<PathBuf>,
}

fn parse_cli() -> Result<Cli> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut cli = Cli {
        config_path: None, tag: None, provenance_link: None, code_version: None, layer: None,
        no_ensemble: env::var_os("OHC_NO_ENSEMBLE").is_some(),
        dir_mean: None, dir_ensemble: None, dir_out: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-ensemble" => {
                cli.no_ensemble = true;
            }
            "--dir_mean" => {
                i += 1;
                cli.dir_mean = Some(PathBuf::from(args.get(i).context("--dir_mean needs a value")?.clone()));
            }
            "--dir_ensemble" => {
                i += 1;
                cli.dir_ensemble = Some(PathBuf::from(args.get(i).context("--dir_ensemble needs a value")?.clone()));
            }
            "--dir_out" => {
                i += 1;
                cli.dir_out = Some(PathBuf::from(args.get(i).context("--dir_out needs a value")?.clone()));
            }
            "--tag" => {
                i += 1;
                cli.tag = Some(args.get(i).context("--tag needs a value")?.clone());
            }
            "--provenance-link" => {
                i += 1;
                cli.provenance_link =
                    Some(args.get(i).context("--provenance-link needs a value")?.clone());
            }
            "--code-version" => {
                i += 1;
                cli.code_version =
                    Some(args.get(i).context("--code-version needs a value")?.clone());
            }
            "--layer" => {
                i += 1;
                cli.layer = Some(parse_layer(args.get(i).context("--layer needs a value")?)?);
            }
            s if s.starts_with("--") => bail!("unknown flag {s}"),
            s => cli.config_path = Some(s.to_string()),
        }
        i += 1;
    }
    Ok(cli)
}

fn load_run_config(config_path: &Option<String>) -> Result<RunConfig> {
    if let Some(p) = config_path {
        let text = std::fs::read_to_string(p).with_context(|| format!("reading config {p}"))?;
        Ok(toml::from_str(&text)?)
    } else {
        let mut c = RunConfig::defaults();
        if let Some(v) = env::var_os("OHC_DIR_MEAN") { c.dir_mean = PathBuf::from(v); }
        if let Some(v) = env::var_os("OHC_DIR_ENSEMBLE") { c.dir_ensemble = PathBuf::from(v); }
        if let Some(v) = env::var_os("OHC_DIR_OUT") { c.dir_out = PathBuf::from(v); }
        if let Some(v) = env::var_os("OHC_ETOPO") { c.etopo_path = PathBuf::from(v); }
        if let Some(v) = env::var_os("OHC_BASINMASK") { c.basinmask_path = PathBuf::from(v); }
        Ok(c)
    }
}

fn resolve_layer(cli: &Cli) -> Result<LayerSpec> {
    match cli.layer {
        Some(l) => Ok(l),
        None => match env::var("OHC_LAYER") {
            Ok(v) => parse_layer(&v),
            Err(_) => bail!("--layer is required (one layer per run), e.g. --layer 15-20"),
        },
    }
}

fn main() -> Result<()> {
    let cli = parse_cli()?;
    let mut cfg = load_run_config(&cli.config_path)?;
    // CLI path overrides win over config.toml / env (handy for munging dirs per shell submission).
    if let Some(p) = &cli.dir_mean { cfg.dir_mean = p.clone(); }
    if let Some(p) = &cli.dir_ensemble { cfg.dir_ensemble = p.clone(); }
    if let Some(p) = &cli.dir_out { cfg.dir_out = p.clone(); }
    cfg.run_tag = match &cli.tag {
        Some(t) => t.clone(),
        None => match env::var("OHC_TAG") {
            Ok(v) => v,
            Err(_) => bail!("--tag is required (run identifier), e.g. --tag OP20260110"),
        },
    };
    // Strip whitespace; never lowercase or otherwise munge — the tag must match the provenance
    // record char-for-char (and it is the store's directory-name token).
    cfg.run_tag = cfg.run_tag.split_whitespace().collect();
    cfg.provenance_link = match &cli.provenance_link {
        Some(l) => l.clone(),
        None => match env::var("OHC_PROVENANCE_LINK") {
            Ok(v) => v,
            Err(_) => bail!("--provenance-link is required (pointer to the provenance record)"),
        },
    };
    cfg.code_version = match &cli.code_version {
        Some(v) => v.clone(),
        None => match env::var("OHC_CODE_VERSION") {
            Ok(v) => v,
            Err(_) => bail!("--code-version is required (link to the localgp_ogp_ingest commit/release)"),
        },
    };
    let layer = resolve_layer(&cli)?;
    let grid = GridDef::mapping();

    // Print the paths first, so a wrong dir is visible in context if the discovery below errors.
    match &cli.config_path {
        Some(p) => eprintln!("config: {p}"),
        None => eprintln!("config: none — using built-in defaults + env vars"),
    }
    eprintln!("  dir_mean     = {}", cfg.dir_mean.display());
    eprintln!("  dir_ensemble = {}", cfg.dir_ensemble.display());
    eprintln!("  dir_out      = {}", cfg.dir_out.display());
    eprintln!("  etopo        = {}", cfg.etopo_path.display());
    eprintln!("  basinmask    = {}", cfg.basinmask_path.display());

    // The time axis is discovered from the mapping files, not declared: whole calendar years, gaps
    // are fatal, and (with the ensemble) mean and members must cover the same axis.
    let years = cfg
        .discover_years(&layer, cli.no_ensemble)
        .with_context(|| format!("discovering the time axis for layer {}", layer.tag()))?;
    let slice = Slice { layer, years };

    eprintln!(
        "Run {}: layer {} dbar, years {}..={} (autodetected), {} timestep(s)",
        cfg.run_tag,
        slice.layer.tag(),
        slice.years[0],
        slice.years[1],
        slice.time_axis().len(),
    );

    eprintln!("Loading ancillary grids…");
    let etopo = ncread::read_etopo(&cfg.etopo_path, &grid)
        .with_context(|| format!("reading etopo {}", cfg.etopo_path.display()))?;
    let basin_id = basinmask::read_basin_id(&cfg.basinmask_path, &grid)
        .with_context(|| format!("reading basin mask {}", cfg.basinmask_path.display()))?;
    let cell_area = gridmod::cell_area(&grid);

    let t0 = Instant::now();
    eprintln!(">>> layer {} dbar{}", slice.layer.tag(),
        if cli.no_ensemble { " (mean-only, --no-ensemble)" } else { "" });
    let data = ingest::ingest_layer(&cfg, &slice, &grid, cli.no_ensemble)
        .with_context(|| format!("ingesting layer {}", slice.layer.tag()))?;
    let (never, incomplete) = masks::compute_validity(&data.field_mean);
    // Union the member NaN footprints (matches the original's mean∪members mask); None if mean-only.
    let ens_incomplete = data.field_ensemble.as_ref().map(masks::compute_ensemble_incomplete);
    let flags = masks::build_flags(
        &grid, &slice.layer, &etopo, Some(&basin_id),
        cfg.latitude_range_to_keep, &cfg.basins_to_remove, cfg.bathy_clip_m, &never, &incomplete,
        ens_incomplete.as_ref(),
    )?;
    let store = zarrwrite::write_layer_store(
        &cfg, &slice, &grid, &data, &flags, &etopo, &basin_id, &cell_area,
    )?;
    eprintln!("wrote {} ({:.1}s)", store.display(), t0.elapsed().as_secs_f64());
    Ok(())
}
