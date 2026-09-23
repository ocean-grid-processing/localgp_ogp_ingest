//! Minimal zarr **v3** writer (pure Rust).
//!
//! A zarr store is a directory tree: each array is a directory with a `zarr.json` metadata
//! file plus one file per chunk (`c/<i>/<j>/...`). We emit v3 with the `bytes` (little-endian)
//! + `gzip` codecs — gzip via flate2 is pure Rust and read natively by zarr-python/xarray.
//!
//! Layout per layer (see ../zarr_schema.md):
//!   field_mean      (time,lat,lon)         f64, 1 chunk   (units/long_name from [quantity])
//!   field_ensemble  (member,time,lat,lon)  f64, chunk (1,time,lat,lon) → one file per member;
//!                 the member count is whatever the mapping files held (absent when mean-only,
//!                 and then the `member` coordinate is absent too)
//!   mask_flags    (lat,lon)              u8
//!   etopo         (lat,lon)              f32
//!   basin_id      (lat,lon)              i16
//!   cell_area     (lat,lon)              f64
//!   coords: lon, lat, time, member

use anyhow::{Context, Result};
use flate2::{write::GzEncoder, Compression};
use ndarray::{Array2, ArrayView, Axis, Dimension};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::{RunConfig, Slice};
use crate::ingest::LayerData;
use crate::masks;
use crate::GridDef;

const GZIP_LEVEL: u32 = 5;

/// This pipeline step's identity, used to namespace its provenance attrs
/// (`<STAGE>_run_config` / `_run_facts` / `_code_version`). Fixed regardless of the binary name, so a
/// future rename doesn't move the keys. Downstream steps roll every `*_run_config` etc. forward and
/// add their own — so each step's block accretes without collisions, even across fan-ins.
const STAGE: &str = "localgp_ingest";

/// Trait for the small set of element types we serialize, with their zarr v3 names + fill.
trait ZElem: Copy {
    const DTYPE: &'static str;
    fn fill() -> Value;
    fn to_le(self, out: &mut Vec<u8>);
}
impl ZElem for f32 {
    const DTYPE: &'static str = "float32";
    fn fill() -> Value { json!("NaN") }
    fn to_le(self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_le_bytes()); }
}
impl ZElem for f64 {
    const DTYPE: &'static str = "float64";
    fn fill() -> Value { json!("NaN") }
    fn to_le(self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_le_bytes()); }
}
impl ZElem for i16 {
    const DTYPE: &'static str = "int16";
    fn fill() -> Value { json!(0) }
    fn to_le(self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_le_bytes()); }
}
impl ZElem for u8 {
    const DTYPE: &'static str = "uint8";
    fn fill() -> Value { json!(0) }
    fn to_le(self, out: &mut Vec<u8>) { out.push(self); }
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut e = GzEncoder::new(Vec::new(), Compression::new(GZIP_LEVEL));
    e.write_all(bytes)?;
    Ok(e.finish()?)
}

/// C-order little-endian bytes of any array view, gzip-compressed (one chunk).
fn chunk_bytes<T: ZElem, D: Dimension>(view: &ArrayView<T, D>) -> Result<Vec<u8>> {
    let mut raw = Vec::with_capacity(view.len() * std::mem::size_of::<T>());
    for &x in view.iter() {
        x.to_le(&mut raw); // .iter() is logical C order regardless of memory layout
    }
    gzip(&raw)
}

fn array_meta<T: ZElem>(
    shape: &[usize],
    chunk_shape: &[usize],
    dim_names: &[&str],
    attrs: Value,
) -> Value {
    json!({
        "zarr_format": 3,
        "node_type": "array",
        "shape": shape,
        "data_type": T::DTYPE,
        "chunk_grid": { "name": "regular",
                        "configuration": { "chunk_shape": chunk_shape } },
        "chunk_key_encoding": { "name": "default",
                                "configuration": { "separator": "/" } },
        "fill_value": T::fill(),
        "codecs": [
            { "name": "bytes", "configuration": { "endian": "little" } },
            { "name": "gzip",  "configuration": { "level": GZIP_LEVEL } }
        ],
        "attributes": attrs,
        "dimension_names": dim_names,
    })
}

fn write_json(path: &Path, v: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(v)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Write a single-chunk array (whole array in one chunk file `c/0/0/...`).
fn write_array_single_chunk<T: ZElem, D: Dimension>(
    root: &Path,
    name: &str,
    arr: &ArrayView<T, D>,
    dim_names: &[&str],
    attrs: Value,
) -> Result<()> {
    let shape: Vec<usize> = arr.shape().to_vec();
    let dir = root.join(name);
    fs::create_dir_all(&dir)?;
    write_json(&dir.join("zarr.json"), &array_meta::<T>(&shape, &shape, dim_names, attrs))?;
    // chunk key c/0/0/... (one zero per dim)
    let mut cdir = dir.join("c");
    for _ in 0..shape.len().saturating_sub(1) {
        cdir = cdir.join("0");
    }
    fs::create_dir_all(&cdir)?;
    let chunk_path = if shape.is_empty() { dir.join("c").join("0") } else { cdir.join("0") };
    fs::write(&chunk_path, chunk_bytes(arr)?)?;
    Ok(())
}

/// The per-run derived facts (everything not in `RunConfig`): the discovered axis, the layer, the
/// ensemble presence/size, and the grid — assembled from the objects the run actually used, so what
/// is stamped can't drift from what ran.
fn run_facts_value(slice: &Slice, grid: &GridDef, n_members: Option<usize>) -> Value {
    let members = match n_members {
        Some(n) => Value::from(n as u64),
        None => Value::Null,
    };
    json!({
        "layer_top": slice.layer.top,
        "layer_bottom": slice.layer.bottom,
        "year_min": slice.years[0],
        "year_max": slice.years[1],
        "n_timesteps": slice.time_axis().len() as u64,
        "ensemble": !members.is_null(),
        "n_members": members,
        "grid_nlat": grid.nlat() as u64,
        "grid_nlon": grid.nlon() as u64,
    })
}

/// Write the per-layer zarr store.
#[allow(clippy::too_many_arguments)]
pub fn write_layer_store(
    cfg: &RunConfig,
    slice: &Slice,
    grid: &GridDef,
    data: &LayerData,
    mask_flags: &Array2<u8>,
    etopo: &Array2<f64>,
    basin_id: &Array2<i16>,
    cell_area: &Array2<f64>,
) -> Result<PathBuf> {
    let layer = &slice.layer;
    let root = cfg.store_path(layer, slice.years);
    fs::create_dir_all(&root)?;

    // ---- group metadata ----
    let (time_days, (y0, m0)) = slice.time_days_since_start();
    // Provenance blocks: the whole resolved config (cold-serialized) plus the derived run facts, both
    // compact JSON strings — one line each, so they read cleanly in `ncdump -h` (pretty-printing
    // there collapses to `\n`-littered noise) and travel unchanged into the downstream netCDF attrs.
    let n_members = data.field_ensemble.as_ref().map(|e| e.shape()[0]);
    let run_config_json = serde_json::to_string(cfg).context("serializing run_config")?;
    let run_facts_json = serde_json::to_string(&run_facts_value(slice, grid, n_members))
        .context("serializing run_facts")?;
    let mut group_attrs = json!({
        "Conventions": "CF-1.10",
        "title": format!("LocalGP {} — {}, {}-{} dbar",
                         cfg.quantity.long_name, cfg.run_tag, layer.top, layer.bottom),
        "source": format!("LocalGP {}; var={}; run={}", cfg.model_name, cfg.var_name, cfg.run_tag),
        "mapped_fields_tag": cfg.run_tag.clone(),
        "provenance_tag": cfg.run_tag.clone(),          // global run token (shared, inherited downstream)
        "provenance_link": cfg.provenance_link.clone(), // global: pointer to this run's documentation
        "var_name": cfg.var_name,
        "model_name": cfg.model_name,
        "layer_top": layer.top,
        "layer_bottom": layer.bottom,
        "domain": "lon 20.5..379.5E, lat -89.5..89.5N, 1deg",
        // the mapped quantity and its scaling, as one compact JSON string (travels unchanged into
        // the downstream netCDF attrs, like the provenance blocks)
        "quantity": serde_json::to_string(&cfg.quantity).context("serializing quantity")?,
    });
    // Legacy standalone attrs for readers that still look them up by name (publish, verify,
    // derive, gcos); present only when the quantity has these terms.
    if let (Some(obj), Some(cp0), Some(rho0)) =
        (group_attrs.as_object_mut(), cfg.quantity.term("cp0"), cfg.quantity.term("rho0"))
    {
        obj.insert("cp0".into(), json!(cp0));
        obj.insert("rho0".into(), json!(rho0));
    }
    // Stage-namespaced local provenance — keyed off STAGE so a binary rename doesn't move them, and
    // so downstream steps can roll every `*_run_config` / `_run_facts` / `_code_version` forward as-is.
    if let Some(obj) = group_attrs.as_object_mut() {
        obj.insert(format!("{STAGE}_code_version"), Value::String(cfg.code_version.clone()));
        obj.insert(format!("{STAGE}_run_config"), Value::String(run_config_json));
        obj.insert(format!("{STAGE}_run_facts"), Value::String(run_facts_json));
    }
    write_json(
        &root.join("zarr.json"),
        &json!({ "zarr_format": 3, "node_type": "group", "attributes": group_attrs }),
    )?;

    // ---- coordinates ----
    let lon = Array2::from_shape_vec((grid.nlon(), 1), grid.lon.clone())?; // reuse 2D helper via 1D view
    write_array_single_chunk(&root, "lon", &lon.column(0), &["lon"],
        json!({"units":"degrees_east","standard_name":"longitude"}))?;
    let lat = Array2::from_shape_vec((grid.nlat(), 1), grid.lat.clone())?;
    write_array_single_chunk(&root, "lat", &lat.column(0), &["lat"],
        json!({"units":"degrees_north","standard_name":"latitude"}))?;
    let time = Array2::from_shape_vec((time_days.len(), 1), time_days)?;
    write_array_single_chunk(&root, "time", &time.column(0), &["time"],
        json!({"units": format!("days since {:04}-{:02}-15", y0, m0), "calendar":"proleptic_gregorian"}))?;
    // The member coordinate exists only when the ensemble does; its length is the ensemble's.
    if let Some(nm) = n_members {
        let member: Vec<i16> = (1..=nm as i16).collect();
        let member = Array2::from_shape_vec((member.len(), 1), member)?;
        write_array_single_chunk(&root, "member", &member.column(0), &["member"],
            json!({"long_name":"conditional simulation member"}))?;
    }

    // ---- ancillary grids ----
    write_array_single_chunk(&root, "etopo", &etopo.view(), &["lat","lon"],
        json!({"units":"m","long_name":"bathymetry (relief), negative below sea level"}))?;
    write_array_single_chunk(&root, "basin_id", &basin_id.view(), &["lat","lon"],
        json!({"long_name":"WOA basin id (surface)"}))?;
    write_array_single_chunk(&root, "cell_area", &cell_area.view(), &["lat","lon"],
        json!({"units":"m2","long_name":"grid cell area"}))?;
    write_array_single_chunk(&root, "mask_flags", &mask_flags.view(), &["lat","lon"],
        json!({
            "long_name":"mask flag band",
            "flag_masks": masks::FLAG_MASKS.to_vec(),
            "flag_meanings": masks::FLAG_MEANINGS,
        }))?;

    // ---- field_mean (single chunk) ----
    let units = &cfg.quantity.units;
    let long_name = &cfg.quantity.long_name;
    write_array_single_chunk(&root, "field_mean", &data.field_mean.view(), &["time","lat","lon"],
        json!({"units": units, "long_name": format!("{long_name} (posterior mean)")}))?;

    // ---- field_ensemble: (member,time,lat,lon), chunk (1,time,lat,lon) per member ----
    // Omitted entirely for a mean-only store (ingested with --no-ensemble).
    if let Some(ens) = &data.field_ensemble {
        let (nm, nt, nlat, nlon) = ens.dim();
        let dir = root.join("field_ensemble");
        fs::create_dir_all(&dir)?;
        write_json(
            &dir.join("zarr.json"),
            &array_meta::<f64>(
                &[nm, nt, nlat, nlon],
                &[1, nt, nlat, nlon],
                &["member", "time", "lat", "lon"],
                json!({"units": units, "long_name": format!("{long_name} (conditional simulations)")}),
            ),
        )?;
        for m in 0..nm {
            let slab = ens.index_axis(Axis(0), m); // [time,lat,lon]
            // chunk key c/<m>/0/0/0
            let cdir = dir.join("c").join(m.to_string()).join("0").join("0");
            fs::create_dir_all(&cdir)?;
            fs::write(cdir.join("0"), chunk_bytes(&slab)?)?;
        }
    }

    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LayerSpec;

    #[test]
    fn run_facts_capture_axis_layer_and_ensemble() {
        let slice = Slice { layer: LayerSpec { top: 15, bottom: 20 }, years: [2004, 2005] };
        let grid = GridDef::mapping();

        let facts = run_facts_value(&slice, &grid, Some(100));
        assert_eq!(facts["layer_top"].as_i64(), Some(15));
        assert_eq!(facts["layer_bottom"].as_i64(), Some(20));
        assert_eq!(facts["year_min"].as_i64(), Some(2004));
        assert_eq!(facts["year_max"].as_i64(), Some(2005));
        assert_eq!(facts["n_timesteps"].as_u64(), Some(24)); // two whole years
        assert_eq!(facts["ensemble"].as_bool(), Some(true));
        assert_eq!(facts["n_members"].as_u64(), Some(100));
        assert_eq!(facts["grid_nlat"].as_u64(), Some(grid.nlat() as u64));
        assert_eq!(facts["grid_nlon"].as_u64(), Some(grid.nlon() as u64));

        // mean-only: ensemble off, member count null
        let mean_only = run_facts_value(&slice, &grid, None);
        assert_eq!(mean_only["ensemble"].as_bool(), Some(false));
        assert!(mean_only["n_members"].is_null());
    }
}
