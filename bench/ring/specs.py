#!/usr/bin/env python3
"""The ring specs bench/ring/measure.sh builds, one JSON per bitstream.

    bench/ring/specs.py <out dir>

Each bitstream holds up to eight rings (one global clock each). The same
structures are built in several regions of the die, so the spread across
placements shows next to the spread across reads.
"""
import json
import os
import sys

# LAB columns are odd x (minus M4K and multiplier columns), rows 1 to 35
# (the columns left of 15 are shorter).
LAB_COLUMNS = [x for x in range(15, 130, 2) if x not in (27, 53, 79, 105)]
REGIONS = [(61, 12), (101, 20), (37, 26), (83, 4)]


def in_lab(name, pin, x, y, n=15):
    return {"name": name, "pin": pin, "les": [[x, y, s] for s in range(n)], "tap": [x, y, 15]}


def across(name, pin, a, b, n=15):
    """Stages alternate between LABs a and b: every hop leaves its LAB."""
    les = [[*(a if s % 2 == 0 else b), s // 2] for s in range(n)]
    return {"name": name, "pin": pin, "les": les, "tap": [*a, 15]}


def column(x, k):
    """The k-th LAB column from x on, wrapping round to the left edge."""
    i = next((i for i, c in enumerate(LAB_COLUMNS) if c >= x), 0)
    return LAB_COLUMNS[(i + k) % len(LAB_COLUMNS)]


def carry(name, pin, x, y, first, n, split=None):
    """A carry ring from LE `first` of LAB (x, y), down the column (a chain
    continues in the LAB below); `split` makes it two chains."""
    les = []
    for s in range(n):
        k = first + s
        les.append([x, y - k // 16, k % 16])
    tail = les[-1]
    tap = [column(x, 1), tail[1], 15]
    ring = {"name": name, "pin": pin, "kind": "carry", "les": les, "tap": tap}
    if split:
        ring["chains"] = [split, n - split]
    return ring


def chained(name, pin, parts):
    """A carry ring of several chains, each (x, y, first LE, length) on its
    own: rings that differ only in where one chain sits tell the link
    costs from head + tail."""
    les, lens = [], []
    for x, y, first, n in parts:
        for s in range(n):
            k = first + s
            les.append([x, y - k // 16, k % 16])
        lens.append(n)
    tail = les[-1]
    return {"name": name, "pin": pin, "kind": "carry", "les": les, "chains": lens,
            "tap": [column(parts[-1][0], 1), tail[1], 15]}


def specs():
    out = {}
    for i, (x, y) in enumerate(REGIONS):
        # Each physical input, and pin D at three lengths (the tap and the
        # enable cost the same in each, so the slope is one stage).
        rings = [in_lab(f"pin{p}", p, x, y + 2 * j) for j, p in enumerate("ABCD")]
        rings += [in_lab(f"D{n}", "D", x + 2, y + 2 * j, n) for j, n in enumerate([7, 9, 13])]
        rings += [in_lab("A7", "A", x + 2, y + 6, 7)]
        out[f"lut{i}"] = rings
        # Hops out of the LAB: to the neighbour column, four columns over,
        # the row above, four rows over.
        out[f"wire{i}"] = [
            across("h2", "D", (x, y), (x + 2, y)),
            across("h4", "D", (x, y + 2), (x + 6, y + 2)),
            across("v1", "D", (x + 10, y), (x + 10, y + 1)),
            across("v4", "D", (x + 12, y), (x + 12, y + 4)),
            across("h2A", "A", (x, y + 4), (x + 2, y + 4)),
            across("v1A", "A", (x + 10, y + 6), (x + 10, y + 7)),
        ]
        # Carry rings: chains of 16 to 40 LEs (shorter ones ran faster than
        # the counter's clock can follow: a 12-LE ring read 904 MHz, twice its
        # rate, and 16-LE ones read 351 or 703 MHz depending on the load;
        # bench/ring/fit.py drops anything above 330 MHz), through the LAB's half-way
        # buffer (LE 7 to 8) and into the LAB below (LE 15 to 0), heads on
        # B and A; one of two chains, to tell the constant from head + tail.
        c = [column(x + 16, 2 * j) for j in range(4)]
        out[f"carry{i}"] = [
            carry("c40B", "B", c[0], y + 4, 0, 40),
            carry("c24B", "B", c[1], y + 4, 0, 24),
            carry("c2x16B", "B", c[2], y + 4, 0, 32, split=16),
            carry("c32B", "B", c[3], y + 4, 0, 32),
            carry("c16Bo", "B", c[0], y + 9, 8, 16),
            carry("c2x16A", "A", c[1], y + 9, 0, 32, split=16),
            carry("c24A", "A", c[2], y + 9, 4, 24),
            carry("c2x8B", "B", c[3], y + 9, 0, 16, split=8),
        ]
    # Two 8-LE chains, the first inside a LAB half, across the half-way
    # buffer, or across into the LAB below; the second always inside one.
    for i, (x, y) in enumerate(REGIONS[:2]):
        c = [column(x + 40, 2 * j) for j in range(8)]
        out[f"carryx{i}"] = [
            chained("in+in", "B", [(c[0], y + 4, 0, 8), (c[1], y + 4, 0, 8)]),
            chained("lcb+in", "B", [(c[2], y + 4, 4, 8), (c[3], y + 4, 0, 8)]),
            chained("lab+in", "B", [(c[4], y + 4, 12, 8), (c[5], y + 4, 0, 8)]),
            chained("lcb+lcb", "B", [(c[6], y + 4, 4, 8), (c[7], y + 4, 4, 8)]),
            chained("in+inA", "A", [(c[0], y + 8, 0, 8), (c[1], y + 8, 0, 8)]),
            chained("lab+lab", "B", [(c[2], y + 8, 12, 8), (c[3], y + 8, 12, 8)]),
            chained("in+in2", "B", [(c[4], y + 8, 0, 8), (c[5], y + 8, 8, 8)]),
            chained("lcb+in2", "B", [(c[6], y + 8, 4, 8), (c[7], y + 8, 8, 8)]),
        ]
    # Through an embedded multiplier (placed by the placer, stage 0 next to
    # it): product bits 35, 26, 19 and 0 of an 18 x 18, twice each.
    out["mult"] = [{"name": f"m{o}{c}", "pin": "D", "kind": "mult", "out": o, "les": [], "tap": []}
                   for o in (35, 26, 19, 0) for c in "ab"]
    # And 9 x 9 (one half of a block): A is 0 or 2^9 - 1, (2^9 - 1)^2 =
    # 0x3fc01, bits 10 to 17 follow it (bit 0 is A0 & B0, which synthesis
    # makes a LUT).
    out["mult9"] = [{"name": f"m9_{o}{c}", "pin": "D", "kind": "mult", "width": 9, "out": o, "les": [], "tap": []}
                    for o in (17, 13, 11, 10) for c in "ab"]
    return out


if __name__ == "__main__":
    d = sys.argv[1]
    os.makedirs(d, exist_ok=True)
    for name, rings in specs().items():
        json.dump(rings, open(f"{d}/{name}.json", "w"), indent=1)
        print(name)
