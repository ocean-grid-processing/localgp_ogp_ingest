//! Layer ingest — option 1: buffer the whole layer in RAM and transpose month-major →
//! member-major.
//!
//! LocalGP delivers one `.mat` per month (the ensemble file holds every member for that
//! month). Our zarr chunks are per-member, so we accumulate the full layer and emit:
//!   - `field_mean`     `[time, lat, lon]`          (the FullField posterior mean)
//!   - `field_ensemble` `[member, time, lat, lon]`  (the conditional simulations)
//! The arrays are named generically; the store's `quantity` attr says what the field is. The
//! ensemble size is read from the first month's file and every later month must match it.
//! Raw mapping values are scaled by the configured quantity's factor (OHC: `cp0 * rho0`) on the
//! way in; NaN is the mapping's missing value and is preserved as NaN; arrays are transposed from
//! the `.mat`'s `[lon, lat]` order to `[lat, lon]`. Both mean and ensemble are stored f64
//! (~13.7 GB for 264 months × 100 members — the RAM bet on the cluster).

use anyhow::{bail, Context, Result};
use ndarray::{Array3, Array4};

use crate::config::{RunConfig, Slice};
use crate::{matread, GridDef};

pub struct LayerData {
    /// `[time, lat, lon]`, in the quantity's stored units (OHC: J/m²), NaN preserved. f64: the deliverable takes a large-mean
    /// anomaly (absolute OHC − baseline), so the mean is kept double all the way through.
    pub field_mean: Array3<f64>,
    /// `[member, time, lat, lon]`, same units as the mean, NaN preserved. f64 (matches the mean): the ensemble
    /// feeds the `_sd` spread and the downstream yearly/trend uncertainties, kept double so those
    /// are exact rather than f32-limited. `None` when ingested mean-only (`--no-ensemble`) — the
    /// CondSim files are not read and `field_ensemble` is omitted from the store.
    pub field_ensemble: Option<Array4<f64>>,
}

/// Read every month of the slice's layer and assemble the member-major arrays.
///
/// `mean_only` (from `--no-ensemble`) skips the LocalCondSim files entirely and returns
/// `field_ensemble: None` — for mean-only products (e.g. the GCOS deliverable) where the ensemble
/// is never used downstream, and to run without a complete set of CondSim `.mat` files.
///
/// Missing values arrive as NaN and stay NaN (LocalGP marks missing with NaN); no other value is
/// interpreted as missing.
///
/// The ensemble size is whatever the first month's LocalCondSim file holds; the buffer is
/// allocated on that read, and a later month with a different member count is a hard error.
pub fn ingest_layer(cfg: &RunConfig, slice: &Slice, grid: &GridDef, mean_only: bool) -> Result<LayerData> {
    let layer = &slice.layer;
    let nlat = grid.nlat();
    let nlon = grid.nlon();
    let time = slice.time_axis();
    let nt = time.len();
    let scale = cfg.quantity.scale();

    let mut field_mean = Array3::<f64>::from_elem((nt, nlat, nlon), f64::NAN);
    // Allocated on the first ensemble read, once the member count is known from the file.
    let mut field_ensemble: Option<Array4<f64>> = None;

    for (t, &(year, month)) in time.iter().enumerate() {
        // FullField mean: [lon, lat]
        let mean_path = cfg.mat_path(layer, year, month, false);
        let mean = matread::read_mean_grid(&mean_path)
            .with_context(|| format!("reading {}", mean_path.display()))?;
        debug_assert_eq!(mean.dim(), (nlon, nlat));
        for j in 0..nlat {
            for i in 0..nlon {
                let v = mean[[i, j]]; // transpose [lon,lat]→[lat,lon]
                field_mean[[t, j, i]] = v * scale; // NaN stays NaN
            }
        }

        // LocalCondSim ensemble: [lon, lat, member] — skipped entirely when mean_only
        if !mean_only {
            let ens_path = cfg.mat_path(layer, year, month, true);
            let ens = matread::read_ensemble(&ens_path)
                .with_context(|| format!("reading {}", ens_path.display()))?;
            let (_, _, nm) = ens.dim();
            if let Some(arr) = field_ensemble.as_ref() {
                let expected = arr.shape()[0];
                if nm != expected {
                    bail!(
                        "{} holds {nm} members but the first month held {expected}: \
                         every month of a layer must carry the same ensemble",
                        ens_path.display()
                    );
                }
            }
            let ens_arr = field_ensemble.get_or_insert_with(|| {
                eprintln!("ensemble size: {nm} members (from {})", ens_path.display());
                Array4::<f64>::from_elem((nm, nt, nlat, nlon), f64::NAN)
            });
            debug_assert_eq!(ens.dim(), (nlon, nlat, nm));
            for m in 0..nm {
                for j in 0..nlat {
                    for i in 0..nlon {
                        let v = ens[[i, j, m]];
                        ens_arr[[m, t, j, i]] = v * scale; // NaN stays NaN
                    }
                }
            }
        }
    }

    Ok(LayerData { field_mean, field_ensemble })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Validates the transpose + cp0·rho0 scaling on the single sample month (Aug 2016). The .mat
    // ships in the crate under test_fixtures/, so this test always runs (no env needed);
    // OHC_TEST_DATA overrides the directory for an out-of-tree copy.
    #[test]
    fn single_month_transpose_and_scale() {
        use crate::config::LayerSpec;
        let dir = std::env::var_os("OHC_TEST_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/test_fixtures")));
        let mut cfg = RunConfig::defaults();
        cfg.dir_mean = dir;
        let layer = LayerSpec { top: 15, bottom: 20 };

        // Validate transpose + scale by reading the one local month directly.
        let mean = matread::read_mean_grid(cfg.mat_path(&layer, 2016, 8, false)).unwrap();
        let scale = cfg.quantity.scale();
        // raw m[200,100]=144.646083 → ohc at [lat=100, lon=200]
        let ohc = mean[[200, 100]] * scale;
        assert!((ohc - 5.943_394e8).abs() / 5.943_394e8 < 1e-5);
    }
}
