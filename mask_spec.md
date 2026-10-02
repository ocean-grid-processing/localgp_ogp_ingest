# Mask bit band (`mask_flags`)

`localgp_ogp_ingest` never deletes data. Instead of NaN-ing cells out, it records — per grid cell — one
bit for each independent *reason* the cell might be excluded. Downstream code chooses which
reasons to honor with a bitwise test, and `publish.py` collapses a chosen set to NaN when it
writes a submission. This document defines the bits.

## The variable

| field | value |
|---|---|
| name | `mask_flags` |
| dtype | `uint8` |
| dims | `(lat, lon)` — time-invariant; one per layer store |
| fill | none; every cell is defined |

CF attributes on the variable:

```
flag_masks    = 1, 2, 4, 8, 16, 32, 64, 128
flag_meanings = "bed_above_shallow bed_above_deep outside_latitude removed_basin never_estimated incomplete_timeseries bed_above_clip ensemble_incomplete"
```

## Bits

| bit | value | name | set when | derived from |
|:--:|:--:|---|---|---|
| 0 | 1 | `bed_above_shallow` | seafloor shallower than the layer's shallow edge (layer entirely in rock) | `etopo > −shallow_edge` |
| 1 | 2 | `bed_above_deep` | seafloor shallower than the layer's deep edge (seabed cuts through the layer) | `etopo > −deep_edge` |
| 2 | 4 | `outside_latitude` | cell outside the kept latitude band | `lat ∉ [lo, hi]` |
| 3 | 8 | `removed_basin` | cell in a dropped basin (marginal / enclosed seas) | `basin_id ∈ remove-list` |
| 4 | 16 | `never_estimated` | LocalGP produced no value in any month | `all t: ¬isfinite` |
| 5 | 32 | `incomplete_timeseries` | valid in some months but not all | `(any t: ¬isfinite) ∧ ¬never_estimated` |
| 6 | 64 | `bed_above_clip` | seafloor shallower than a fixed clip depth, applied uniformly to every layer | `etopo > −clip` |
| 7 | 128 | `ensemble_incomplete` | some CondSim member is NaN-in-time here (the mean may be finite) | `∃ member, t: ¬isfinite` |

Bits 0/1/4/5/7 are **physical / validity** reasons; bits 2/3/6 are **policy** reasons. Bits 4 and 5
together encode the temporal-validity state: neither set = always valid, bit 5 = sometimes
valid, bit 4 = never valid. Per-timestep validity itself is not stored — it's `isfinite(data)`.

`bed_above_clip` differs from `bed_above_deep`: the deep bit is per-layer (against that layer's
own bottom edge), while the clip bit is a single depth applied to **all** layers alike. It is set
only when `bathy_clip_m` is configured (`None` = off). The WMO/GCOS product uses a 300 m clip so
every layer shares one open-ocean footprint (matches the original `max_pressure_to_keep = 300`).

`ensemble_incomplete` is the member half of the original's mean∪members mask: `never`/`incomplete`
carry the mean's NaNs, and this bit adds cells that only some CondSim members drop. It is set from the ingested ensemble and is **unset** for a mean-only store (`--no-ensemble`),
so honoring it never over-masks a store that has no members to union.

## Selecting cells

"Fully usable" is "no bit set":

```
USABLE = 0b11111111   (= 255)
keep = (mask_flags & USABLE) == 0
```

Drop bits from the selector to relax a policy: a global (un-cropped) integral uses
`USABLE & ~outside_latitude`; tolerating partial-depth cells drops `bed_above_deep`; including
marginal seas drops `removed_basin`.

## Presets

`publish.py` takes the honored set as an explicit `--mask-bits` list of names, or as a `--preset`
alias for one of these lists. A cell is dropped (set to NaN) when **any** honored bit is set.

| bit | `me4oh` | `wmo` | `wmo_wet` | `wmo_layerless` |
|---|:--:|:--:|:--:|:--:|
| `bed_above_shallow` | ✓ | ✓ | | |
| `bed_above_deep` | ✓ | | ✓ | |
| `outside_latitude` | | ✓ | ✓ | ✓ |
| `removed_basin` | | ✓ | ✓ | ✓ |
| `never_estimated` | ✓ | ✓ | ✓ | ✓ |
| `incomplete_timeseries` | ✓ | ✓ | ✓ | ✓ |
| `bed_above_clip` | | ✓ | ✓ | ✓ |
| `ensemble_incomplete` | | ✓ | ✓ | ✓ |
| *for* | the maximal valid field; the assessment defines the common domain | the WMO/GCOS domain: cropped, fully-dry cells out, partial slope cells kept | as `wmo` but a whole cell must be wet | as `wmo` for a quantity with no layer (below) |

On the two bed bits: `bed_above_shallow ⇒ bed_above_deep` always (a seabed above the layer's top is
above its bottom too), so honoring `bed_above_deep` supersedes `bed_above_shallow`. `wmo` honors
only the shallow bit deliberately — drop cells where the layer is *entirely* below the seabed, keep
the *partial* cells where the seabed cuts through it, the continental-slope cells the original
retains — and this also guarantees a deeper-topped layer's footprint is a subset of a
shallower-topped one with the same bottom (`A(1800_1850) ⊆ A(700_1850)`), which the mapping's own
NaNs don't always enforce. `bed_above_clip` is a fixed depth independent of the layer, so it stays
meaningful for a layerless quantity while the two bed bits do not.

## Quantities with no layer

A mapped quantity that is not a depth layer (a mixed layer depth, say) still arrives from LocalGP
with a `<top>_<bottom>` token in its filenames, and ingest still requires `--layer`. The token is
then a **name**: it keys the files, the store, and the submission, and nothing more. Two
consequences:

- Bits 0 and 1 (`bed_above_shallow`, `bed_above_deep`) are computed against the nominal token and
  carry no meaning. They are still written (the band's layout doesn't change), and must not be
  honored: use `wmo_layerless`, or a `--mask-bits` list without them.
- The token must satisfy `top <= bottom`, so the shallow ⇒ deep sentinel (below) holds.

`bed_above_clip` is unaffected — it is a fixed depth independent of any layer — and remains a
policy choice for such a quantity.

## Conventions

- **"above" means shallower** (smaller depth, higher in the water column): `bed_above_shallow`
  means the seafloor is shallower than the layer's shallow edge.
- **Bits may co-occur** — each is an independent reason (a dry shelf cell can be both
  `bed_above_shallow` and `never_estimated`; a Mediterranean cell both `removed_basin` and
  `outside_latitude`).
- **Depth bounds (dbar) are compared to `etopo` (m) as if equal** — ~1% error near the surface,
  a few % at depth, negligible at a 1° land/sea boundary.
- **`incomplete_timeseries` is relative to the stored time axis.** If a consumer subsets time,
  the authoritative per-month truth is `isfinite(data)`.
