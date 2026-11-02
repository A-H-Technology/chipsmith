#!/usr/bin/env python3
"""Where a packed design's LEs go, in the categories of Quartus' fit report.

    bench/qor-stats.py <build/packed.json> [<quartus top.fit.rpt>]

Counts chipsmith's LUTs by mode (normal, arithmetic) and by used inputs,
its registers by how they sit (beside their LUT, register only on the sync
load path) and what drives a register-only one, and prints Quartus' fit
report's "Fitter Resource Usage Summary" beside them when given.
"""
import json
import re
import sys
from collections import Counter

INPUTS = ["DATAA", "DATAB", "DATAC", "DATAD"]


def net(c, p):
    bits = c["connections"].get(p) or []
    return bits[0] if bits else None


def chipsmith(path):
    cells = json.load(open(path))["modules"]["top"]["cells"]
    driver, users = {}, Counter()
    for name, c in cells.items():
        for p, d in c["port_directions"].items():
            n = net(c, p)
            if n is None:
                continue
            if d == "output":
                driver[n] = (name, p)
            else:
                users[n] += 1
    s = Counter()
    for name, c in cells.items():
        t = c["type"]
        if t == "LCELL_COMB":
            s["luts"] += 1
            arith = any(net(c, p) is not None and (p != "COUT" or users[net(c, p)])
                        for p in ["CIN", "COUT"])
            k = sum(net(c, p) is not None for p in INPUTS)
            if arith:
                s["arithmetic"] += 1
            else:
                s["normal"] += 1
                s[f"normal {k}-input" if k > 2 else "normal <=2-input"] += 1
        elif t == "LCELL_FF":
            s["registers"] += 1
            sd = net(c, "SDATA")
            if sd is None:
                s["with LUT"] += 1
                continue
            s["register only"] += 1
            src = driver.get(sd)
            kind = cells[src[0]]["type"] if src else "constant"
            s[f"register only, from {kind}"] += 1
    return s


def quartus(path):
    text = open(path, errors="replace").read()
    block = text.split("; Fitter Resource Usage Summary", 1)[1].split("\n\n", 1)[0]
    q = {}
    for line in block.splitlines():
        m = re.match(r";\s*(--\s*)?([^;]+?)\s*;\s*([\d,]+)", line)
        if m:
            q[m.group(2)] = int(m.group(3).replace(",", ""))
    return q


def main():
    s = chipsmith(sys.argv[1])
    les = s["luts"] + s["register only"]
    print(f"chipsmith: {les} LEs")
    for k in sorted(s):
        print(f"  {k:32} {s[k]}")
    if len(sys.argv) > 2:
        q = quartus(sys.argv[2])
        print(f"quartus: {q.get('Total logic elements')} LEs")
        for k, v in q.items():
            print(f"  {k:44} {v}")


if __name__ == "__main__":
    main()
