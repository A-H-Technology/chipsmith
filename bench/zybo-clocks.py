#!/usr/bin/env python3
"""Vivado's Fmax per clock from a timing_summary.rpt: 1 / (period - WNS).

    bench/zybo-clocks.py <timing_summary.rpt>
"""
import re
import sys

text = open(sys.argv[1]).read()


def table(title):
    m = re.search(r"^\| " + title + r"\n.*?\n-+\n\n(.*?)\n\n", text, re.S | re.M)
    rows = m[1].splitlines()[2:] if m else []
    return [r.split() for r in rows if r.strip()]


period = {r[0]: float(r[-2]) for r in table("Clock Summary") if len(r) >= 4}
out = []
for r in table("Intra Clock Table"):
    if r[0] in period and re.match(r"-?\d", r[1]):
        out.append(f"{r[0]} {1000 / (period[r[0]] - float(r[1])):.1f} MHz")
print(", ".join(out))
