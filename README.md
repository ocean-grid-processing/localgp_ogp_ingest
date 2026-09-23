# ohc_ingest

`ohc_ingest` turns LocalGP ocean-heat-content (OHC) mapping output into a clean, analysis-ready store, and then into an ME4OH-protocol submission.

The Rust binary is deliberately pure-Rust (no C deps) so it builds to a single static binary
for clusters with no Docker/Rust; all NetCDF work lives in the Python step, where libnetcdf is
already available. Validation is primarily via round-trip crosschecks that comapare outputs to inputs after the fact.

## How this reflects the ME4OH protocol

The submission format is defined by the [ME4OH protocol](https://zenodo.org/records/10291852). The numbers baked into this code come from
there:

- **Grid:** 1×1°, `lon 20.5…379.5`, `lat −89.5…89.5` — the protocol mandates this so no
  regridding is needed for the intercomparison.
- **Units:** OHC density in TJ/m², with `cp0 = 3989.244 J/kg/K`, `rho0 = 1030 kg/m³`.
- **Time:** monthly, days since 1900-01-01.
- **Submission file:** one NetCDF per layer, `DATA(LONGITUDE, LATITUDE, TIME)`, named
  `OHC_<Y0>_<Y1>_lev<low>_<high>_exp<X>_<tag>.nc` (the trailing token is the provenance tag,
  inherited from the store; the product identity lives in the provenance record the tag points to).
- **Mask:** the protocol's `ocean_mask` is "points not-NaN at all timesteps," so a submission
  communicates "don't use this cell" only via NaN. `publish.py` is where our richer internal
  mask collapses to that NaN convention.

The store the Rust core writes is *not* a protocol artifact — it's our internal representation
(raw data + a bit-band mask, masking nothing). `publish.py` projects it down to the protocol.

## What the Rust core does (scope)

For one mapped layer:

1. Read the LocalGP `.mat` files (FullField mean + the LocalCondSim ensemble, whose size is read
   from the files — 100 members to date), month by
   month.
2. Scale the raw values by the configured quantity's factor (OHC: `× cp0 × rho0`), preserving NaNs.
3. Transpose month-major input → member-major arrays (buffer the whole layer in RAM).
4. Derive ancillary grids (`etopo`, `basin_id`, `cell_area`) and the `mask_flags` bit band.
5. Write a per-layer zarr v3 store. **No masks are applied to the data** — masking is carried in
   `mask_flags` and applied lazily downstream.

## Inputs

| input | notes |
|---|---|
| LocalGP `.mat` | MATLAB v7 (zlib-compressed); variable `fullFieldGrid`, `[lon,lat]` for the mean and `[lon,lat,100]` for the ensemble. Mean and ensemble live in **separate directories**. |
| `etopo60.cdf` | 1° bathymetry (classic NetCDF); vars `ETOPO60X`/`ETOPO60Y`/`ROSE`; its grid is identical to the mapping grid (asserted, not regridded). Reproduced in this repo under `data/` |
| [`basinmask_04.msk`](https://www.ncei.noaa.gov/data/oceans/woa/WOA18/MASKS/basinmask_04.msk) | WOA 0.25° basin table; nearest-neighbour to the 1° grid, surface column. |

Full provenance, versions, and citations are in [`data/README.md`](data/README.md).

## Output (the zarr store)

`<quantity>_<tag>_<Ymin>_<Ymax>_plev<top>_<bottom>.zarr` (the leading token is the `[quantity]`
`name`, e.g. `ohc`; the years are the discovered data span), containing:

- `field_mean` `(time, lat, lon)` — the posterior-mean field in the quantity's stored units (OHC: J/m²), NaN preserved.
- `field_ensemble` `(member, time, lat, lon)` — the conditional simulations (as many as the mapping
  files held), chunked one file per member.
- `mask_flags` `(lat, lon)` — the bit band, with CF `flag_masks`/`flag_meanings` (see
  [`mask_spec.md`](mask_spec.md)).
- `etopo`, `basin_id`, `cell_area` `(lat, lon)` — ancillaries.

zarr v3, `bytes`+`gzip` codecs (pure Rust), xarray-readable. Layout details in
[`zarr_schema.md`](zarr_schema.md).

## The mask bit band

`mask_flags` is a `uint8` per cell carrying one bit per *reason* a cell might be excluded, so
the store never destroys data — masking is a downstream choice (and, for a submission, collapses
to NaN in `publish.py`). The bits:

| bit | value | name | meaning |
|---|---|---|---|
| 0 | 1 | `bed_above_shallow` | seafloor shallower than the layer's shallow edge (layer entirely in rock) |
| 1 | 2 | `bed_above_deep` | seafloor shallower than the layer's deep edge (seabed cuts through the layer) |
| 2 | 4 | `outside_latitude` | cell outside the kept latitude band |
| 3 | 8 | `removed_basin` | cell in a dropped basin (marginal / enclosed seas) |
| 4 | 16 | `never_estimated` | LocalGP produced no value in any month |
| 5 | 32 | `incomplete_timeseries` | valid in some months but not all |
| 6 | 64 | `bed_above_clip` | seafloor shallower than a fixed clip depth, applied uniformly to every layer (set only when `bathy_clip_m` is configured) |
| 7 | 128 | `ensemble_incomplete` | some CondSim member is NaN-in-time here even if the mean is finite (unset for a `--no-ensemble` store) |

Bits 0/1/4/5/7 are physical/validity reasons; bits 2/3/6 are policy reasons. `publish.py`'s
presets pick different subsets — notably `wmo` honors `bed_above_shallow` (fully-dry cells) but
*not* `bed_above_deep` (partial slope cells are kept), while `wmo_wet` honors `bed_above_deep`
(whole cell wet) and drops those partials, and `wmo_layerless` honors neither (for a quantity with
no layer). Full definitions, the exact preset subsets,
the selector conventions, and the monotonic-bathymetry sentinel are in [`mask_spec.md`](mask_spec.md).

## Usage

Here we enumerate and illustrate how to build, test and run the rust and python components in this repo, with examples and tables of options.

### Ingest step: .mat -> .zarr, in rust

#### Test

Test locally from a bare rust container, in the root of this repo:

```
docker container run -w /app -v $(pwd):/app rust:1.88 cargo test --lib
```

Note you need to download `basinmask_04.msk` into `data/`, or at least one of these tests will fail. Don't forget to do the same on your production cluster.

#### Build

The rust ingestion script is meant to be easy to compile into a fully static binary `dist/ohc_ingest` that can be committed to this repo and checked out along with the slurm and config files for running on Blanca:


```bash
docker build -f Dockerfile.static --target bin --output type=local,dest=dist .
```

#### Run

See [`ohc_ingest.slurm`](ohc_ingest.slurm) for a practical run example on blanca.

Settings fall into three kinds by where they live:

- **Per-run slice** — the one layer + time window this run processes. Required; given on the CLI
  (or env). Never in the config file.
- **Run options** — modifiers for this one invocation (mean-only mode, per-run I/O dir overrides).
- **Static config** — product/environment constants (units, domain, mask policy, grid paths).
  From a `config.toml` (positional arg), or built-in defaults when no config is given. Template:
  [`config.example.toml`](config.example.toml).

**Precedence** — wherever a setting has more than one possible source, the decending order of precedence is **CLI flag → environment variable → `config.toml` → built-in default**, with two wrinkles:

1. The **path env vars are read only when no `config.toml` is passed** — a config file and the env
   don't mix (the config is taken as authoritative for everything it can hold).
2. The **`--dir_*` CLI flags always win**, overriding the directory whether it came from config or
   env — the one hook for munging I/O paths per submission while keeping constants in one config.

##### Per-run layer — required (CLI flag, or env)

| setting | CLI | env | example |
|---|---|---|---|
| run tag | `--tag` | `OHC_TAG` | `OP20260110` — names the store `<quantity>_<tag>_<Ymin>_<Ymax>_plev<layer>.zarr` (years = discovered data span) and is written to the `provenance_tag` attr (whitespace-stripped, never lowercased — must match the provenance record char-for-char) |
| provenance link | `--provenance-link` | `OHC_PROVENANCE_LINK` | URL/path to this run's documentation; written to the `provenance_link` attr |
| code version | `--code-version` | `OHC_CODE_VERSION` | URL to the exact ohc_ingest code (commit/release); written to the `localgp_ingest_code_version` attr |
| layer | `--layer` | `OHC_LAYER` | `15-300`, `300_700`, `700:1850` (integer dbar; exactly one; sep `-`/`_`/`:`) |

##### Time axis — autodetected (no flag)

The run's year range is discovered from the mapping files present in `dir_mean` (and, with the
ensemble, `dir_ensemble`) for the given layer. LocalGP writes whole calendar years, so the discovered
months must tile every year `1..=12` with no gap; a missing month, a partial trailing year, or a
mean/ensemble axis mismatch is a **hard error** naming the holes. The discovered `Ymin..=Ymax` is
echoed in the run banner.

##### Run options (CLI flag, or env)

| setting | CLI | env | default | effect |
|---|---|---|---|---|
| mean-only | `--no-ensemble` | `OHC_NO_ENSEMBLE` (set = on) | off | skip the LocalCondSim files; omit `field_ensemble` from the store |
| mean dir | `--dir_mean` | `OHC_DIR_MEAN` ¹ | config / `.` | FullField mean `.mat` directory |
| ensemble dir | `--dir_ensemble` | `OHC_DIR_ENSEMBLE` ¹ | config / `.` | LocalCondSim `.mat` directory |
| output dir | `--dir_out` | `OHC_DIR_OUT` ¹ | config / `.` | where the zarr store is written |

¹ path env vars apply **only when no `config.toml` is passed**; `--dir_*` flags override regardless.

###### The `[quantity]` table

What the mapped grid is and how it is scaled. Every raw mapping value is multiplied by the
product of `scale_terms` at ingest; the terms are kept by name (not just their product) in the
store's `quantity` attr and in `run_config`, so the physics stays legible in the provenance and a
downstream step can look a term up by name. Omit the table for the OHC defaults; give
`scale_terms = {}` to store the mapping values as they are.

| key | default | effect |
|---|---|---|
| `name` | `ohc` | short slug for the quantity |
| `kind` | `extensive` | `extensive` (a per-area density that sums over area and stacks over layers) or `intensive` (a per-cell value, e.g. a mixed layer depth). Recorded for downstream stages; ingest treats both alike |
| `units` | `J/m2` | units of the stored, scaled field |
| `long_name` | `ocean heat content` | |
| `scale_terms` | `{ cp0 = 3989.244, rho0 = 1030.0 }` | named factors; their product is the ingest scale (empty = 1) |
| `publish_unit_factor` | `1e12` | one published unit is this many stored units; publish divides by it (1 TJ/m² = 1e12 J/m²) |
| `publish_units` | `TJ/m^2` | units of the published field |
`--no-ensemble` is for mean-only products or an incomplete CondSim set:
`publish.py` then emits `DATA` without `DATA_SD`, and `--ensemble` on such a store errors.

##### Static config (`config.toml` key, or built-in default)

No CLI or env for these except the three dirs above (and the two grid paths, via env in no-config
mode). Keys marked **req** have no built-in default *when a `config.toml` is present* — the example
config sets them; in no-config mode the listed default applies.

| key | default | req | effect |
|---|---|:--:|---|
| `var_name` | `potentialTemperature` | req | `.mat` filename token — the mapped variable |
| `model_name` | `SpaceTimeTrend` | req | `.mat` filename token — the mapping model |
| `latitude_range_to_keep` | `[-64.5, 64.5]` | req | latitude band kept → the `outside_latitude` bit |
| `basins_to_remove` | `[0, 5, 6, 7, 8, 9, 53]` | req | basin ids dropped → the `removed_basin` bit |
| `etopo_path` | `etopo60.cdf` | req | bathymetry grid; env `OHC_ETOPO` in no-config mode |
| `basinmask_path` | `basinmask_04.msk` | req | basin table; env `OHC_BASINMASK` in no-config mode |
| `bathy_clip_m` | *(none = off)* | | uniform clip depth (m) → the `bed_above_clip` bit; WMO/GCOS uses `300.0` |
| `[quantity]` | *(the OHC product)* | | the mapped quantity and its scaling — see the table below |
| `dir_mean` / `dir_ensemble` / `dir_out` | `.` | | I/O directories (usually set per-run via the `--dir_*` flags above) |

### Pythonic publish (.zarr -> .nc) & crosschecks (.mat vs .zarr and .mat vs .nc)

After a .zarr store is formed, publish.py applies opinionated masking decisions and generates a .nc compliant with the ME4OH spec. Additionally, we validate this piece of the pipeline with two crosscheck scripts, that compare the contents of the .zarr store with the contents of the original .mat, and similarly compare the final .nc with the original .mat.

#### Environmnet

The python environment for performing integrity crosschecks and publishing to an ME4OH-compliant .nc is described in `Dockerfile.python`; build and mount into this environment, or make an equivalent one in anaconda on CU's cluster for use with slurm.

#### Run

##### publish.py — make the ME4OH submission

Projects a store to a compliant `.nc`: collapses the selected mask bits to NaN, converts
the stored units to the published ones (dividing by the quantity's `publish_unit_factor`, e.g.
J/m² → TJ/m²) and the time axis to days-since-1900, and writes `DATA(LONGITUDE, LATITUDE, TIME)`
(float64 by default) under `<NAME>_<tag>_<Y0>_<Y1>_lev<low>_<high>[_exp<X>].nc`, where `NAME` is the quantity's `name` upper-cased (`OHC_`, `MLD_`). The `quantity` attr is carried onto the submission. By default it also adds `DATA_SD` (ensemble 1σ —
the protocol's "associated uncertainties, where available"), computed from `field_ensemble`. See [`publish.slurm`](publish.slurm) for a submission example.

###### Script options:

| option | default | effect |
|---|---|---|
| `STORE.zarr` (positional) | *(required)* | the input zarr store |
| `--experiment` | *(none)* | ME4OH experiment letter (`A`/`B`/…) — adds the `exp<X>` filename token and the `experiment` attr. Omit for a product that isn't an ME4OH submission. |
| `--tag` | *inherited from the store's `provenance_tag`* | provenance tag: the **run token** in the filename (`<NAME>_<tag>_…[_exp<X>].nc`) **and** the `provenance_tag` header attr. Defaults to what the ingest `--tag` stamped on the store; pass only to override. |
| `--provenance-link` | *inherited from the store's `provenance_link`* | URL/path to the provenance record; written to the `provenance_link` header attr. Pass only to override. |
| `--code-version` | *(required)* | URL to the exact publish code (commit/release); stamped as `localgp_publish_code_version`. This step's own code, distinct from the store's ingest code version. |
| `--preset` | `me4oh` | named mask policy — `me4oh`, `wmo`, `wmo_wet`, or `wmo_layerless` (see below); an alias for a `--mask-bits` list |
| `--mask-bits` | *(none)* | explicit comma list of mask bit names to honor (from [`mask_spec.md`](mask_spec.md)); give this or `--preset`, not both. The resolved list lands in the `mask_applied` attr either way |
| `--levels LOW,HIGH` | store's layer bounds | override the filename's layer bounds (meters) |
| `--no-uncertainty` | off | skip `DATA_SD` (and the full-ensemble read) |
| `--ensemble` | off | also write the full ensemble sibling `<NAME>ENS_<...>.nc` (see below) |
| `--out` | `.` | output directory |

**Provenance chain.** The submission carries the store's `localgp_ingest_run_config` /
`_run_facts` / `_code_version` forward untouched (opaque JSON strings) and adds this step's own
`localgp_publish_run_config` (resolved args), `localgp_publish_run_facts` (preset, bounds, ensemble
size, grid), and `localgp_publish_code_version`. Each step namespaces its block by identity, so the
chain accretes without collision and rolls forward at every stage; the global `provenance_tag` /
`provenance_link` stay unprefixed and shared.

**Mask presets** (`--preset`): `me4oh` (default) honors only the physical/validity bits
(`never_estimated`, `incomplete_timeseries`, `bed_above_shallow`, `bed_above_deep`) — the honest,
maximal valid field, letting the assessment define the common domain. `wmo` is our
latitude/basin-cropped product: it adds `outside_latitude`, `removed_basin`, `bed_above_clip`, and
`ensemble_incomplete`, and — deliberately — honors `bed_above_shallow` (fully-dry cells) but **not**
`bed_above_deep` (partial continental-slope cells are kept). `wmo_wet` is the same crops but honors
`bed_above_deep` in place of `bed_above_shallow`, requiring a whole cell wet (partials drop too).
`wmo_layerless` is the `wmo` crops with neither bed bit, for a quantity that has no layer (a mixed
layer depth): its filename layer token is a name, so the per-layer bathymetry bits mean nothing there.
Exact bit subsets, and the layerless-token rule, in
[`mask_spec.md`](mask_spec.md).

**Mean-only stores:** a store produced by the rust with `--no-ensemble` has no `field_ensemble`; publish detects this, writes `DATA` without `DATA_SD` (with a note), and `--ensemble` on such a store is an error.

`--ensemble` writes the full ensemble as a sibling `<NAME>ENS_<...>.nc` with
`DATA(MEMBER, LONGITUDE, LATITUDE, TIME)` — same mask, units (f64), and time axis as the
submission — for downstream uses that derive per-member quantities before collapsing to a spread.
It is not an ME4OH submission (distinct filename, extra dimension), so the assessment's `OHC_*.nc`
discovery won't pick it up.

##### Crosscheck verification (two independent round-trips against the `.mat`)

Both use `scipy.io.loadmat` as an independent reader (not our Rust parser), so they're genuine
oracles; both load sizeable arrays, so run them inside the job allocation.

- **`verify_store.py`** — three positional args (`STORE.zarr DIR_MEAN DIR_ENSEMBLE`), no flags. It
  auto-detects a mean-only store and skips the ensemble check (then `DIR_ENSEMBLE` is unused).
  Before the PASS line it also tallies the raw mapping values (before scaling) — how many are
  positive, exactly zero, negative, or NaN, the finite range, and any months holding exact zeros —
  for the mean and the ensemble. That block is a report, not a check: it records the mapping's
  value conventions in the log, so a change upstream (a numeric fill instead of NaN, a sign flip, an
  unphysical range for the mapped quantity) is visible here.
- **`verify_publish.py`** — the same three positional args, plus `--no-sd` (skip the `DATA_SD`
  check) and `--ensemble` (also check the `<NAME>ENS_<...>.nc` sibling member-by-member).

Complete cluster jobs: [`verify_store.slurm`](verify_store.slurm) and
[`verify_publish.slurm`](verify_publish.slurm).

**Tolerances** track the stored precision, not bit-for-bit. `verify_store` compares at float32
precision (both sides cast; exact match expected). `verify_publish` adapts to each variable's stored
dtype — for float64 (the default) `DATA`/`ENS` match to ~1e-12 and `DATA_SD` to ~1e-9 (a std
cancels more), and ~1e-6 for float32 (the float32 `/1e12` rounding). A real bug (transpose flip,
unit error, wrong month) is still caught.
