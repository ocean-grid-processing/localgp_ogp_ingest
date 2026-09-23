#!/usr/bin/env python3
"""Project an ohc_ingest zarr store to an ME4OH-compliant NetCDF submission.

The zarr store is our source of truth (raw OHC + a bit-band mask, nothing masked out). A
compliant submission can only say "don't use this point" via NaN, so this step collapses the
selected mask bits to NaN, converts J/m^2 -> TJ/m^2 and the time axis to days since 1900-01-01,
and writes DATA(LONGITUDE, LATITUDE, TIME) under the ME4OH filename.

    python publish.py STORE.zarr --experiment B --code-version URL \
        [--tag OP20260127b] [--provenance-link URL] [--preset me4oh|wmo|wmo_wet] \
        [--levels LOW,HIGH] [--ensemble] [--out DIR]

The provenance tag and link are inherited from the store (stamped by the ingest --tag /
--provenance-link) and carried onto the submission; --tag / --provenance-link override them. The
tag is the run token in the filename (OHC_<tag>_..._exp<E>.nc) and the provenance_tag header attr.

Provenance chain: each step namespaces its own local provenance by identity — `<step>_run_config`,
`<step>_run_facts`, `<step>_code_version` — and every step rolls all upstream `*_run_config` /
`*_run_facts` / `*_code_version` attrs forward untouched (opaque JSON strings) before adding its own.
So this step copies the store's `localgp_ingest_*` blocks onto the submission verbatim and stamps
`localgp_publish_*` (its resolved args, derived facts, and --code-version, this step's own code).

Mask presets (see ../mask_spec.md):
  me4oh (default) = physical/validity bits only (never_estimated, incomplete_timeseries,
                    bed_above_shallow, bed_above_deep) — submit the honest, maximal valid
                    field and let the assessment define the common domain.
  wmo             = validity + outside_latitude + removed_basin + bed_above_shallow +
                    bed_above_clip (+ ensemble_incomplete) — our cropped product. Drops fully-dry
                    cells (bed_above_shallow) and shelves shallower than the uniform clip, but
                    KEEPS partial cells where the seabed cuts through the layer (bed_above_deep is
                    NOT honored) — matching the original WMO/GCOS domain.
  wmo_wet         = the wmo crops but with bed_above_deep in place of bed_above_shallow — requires a
                    whole cell wet, so the partial (continental-slope) cells drop too. deep
                    supersedes shallow (the shallow=>deep sentinel); bed_above_clip is kept but
                    currently redundant (deep already covers it).

--ensemble additionally writes the full conditional-simulation ensemble as a sibling file
  OHCENS_<...>.nc with DATA(MEMBER, LONGITUDE, LATITUDE, TIME) — same mask, units, and time
  axis — for downstream uses that derive per-member quantities before collapsing to a spread.
  It is NOT an ME4OH submission (different filename, extra dimension). Reads all members.

Requires: xarray, zarr>=3, numpy, netCDF4.
"""
import argparse
import datetime
import json
import os

import numpy as np
import xarray as xr

# mask bit values (mask_spec.md)
BITS = {
    "bed_above_shallow": 1,
    "bed_above_deep": 2,
    "outside_latitude": 4,
    "removed_basin": 8,
    "never_estimated": 16,
    "incomplete_timeseries": 32,
    "bed_above_clip": 64,
    "ensemble_incomplete": 128,
}
PRESETS = {
    "me4oh": ["never_estimated", "incomplete_timeseries", "bed_above_shallow", "bed_above_deep"],
    # WMO/GCOS domain. Bathymetry: honor bed_above_shallow (drop cells where the layer is
    # ENTIRELY below the seabed — fully dry, zero water) but NOT bed_above_deep (keep PARTIAL
    # cells where the seabed cuts through the layer — they hold water and the original retains
    # them). This keeps the continental-slope partial cells while excluding fully-dry cells the
    # mapping sometimes leaves as values rather than NaN (the deepest layer, 1800_1850). Plus the
    # uniform bathy clip. `ensemble_incomplete` reproduces the original's mean∪members mask
    # (unset for --no-ensemble stores, so mean-only stays mean-only).
    "wmo": ["never_estimated", "incomplete_timeseries", "outside_latitude", "removed_basin",
            "bed_above_shallow", "bed_above_clip", "ensemble_incomplete"],
    # Whole-cell-wet variant of wmo: bed_above_deep (drops the partial slope cells too) replaces
    # bed_above_shallow, which it supersedes via the shallow=>deep sentinel. bed_above_clip is kept
    # but currently redundant (bed_above_deep already covers every clipped cell).
    "wmo_wet": ["never_estimated", "incomplete_timeseries", "outside_latitude", "removed_basin",
                "bed_above_deep", "bed_above_clip", "ensemble_incomplete"],
}
TERA = 1e12


def preset_mask_value(preset):
    v = 0
    for name in PRESETS[preset]:
        v |= BITS[name]
    return v


def fmt_lev(x):
    xf = float(x)
    return str(int(xf)) if xf == int(xf) else ("%g" % xf)


def _sanitize_tag(tag):
    """Strip all whitespace from a provenance tag; never lowercase or otherwise munge it — it must
    match the provenance record char-for-char."""
    return "".join(tag.split())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("store")
    ap.add_argument("--experiment", required=True)
    ap.add_argument("--tag", default=None,
                    help="provenance tag: the filename's run token AND the provenance_tag header attr. "
                         "Default: inherited from the store's provenance_tag (the ingest --tag); pass "
                         "only to override.")
    ap.add_argument("--provenance-link", default=None,
                    help="URL/path to the provenance record; written to the provenance_link header "
                         "attr. Default: inherited from the store's provenance_link.")
    ap.add_argument("--code-version", required=True,
                    help="URL to the exact publish code (commit/release); stamped as "
                         "localgp_publish_code_version. (This step's own code, not the store's.)")
    ap.add_argument("--preset", default="me4oh", choices=list(PRESETS))
    ap.add_argument("--levels", default=None, help="LOW,HIGH meters for the filename (default: store layer bounds)")
    ap.add_argument("--no-uncertainty", action="store_true",
                    help="skip the ensemble standard-deviation field DATA_SD (reads all members)")
    ap.add_argument("--ensemble", action="store_true",
                    help="also write the full ensemble as OHCENS_<...>.nc, DATA(MEMBER,LON,LAT,TIME)")
    ap.add_argument("--out", default=".")
    args = ap.parse_args()

    ds = xr.open_zarr(args.store, consolidated=False)  # decodes time -> datetime64
    g = ds.attrs
    # Tag + provenance link default to what the ingest step stamped on the store; --tag/--provenance-link
    # override. The tag is the run token in the filename and the provenance_tag attr.
    tag = _sanitize_tag(args.tag if args.tag is not None else g.get("provenance_tag") or g.get("mapped_fields_tag") or "")
    if not tag:
        raise SystemExit("no provenance tag: pass --tag, or ingest the store with --tag so it carries "
                         "provenance_tag")
    prov_link = args.provenance_link if args.provenance_link is not None else g.get("provenance_link")
    has_ens = "field_ensemble" in ds.data_vars           # False for mean-only (--no-ensemble) stores
    if args.ensemble and not has_ens:
        raise SystemExit("--ensemble requested but %s has no field_ensemble "
                         "(ingested with --no-ensemble)" % args.store)
    if not has_ens and not args.no_uncertainty:
        print("note: mean-only store (no field_ensemble) — writing DATA without DATA_SD")

    # --- collapse the selected mask bits to NaN ---
    mval = preset_mask_value(args.preset)
    masked = xr.DataArray((ds["mask_flags"].values.astype("uint8") & mval) != 0,
                          dims=("lat", "lon"))
    data = ds["field_mean"].where(~masked) / TERA        # [time, lat, lon], TJ/m^2

    # --- ensemble 1-sigma (the protocol's "associated uncertainties, where available") ---
    # ddof=1 (sample standard deviation); this reads all ensemble members.
    include_sd = not args.no_uncertainty and has_ens
    sd = (ds["field_ensemble"].std("member", ddof=1).where(~masked) / TERA) if include_sd else None

    # --- time -> days since 1900-01-01 ---
    t = ds["time"].values                              # datetime64
    days1900 = (t - np.datetime64("1900-01-01T00:00:00")) / np.timedelta64(1, "D")
    years = t.astype("datetime64[Y]").astype(int) + 1970
    y0, y1 = int(years.min()), int(years.max())

    # --- layer bounds (meters) for the filename ---
    if args.levels:
        low, high = (s.strip() for s in args.levels.split(","))
    else:
        low, high = fmt_lev(g["layer_top"]), fmt_lev(g["layer_bottom"])

    # --- compliant dataset: DATA(LONGITUDE, LATITUDE, TIME) [+ optional DATA_SD] ---
    def to_lon_lat_time(da):
        return da.transpose("lon", "lat", "time").values.astype("float64")

    data_vars = {"DATA": (("LONGITUDE", "LATITUDE", "TIME"), to_lon_lat_time(data))}
    if include_sd:
        data_vars["DATA_SD"] = (("LONGITUDE", "LATITUDE", "TIME"), to_lon_lat_time(sd))
    out = xr.Dataset(
        data_vars,
        coords={
            "LONGITUDE": ("LONGITUDE", ds["lon"].values),
            "LATITUDE": ("LATITUDE", ds["lat"].values),
            "TIME": ("TIME", days1900.astype("float64")),
        },
    )
    out["LONGITUDE"].attrs = {"units": "degrees_east", "axis": "X"}
    out["LATITUDE"].attrs = {"units": "degrees_north", "axis": "Y"}
    out["TIME"].attrs = {"units": "days since 1900-01-01 00:00:00",
                         "calendar": "proleptic_gregorian", "axis": "T"}
    out["DATA"].attrs = {"units": "TJ/m^2", "long_name": "ocean heat content density"}
    if include_sd:
        out["DATA_SD"].attrs = {
            "units": "TJ/m^2",
            "long_name": "ocean heat content density, ensemble standard deviation (1-sigma)",
            "comment": "std across %d conditional-simulation members (ddof=1)" % ds.sizes["member"],
        }
    out.attrs = {
        "Conventions": "CF-1.8",
        "experiment": args.experiment,
        "period": "%d_%d" % (y0, y1),
        "layer_m": "%s_%s" % (low, high),
        "source": g.get("source", ""),
        "var_name": g["var_name"],
        "model_name": g["model_name"],
        "mapped_layer": "%d_%d" % (int(g["layer_top"]), int(g["layer_bottom"])),
        "cp0": g["cp0"], "rho0": g["rho0"],
        "mask_preset": args.preset,
        "mask_applied": " ".join(PRESETS[args.preset]),
        "provenance_tag": tag,                         # run token; pointer to the provenance record
        "created": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    }
    if prov_link:
        out.attrs["provenance_link"] = prov_link
    if include_sd:
        out.attrs["ensemble_size"] = int(ds.sizes["member"])

    # --- provenance katamari: roll every upstream step's block forward untouched, then add ours ---
    # Each step namespaces its own local provenance by identity, so the chain accretes without
    # collision and downstream never has to know who produced what. We copy the upstream blocks as
    # opaque JSON strings (no parse/re-serialize) and stamp localgp_publish_* for this step.
    STAGE = "localgp_publish"
    for k, v in g.items():
        if k.endswith(("_run_config", "_run_facts", "_code_version")):
            out.attrs[k] = v
    resolved_cfg = dict(vars(args))                    # every resolved flag, no schema to maintain
    resolved_cfg["tag"] = tag                          # effective values (inherited-or-overridden)
    resolved_cfg["provenance_link"] = prov_link
    compact = dict(separators=(",", ":"), default=str)     # one-line JSON, clean in `ncdump -h`
    out.attrs["%s_code_version" % STAGE] = args.code_version
    out.attrs["%s_run_config" % STAGE] = json.dumps(resolved_cfg, **compact)
    out.attrs["%s_run_facts" % STAGE] = json.dumps({
        "period": "%d_%d" % (y0, y1),
        "layer_m": "%s_%s" % (low, high),
        "mapped_layer": "%d_%d" % (int(g["layer_top"]), int(g["layer_bottom"])),
        "preset": args.preset,
        "mask_applied": PRESETS[args.preset],
        "mask_value": int(mval),
        "include_sd": bool(include_sd),
        "ensemble_written": bool(args.ensemble),
        "ensemble_size": int(ds.sizes["member"]) if has_ens else None,
        "n_timesteps": int(len(days1900)),
        "grid_nlon": int(len(ds["lon"])), "grid_nlat": int(len(ds["lat"])),
        "source_store": os.path.abspath(args.store),
    }, **compact)

    fname = "OHC_%s_%d_%d_lev%s_%s_exp%s.nc" % (tag, y0, y1, low, high, args.experiment)
    path = os.path.join(args.out, fname)
    fill = np.float64(np.nan)
    chunk_enc = {"zlib": True, "complevel": 4, "_FillValue": fill}
    enc = {"DATA": dict(chunk_enc)}
    if include_sd:
        enc["DATA_SD"] = dict(chunk_enc)
    out.to_netcdf(path, engine="netcdf4", format="NETCDF4", encoding=enc)
    print("wrote", path, "(%d timesteps, preset=%s, uncertainty=%s)"
          % (len(days1900), args.preset, include_sd))

    # --- optional: the full ensemble as a member-dimensioned sibling file ---
    if args.ensemble:
        ens = (ds["field_ensemble"].where(~masked) / TERA).astype("float64")
        ens = ens.transpose("member", "lon", "lat", "time").rename(
            {"member": "MEMBER", "lon": "LONGITUDE", "lat": "LATITUDE", "time": "TIME"})
        ens = ens.assign_coords(MEMBER=ds["member"].values,
                                LONGITUDE=ds["lon"].values,
                                LATITUDE=ds["lat"].values,
                                TIME=days1900.astype("float64"))
        eds = ens.to_dataset(name="DATA")
        eds["LONGITUDE"].attrs = {"units": "degrees_east", "axis": "X"}
        eds["LATITUDE"].attrs = {"units": "degrees_north", "axis": "Y"}
        eds["TIME"].attrs = {"units": "days since 1900-01-01 00:00:00",
                             "calendar": "proleptic_gregorian", "axis": "T"}
        eds["MEMBER"].attrs = {"long_name": "conditional-simulation member"}
        eds["DATA"].attrs = {"units": "TJ/m^2",
                             "long_name": "ocean heat content density (per ensemble member)"}
        eds.attrs = dict(out.attrs)
        eds.attrs["ensemble_size"] = int(ds.sizes["member"])
        eds.attrs["note"] = ("full conditional-simulation ensemble for per-member downstream "
                             "analysis; NOT a single-field ME4OH submission")

        ename = "OHCENS_%s_%d_%d_lev%s_%s_exp%s.nc" % (tag, y0, y1, low, high, args.experiment)
        epath = os.path.join(args.out, ename)
        nlon, nlat, ntime = len(ds["lon"]), len(ds["lat"]), len(days1900)
        eenc = {"DATA": {"zlib": True, "complevel": 4, "_FillValue": np.float64(np.nan),
                         "chunksizes": (1, nlon, nlat, ntime)}}
        eds.to_netcdf(epath, engine="netcdf4", format="NETCDF4", encoding=eenc)
        print("wrote", epath, "(ensemble: %d members, preset=%s)"
              % (ds.sizes["member"], args.preset))


if __name__ == "__main__":
    main()
