//! Configuration.
//!
//! [`RunConfig`] holds the static constants + paths for a product/environment (one per
//! `config.toml`). The per-run **slice** — which single layer, over which months — is
//! supplied separately as a [`Slice`] and is mandatory on the command line. One run
//! processes exactly one layer; batching across layers is the job scheduler's job.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Whether the mapped quantity is a per-area density that sums over area and stacks over layers
/// (`extensive`, e.g. OHC in J/m²) or a per-cell value that does neither (`intensive`, e.g. a
/// mixed layer depth in m). Recorded for downstream stages; ingest itself treats both alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Extensive,
    Intensive,
}

/// What the mapped grid is, and how it is scaled into the store and the submission. One
/// `[quantity]` table per config; every field has a default that reproduces the OHC product.
///
/// `scale_terms` is a named table of factors whose product is applied to every raw mapping value
/// at ingest (`{cp0 = 3989.244, rho0 = 1030.0}` turns integrated temperature in K·m into J/m²).
/// The names are kept, not just the product, so the physics stays legible in the provenance and a
/// downstream step can recover a term by name. An empty table is the identity: the store holds
/// the mapping values as they are.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quantity {
    /// short slug naming the quantity (`ohc`, `mld`, …)
    #[serde(default = "default_quantity_name")]
    pub name: String,
    #[serde(default = "default_kind")]
    pub kind: Kind,
    /// units of the stored (scaled) field
    #[serde(default = "default_units")]
    pub units: String,
    #[serde(default = "default_long_name")]
    pub long_name: String,
    /// named factors; their product scales the raw mapping values at ingest (empty = 1)
    #[serde(default = "default_scale_terms")]
    pub scale_terms: BTreeMap<String, f64>,
    /// factor applied when publishing the store to a submission (OHC: J/m² → TJ/m²)
    #[serde(default = "default_publish_scale")]
    pub publish_scale: f64,
    /// units of the published field
    #[serde(default = "default_publish_units")]
    pub publish_units: String,
}

fn default_quantity_name() -> String { "ohc".into() }
fn default_kind() -> Kind { Kind::Extensive }
fn default_units() -> String { "J/m2".into() }
fn default_long_name() -> String { "ocean heat content".into() }
fn default_scale_terms() -> BTreeMap<String, f64> {
    BTreeMap::from([("cp0".to_string(), crate::consts::CP0), ("rho0".to_string(), crate::consts::RHO0)])
}
fn default_publish_scale() -> f64 { 1e-12 }
fn default_publish_units() -> String { "TJ/m^2".into() }

impl Quantity {
    /// The OHC product: integrated potential temperature × cp0·rho0, J/m² in the store, TJ/m²
    /// when published. This is the default when a config has no `[quantity]` table.
    pub fn ohc() -> Self {
        Quantity {
            name: default_quantity_name(),
            kind: default_kind(),
            units: default_units(),
            long_name: default_long_name(),
            scale_terms: default_scale_terms(),
            publish_scale: default_publish_scale(),
            publish_units: default_publish_units(),
        }
    }

    /// The ingest factor: the product of `scale_terms` (1 when empty).
    pub fn scale(&self) -> f64 {
        self.scale_terms.values().product()
    }

    /// One named term, if the table has it (e.g. `cp0`).
    pub fn term(&self, name: &str) -> Option<f64> {
        self.scale_terms.get(name).copied()
    }
}

/// One mapped pressure layer, bounds in dbar (shallow `top`, deep `bottom`).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct LayerSpec {
    pub top: i32,
    pub bottom: i32,
}

impl LayerSpec {
    pub fn tag(&self) -> String {
        format!("{}_{}", self.top, self.bottom)
    }
}

/// Static constants + paths for a run (everything except the per-run slice; see `Slice`). The whole
/// resolved struct is cold-serialized into the store's `run_config` provenance attr, so every field
/// here — set anywhere in the precedence chain, or left at its default — is recorded verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    /// run identifier, set per-run via `--tag` (this default is a placeholder)
    #[serde(default = "default_tag")]
    pub run_tag: String,
    /// pointer to the provenance record for this run, set per-run via `--provenance-link`
    /// (the `--tag` metadata document). Required at runtime; this default is a placeholder.
    #[serde(default)]
    pub provenance_link: String,
    /// link to the exact ohc_ingest code (a commit or release URL), set per-run via `--code-version`.
    /// Required at runtime; this default is a placeholder.
    #[serde(default)]
    pub code_version: String,
    /// e.g. "potentialTemperature"
    pub var_name: String,
    /// e.g. "SpaceTimeTrend"
    pub model_name: String,
    /// keep cells with lat in [lo, hi]
    pub latitude_range_to_keep: [f64; 2],
    /// basin ids to drop (land + marginal seas)
    pub basins_to_remove: Vec<i16>,
    /// uniform bathymetry clip depth (m): flag cells shallower than this for every layer
    /// (`bed_above_clip`). `None` = no clip. WMO/GCOS product uses 300.
    #[serde(default)]
    pub bathy_clip_m: Option<f64>,
    /// dir holding the FullField mean `.mat` files (may be omitted here and set via `--dir_mean`)
    #[serde(default = "default_dir")]
    pub dir_mean: PathBuf,
    /// dir holding the LocalCondSim ensemble `.mat` files (or set via `--dir_ensemble`)
    #[serde(default = "default_dir")]
    pub dir_ensemble: PathBuf,
    /// where the zarr stores are written (or set via `--dir_out`)
    #[serde(default = "default_dir")]
    pub dir_out: PathBuf,
    /// path to etopo60.cdf
    pub etopo_path: PathBuf,
    /// path to basinmask_04.msk
    pub basinmask_path: PathBuf,
    /// the mapped quantity and its scaling (`[quantity]` table; defaults to the OHC product)
    #[serde(default = "Quantity::ohc")]
    pub quantity: Quantity,
}

fn default_tag() -> String { "UNSET".into() }
fn default_dir() -> PathBuf { PathBuf::from(".") }

impl RunConfig {
    /// Constant defaults (current LocalGP conventions). The run tag and paths are
    /// placeholders — set them via `--tag` / config.toml / env vars.
    pub fn defaults() -> Self {
        RunConfig {
            run_tag: default_tag(), // required via --tag
            provenance_link: String::new(), // required via --provenance-link
            code_version: String::new(), // required via --code-version
            var_name: "potentialTemperature".into(),
            model_name: "SpaceTimeTrend".into(),
            latitude_range_to_keep: [-64.5, 64.5],
            basins_to_remove: vec![0, 5, 6, 7, 8, 9, 53],
            bathy_clip_m: None,
            dir_mean: PathBuf::from("."),
            dir_ensemble: PathBuf::from("."),
            dir_out: PathBuf::from("."),
            etopo_path: PathBuf::from("etopo60.cdf"),
            basinmask_path: PathBuf::from("basinmask_04.msk"),
            quantity: Quantity::ohc(),
        }
    }

    fn fname_prefix(&self, cond_sim: bool) -> String {
        let cs = if cond_sim { "LocalCondSim" } else { "" };
        format!("{}FullField{}{}", self.var_name, cs, self.model_name)
    }

    /// Full path to one monthly `.mat` for a layer.
    /// `cond_sim=false` → FullField mean; `true` → LocalCondSim ensemble.
    pub fn mat_path(&self, layer: &LayerSpec, year: i32, month: u32, cond_sim: bool) -> PathBuf {
        let fname = format!(
            "{}_{}_{:02}_{}.mat",
            self.fname_prefix(cond_sim),
            layer.tag(),
            month,
            year
        );
        let dir = if cond_sim { &self.dir_ensemble } else { &self.dir_mean };
        dir.join(fname)
    }

    /// zarr store directory for a layer. `years` is the discovered `[Ymin, Ymax]` data span, so the
    /// store name carries the years it covers: `ohc_<tag>_<Ymin>_<Ymax>_plev<layer>.zarr`.
    pub fn store_path(&self, layer: &LayerSpec, years: [i32; 2]) -> PathBuf {
        self.dir_out.join(format!(
            "ohc_{}_{}_{}_plev{}.zarr",
            self.run_tag, years[0], years[1], layer.tag()
        ))
    }

    /// Fixed filename stem for a layer's monthly files: `{prefix}_{top}_{bottom}_`. A file on disk is
    /// `{stem}{MM}_{YYYY}.mat`. The trailing underscore keeps `15_20` from matching `15_200`.
    fn mat_stem(&self, layer: &LayerSpec, cond_sim: bool) -> String {
        format!("{}_{}_", self.fname_prefix(cond_sim), layer.tag())
    }

    /// Discover the layer's year range by scanning the mapping directories — the run's time axis is
    /// whatever is on disk. LocalGP writes whole calendar years, so the discovered months must tile
    /// every year `1..=12` with no gap; a hole is a hard error naming the missing months (a missing or
    /// misnamed file). With the ensemble on, the mean and ensemble directories must cover the
    /// identical axis. Returns the inclusive `[Ymin, Ymax]`.
    pub fn discover_years(&self, layer: &LayerSpec, mean_only: bool) -> Result<[i32; 2]> {
        let mean = scan_axis(&self.dir_mean, &self.mat_stem(layer, false))?;
        let years = validate_complete_years(&mean, "FullField mean", &self.dir_mean, layer)?;
        if !mean_only {
            let ens = scan_axis(&self.dir_ensemble, &self.mat_stem(layer, true))?;
            let ens_years = validate_complete_years(&ens, "LocalCondSim ensemble", &self.dir_ensemble, layer)?;
            if ens != mean {
                bail!(
                    "mean/ensemble time axes disagree for layer {}: mean covers {}..={}, ensemble {}..={} \
                     — fix the missing files, or pass --no-ensemble for a mean-only product",
                    layer.tag(), years[0], years[1], ens_years[0], ens_years[1]
                );
            }
        }
        Ok(years)
    }
}

/// Read one directory, returning the `(year, month)` set of files named `{stem}{MM}_{YYYY}.mat`.
/// Names that don't start with the stem are ignored (other layers, other files); a name that starts
/// with the stem but doesn't end `MM_YYYY.mat` is a hard error (a malformed mapping filename).
fn scan_axis(dir: &Path, stem: &str) -> Result<BTreeSet<(i32, u32)>> {
    let mut set = BTreeSet::new();
    let entries = std::fs::read_dir(dir).with_context(|| format!("reading mapping dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if let Some((y, m)) = parse_month_year(&name.to_string_lossy(), stem)? {
            set.insert((y, m));
        }
    }
    Ok(set)
}

/// Parse `(year, month)` from `{stem}{MM}_{YYYY}.mat`. `Ok(None)` if the name isn't one of this
/// layer's files (doesn't carry the stem, or isn't a `.mat`); `Err` if it carries the stem but the
/// `MM_YYYY` tail is unparseable or the month is out of range.
fn parse_month_year(name: &str, stem: &str) -> Result<Option<(i32, u32)>> {
    let rest = match name.strip_prefix(stem).and_then(|r| r.strip_suffix(".mat")) {
        Some(r) => r,
        None => return Ok(None),
    };
    let (mm, yyyy) = rest
        .split_once('_')
        .with_context(|| format!("malformed mapping filename {name:?}: expected {stem}MM_YYYY.mat"))?;
    let month: u32 = mm.parse().with_context(|| format!("bad month in {name:?}"))?;
    let year: i32 = yyyy.parse().with_context(|| format!("bad year in {name:?}"))?;
    if !(1..=12).contains(&month) {
        bail!("month out of range 1..=12 in {name:?}");
    }
    Ok(Some((year, month)))
}

/// Require the discovered axis to be whole calendar years: every year from min to max present with
/// all twelve months. Returns `[Ymin, Ymax]`; errors (listing the holes) otherwise. LocalGP writes
/// complete years, so a gap means missing or misnamed files.
fn validate_complete_years(
    axis: &BTreeSet<(i32, u32)>, label: &str, dir: &Path, layer: &LayerSpec,
) -> Result<[i32; 2]> {
    if axis.is_empty() {
        bail!("no {label} .mat files found for layer {} in {}", layer.tag(), dir.display());
    }
    let ymin = axis.iter().map(|&(y, _)| y).min().unwrap();
    let ymax = axis.iter().map(|&(y, _)| y).max().unwrap();
    let missing: Vec<String> = (ymin..=ymax)
        .flat_map(|y| (1..=12u32).map(move |m| (y, m)))
        .filter(|ym| !axis.contains(ym))
        .map(|(y, m)| format!("{y}-{m:02}"))
        .collect();
    if !missing.is_empty() {
        bail!(
            "{label} for layer {} spans {ymin}..={ymax} but is missing {} month(s): {} \
             (LocalGP years must be complete)",
            layer.tag(), missing.len(), missing.join(", ")
        );
    }
    Ok([ymin, ymax])
}

/// One unit of work: a single layer over an (inclusive) year range. The year range is discovered
/// from the mapping files (see `RunConfig::discover_years`), and every year is whole (all 12 months).
#[derive(Debug, Clone)]
pub struct Slice {
    pub layer: LayerSpec,
    pub years: [i32; 2],
}

impl Slice {
    /// Monthly (year, month) axis, day-15 implied — every month of every year in range.
    pub fn time_axis(&self) -> Vec<(i32, u32)> {
        let mut v = Vec::new();
        for y in self.years[0]..=self.years[1] {
            for m in 1..=12u32 {
                v.push((y, m));
            }
        }
        v
    }

    /// CF time values: days since day-15 of the first month of the slice.
    pub fn time_days_since_start(&self) -> (Vec<f64>, (i32, u32)) {
        let axis = self.time_axis();
        let (y0, m0) = axis[0];
        let base = days_from_civil(y0, m0, 15);
        let days = axis
            .iter()
            .map(|&(y, m)| (days_from_civil(y, m, 15) - base) as f64)
            .collect();
        (days, (y0, m0))
    }
}

/// Days since 1970-01-01 (proleptic Gregorian). Hinnant's algorithm.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let m = m as i64;
    let d = d as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse `--layer` / `OHC_LAYER`: exactly one `top-bottom` (inner separator `-`, `_`, or `:`),
/// e.g. `"15-20"`, `"300_700"`, `"700:1850"`. Rejects multiple layers.
pub fn parse_layer(s: &str) -> Result<LayerSpec> {
    if s.contains(',') {
        bail!("--layer takes exactly one layer; got a list: {s:?}");
    }
    let parts: Vec<&str> = s.split(|c| c == '-' || c == '_' || c == ':').collect();
    if parts.len() != 2 {
        bail!("bad --layer value {s:?}, expected top-bottom (one layer)");
    }
    Ok(LayerSpec {
        top: parts[0].trim().parse()?,
        bottom: parts[1].trim().parse()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{CP0, RHO0};

    #[test]
    fn run_config_cold_serializes_all_fields() {
        // The provenance block is the whole resolved struct — every field lands, defaults included.
        let mut cfg = RunConfig::defaults();
        cfg.code_version = "https://github.com/argovis/ohc_ingest/commit/abc123".into();
        let json = serde_json::to_string(&cfg).unwrap();
        for key in [
            "run_tag", "provenance_link", "code_version", "var_name", "model_name",
            "latitude_range_to_keep", "basins_to_remove", "bathy_clip_m",
            "dir_mean", "dir_ensemble", "dir_out", "etopo_path", "basinmask_path", "quantity",
            "scale_terms", "cp0", "rho0", "publish_scale",
        ] {
            assert!(json.contains(key), "run_config missing {key}: {json}");
        }
        assert!(json.contains("abc123"));
    }

    #[test]
    fn quantity_defaults_to_ohc_and_scales_by_the_product_of_terms() {
        let q = Quantity::ohc();
        assert_eq!(q.name, "ohc");
        assert_eq!(q.kind, Kind::Extensive);
        assert_eq!(q.scale(), crate::consts::CP0 * crate::consts::RHO0);
        assert_eq!(q.term("cp0"), Some(crate::consts::CP0));
        assert_eq!(q.term("nope"), None);
        // a config with no [quantity] table is the OHC product
        let cfg: RunConfig = toml::from_str(
            "var_name='potentialTemperature'\nmodel_name='SpaceTimeTrend'\n\
             latitude_range_to_keep=[-64.5,64.5]\nbasins_to_remove=[0]\n\
             etopo_path='e'\nbasinmask_path='b'\n",
        ).unwrap();
        assert_eq!(cfg.quantity.scale(), CP0 * RHO0);
    }

    #[test]
    fn quantity_table_parses_and_empty_terms_are_identity() {
        let cfg: RunConfig = toml::from_str(
            "var_name='mld'\nmodel_name='SpaceTimeTrend'\n\
             latitude_range_to_keep=[-64.5,64.5]\nbasins_to_remove=[0]\n\
             etopo_path='e'\nbasinmask_path='b'\n\
             [quantity]\nname='mld'\nkind='intensive'\nunits='m'\nlong_name='mixed layer depth'\n\
             scale_terms={}\npublish_scale=1.0\npublish_units='m'\n",
        ).unwrap();
        let q = &cfg.quantity;
        assert_eq!(q.kind, Kind::Intensive);
        assert_eq!(q.scale(), 1.0);
        assert!(q.scale_terms.is_empty());
        assert_eq!(q.publish_scale, 1.0);
        // a partial table keeps the defaults for what it omits
        let cfg: RunConfig = toml::from_str(
            "var_name='x'\nmodel_name='y'\nlatitude_range_to_keep=[-64.5,64.5]\nbasins_to_remove=[0]\n\
             etopo_path='e'\nbasinmask_path='b'\n[quantity]\nscale_terms={a=2.0,b=3.0}\n",
        ).unwrap();
        assert_eq!(cfg.quantity.scale(), 6.0);
        assert_eq!(cfg.quantity.name, "ohc");
    }

    #[test]
    fn parse_layer_one_layer_only() {
        let l = parse_layer("15-20").unwrap();
        assert_eq!((l.top, l.bottom), (15, 20));
        assert_eq!(parse_layer("300_700").unwrap().bottom, 700);
        assert!(parse_layer("15-20,300-700").is_err()); // no lists
        assert!(parse_layer("15").is_err());
    }

    #[test]
    fn parse_month_year_matches_and_rejects() {
        let stem = "potentialTemperatureFullFieldSpaceTimeTrend_15_20_";
        assert_eq!(
            parse_month_year(&format!("{stem}08_2016.mat"), stem).unwrap(),
            Some((2016, 8))
        );
        // other layer / other file: ignored, not an error
        assert_eq!(parse_month_year("something_else_08_2016.mat", stem).unwrap(), None);
        // 15_20 stem must not swallow 15_200
        let other = "potentialTemperatureFullFieldSpaceTimeTrend_15_200_08_2016.mat";
        assert_eq!(parse_month_year(other, stem).unwrap(), None);
        // carries the stem but the tail is malformed / out of range: hard error
        assert!(parse_month_year(&format!("{stem}13_2016.mat"), stem).is_err());
        assert!(parse_month_year(&format!("{stem}zz_2016.mat"), stem).is_err());
    }

    #[test]
    fn validate_complete_years_ok_and_holes() {
        let layer = LayerSpec { top: 15, bottom: 20 };
        let dir = Path::new("/tmp");
        let mut full: BTreeSet<(i32, u32)> = BTreeSet::new();
        for y in 2004..=2005 {
            for m in 1..=12 {
                full.insert((y, m));
            }
        }
        assert_eq!(
            validate_complete_years(&full, "mean", dir, &layer).unwrap(),
            [2004, 2005]
        );
        // a mid-record hole is fatal
        let mut holed = full.clone();
        holed.remove(&(2004, 7));
        assert!(validate_complete_years(&holed, "mean", dir, &layer).is_err());
        // a partial final year is a hole too (LocalGP years are whole)
        let mut partial = full.clone();
        partial.remove(&(2005, 12));
        assert!(validate_complete_years(&partial, "mean", dir, &layer).is_err());
        // empty set errors
        assert!(validate_complete_years(&BTreeSet::new(), "mean", dir, &layer).is_err());
    }

    #[test]
    fn discover_years_scans_and_validates() {
        // build a throwaway dir of touch-files and confirm discovery + the mean/ensemble agreement.
        let base = std::env::temp_dir().join(format!("ohc_ingest_discover_{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok(); // clean slate if a prior run left it behind
        let mean = base.join("mean");
        let ens = base.join("ens");
        std::fs::create_dir_all(&mean).unwrap();
        std::fs::create_dir_all(&ens).unwrap();
        let mut cfg = RunConfig::defaults();
        cfg.dir_mean = mean.clone();
        cfg.dir_ensemble = ens.clone();
        let layer = LayerSpec { top: 15, bottom: 20 };
        // mat_path routes to dir_mean / dir_ensemble by the cond_sim flag.
        let touch = |cond_sim: bool, year: i32, month: u32| {
            std::fs::write(cfg.mat_path(&layer, year, month, cond_sim), b"").unwrap();
        };
        for y in 2004..=2005 {
            for m in 1..=12 {
                touch(false, y, m);
                touch(true, y, m);
            }
        }
        assert_eq!(cfg.discover_years(&layer, false).unwrap(), [2004, 2005]);
        assert_eq!(cfg.discover_years(&layer, true).unwrap(), [2004, 2005]);

        // drop one ensemble month: mean-only still fine, ensemble run must complain.
        std::fs::remove_file(cfg.mat_path(&layer, 2005, 6, true)).unwrap();
        assert_eq!(cfg.discover_years(&layer, true).unwrap(), [2004, 2005]);
        assert!(cfg.discover_years(&layer, false).is_err());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn filenames_match_localgp_convention() {
        let c = RunConfig::defaults();
        let l = LayerSpec { top: 15, bottom: 20 };
        assert!(c
            .mat_path(&l, 2016, 8, false)
            .to_string_lossy()
            .ends_with("potentialTemperatureFullFieldSpaceTimeTrend_15_20_08_2016.mat"));
        assert!(c.mat_path(&l, 2016, 8, true).to_string_lossy().ends_with(
            "potentialTemperatureFullFieldLocalCondSimSpaceTimeTrend_15_20_08_2016.mat"
        ));
    }

    #[test]
    fn slice_time_axis_and_days() {
        // every year is whole: one year -> 12 months, Jan..Dec.
        let s = Slice { layer: LayerSpec { top: 15, bottom: 20 }, years: [2016, 2016] };
        let axis = s.time_axis();
        assert_eq!(axis.len(), 12);
        assert_eq!(axis[0], (2016, 1));
        assert_eq!(axis[11], (2016, 12));
        let (days, base) = s.time_days_since_start();
        assert_eq!(base, (2016, 1));
        assert_eq!(days[0], 0.0);
        assert_eq!(days[1], 31.0); // Jan15 -> Feb15

        // two years -> 24 months, contiguous across the year boundary.
        let s2 = Slice { layer: LayerSpec { top: 15, bottom: 20 }, years: [2004, 2005] };
        assert_eq!(s2.time_axis().len(), 24);
        assert_eq!(s2.time_axis()[12], (2005, 1));
    }
}
