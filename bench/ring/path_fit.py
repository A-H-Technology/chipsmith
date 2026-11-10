#!/usr/bin/env python3
"""Fit the timing model's slow-corner scales and register, M4K and I/O-cell
constants to quartus_sta's path totals on chipsmith's own placements.

    bench/ring/path_fit.py <silicon.json> <qor run dir> ...

Each run dir is bench/qor.sh's with CALIB=1 (build/routed.json,
connections.txt, quartus/paths.txt: quartus_sta's 300 worst setup paths),
plus bench/ring/extra-paths.sh's paths-extra.txt where it exists.
Per path, the total quartus_sta needs (setup relationship less slack) is
taken as

    launch constant + capture constant
        + the model's routing of every connection on the path
          (timing.rs's wire delays of the wires chipsmith routed it on)
        + k_lut, k_carry, k_mult x the silicon delays of the path's LUT,
          carry-chain and multiplier cells (<silicon.json>, derive.py;
          derive.model is how the model's values come from them)

and the scales and constants (one per kind of path start, one per register
input a path ends on) are solved by least squares. So silicon gives each
element's delay against the others of its class (per LUT input, per carry
link, per product bit) and quartus_sta's paths, in aggregate, one
slow-corner factor per class; a factor below 1 is held at 1. Only path totals are used, not
quartus_sta's delay of any one element, and the routing is chipsmith's
model rather than Quartus' delays, so each constant also takes up what
the model's routing misses on such paths. One sum is not identifiable
(every path has one start and one end): LE register setup through DATAIN
is held at 0.

Prints the scales and constants, the residuals per design, and writes them
to <silicon.json>'s directory as constants.json.
"""
import json
import math
import os
import re
import statistics
import sys
from collections import defaultdict

WIRE = {"R4": 235.0, "C4": 285.0, "R24": 433.0, "C16": 465.0, "LOCAL_INTERCONNECT": 362.0,
        "LOCAL_LINE": 229.0, "LE_BUFFER": 56.0, "M4K_BUFFER": 143.0, "DSP_BUFFER": 143.0,
        "IO_DATAIN": 66.0, "BLK_CLK_BUF": 96.0, "LAB_CLK": 363.0, "GLOBAL_CLK_V": 181.0,
        "GLOBAL_CLK_H": 385.0, "CLK_BUFFER": 118.0}
ROW = re.compile(r"^;\s+([-0-9.]+)\s*;\s+([-0-9.]+)\s*;\s*(\w*)\s*;\s*(\w*)\s*;\s*(\d*)\s*;\s*(\S*)\s*;\s*(.*?)\s*;$")
LE_PORTS = {"dataa": "DATAA", "datab": "DATAB", "datac": "DATAC", "datad": "DATAD", "cin": "CIN",
            "datain": "DATAIN", "sdata": "SDATA", "ena": "ENA", "sclr": "SCLR", "sload": "SLOAD",
            "aclr": "ACLR"}


def solve(rows, ys, n):
    a = [[sum(r[i] * r[j] for r in rows) for j in range(n)] for i in range(n)]
    b = [sum(r[i] * y for r, y in zip(rows, ys)) for i in range(n)]
    m = len(a)
    aug = [row[:] + [float(i == j) for j in range(m)] for i, row in enumerate(a)]
    for c in range(m):
        p = max(range(c, m), key=lambda r: abs(aug[r][c]))
        assert abs(aug[p][c]) > 1e-9, f"unknown {c} not identifiable"
        aug[c], aug[p] = aug[p], aug[c]
        piv = aug[c][c]
        aug[c] = [v / piv for v in aug[c]]
        for r in range(m):
            if r != c and aug[r][c]:
                f = aug[r][c]
                aug[r] = [v - f * w for v, w in zip(aug[r], aug[c])]
    inv = [row[m:] for row in aug]
    x = [sum(inv[i][j] * b[j] for j in range(n)) for i in range(n)]
    res = [y - sum(r[i] * x[i] for i in range(n)) for r, y in zip(rows, ys)]
    s2 = sum(e * e for e in res) / max(1, len(rows) - n)
    return x, [math.sqrt(max(0, s2 * inv[i][i])) for i in range(n)], res


def cell_index(routed):
    """Quartus location prefix (X_Y or X_Y_N) -> (chipsmith cell, type, params)."""
    cells = json.load(open(routed))["modules"]["top"]["cells"]
    out = {}
    for name, c in cells.items():
        bel = c["attributes"].get("NEXTPNR_BEL", "")
        m = re.match(r"X(\d+)Y(\d+)/(.*)$", bel)
        if not m:
            continue
        x, y, b = int(m[1]), int(m[2]), m[3]
        le = re.match(r"LE(\d+)\.(LUT|FF)$", b)
        if le:
            k = 2 * int(le[1]) + (le[2] == "FF")
            out[f"{'LCCOMB' if le[2] == 'LUT' else 'LCFF'}_X{x // 2}_Y{y}_N{k}"] = (name, c["type"], c["parameters"])
        else:
            out.setdefault(f"X{x // 2}_Y{y}", []).append((name, c["type"], c["parameters"]))
    return out


def model_routing(path):
    """(sink cell, port) -> model routing ps; (driver cell, sink cell) -> the same."""
    by_sink, by_pair = {}, {}
    for line in open(path):
        f = line.split()
        (dc, _), (sc, sp) = f[0].rsplit(".", 1), f[1].rsplit(".", 1)
        d = sum(WIRE.get(w, 0.0) for w in f[4:])
        by_sink[(sc, sp)] = d
        by_pair[(dc, sc)] = max(d, by_pair.get((dc, sc), 0.0))
    return by_sink, by_pair


def paths(report):
    """(relationship - slack in ps, data path rows) per path."""
    out, rows, slack, rel, in_data = [], None, None, None, False
    for line in open(report):
        if line.startswith("; Slack ") and "Node" not in line:
            slack = float(line.split(";")[2])
        elif line.startswith("; Setup Relationship"):
            rel = float(line.split(";")[2])
        elif line.startswith("; Data Arrival Path"):
            rows, in_data = [], False
        elif rows is not None:
            if line.startswith("+") and in_data and rows:
                out.append(((rel - slack) * 1000, rows))
                rows = None
                continue
            m = ROW.match(line)
            if m:
                _, incr, _, typ, _, loc, elem = m.groups()
                if elem == "data path":
                    in_data = True
                elif in_data:
                    rows.append((typ, loc, elem))
    return out


def m4k_outreg(params):
    return any("OUTREG" in k and str(v).strip() not in ("NONE", "\"NONE\"") for k, v in params.items())


# A kind of path start or end needs this many of the worst paths to be
# fitted from them (stage 1, `main`).
MIN_PATHS = 20


class Skip(Exception):
    pass


def block(index, loc):
    """The hard block cell at a Quartus location like M4K_X26_Y19 or DSPMULT_X..."""
    m = re.search(r"_X(\d+)_Y(\d+)", loc)
    cands = index.get(f"X{m[1]}_Y{m[2]}") or []
    if not cands:
        raise Skip(f"no cell at {loc}")
    return cands[0]


def path_terms(rows, index, by_sink, by_pair, si):
    """(launch kind, capture kind, known ps, class sums) of a path: `known`
    is the model's routing and the cells' fixed parts, each class sum
    (`k_lut`, `k_carry`, `k_mult`) what its slow-corner scale multiplies."""
    known = 0.0
    feats = defaultdict(float)
    first = next((r for r in rows if r[0] == "uTco"), None) or rows[0]
    loc = first[1]
    if loc.startswith("LCFF"):
        launch = "tco LE"
    elif loc.startswith("M4K"):
        launch = "tco M4K outreg" if m4k_outreg(block(index, loc)[2]) else "tco M4K"
    elif loc.startswith("IOC"):
        launch = "tco IOE"
    elif loc.startswith("DSP"):
        launch = "tco MULT"
    else:
        raise Skip(f"start {loc}")
    prev = index.get(loc, None) if loc.startswith("LC") else block(index, loc)
    cur_in = None
    capture = None
    in_mult = 0.0
    for typ, loc, elem in rows:
        if typ == "IC":
            port = re.sub(r"\[\d+\]$", "", elem.split("|")[-1])
            if loc.startswith("LC"):
                if loc not in index:
                    raise Skip(f"no LE at {loc}")
                sink = index[loc]
                p = LE_PORTS.get(port)
                if p is None:
                    raise Skip(f"port {port}")
                if p != "CIN":
                    # By driver and sink: the dump names a LUT's inputs as
                    # the netlist does, Quartus by the physical pin the
                    # router put them on.
                    key = (prev[0], sink[0]) if prev else None
                    if key in by_pair:
                        known += by_pair[key]
                    elif not (p == "DATAIN" and prev and prev[1] == "LCELL_COMB"):
                        # A register's own LE's LUT reaches it without
                        # routing; anything else is a connection missing.
                        raise Skip(f"connection into {loc}|{port}")
                if loc.startswith("LCFF"):
                    capture = f"tsu {p}"
                cur_in = p
                prev = sink
            else:
                sink = block(index, loc)
                if prev is None:
                    raise Skip("block after block")
                if loc.startswith("DSP") and (prev[1] == "CY2_MULT"):
                    prev = sink
                    continue
                d = by_pair.get((prev[0], sink[0]))
                if d is None:
                    raise Skip(f"connection into {loc}")
                known += d
                if loc.startswith("M4K"):
                    capture = "tsu M4K"
                elif loc.startswith("IOC"):
                    capture = "tsu IOE"
                elif loc.startswith("DSP"):
                    capture = "tsu MULT"
                    if not in_mult:
                        in_mult = mult_si(si, sink)
                        feats["k_mult"] += in_mult
                prev = sink
        elif typ == "CELL" and loc.startswith("LCCOMB") and "|" in elem:
            out = elem.split("|")[-1]
            n = int(loc.rsplit("_N", 1)[1]) // 2
            if cur_in is None:
                raise Skip("cell without input")
            fixed, f = arc(si, cur_in, out, n)
            known += fixed
            for k, v in f.items():
                feats[k] += v
            capture = None
        elif typ == "CELL" and loc.startswith("DSP"):
            # Through the multiplier unregistered: it is the capture only if
            # the path ends in it (decided by its last IC).
            if prev and prev[1] == "CY2_MULT" and not in_mult:
                in_mult = mult_si(si, prev)
                feats["k_mult"] += in_mult
                capture = None
    if capture is None:
        raise Skip("no capture")
    if capture == "tsu MULT" and in_mult:
        feats["k_mult"] -= in_mult
    return launch, capture, known, dict(feats)


def mult_si(si, block_cell):
    """An unregistered multiplier's silicon delay through: nine-bit rows or
    a full block (chipsmith-cyclone2's mult::nine_bit_params)."""
    params = block_cell[2]
    nine = all(int(str(params.get(p, "0")).strip() or "0", 2) <= 9 for p in ("A_WIDTH", "B_WIDTH"))
    return si["mult9"] if nine else si["mult"]


LL = 229.0


def arc(si, i, o, le):
    """(fixed ps, class coefficients) of a LUT arc, as derive.model builds
    the model's values from silicon."""
    link = si["lcb"] if le == 7 else si["lab"] if le == 15 else 0.0
    if o == "combout":
        pin = 2 if i == "CIN" else "ABCD".index(i[-1])
        return -LL, {"k_lut": si["lut"][pin]}
    if o == "cout":
        if i == "CIN":
            return 0.0, {"k_carry": si["cin_cout"] + link}
        pin = "ABCD".index(i[-1])
        if pin > 1:
            raise Skip(f"carry head on {i}")
        return 0.0, {"k_lut": si["head"][pin] - si["lut"][2], "k_carry": link}
    raise Skip(f"arc {i}->{o}")
def fit_stage(data, fixed):
    """Least squares for every launch and capture kind and class scale not
    in `fixed`, which are taken off the totals."""
    names = sorted({x for _, l, c, _, _ in data for x in (l, c)} - set(fixed))
    extra = sorted({k for *_, f in data for k in f} - set(fixed))
    cols = names + extra
    rows = [[float(n in (l, c)) for n in names] + [f.get(n, 0.0) for n in extra] for _, l, c, _, f in data]
    ys = [y - sum(v for k, v in fixed.items() if k in (l, c)) - sum(fixed[k] * v for k, v in f.items() if k in fixed)
          for _, l, c, y, f in data]
    x, se, res = solve(rows, ys, len(cols))
    return dict(zip(cols, x)), dict(zip(cols, se)), res


def main():
    si = json.load(open(sys.argv[1]))
    worst, extra, skipped = [], [], defaultdict(int)
    for d in sys.argv[2:]:
        index = cell_index(f"{d}/build/routed.json")
        by_sink, by_pair = model_routing(f"{d}/connections.txt")
        seen = set()
        for report, into in [("paths.txt", worst), ("paths-extra.txt", extra)]:
            if not os.path.exists(f"{d}/quartus/{report}"):
                continue
            for total, rows in paths(f"{d}/quartus/{report}"):
                if tuple(rows) in seen:
                    continue
                seen.add(tuple(rows))
                try:
                    launch, capture, known, extra_feats = path_terms(rows, index, by_sink, by_pair, si)
                except Skip as e:
                    skipped[str(e).split(" ")[0]] += 1
                    continue
                into.append((os.path.basename(d.rstrip("/")), launch, capture, total - known, extra_feats))
    # Stage 1: every kind the designs' 300 worst paths have enough of (the
    # paths that set Fmax). Stage 2: the other kinds from the extra
    # reports, stage 1's held: the model's routing falls further short of
    # Quartus' on long paths than on short ones, so constants fitted to
    # short paths would understate the critical ones.
    count = defaultdict(int)
    for _, l, c, _, _ in worst:
        count[l] += 1
        count[c] += 1
    fixed = {"tsu DATAIN": 0.0}
    enough = {k for k, n in count.items() if n >= MIN_PATHS} | set(fixed)
    stage1 = [p for p in worst if p[1] in enough and p[2] in enough]
    x1, se1, res1 = fit_stage(stage1, fixed)
    # A class's slow corner can't be faster than this chip at room
    # temperature: a scale below 1 is held at 1.
    while any(x1[k] < 1.0 for k in x1 if k.startswith("k_") and k not in fixed):
        for k in [k for k in x1 if k.startswith("k_") and x1[k] < 1.0]:
            fixed[k] = 1.0
        x1, se1, res1 = fit_stage(stage1, fixed)
    held = fixed | x1
    held_k = {k: v for k, v in held.items() if k.startswith("k_")}
    # Stage 2 holds the scales too: their cells go back into the totals.
    rest = [(d, l, c, y - sum(held_k[k] * v for k, v in f.items()), {})
            for d, l, c, y, f in worst + extra if (l not in held) != (c not in held)]
    x2, se2, res2 = fit_stage(rest, held) if rest else ({}, {}, [])
    print(f"stage 1: {len(stage1)} of the worst paths of {len(sys.argv) - 2} designs; "
          f"stage 2: {len(rest)} paths; skipped {dict(skipped)}")
    every = defaultdict(int)
    for _, l, c, _, _ in stage1 + rest:
        every[l] += 1
        every[c] += 1
    for stage, x, se in [(1, x1, se1), (2, x2, se2)]:
        for n in sorted(x):
            if n.startswith("k_"):
                print(f"  {n:16s} {x[n]:8.3f}    +- {se[n]:5.3f}  (stage {stage}, class scale)")
            else:
                print(f"  {n:16s} {x[n]:8.1f} ps +- {se[n]:5.1f}  (stage {stage}, {every[n]} paths)")
    for k, v in fixed.items():
        print(f"  {k:16s} {v:8.1f} ps (held)")
    print(f"  residual rms {statistics.pstdev(res1):.0f} ps (stage 1)")
    by = defaultdict(list)
    for (dsg, l, c, y, _), e in zip(stage1, res1):
        by[dsg].append(e)
    for dsg, es in sorted(by.items()):
        print(f"    {dsg:16s} n={len(es):3d} mean residual {statistics.mean(es):7.1f} max {max(es):7.1f} min {min(es):7.1f}")
    out = held | x2
    json.dump(out, open(os.path.join(os.path.dirname(os.path.abspath(sys.argv[1])), "constants.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
