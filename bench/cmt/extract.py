#!/usr/bin/env python3
"""Vivado's PLL and MMCM bits, out of bench/cmt/harvest.py's bitstreams.

    bench/cmt/extract.py <harvest-dir> <tilegrid.json> <fixture-out> <tables-out>

Reads each design's top.fasm (prjxray's bit2fasm of Vivado's top.bit),
placed.txt (each block's site) and blocks.json (its attributes), and writes:

- <fixture-out>: per block its attributes and the features Vivado set in
  its site, which `chipsmith_xc7::cmt`'s test compares Config::features
  against (crates/chipsmith-xc7/tests/data/cmt_vivado.txt);
- <tables-out>: the features that depend on the multiplier alone (the
  lock and loop-filter tables) per block kind and whole multiplier, and the
  ones Vivado sets on every block of a kind
  (crates/chipsmith-xc7/src/cmt_tables.txt).

Only BANDWIDTH OPTIMIZED blocks go into the tables. A table feature that
differs between two blocks of the same multiplier is an error: the table
would depend on something else.
"""

import json
import re
import sys
from collections import defaultdict
from pathlib import Path

# Counter fields: what Config::features computes from the counters.
COUNTER = re.compile(r"^(DIVCLK|CLKFBOUT|CLKOUT\d)_(DIVCLK|CLKOUT1|CLKOUT2)_")
# Per-block settings Config::features writes itself.
OWN = {"IN_USE", "ZINV_PWRDWN", "ZINV_RST", "ZINV_PSEN", "ZINV_PSINCDEC", "INV_CLKINSEL",
       "COMPENSATION.Z_ZHOLD_OR_CLKIN_BUF", "COMPENSATION.ZHOLD_NO_CLKIN_BUF",
       "COMP.Z_ZHOLD", "COMP.ZHOLD", "STARTUP_WAIT", "SS_EN"}


def bits_of(line):
    """FASM line -> set of NAME[i] for set bits."""
    line = line.split("#")[0].strip()
    if not line:
        return []
    if "=" in line:
        lhs, rhs = [s.strip() for s in line.split("=", 1)]
        m = re.match(r"(.*)\[(\d+):(\d+)\]$", lhs)
        digits = rhs.split("'b", 1)[1] if "'b" in rhs else bin(int(rhs))[2:]
        hi, lo = (int(m.group(2)), int(m.group(3))) if m else (0, 0)
        name = m.group(1) if m else lhs
        out = []
        for i, c in enumerate(reversed(digits)):
            if c == "1":
                out.append(f"{name}[{lo + i}]")
        return out
    # A single-bit feature (IN_USE) has no index.
    return [line]


def main():
    harvest, tilegrid, fixture_out, tables_out = (Path(a) for a in sys.argv[1:5])
    grid = json.load(open(tilegrid))
    site_tile = {}
    for tile, t in grid.items():
        for site in t.get("sites", {}):
            site_tile[site] = tile
    blocks = []
    designs = [d for d in harvest.iterdir() if d.is_dir() and re.fullmatch(r"[df]\d+", d.name)]
    for d in sorted(designs, key=lambda p: (p.name[0] == "f", int(p.name[1:]))):
        fasm = d / "top.fasm"
        if not fasm.exists():
            continue
        feats = set()
        for line in fasm.read_text().splitlines():
            feats.update(bits_of(line))
        attrs = json.load(open(d / "blocks.json"))
        for line in (d / "placed.txt").read_text().splitlines():
            cell, site = line.split()
            b = attrs[int(cell.removeprefix("blk"))]
            prim_cell = "PLLE2_ADV" if b["prim"] == "PLL" else "MMCME2_ADV"
            prefix = f"{site_tile[site]}.{prim_cell}."
            own = sorted(f[len(prefix):] for f in feats if f.startswith(prefix))
            blocks.append((d.name, cell, b, own))

    with open(fixture_out, "w") as f:
        f.write("# Vivado 2026.1's PLLE2_ADV and MMCME2_ADV bits (bench/cmt/harvest.py,\n"
                "# bench/cmt/extract.py): per block its attributes, then the features\n"
                "# Vivado set in its site, one per set bit.\n")
        for name, cell, b, own in blocks:
            zhold = cell == "blk0" or b.get("zhold", False)
            a = [f"CLKIN1_PERIOD={b['period']:.3f}", f"DIVCLK_DIVIDE={b['divclk']}",
                 f"BANDWIDTH={b['bandwidth']}", f"COMPENSATION={'ZHOLD' if zhold else 'INTERNAL'}"]
            a.append(f"CLKFBOUT_MULT={int(b['mult'])}" if b["prim"] == "PLL" else f"CLKFBOUT_MULT_F={b['mult']:.3f}")
            for k, o in enumerate(b["outs"]):
                if o is None:
                    continue
                if b["prim"] == "MMCM" and k == 0:
                    a.append(f"CLKOUT0_DIVIDE_F={o['divide']:.3f}")
                else:
                    a.append(f"CLKOUT{k}_DIVIDE={int(o['divide'])}")
                a += [f"CLKOUT{k}_PHASE={o['phase']:.3f}", f"CLKOUT{k}_DUTY_CYCLE={o['duty']:.6f}"]
            f.write(f"block {b['prim']} {name}/{cell} {' '.join(a)}\n")
            for x in own:
                f.write(f"  {x}\n")

    # Tables: features outside the counters and the block's own settings.
    by_mult = defaultdict(list)  # (prim, mult) -> [(design, {feature: value})]
    every = defaultdict(list)
    scalars = set()
    for name, cell, b, own in blocks:
        if b["bandwidth"] != "OPTIMIZED":
            continue
        prim, m = b["prim"], int(b["mult"])
        rest = [x for x in own if not COUNTER.match(x) and x.split("[")[0] not in OWN]
        vec = defaultdict(int)
        for x in rest:
            m_ = re.match(r"(.*)\[(\d+)\]$", x)
            n, i = (m_.group(1), int(m_.group(2))) if m_ else (x, None)
            vec[n] |= 1 << (i or 0)
            if i is None:
                scalars.add(n)
        every[prim].append(dict(vec))
        by_mult[(prim, m)].append((f"{name}/{cell}", dict(vec)))
    names = defaultdict(set)
    for prim, vs in every.items():
        for v in vs:
            names[prim].update(v)
    lines = ["# Vivado 2026.1's PLLE2_ADV and MMCME2_ADV settings that don't come from",
             "# the counters (bench/cmt/extract.py, BANDWIDTH OPTIMIZED). `const`: set on",
             "# every block of the kind; `table <kind> <mult> <feature> <width> <hex>`: by",
             "# whole CLKFBOUT_MULT, the lock and loop-filter tables."]
    widths = {"LKTABLE": 40, "TABLE": 10}
    for prim in sorted(every):
        consts = []
        for n in sorted(names[prim]):
            vals = {v.get(n, 0) for v in every[prim]}
            if len(vals) == 1:
                consts.append(n)
                (v,) = vals
                if n in scalars:
                    lines.append(f"const {prim} {n}")
                    continue
                for i in range(v.bit_length()):
                    if v >> i & 1:
                        lines.append(f"const {prim} {n}[{i}]")
        missing = [m for m in range(2, 65) if (prim, m) not in by_mult]
        if missing:
            print(f"{prim}: no OPTIMIZED block at CLKFBOUT_MULT {missing}", file=sys.stderr)
        for m in range(2, 65):
            t = by_mult.get((prim, m))
            if not t:
                continue
            for n in sorted(names[prim]):
                if n in consts:
                    continue
                vals = {v.get(n, 0) for _, v in t}
                if len(vals) != 1:
                    sys.exit(f"{prim} mult {m} {n}: values {vals} differ between {[b for b, _ in t]}")
                w = widths.get(n, max(1, max(vals).bit_length()))
                lines.append(f"table {prim} {m} {n} {w} {next(iter(vals)):x}")
    Path(tables_out).write_text("\n".join(lines) + "\n")
    print(f"{len(blocks)} blocks", file=sys.stderr)


if __name__ == "__main__":
    main()
