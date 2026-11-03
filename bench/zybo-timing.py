#!/usr/bin/env python3
"""chipsmith's timing estimate against Vivado's timer, endpoint by endpoint,
on the same placement and routing.

    bench/zybo-timing.py <design dir> ...

Each <design dir> is one of bench/zybo-timing.sh's: chipsmith.tsv
(CHIPSMITH_XC7_ENDPOINTS: site, bel, port, arrival ps, setup ps),
cells.tsv (the exporter's Vivado cell -> site, bel) and endpoints.tsv
(xc7-re tools/import.sh: Vivado's worst setup path per endpoint). Joins
the two by site, bel and port and prints, per design and over all, the
error of chipsmith's arrival against Vivado's data path delay: median and
95th percentile of |error|, and the median signed.
"""
import statistics
import sys

# Vivado's register pins, as chipsmith's placer names them.
FF_PIN = {"D": "D", "CE": "CE", "R": "SR", "S": "SR", "CLR": "SR", "PRE": "SR"}


def load(d):
    chip = {}
    for line in open(f"{d}/chipsmith.tsv"):
        site, bel, port, arr, setup = line.rstrip("\n").split("\t")
        if arr != "-":
            chip[(site, bel, port)] = float(arr) / 1000
    cell = {}
    for line in open(f"{d}/cells.tsv"):
        name, site, bel = line.rstrip("\n").split("\t")
        cell[name] = (site, bel)
    pairs = []
    for line in open(f"{d}/endpoints.tsv"):
        f = line.rstrip("\n").split("\t")
        end, start, delay = f[0], f[1], f[2]
        if "/" not in end or not delay or "/" not in start:
            continue  # a port: chipsmith times register to register only
        name, pin = end.rsplit("/", 1)
        if name not in cell:
            continue
        site, bel = cell[name]
        if bel.endswith("FF"):
            pin = FF_PIN.get(pin, pin)
        else:
            pin = pin.replace("[", "").replace("]", "")
        key = (site, bel, pin)
        if key in chip:
            pairs.append((key, chip[key], float(delay)))
    return pairs


def stats(pairs):
    err = [(c - v) / v for _, c, v in pairs if v > 0]
    if not err:
        return "no endpoints joined"
    a = sorted(abs(e) for e in err)
    p95 = a[min(len(a) - 1, int(0.95 * len(a)))]
    return (f"{len(err)} endpoints, |error| median {100 * statistics.median(a):.1f}% "
            f"p95 {100 * p95:.1f}%, signed median {100 * statistics.median(err):+.1f}%")


every = []
for d in sys.argv[1:]:
    pairs = load(d)
    every += pairs
    worst = max(pairs, key=lambda p: p[2], default=None)
    print(f"{d}: {stats(pairs)}")
    if worst:
        print(f"  Vivado's longest: {worst[0]} chipsmith {worst[1]:.3f} ns, Vivado {worst[2]:.3f} ns")
if len(sys.argv) > 2:
    print(f"all: {stats(every)}")
