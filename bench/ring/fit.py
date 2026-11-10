#!/usr/bin/env python3
"""Fit per-element delays to bench/ring's silicon counts and quartus_sta paths.

    bench/ring/fit.py <measure.sh out dir> [<sta dir> ...]

Silicon (measure.sh): a ring's half period is the sum, over its stages, of
the LUT arc from the stage's physical input and the wires the router took
into it (from CHIPSMITH_DUMP_CONNECTIONS), plus a constant for the tap and
its global clock. quartus_sta (sta.sh, the same placements built open):
report_path from the register feeding stage 0 to the last stage's output,
the same sum plus a constant for the register's output. Unknowns, solved by
least squares over every ring (each source on its own):

- `lut+LL <pin>`: the LUT arc from that input plus a LOCAL_LINE hop (every
  hop ends on a LOCAL_LINE or a LAB line, so the two can't be told apart);
- `LAB-LL`: what a LE_BUFFER + LAB line hop costs over a LOCAL_LINE;
- one per long wire (R4, C4, ...): its delay;
- carry rings: per chain `head <pin>+tail+LL` (the head's input to COUT,
  the tail's CIN to COMBOUT and the LOCAL_LINE part of the hop out of the
  tail, which always come together), `cin->cout` per middle LE, `link lcb`
  per LE 7 to 8, `link lab` per LE 15 to the next LAB's LE 0;
- the constants.

Prints both fits side by side with their ratio (quartus_sta's slow corner
over this chip at room temperature), the residuals, the spread across
reads, loads and placements, and the single scale that takes the silicon
fit to quartus_sta's segment totals. Writes both fits to <out>/fit.json.
"""
import glob
import json
import math
import os
import statistics
import sys
from collections import defaultdict

# Above about 340 MHz the tap's clock path sometimes counts every edge
# twice (16-LE carry rings read 351 or 703 MHz, 387 or 765, depending on
# the load); nothing below that has.
MAX_MHZ = 330.0
IGNORED = {"LE_INPUT", "LUT_INPUT", "LOCAL_LINE", "LE_BUFFER", "LOCAL_INTERCONNECT"}


def solve(rows, ys, n):
    """Least squares by normal equations: (x, standard errors, residuals)."""
    a = [[sum(r[i] * r[j] for r in rows) for j in range(n)] for i in range(n)]
    b = [sum(r[i] * y for r, y in zip(rows, ys)) for i in range(n)]
    inv = invert(a)
    x = [sum(inv[i][j] * b[j] for j in range(n)) for i in range(n)]
    res = [y - sum(r[i] * x[i] for i in range(n)) for r, y in zip(rows, ys)]
    s2 = sum(e * e for e in res) / max(1, len(rows) - n)
    se = [math.sqrt(max(0.0, s2 * inv[i][i])) for i in range(n)]
    return x, se, res


def invert(m):
    n = len(m)
    a = [row[:] + [float(i == j) for j in range(n)] for i, row in enumerate(m)]
    for c in range(n):
        p = max(range(c, n), key=lambda r: abs(a[r][c]))
        if abs(a[p][c]) < 1e-9:
            raise SystemExit(f"singular: unknown {c} is not identifiable")
        a[c], a[p] = a[p], a[c]
        piv = a[c][c]
        a[c] = [v / piv for v in a[c]]
        for r in range(n):
            if r != c and a[r][c]:
                f = a[r][c]
                a[r] = [v - f * w for v, w in zip(a[r], a[c])]
    return [row[n:] for row in a]


def hop(f, wires, lut=None):
    """A hop's wires into features; `lut` adds the LUT arc it ends on."""
    if lut:
        f[f"lut+LL {lut}"] += 1
    if "LOCAL_INTERCONNECT" in wires:
        f["LAB-LL"] += 1
    for w in wires:
        if w not in IGNORED:
            f[w] += 1


def features(k, ring, conns, opened):
    """A ring's features: round the ring, or (`opened`) from the register
    feeding stage 0 to the last stage's output."""
    f = defaultdict(float)
    n, pin = len(ring["les"]), ring["pin"]

    def into(dst_stage, src_stage):
        dst = f"r{k}_s{dst_stage}.DATA{pin}"
        if opened and dst_stage == 0:
            return next(w for (s, d), w in conns.items() if d == dst and s.endswith(".REGOUT"))
        return conns[(f"r{k}_s{src_stage}.COMBOUT", dst)]

    if ring.get("kind", "lut") == "lut":
        for s in range(n):
            hop(f, into(s, (s - 1) % n), pin)
        f["const open" if opened else "const lut"] = 1
        return f
    les, first = ring["les"], 0
    for m in ring.get("chains", [n]):
        last = first + m - 1
        f[f"head {pin}+tail+LL"] += 1
        f["cin->cout"] += m - 2
        for s in range(first, last):
            link = (les[s][2], les[s + 1][2])
            if link == (7, 8):
                f["link lcb"] += 1
            elif link == (15, 0):
                f["link lab"] += 1
        hop(f, into(first, (first - 1) % n))
        first = last + 1
    f["const open" if opened else "const carry"] = 1
    return f


def connections(path):
    out = {}
    for line in open(path):
        w = line.split()
        out[(w[0], w[1])] = w[4:]
    return out


def silicon(out_dir):
    """(features, half period ps, structure key, load key) per window read."""
    data = []
    for counts in sorted(glob.glob(f"{out_dir}/*/*/counts.txt")):
        run = os.path.dirname(counts)
        spec_name, rep = run.split("/")[-2:]
        spec = json.load(open(f"{run}/project/ring.json"))["rings"]
        conns = connections(f"{run}/connections.txt")
        for line in open(counts):
            _, k, _, mhz = line.split()
            k, mhz = int(k), float(mhz)
            ring = spec[k]
            # Multiplier rings: bench/ring/derive.py. Carry rings whose
            # chains ended up across the die (specs.py's column wrap) are
            # mostly R24 routing, which per-type medians model worst.
            f = features(k, ring, conns, False) if ring.get("kind") != "mult" else None
            if f and ring.get("kind") == "carry" and (f.get("R24") or f.get("C16")):
                continue
            if 0 < mhz < MAX_MHZ and f:
                data.append((f, 1e6 / mhz / 2,
                             (spec_name, ring["name"]), (spec_name, rep, ring["name"])))
    return data


def sta(dirs):
    data = []
    for d in dirs:
        spec = json.load(open(f"{d}/spec.json"))
        conns = connections(f"{d}/connections.txt")
        for line in open(f"{d}/sta.txt"):
            _, k, _, ns = line.split()
            k, ns = int(k), float(ns)
            ring = spec[k]
            if ns > 0 and ring.get("kind") != "mult":
                data.append((features(k, ring, conns, True), ns * 1000,
                             (os.path.basename(d), ring["name"]), None))
    return data


def fit(data):
    names = sorted({n for f, *_ in data for n in f})
    rows = [[f.get(n, 0.0) for n in names] for f, *_ in data]
    x, se, res = solve(rows, [t for _, t, *_ in data], len(names))
    return dict(zip(names, x)), dict(zip(names, se)), res


def spreads(data, res):
    by_load = defaultdict(list)
    for _, t, _, load in data:
        by_load[load].append(t)
    reads = [statistics.pstdev(v) / statistics.mean(v) for v in by_load.values() if len(v) > 1]
    by_ring = defaultdict(list)
    for (spec, rep, name), v in by_load.items():
        by_ring[(spec, name)].append(statistics.mean(v))
    loads = [statistics.pstdev(v) / statistics.mean(v) for v in by_ring.values() if len(v) > 1]
    print(f"  spread across reads of one load: median {1e6 * statistics.median(reads):.0f} ppm, "
          f"max {1e6 * max(reads):.0f}")
    print(f"  spread across loads of one bitstream: median {1e6 * statistics.median(loads):.0f} ppm, "
          f"max {1e6 * max(loads):.0f}")


def residuals_by_structure(data, res, label):
    """Mean residual of each structure (ring name over every region)."""
    by = defaultdict(list)
    for (_, t, (spec, name), _), e in zip(data, res):
        by[(spec.rstrip("0123456789"), name)].append((e, t))
    print(f"  {label} residual by structure over placements (mean, sd, ps; % of total):")
    for (kind, name), v in sorted(by.items()):
        es = [e for e, _ in v]
        mean_t = statistics.mean(t for _, t in v)
        print(f"    {kind:5s} {name:6s} n={len(v):3d} {statistics.mean(es):7.1f} {statistics.pstdev(es):6.1f}"
              f"  ({100 * statistics.mean(es) / mean_t:+.1f}%)")


def main():
    out = sys.argv[1]
    si = silicon(out)
    xs, es, rs = fit(si)
    print(f"silicon: {len(si)} reads of {len({d[3] for d in si})} ring loads; residual rms "
          f"{statistics.pstdev(rs):.1f} ps of mean half period {statistics.mean(t for _, t, *_ in si):.0f}")
    spreads(si, rs)
    result = {"silicon": {n: [xs[n], es[n]] for n in xs}}
    if len(sys.argv) > 2:
        st = sta(sys.argv[2:])
        xq, eq, rq = fit(st)
        print(f"quartus_sta: {len(st)} open segments; residual rms {statistics.pstdev(rq):.1f} ps "
              f"of mean {statistics.mean(t for _, t, *_ in st):.0f}")
        result["quartus_sta"] = {n: [xq[n], eq[n]] for n in xq}
        print(f"\n  {'unknown':20s} {'silicon':>14s} {'quartus_sta':>14s}  ratio")
        for n in sorted(set(xs) | set(xq)):
            a = f"{xs[n]:7.1f} +-{es[n]:4.1f}" if n in xs else " " * 14
            b = f"{xq[n]:7.1f} +-{eq[n]:4.1f}" if n in xq else " " * 14
            r = f"{xq[n] / xs[n]:.3f}" if n in xs and n in xq and abs(xs[n]) > 20 else ""
            print(f"  {n:20s} {a} {b}  {r}")
        # One scale: quartus_sta's segments against the silicon fit's
        # prediction of the same segments (their own constant aside).
        pred = [sum(xs.get(n, 0.0) * v for n, v in f.items() if not n.startswith("const")) for f, *_ in st]
        tot = [t for _, t, *_ in st]
        rows = [[p, 1.0] for p in pred]
        (k, c), (ek, ec), r = solve(rows, tot, 2)
        print(f"\n  quartus_sta segment = {k:.4f} (+-{ek:.4f}) x silicon + {c:.0f} ps; "
              f"residual rms {statistics.pstdev(r):.0f} ps")
        ratios = sorted(t / p for t, p in zip(tot, pred) if p > 0)
        print(f"  per segment quartus_sta / silicon: min {ratios[0]:.3f} median "
              f"{statistics.median(ratios):.3f} max {ratios[-1]:.3f}")
        result["scale"] = [k, ek, c]
        if os.environ.get("FIT_VERBOSE"):
            residuals_by_structure(st, rq, "quartus_sta")
    if os.environ.get("FIT_VERBOSE"):
        residuals_by_structure(si, rs, "silicon")
    json.dump(result, open(f"{out}/fit.json", "w"), indent=1)


if __name__ == "__main__":
    main()
