#!/usr/bin/env python3
"""Silicon cell delays from bench/ring/fit.py's fit, and the timing model's
values from them.

    bench/ring/derive.py <measure.sh out dir> <out silicon.json>

Writes this DE2's EP2C35 at room temperature, per element (ps):

- `lut`: per physical input A to D, the ring stage's LUT arc plus its
  LOCAL_LINE hop (fit.py's `lut+LL`; the two always come together);
- `head`: per head input A, B, a chain's head + tail + LOCAL_LINE hop;
- `cin_cout`, `lcb`, `lab`: CIN to COUT, and what the LAB's half-way
  buffer and the next LAB add to a carry;
- `mult`, `mult9`: an 18 x 18 and a 9 x 9 multiplier, data in to the
  slowest product bit measured: the rings' half period less stage 0's
  LUT arc and the hops into and out of the block. Those hops are the
  model's routing over ROUTE_SCALE (the open builds' R4 and C4 ratio of
  quartus_sta to silicon, bench/ring/fit.py), stage 0's arc `lut D` less
  the model's LOCAL_LINE over it too.

`model(silicon, k_lut, k_carry, k_mult)` turns them into the model's
values, each class scaled by its own slow-corner factor (bench/ring/
path_fit.py fits them).
"""
import glob
import json
import os
import statistics
import sys

LL = 229.0
ROUTE_SCALE = 1.62
WIRE = {"R4": 235.0, "C4": 285.0, "R24": 433.0, "C16": 465.0, "LOCAL_INTERCONNECT": 362.0,
        "LOCAL_LINE": 229.0, "LE_BUFFER": 56.0, "M4K_BUFFER": 143.0, "DSP_BUFFER": 143.0}


def mult(out_dir, lut_d):
    """Per (width, product bit): (delay, sd over rings, rings)."""
    by_bit = {}
    for counts in sorted(glob.glob(f"{out_dir}/mult*/*/counts.txt")):
        run = os.path.dirname(counts)
        spec = json.load(open(f"{run}/build/placed-spec.json"))
        conns = {}
        for line in open(f"{run}/connections.txt"):
            f = line.split()
            conns[(f[0], f[1])] = sum(WIRE.get(w, 0.0) for w in f[4:])
        reads = {}
        for line in open(counts):
            _, r, _, mhz = line.split()
            # bench/ring/fit.py's MAX_MHZ: faster reads are miscounted; a
            # ring that doesn't oscillate reads a count or two a window.
            if 10 < float(mhz) < 330:
                reads.setdefault(int(r), []).append(float(mhz))
        for r, ring in enumerate(spec):
            if r not in reads:
                continue
            half = 1e6 / statistics.mean(reads[r]) / 2
            into = max(v for (s, d), v in conns.items() if s == f"r{r}_s0.COMBOUT" and d.startswith(f"r{r}_mult."))
            back = conns[(f"r{r}_mult.MULTOUT{ring['out']}", f"r{r}_s0.DATA{ring['pin']}")]
            stage = lut_d - LL / ROUTE_SCALE
            by_bit.setdefault((ring.get("width", 18), ring["out"]), []).append(
                half - (into + back) / ROUTE_SCALE - stage)
    return {b: (statistics.mean(v), statistics.pstdev(v), len(v)) for b, v in by_bit.items()}


def model(si, k_lut, k_carry, k_mult):
    """The timing model's cell values (timing.rs, bench/ring/emit.py)."""
    lut = [k_lut * v - LL for v in si["lut"]]
    tail = lut[2]  # CIN enters the LUT as its C
    return {
        "k": [k_lut, k_carry, k_mult],
        "lut": lut,
        "cin_combout": tail,
        "head_a": k_lut * si["head"][0] - LL - tail,
        "head_b": k_lut * si["head"][1] - LL - tail,
        "cin_cout": k_carry * si["cin_cout"],
        "lcb": k_carry * si["lcb"],
        "lab": k_carry * si["lab"],
        "mult_comb": k_mult * si["mult"],
        "mult9_comb": k_mult * si["mult9"],
        "tsu_datain": 0.0,
    }


def main():
    out_dir, dest = sys.argv[1], sys.argv[2]
    fit = json.load(open(f"{out_dir}/fit.json"))["silicon"]
    v = {n: x for n, (x, _) in fit.items()}
    e = {n: x for n, (_, x) in fit.items()}
    bits = mult(out_dir, v["lut+LL D"])
    si = {
        "lut": [v[f"lut+LL {p}"] for p in "ABCD"],
        "head": [v["head A+tail+LL"], v["head B+tail+LL"]],
        "cin_cout": v["cin->cout"],
        "lcb": v["link lcb"],
        "lab": v["link lab"],
        "mult": max(m for (w, _), (m, _, _) in bits.items() if w == 18),
        "mult9": max(m for (w, _), (m, _, _) in bits.items() if w == 9),
        "se": {"lut": [e[f"lut+LL {p}"] for p in "ABCD"],
               "head": [e["head A+tail+LL"], e["head B+tail+LL"]],
               "cin_cout": e["cin->cout"], "lcb": e["link lcb"], "lab": e["link lab"]},
        "mult_bits": {f"{w}x{w} bit {b}": list(x) for (w, b), x in sorted(bits.items())},
    }
    json.dump(si, open(dest, "w"), indent=1)
    print(json.dumps(si, indent=1))


if __name__ == "__main__":
    main()
