# Input data provenance

Two small reference grids are used by localgp_ogp_ingest. Both are third-party
upstream datasets; this records where they came from.

## etopo60.cdf — 1° global relief (bathymetry)

- **What:** ETOPO 1° (60 arc-minute) relief of the Earth's surface. Variables `ETOPO60X`
  (longitude), `ETOPO60Y` (latitude), and `ROSE` ("Relief Of the Surface of the Earth", meters,
  negative below sea level). Created with Ferret V4.45 (1997).
- **Source:** the standard dataset bundled with NOAA/PMEL Ferret & PyFerret, distributed via the
  `NOAA-PMEL/FerretDatasets` repository (also installed by the conda `ferret_datasets` package).
  - https://github.com/NOAA-PMEL/FerretDatasets — `data/etopo60.cdf`
  - raw: https://raw.githubusercontent.com/NOAA-PMEL/FerretDatasets/master/data/etopo60.cdf
- **This copy:** git blob SHA-1 `377d1986df45517751b1d8d5c9e01f4514a8987d`,
  byte-identical to the repository copy (`git hash-object etopo60.cdf` to confirm).
- **Why this grid:** its lon/lat axes are identical to the mapping grid (20.5…379.5,
  −89.5…89.5), so no regridding is needed — `ncread.rs` asserts the match rather than
  interpolating.

## basinmask_04.msk — WOA 0.25° ocean basin mask

- **What:** ocean basin numbers on a 0.25° grid. File header: "Basin number for WOA climatology
  objective analysis : 0.25 degree grid". Text table with `Latitude, Longitude, Basin_<depth>m`
  columns (ocean points only). Basin codes: 1 Atlantic, 2 Pacific, 3 Indian, 4 Mediterranean,
  5 Baltic, 6 Black Sea, 7 Red Sea, 8 Persian Gulf, 9 Hudson Bay, 10 Southern, 11 Arctic, …
- **Source:** NOAA World Ocean Atlas (WOA) basin masks, distributed by NOAA NCEI. The "04"
  denotes the 0.25° series (the sibling `basinmask_01.msk` is 1°).
  - https://www.ncei.noaa.gov/products/world-ocean-atlas
- **This copy:** dated 2018-09 (WOA18-era).
- **Use:** `basinmask.rs` assigns each 1° grid cell the surface (`Basin_0m`) basin of its nearest
  mask point; the `removed_basin` mask bit drops the configured marginal/enclosed seas.
