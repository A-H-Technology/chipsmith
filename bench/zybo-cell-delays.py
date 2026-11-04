#!/usr/bin/env python3
"""Vivado's cell delays as its timing reports show them.

    bench/zybo-cell-delays.py <design dir> ...

Reads each dir's paths.rpt (xc7-re tools/import.sh: `report_timing` of the
1,000 worst endpoints) and prints every cell arc Vivado times
(`Prop_lut6_I0_O`, `Setup_fdre_C_D`, ...) with its count and its delays
(min, median, max, ns), the numbers to hold chipsmith's cell delays
(prjxray-db's SDF) against.
"""
import re
import statistics
import sys
from collections import defaultdict

ARC = re.compile(r"\s(\w+) \(((?:Prop|Setup|Recov|Hold)_[^)]+)\)\s*(-?\d+\.\d+)?")
NUM = re.compile(r"^\s+(-?\d+\.\d+)\s+(-?\d+\.\d+)")

arcs = defaultdict(list)
for d in sys.argv[1:]:
    pending = None
    for line in open(f"{d}/paths.rpt"):
        m = ARC.search(line)
        if m:
            if m[3] is not None:
                arcs[(m[1], m[2])].append(float(m[3]))
                pending = None
            else:
                pending = (m[1], m[2])
            continue
        if pending:
            n = NUM.match(line)
            if n:
                arcs[pending].append(float(n[1]))
            pending = None
for (cell, arc), v in sorted(arcs.items(), key=lambda kv: -len(kv[1])):
    print(f"{cell:<10} {arc:<34} {len(v):>6} {min(v):7.3f} {statistics.median(v):7.3f} {max(v):7.3f}")
