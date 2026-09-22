//! ohc_ingest — turn LocalGP `.mat` output into clean OHC zarr grids + bit-band masks.
//!
//! Scope: read `.mat` (FullField mean + LocalCondSim ensemble), convert integrated
//! temperature to OHC (`* cp0 * rho0`), preserve NaNs, derive ancillary grids and the
//! `mask_flags` bit band, and write one zarr store per layer (one chunk file per member).
//! Combined layers, anomalies, integrals, trends, plotting and consumer exports are out
//! of scope and handled by a downstream Python stage.
//!
//! See ../mask_spec.md and ../zarr_schema.md.

pub mod config;
pub mod matread;
pub mod ncread;
pub mod grid;
pub mod masks;
pub mod basinmask;
pub mod ingest;
pub mod zarrwrite;

/// Physical / grid constants for the LocalGP product. cp0/rho0 are the ME4OH-protocol values.
pub mod consts {
    /// Specific heat capacity, J/(kg·K) — ME4OH protocol (McDougall 2003).
    pub const CP0: f64 = 3989.244;
    /// Reference density, kg/m³ — ME4OH protocol.
    pub const RHO0: f64 = 1030.0;
    /// Earth radius used for cell areas, m (mean spherical Earth radius).
    pub const EARTH_RADIUS_M: f64 = 6_371_000.0;

    /// Mapping grid longitude count (20.5 … 379.5, 1°).
    pub const NLON: usize = 360;
    /// Mapping grid latitude count (−89.5 … 89.5, 1°).
    pub const NLAT: usize = 180;
}

/// The canonical 1° mapping grid (cell centres).
#[derive(Debug, Clone)]
pub struct GridDef {
    pub lon: Vec<f64>, // length NLON, 20.5 … 379.5
    pub lat: Vec<f64>, // length NLAT, −89.5 … 89.5
}

impl GridDef {
    /// The fixed LocalGP mapping grid.
    pub fn mapping() -> Self {
        let lon = (0..consts::NLON).map(|i| 20.5 + i as f64).collect();
        let lat = (0..consts::NLAT).map(|j| -89.5 + j as f64).collect();
        GridDef { lon, lat }
    }
    pub fn nlon(&self) -> usize { self.lon.len() }
    pub fn nlat(&self) -> usize { self.lat.len() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mapping_grid_endpoints() {
        let g = GridDef::mapping();
        assert_eq!(g.nlon(), consts::NLON);
        assert_eq!(g.nlat(), consts::NLAT);
        assert_eq!(g.lon[0], 20.5);
        assert_eq!(*g.lon.last().unwrap(), 379.5);
        assert_eq!(g.lat[0], -89.5);
        assert_eq!(*g.lat.last().unwrap(), 89.5);
    }
}
