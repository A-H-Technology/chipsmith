#!/usr/bin/env python3
"""chipsmith's routing delays against quartus_sta's, connection by connection.

    bench/timing-calib.py <design dir> ...

Each <design dir> is one of bench/de2-timing.sh's (build/routed.json,
connections.txt from CHIPSMITH_DUMP_CONNECTIONS, quartus/paths.txt from
quartus_sta's `report_timing -show_routing`). Joins every connection on
Quartus' reported paths to the model's delay for it, by location and port,
and prints:

- per routing element type: Quartus' delays (n, median, mean) next to the
  model's wire_delay, which is what the router sums;
- per connection (less the register output stage, which the model counts
  in its TCO): Quartus / model, overall and by route shape;
- per design: the critical path split into cells, uTco/uTsu and routing,
  with the model's routing for the same connections.
"""
import json
import re
import statistics
import sys
from collections import defaultdict

ROW = re.compile(r"^;\s+([-0-9.]+)\s*;\s+([-0-9.]+)\s*;\s*(\w*)\s*;\s*(\w*)\s*;\s*(\d*)\s*;\s*(\S*)\s*;\s*(.*?)\s*;$")


def locations(routed):
    """chipsmith cell name -> Quartus location name (LCCOMB_X63_Y2_N0)."""
    cells = json.load(open(routed))["modules"]["top"]["cells"]
    out = {}
    for name, c in cells.items():
        bel = c["attributes"].get("NEXTPNR_BEL", "")
        m = re.match(r"X(\d+)Y(\d+)/LE(\d+)\.(LUT|FF)$", bel)
        if m:
            x, y, le, kind = int(m[1]), int(m[2]), int(m[3]), m[4]
            n = 2 * le + (kind == "FF")
            out[name] = f"{'LCCOMB' if kind == 'LUT' else 'LCFF'}_X{x // 2}_Y{y}_N{n}"
    return out


def model_connections(path, loc):
    """(driver location, port, sink location, port) -> (model ps, wire types)."""
    out = {}
    for line in open(path):
        f = line.split()
        (dc, dp), (sc, sp) = f[0].rsplit(".", 1), f[1].rsplit(".", 1)
        if dc in loc and sc in loc:
            out[(loc[dc], dp, loc[sc], sp)] = (float(f[2]), f[4:])
    return out


def paths(report):
    """Each data path as rows (total, incr, type, location, element)."""
    out, cur, in_data = [], None, False
    for line in open(report):
        if line.startswith("; Data Arrival Path"):
            cur, in_data = [], False
            continue
        if cur is None:
            continue
        if line.startswith("+") and in_data and cur:
            out.append(cur)
            cur = None
            continue
        m = ROW.match(line)
        if not m:
            continue
        total, incr, _, typ, _, location, element = m.groups()
        if element == "data path":
            in_data = True
            continue
        if in_data:
            cur.append((float(total), float(incr), typ, location, element))
    return out


def split(rows):
    """A path's connections: (driver loc, port, sink loc, port, delay ps, elements)."""
    conns = []
    src, t0, res = None, 0.0, []
    for total, incr, typ, location, element in rows:
        if typ == "CELL" and "|" in element:
            src, t0, res = (location, element.split("|")[1].upper()), total, []
        elif typ == "RE":
            res.append((location.split("_X")[0], incr * 1000))
        elif typ == "IC" and src:
            port = re.sub(r"\[\d+\]$", "", element.split("|")[1]).upper()
            # The register's output stage is in the model's TCO.
            ff = sum(t for e, t in res if e == "LCFF")
            conns.append((src[0], src[1], location, port, (total - t0) * 1000 - ff, res))
            src = None
    return conns


def main(dirs):
    elems = defaultdict(list)  # element type -> Quartus ps
    ratios = defaultdict(list)  # route shape -> Quartus / model
    seen = set()
    unmatched = 0
    for d in dirs:
        loc = locations(f"{d}/build/routed.json")
        model = model_connections(f"{d}/connections.txt", loc)
        ps = paths(f"{d}/quartus/paths.txt")
        for p in ps:
            for c in split(p):
                key = c[:4]
                if (d, key) in seen:
                    continue
                seen.add((d, key))
                for e, t in c[5]:
                    elems[e].append(t)
                if key in model:
                    m, wires = model[key]
                    shape = " ".join(w for w in wires if w not in ("LE_INPUT", "LUT_INPUT", "LE_OUTPUT"))
                    if m > 0:
                        ratios[shape].append((c[4], m))
                elif c[4] > 0:
                    unmatched += 1
        # The critical path (the report's first) by kind of delay.
        if ps:
            crit = ps[0]
            cells = sum(r[1] for r in crit if r[2] == "CELL") * 1000
            utco = sum(r[1] for r in crit if r[2] == "uTco") * 1000
            ic = sum(c[4] for c in split(crit))
            mic = sum(model.get(c[:4], (0.0 if c[4] == 0 else float("nan"), None))[0] for c in split(crit))
            print(f"{d.rstrip('/').split('/')[-1]}: critical data path {crit[-1][0] - crit[0][0] + crit[0][1]:.3f} ns: "
                  f"uTco {utco:.0f}, cells {cells:.0f}, routing {ic:.0f} ps (model {mic:.0f} ps)")
    print()
    print("| element | n | Quartus median | mean | min | max |")
    print("|---|---|---|---|---|---|")
    for e, ts in sorted(elems.items(), key=lambda kv: -len(kv[1])):
        print(f"| {e} | {len(ts)} | {statistics.median(ts):.0f} | {statistics.mean(ts):.0f} | {min(ts):.0f} | {max(ts):.0f} |")
    print()
    print("| route (model wires) | n | Quartus mean | model mean | Quartus / model (median) |")
    print("|---|---|---|---|---|")
    every = []
    for shape, qm in sorted(ratios.items(), key=lambda kv: -len(kv[1])):
        every += qm
        r = statistics.median(q / m for q, m in qm)
        print(f"| {shape} | {len(qm)} | {statistics.mean(q for q, _ in qm):.0f} | {statistics.mean(m for _, m in qm):.0f} | {r:.3f} |")
    if every:
        print(f"\nall {len(every)} connections: Quartus / model median "
              f"{statistics.median(q / m for q, m in every):.3f}, mean of sums "
              f"{sum(q for q, _ in every) / sum(m for _, m in every):.3f}; {unmatched} not matched")


main(sys.argv[1:])
