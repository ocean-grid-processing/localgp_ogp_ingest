#!/usr/bin/env python3
"""Full round-trip check: does every grid point of the zarr match the upstream .mat?

Compares the entire store against the LocalGP .mat files — all timesteps of field_mean, and all
every member at all timesteps of field_ensemble — for exact equality (after the quantity's scale and the
lon/lat transpose). Uses scipy.io.loadmat as an independent reader (not our Rust parser), and
opens the store via xarray/zarr (the same path the downstream consumer uses).

Alongside the equality check it tallies the raw mapping values (before scaling) — how many are
positive, exactly zero, negative, or NaN, and the overall range — for the mean and the ensemble,
and lists any months holding exact zeros. This is a report, not a check: it records the mapping's
value conventions in the verification log so a change upstream (a numeric fill instead of NaN, a
sign flip, an unphysical range for the mapped quantity) is visible here rather than in a product.

    python verify_store.py STORE.zarr DIR_MEAN DIR_ENSEMBLE

Access pattern: the store is chunked one file per ensemble member, while the .mat are one file
per month, so we load the full ensemble into memory once (~6.8 GB for the 264-month record) and
stream the .mat. Run it where there's enough RAM (e.g. inside the job allocation).

Requires: xarray, zarr (>=3 for the v3 store), numpy, scipy. Recommended env:
    conda create -n ohc -c conda-forge python=3.12 "xarray>=2025.1" "zarr>=3" scipy numpy
"""
import datetime
import json
import os
import sys

import numpy as np
import xarray as xr
from scipy.io import loadmat


def compare(expected, got):
    expected = expected.astype("float32")
    got = got.astype("float32")
    same_nan = np.array_equal(np.isnan(expected), np.isnan(got))
    finite = ~np.isnan(expected)
    max_diff = float(np.abs(expected[finite] - got[finite]).max()) if finite.any() else 0.0
    return same_nan, max_diff


class Tally:
    """Running counts of the raw mapping values: >0, ==0, <0, NaN, plus the finite range."""

    def __init__(self):
        self.pos = self.zero = self.neg = self.nan = 0
        self.vmin = np.inf
        self.vmax = -np.inf
        self.zero_months = []                      # (year, month) with any exact zero

    def add(self, raw, year, month):
        finite = raw[np.isfinite(raw)]
        self.nan += int(raw.size - finite.size)
        self.pos += int((finite > 0).sum())
        self.neg += int((finite < 0).sum())
        nz = int((finite == 0).sum())
        self.zero += nz
        if nz:
            self.zero_months.append((year, month))
        if finite.size:
            self.vmin = min(self.vmin, float(finite.min()))
            self.vmax = max(self.vmax, float(finite.max()))

    def report(self, label):
        total = self.pos + self.zero + self.neg + self.nan
        pct = lambda n: (100.0 * n / total) if total else 0.0
        print("  %-9s >0: %d (%.1f%%)  ==0: %d (%.1f%%)  <0: %d (%.1f%%)  NaN: %d (%.1f%%)  range [%g, %g]"
              % (label, self.pos, pct(self.pos), self.zero, pct(self.zero), self.neg, pct(self.neg),
                 self.nan, pct(self.nan), self.vmin, self.vmax))
        if self.zero_months:
            shown = ", ".join("%04d-%02d" % ym for ym in self.zero_months[:12])
            more = " …" if len(self.zero_months) > 12 else ""
            print("  %-9s months with exact zeros: %d (%s%s)" % ("", len(self.zero_months), shown, more))


def main(store, dir_mean, dir_ensemble):
    ds = xr.open_zarr(store, consolidated=False, decode_times=False)

    quantity = json.loads(ds.attrs["quantity"])       # the ingest [quantity] table
    top = ds.attrs["layer_top"]
    bottom = ds.attrs["layer_bottom"]
    var = ds.attrs["var_name"]
    model = ds.attrs["model_name"]
    # the ingest factor: the product of the named scale terms, multiplied in key order exactly as the
    # Rust does (a BTreeMap iterates sorted by key), so the arithmetic matches; empty terms -> 1
    scale = 1.0
    for k in sorted(quantity["scale_terms"]):
        scale *= float(quantity["scale_terms"][k])
    has_ens = "field_ensemble" in ds        # mean-only stores (ingested --no-ensemble) omit it

    units = ds["time"].attrs["units"]            # "days since YYYY-MM-15"
    y0, m0, d0 = (int(x) for x in units.split("since")[1].strip().split("-"))
    base = datetime.date(y0, m0, d0)
    times = np.asarray(ds["time"].values)
    nt = len(times)
    print("checking %s plev%d_%d — %d timesteps, %s"
          % (ds.attrs.get("mapped_fields_tag"), top, bottom, nt,
             ("%d members" % ds.sizes["member"]) if has_ens else "mean-only (no ensemble)"))

    # Read each side once: full store into memory, then stream the month-major .mat.
    if has_ens:
        print("loading full store into memory (~%.1f GB)…"
              % (ds["field_ensemble"].size * 4 / 1e9))
    zmean_all = ds["field_mean"].values            # [time, lat, lon]
    zens_all = ds["field_ensemble"].values if has_ens else None   # [member, time, lat, lon]

    worst_mean = 0.0
    worst_ens = 0.0
    tally_mean = Tally()
    tally_ens = Tally()
    for t in range(nt):
        dt = base + datetime.timedelta(days=int(round(float(times[t]))))
        year, month = dt.year, dt.month
        stem = "%sFullField%%s%s_%d_%d_%02d_%d.mat" % (var, model, top, bottom, month, year)
        mean_path = os.path.join(dir_mean, stem % "")
        ens_path = os.path.join(dir_ensemble, stem % "LocalCondSim")

        # mean: loadmat gives [lon, lat] -> [lat, lon]
        raw_mean = loadmat(mean_path)["fullFieldGrid"].T
        tally_mean.add(raw_mean, year, month)
        mat_mean = raw_mean * scale
        ok, md = compare(mat_mean, zmean_all[t])
        assert ok, "field_mean NaN footprint differs at %04d-%02d" % (year, month)
        assert md == 0.0, "field_mean differs at %04d-%02d (max %g)" % (year, month, md)
        worst_mean = max(worst_mean, md)

        # ensemble: [lon, lat, member] -> [member, lat, lon]  (skipped for mean-only stores)
        if has_ens:
            raw_ens = np.transpose(loadmat(ens_path)["fullFieldGrid"], (2, 1, 0))
            tally_ens.add(raw_ens, year, month)
            mat_ens = raw_ens * scale
            ok, md = compare(mat_ens, zens_all[:, t])
            assert ok, "field_ensemble NaN footprint differs at %04d-%02d" % (year, month)
            assert md == 0.0, "field_ensemble differs at %04d-%02d (max %g)" % (year, month, md)
            worst_ens = max(worst_ens, md)

        if (t + 1) % 24 == 0 or t == nt - 1:
            print("  checked %d/%d timesteps (through %04d-%02d)" % (t + 1, nt, year, month))

    print("raw mapping values (before scaling):")
    tally_mean.report("mean")
    if has_ens:
        tally_ens.report("ensemble")

    if has_ens:
        print("PASS — %d timesteps × %d members; field_mean max diff=%g, field_ensemble max diff=%g"
              % (nt, ds.sizes["member"], worst_mean, worst_ens))
    else:
        print("PASS — %d timesteps, mean-only; field_mean max diff=%g" % (nt, worst_mean))


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit("error: expected 3 arguments (got %d)\n\nusage: verify_store.py STORE.zarr DIR_MEAN DIR_ENSEMBLE"
                 % (len(sys.argv) - 1))
    main(sys.argv[1], sys.argv[2], sys.argv[3])
