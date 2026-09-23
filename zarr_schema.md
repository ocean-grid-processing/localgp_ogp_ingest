# zarr store layout

`ohc_ingest` writes one zarr store per mapped layer per run. The store is the pipeline's internal
representation — raw OHC plus the `mask_flags` bit band, with nothing masked out. (`publish.py`
projects it to the ME4OH submission; see the crate README.) Companion: [`mask_spec.md`](./mask_spec.md).

## One store per layer

Each store holds exactly one layer of one run; the directory is the unit:

```
<quantity>_<tag>_<Y0>_<Y1>_plev<top>_<bottom>.zarr/     e.g. ohc_OP20260110_2004_2025_plev15_300.zarr/
```

Layers are independent (mapped separately, overlapping in depth), so they are never combined
into a `layer` dimension. The layer bounds are scalar group attributes, not a coordinate.

## On disk

A zarr store is a directory tree: each array is a directory with a small JSON metadata file
(`zarr.json`) plus one file per chunk under `c/`. Chunking is chosen so each file is physically
meaningful:

```
ohc_<tag>_2004_2025_plev15_300.zarr/
├── zarr.json                    group metadata + attributes
├── field_mean/      c/0/0/0       1 file   — posterior-mean field            (time, lat, lon)
├── field_ensemble/  c/<m>/0/0/0   100 files — one per conditional simulation  (member, time, lat, lon)
├── mask_flags/    c/0/0         1 file   — the bit band (lat, lon); see mask_spec.md
├── etopo/         c/0/0         1 file   — bathymetry (lat, lon)
├── basin_id/      c/0/0         1 file   — basin ids (lat, lon)
├── cell_area/     c/0/0         1 file   — spherical cell area (lat, lon)
└── lon/ lat/ time/ member/      coordinate arrays
```

The 101 big files map 1:1 onto LocalGP's output for the layer: one `FullField` mean
(`field_mean`) + 100 `LocalCondSim` members (`field_ensemble`). An ensemble chunk file reads as
"member M's complete record for this layer."

## Arrays

| array | dims | dtype | units | fill | chunk |
|---|---|---|---|---|---|
| `field_mean` | `(time, lat, lon)` | f64 | `quantity.units` (OHC: J/m²) | NaN | whole array (1 chunk) |
| `field_ensemble` | `(member, time, lat, lon)` | f64 | `quantity.units` | NaN | `(1, time, lat, lon)` — 1 per member |

The data arrays are named generically; the group's `quantity` attr (and each array's `units` /
`long_name`, taken from the `[quantity]` table) say what the field is.
| `mask_flags` | `(lat, lon)` | u8 | — | — | 1 chunk |
| `etopo` | `(lat, lon)` | f32 | m | NaN | 1 chunk |
| `basin_id` | `(lat, lon)` | i16 | — | — | 1 chunk |
| `cell_area` | `(lat, lon)` | f64 | m² | — | 1 chunk |

`field_mean` is **f64**: downstream products (e.g. the GCOS deliverable) take a large-mean anomaly
(absolute OHC − baseline), a cancellation that needs double precision. `field_ensemble` is **f64**
too — its yearly-spread and trend uncertainties feed the same cancellation-prone anomalies, so
single precision would leave them a few percent off; the cost is a doubled (~13.7 GB) 100-member
footprint. `field_ensemble` is **absent** in a mean-only store (ingested `--no-ensemble`):
the CondSim files aren't read, and `publish.py` then emits `DATA` without `DATA_SD`.

### Coordinates

| coord | dims | dtype | notes |
|---|---|---|---|
| `lon` | `(lon: 360)` | f64 | degrees_east, `20.5 … 379.5` |
| `lat` | `(lat: 180)` | f64 | degrees_north, `−89.5 … 89.5` |
| `time` | `(time: N)` | f64 | `days since <first-month>-15`, monthly (day 15) |
| `member` | `(member: M)` | i16 | `1 … M`, M = the ensemble size the mapping files held (100 for LocalCondSim to date); absent in a mean-only store |

## Chunking

1. **Never split the map** — `(lat, lon)` is whole in every chunk, so a chunk is a complete
   world map and the horizontal integral reads exactly what it needs.
2. **One chunk per ensemble member** — `(1, time, lat, lon)`, so each file is one simulation's
   full record (~68 MB raw, less compressed).

Codecs: `bytes` (little-endian) + `gzip` — pure Rust (no Blosc/HDF5), read natively by xarray.

## Data conventions

- OHC stored in **J/m²** (after `cp0·rho0`); the `quantity` attr carries the scale terms by name (and `cp0`/`rho0` are also standalone attrs), so the scaling
  is invertible.
- **Absolute, not anomaly** — anomaly referencing is a downstream choice.
- The **full ensemble** is kept (every member the mapping files held); std / percentiles derive on read.
- **Nothing is masked in the data** — masking lives in `mask_flags` (see `mask_spec.md`);
  per-timestep validity is `isfinite(data)`.

## Group attributes

```
Conventions       = "CF-1.10"
title             = "LocalGP <quantity.long_name> — <tag>, <top>-<bottom> dbar"
source            = "LocalGP <model>; var=<var>; run=<tag>"
mapped_fields_tag = "<tag>"
provenance_tag    = "<tag>"        # global run token (= store dir-name token); --tag, char-for-char
provenance_link   = "<url|path>"   # global: this run's documentation; --provenance-link
var_name          = "<var>"        # e.g. potentialTemperature
model_name        = "<model>"      # e.g. SpaceTimeTrend
layer_top         = <top>          # dbar, shallow edge
layer_bottom      = <bottom>       # dbar, deep edge
domain            = "lon 20.5..379.5E, lat -89.5..89.5N, 1deg"
quantity          = "<json>"       # the [quantity] table: name, kind, units, long_name, scale_terms, publish_unit_factor, publish_units
cp0               = 3989.244       # J/(kg K)  — legacy standalone copies of the scale_terms of those
rho0              = 1030           # kg/m3       names, written only when the quantity has them

# stage-namespaced local provenance (STAGE = "localgp_ingest"):
localgp_ingest_code_version = "<url>"   # exact ohc_ingest code (commit/release); --code-version
localgp_ingest_run_config   = "<json>"  # whole resolved RunConfig, cold-serialized (pretty JSON string)
localgp_ingest_run_facts    = "<json>"  # derived per-run facts (pretty JSON string; see below)
```

**Global vs local provenance.** `provenance_tag` / `provenance_link` are *global* — the run token and
this run's documentation, shared across every step and inherited downstream unprefixed. The
`localgp_ingest_*` attrs are this step's *local* provenance, namespaced by the step's identity so they
roll forward untouched through later steps (each step copies every `*_run_config` / `*_run_facts` /
`*_code_version` and adds its own). `…_run_config` is the whole resolved `RunConfig` (every setting
used, whatever its origin — flag, env, `config.toml`, or built-in default — stamped after precedence
collapses). `…_run_facts` is the per-run detail that isn't in the config: `layer_top`/`layer_bottom`,
`year_min`/`year_max`, `n_timesteps`, `ensemble` (bool) and `n_members`, `grid_nlat`/`grid_nlon`. Both
are compact (one-line) JSON **strings** — so they read as a single clean line in `ncdump -h` and carry
unchanged into the downstream netCDF attrs.

## Ingest note: month-major in, member-major out

LocalGP delivers one `.mat` per month (each `LocalCondSim` file holds all 100 members for that
month), but the store is chunked per member. The ingest X
f64 for a 264-month record), reads each monthly file once, scatters its members into the buffer
(applying `cp0·rho0`), then writes the per-member chunks plus the ancillary grids.

## Not in the store

Combined depth layers, anomalies, area-integrated time series, and trends are downstream
products, computed by consumers of the store rather than written here.
