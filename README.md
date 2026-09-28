# localgp_ogp_ingest

`localgp_ogp_ingest` turns LocalGP mapping output — any mapped grid in the LocalGP format: integrated potential temperature scaled to ocean heat content (OHC), a mixed layer depth (MLD), … — into a clean, analysis-ready store by way of its rust core, and then into an annotated ME4OH-shaped submission via its pythonic publishing step. What the field is, and how it is scaled, is the `[quantity]` table in the run's config; that table travels with the data through every later stage of the pipeline.

The Rust binary is deliberately pure-Rust (no C deps) so it builds to a single static binary
for clusters with no Docker/Rust; all NetCDF work lives in the Python step, where libnetcdf is
readily available. Validation is primarily via round-trip crosschecks that compare outputs to inputs after the fact.

## How the submission relates to the ME4OH protocol

The published `.nc` follows the [ME4OH protocol](https://zenodo.org/records/10291852) in its shape, and adds annotations the protocol doesn't define. Matching the protocol:

- **Grid:** 1×1°, `lon 20.5…379.5`, `lat −89.5…89.5` — the mapping grid is the protocol grid, so nothing is regridded.
- **Layout:** one NetCDF per layer, `DATA(LONGITUDE, LATITUDE, TIME)`, float64, with `DATA_SD` as the protocol's "associated uncertainty, where available" (the ensemble 1σ).
- **Time:** monthly, days since 1900-01-01.
- **Mask:** "don't use this cell" is said only by NaN. The store's richer bit-band mask collapses to NaN in `publish.py`, under the preset or bit list you choose.
- **Filename:** `<NAME>_<tag>_<Y0>_<Y1>_lev<low>_<high>[_exp<X>].nc`. For OHC that is `OHC_…_exp<X>.nc`, a protocol submission with the protocol's constants (`cp0 = 3989.244 J/kg/K`, `rho0 = 1030 kg/m³`, TJ/m²). For any other quantity the file is ME4OH-*shaped* — same layout and conventions — but not a protocol submission, and `--experiment` is left off so no `exp` token claims otherwise.

Added on top of the protocol, so downstream stages know what they are holding without guessing:

- **`quantity`** — the run's `[quantity]` table (name, kind, units, scale terms by name, published units) as one compact-JSON attr. This is how `ogp_derive` knows an OHC file from an MLD file.
- **`mapped_layer`** (`<top>_<bottom>`), plus `var_name`, `model_name`, `source` — the native level and the LocalGP run the field came from.
- **`provenance_tag`**, **`provenance_link`** — the run token (also the filename's leading token after `<NAME>_`) and a pointer to its provenance record.
- **`localgp_ingest_*`** and **`localgp_publish_*`** (`_run_config`, `_run_facts`, `_code_version`) — each step's resolved configuration, stamped so the chain accretes at every later stage.
- **`mask_preset`**, **`mask_applied`**, **`ensemble_size`** — which bits were collapsed to NaN, and how many members the `DATA_SD` came from.
- **The `<NAME>ENS_…` sibling** (with `--ensemble`): the full member ensemble, `DATA(MEMBER, LONGITUDE, LATITUDE, TIME)`, same mask and units — what derive uses to carry uncertainty through its own computations. Not a protocol file (extra dimension, distinct prefix).

A file that has only the protocol pieces — another group's submission — can still go into `ogp_derive` with `--contract ME4OH`, which infers the annotations above from the protocol itself; see that README.

The zarr store the Rust core writes is *not* a protocol artifact — it's the pipeline's internal representation (raw scaled data + a bit-band mask, masking nothing). `publish.py` projects it down to the shape above.

## What the Rust core does (scope)

For one mapped layer:

1. Read the LocalGP `.mat` files (FullField mean + the LocalCondSim ensemble, whose size is read
   from the files), month by
   month.
2. Scale the raw values by the configured quantity's factor (OHC: `× cp0 × rho0`), preserving NaNs.
3. Transpose month-major input → member-major arrays (buffer the whole layer in RAM).
4. Derive ancillary grids (`etopo`, `basin_id`, `cell_area`) and the `mask_flags` bit band (an array of flags representing reasons to potentially mask out a cell - see details below).
5. Write a per-layer zarr v3 store. **No masks are applied to the data** — masking is carried in
   `mask_flags` and applied lazily downstream.

## Inputs

| input | notes |
|---|---|
| LocalGP `.mat` | MATLAB v7 (zlib-compressed); variable `fullFieldGrid`, `[lon,lat]` for the mean and `[lon,lat,n]` for the ensemble. Mean and ensemble live in **separate directories**. |
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
- `etopo`, `basin_id`, `cell_area` `(lat, lon)` — ancillaries;
- group attrs: the `quantity` table, `provenance_tag`/`provenance_link`, the layer, and the
  `localgp_ingest_*` provenance blocks.

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

The rust ingestion script is meant to be easy to compile into a fully static binary `dist/localgp_ogp_ingest` that can be committed to this repo and checked out along with the slurm and config files for running on Blanca:


```bash
docker build -f Dockerfile.static --target bin --output type=local,dest=dist .
```

#### Run

[`run.sh`](run.sh) is the orchestrator: one config block at its top (LocalGP dirs, output dir, tag, which
`config.toml`, publish preset and experiment, provenance and code-version URLs), and per layer it
submits [`localgp_ogp_ingest.slurm`](localgp_ogp_ingest.slurm) → [`verify_store.slurm`](verify_store.slurm)
and [`publish.slurm`](publish.slurm) → [`verify_publish.slurm`](verify_publish.slurm) with slurm dependencies.
Edit the block, run the script. The tables below are what those jobs pass through.

**Precedence** — where a setting has more than one source: **CLI flag → environment variable →
`config.toml` → built-in default**, with two wrinkles: the path env vars (¹ below) are read only when
no `config.toml` is passed (a config file is authoritative for everything it can hold), and the
`--dir_*` flags win regardless (the one hook for munging I/O paths per run while the constants stay
in one file). Every resolved setting, whatever its source, is stamped into the store's
`localgp_ingest_run_config`.

##### Settings

| setting | CLI | env | `config.toml` key | required | default | what it does |
|---|---|---|---|:--:|---|---|
| config file | positional `config.toml` | | | | built-in defaults | static constants + paths; one per product/environment (see [`config.example.toml`](config.example.toml)). Without it, the defaults below plus the env path vars apply |
| run tag | `--tag` | `OHC_TAG` | | **yes** | | run identifier: leads the store name after `<quantity>_`, and is the `provenance_tag` attr. Whitespace-stripped, never lowercased — it must match the provenance record char-for-char |
| provenance link | `--provenance-link` | `OHC_PROVENANCE_LINK` | | **yes** | | URL/path to this run's provenance record → `provenance_link` attr |
| code version | `--code-version` | `OHC_CODE_VERSION` | | **yes** | | URL to the exact `localgp_ogp_ingest` commit/release → `localgp_ingest_code_version` attr |
| layer | `--layer` | `OHC_LAYER` | | **yes** | | exactly one `<top>_<bottom>` in integer dbar (separator `-`, `_` or `:`), matching the token in the `.mat` filenames. For a layerless quantity the token is a name, kept as-is; it must satisfy `top <= bottom` |
| mean-only | `--no-ensemble` | `OHC_NO_ENSEMBLE` (set = on) | | | off | skip the LocalCondSim files; the store has no `field_ensemble`, and publish then writes no `DATA_SD` |
| mean dir | `--dir_mean` | `OHC_DIR_MEAN` ¹ | `dir_mean` | | `.` | the FullField mean `.mat` directory |
| ensemble dir | `--dir_ensemble` | `OHC_DIR_ENSEMBLE` ¹ | `dir_ensemble` | | `.` | the LocalCondSim `.mat` directory |
| output dir | `--dir_out` | `OHC_DIR_OUT` ¹ | `dir_out` | | `.` | where the zarr store is written |
| variable token | | | `var_name` | yes ² | `potentialTemperature` | the mapped-variable token in the `.mat` filenames |
| model token | | | `model_name` | yes ² | `SpaceTimeTrend` | the mapping-model token in the `.mat` filenames |
| latitude band | | | `latitude_range_to_keep` | yes ² | `[-64.5, 64.5]` | cells outside it get the `outside_latitude` bit |
| basins removed | | | `basins_to_remove` | yes ² | `[0, 5, 6, 7, 8, 9, 53]` | WOA basin ids that get the `removed_basin` bit |
| bathymetry | | `OHC_ETOPO` ¹ | `etopo_path` | yes ² | `etopo60.cdf` | the 1° bathymetry grid (see [Inputs](#inputs)) |
| basin table | | `OHC_BASINMASK` ¹ | `basinmask_path` | yes ² | `basinmask_04.msk` | the WOA basin table |
| bathy clip | | | `bathy_clip_m` | | off | a fixed depth (m): cells with a shallower seafloor get the `bed_above_clip` bit, whatever the layer. The WMO/GCOS products use `300` |
| quantity | | | `[quantity]` table | | the OHC table | what the field is and how it is scaled — next table |

¹ read only when no `config.toml` is passed. ² required in a `config.toml` (no built-in default is
substituted when the file omits it); the listed default applies only in no-config mode.

**The time axis has no setting.** It is discovered from the `.mat` files present in `dir_mean` (and,
with the ensemble, `dir_ensemble`) for the layer: LocalGP writes whole calendar years, so the
discovered months must tile every year `1..=12` with no gap, and a missing month, a partial trailing
year, or a mean/ensemble mismatch is a hard error naming the holes. The span is echoed in the run
banner and lands in the store name. The ensemble size is likewise read from the first LocalCondSim
file, and every later month must match it.

##### The `[quantity]` table

What the mapped grid is and how it is scaled. Every raw mapping value is multiplied by the product of
`scale_terms` at ingest; the terms are kept by name (not just their product) in the store's `quantity`
attr and in `run_config`, so the physics stays legible in the provenance and a downstream step can look
a term up by name. **If the table is omitted, the OHC table below is used** — a config with no
`[quantity]` is an OHC run; write the table out for any other quantity (see `config.mld.toml`).

| key | default | what it does |
|---|---|---|
| `name` | `ohc` | short slug for the quantity; leads the store and submission filenames (`ohc_…`, `OHC_…`) |
| `kind` | `extensive` | `extensive` (a per-area density that sums over area and stacks over layers) or `intensive` (a per-cell value, e.g. a mixed layer depth). Recorded for downstream stages, which refuse operations that don't suit the kind; ingest treats both alike |
| `units` | `J/m2` | units of the stored, scaled field |
| `long_name` | `ocean heat content` | the field's long name, carried onto the store arrays and the submission |
| `scale_terms` | `{ cp0 = 3989.244, rho0 = 1030.0 }` | named factors; their product is the ingest scale. `{}` stores the mapping values as they are |
| `publish_unit_factor` | `1e12` | one published unit is this many stored units; publish divides by it (1 TJ/m² = 1e12 J/m²). `1` publishes the stored units |
| `publish_units` | `TJ/m^2` | units of the published field |

### Pythonic publish (.zarr -> .nc) & crosschecks (.mat vs .zarr and .mat vs .nc)

After a .zarr store is formed, publish.py applies the chosen masking policy and writes the ME4OH-shaped submission `.nc` (see [How the submission relates to the ME4OH protocol](#how-the-submission-relates-to-the-me4oh-protocol)). Additionally, we validate this piece of the pipeline with two crosscheck scripts, that compare the contents of the .zarr store with the contents of the original .mat, and similarly compare the final .nc with the original .mat.

#### Environment

The python environment for publishing and for the integrity crosschecks is described in `Dockerfile.python`; build and mount into this environment, or make an equivalent one in anaconda on CU's cluster for use with slurm.

#### Run

##### publish.py — make the submission

Projects a store to the submission `.nc`: collapses the selected mask bits to NaN, converts
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
It is not a protocol submission (distinct filename prefix, extra dimension).

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
