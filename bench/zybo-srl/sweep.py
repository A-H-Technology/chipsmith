#!/usr/bin/env python3
"""Vivado's shift-register LUTs (SRL16E/SRLC32E) for every shift-register shape.

    bench/zybo-srl/sweep.py OUT_DIR [--primitives]

One design of delay lines (lengths 2 to 64, with and without a clock
enable, power-up patterns, a dynamic tap, the falling edge), each its own
input and output pin, `srl_style = "srl"` so Vivado makes them plain SRLs,
built by Vivado through xc7-re and read back by bit2fasm. Writes
OUT_DIR/top.fasm, OUT_DIR/placed.tsv (each cell's site and bel) and
OUT_DIR/srl.tsv: per SRL cell its slice, bel and the slice features of its
LUT (`<L>LUT.*`) and of the slice (`WEMUX`, `CLKINV`, ...).

With --primitives the lines are SRLC32E instances instead (one or two
each, a 64-stage line two in series, the dynamic tap on its address
pins), so Vivado makes no SRL16E halves, which chipsmith's decoder doesn't
read: that bitstream checks the decoder against Vivado's bits.

The designs are also crates/chipsmith-xc7/tests/designs/srl and srl_prim. Needs the
devshell (bit2fasm, CHIPSMITH_PRJXRAY_DB) and Vivado ($XC7_RE, default
../xc7-re).
"""

import csv
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
XC7_RE = Path(os.environ.get("XC7_RE", ROOT.parent / "xc7-re"))
DB = Path(os.environ["CHIPSMITH_PRJXRAY_DB"]) / "zynq7"
PART = "xc7z010clg400-1"

# (name, length, clock enable, power-up pattern or None, falling edge)
LINES = [
    ("s2", 2, False, None, False),
    ("s3", 3, True, None, False),
    ("s16", 16, False, 0xA5C3, False),
    ("s17", 17, True, None, False),
    ("s32", 32, True, 0xDEADBEEF, False),
    ("s33", 33, False, None, False),
    ("s64", 64, True, 0x0123456789ABCDEF, False),
    ("sneg", 20, False, 0x5, True),
]


def design_primitives():
    """The same lines as SRLC32E instances: up to 32 stages each, a longer
    line's pieces in series through Q (read at A = 31)."""
    ports = ["input clk", "input ce", "input [4:0] tap"]
    body = []
    for name, n, ce, init, neg in LINES:
        ports += [f"input {name}_in", f"output {name}_out"]
        d = f"{name}_in"
        for k, start in enumerate(range(0, n, 32)):
            length = min(32, n - start)
            piece = (init or 0) >> start & ((1 << length) - 1)
            q = f"{name}_q{k}"
            body.append(f"wire {q};")
            body.append(
                f"SRLC32E #(.INIT(32'h{piece:x}), .IS_CLK_INVERTED(1'b{int(neg)})) {name}_{k} "
                f"(.CLK(clk), .CE({'ce' if ce else "1'b1"}), .D({d}), .A(5'd{length - 1}), .Q({q}));")
            d = q
        body.append(f"assign {name}_out = {d};")
    ports += ["input dyn_in", "output dyn_out"]
    body.append("SRLC32E dyn_0 (.CLK(clk), .CE(ce), .D(dyn_in), .A(tap), .Q(dyn_out));")
    return "module top(" + ", ".join(ports) + ");\n" + "\n".join(body) + "\nendmodule\n"


def design():
    """Each line: its input pin in at the bottom, its last stage out; a
    32-deep one more read at a tap its address pins pick."""
    ports = ["input clk", "input ce", "input [4:0] tap"]
    body = []
    for name, n, ce, init, neg in LINES:
        ports += [f"input {name}_in", f"output {name}_out"]
        init_bits = f"{n}'h{init:x}" if init is not None else f"{n}'d0"
        body.append(f'(* srl_style = "srl" *) reg [{n - 1}:0] {name} = {init_bits};')
        edge = "negedge" if neg else "posedge"
        en = "if (ce) " if ce else ""
        body.append(f"always @({edge} clk) {en}{name} <= {{{name}[{n - 2}:0], {name}_in}};"
                    if n > 1 else "")
        body.append(f"assign {name}_out = {name}[{n - 1}];")
    ports += ["input dyn_in", "output dyn_out"]
    body.append('(* srl_style = "srl" *) reg [31:0] dyn = 0;')
    body.append("always @(posedge clk) if (ce) dyn <= {dyn[30:0], dyn_in};")
    body.append("assign dyn_out = dyn[tap];")
    return "module top(" + ", ".join(ports) + ");\n" + "\n".join(body) + "\nendmodule\n"


TCL = f"""set_param general.maxThreads 1
read_verilog top.v
synth_design -top top -part {PART}
opt_design
place_design
route_design
write_checkpoint -force route.dcp
write_bitstream -force top.bit
set f [open placed.tsv w]
foreach c [get_cells -hierarchical -filter {{IS_PRIMITIVE}}] {{ puts $f "[get_property NAME $c]\\t[get_property REF_NAME $c]\\t[get_property LOC $c]\\t[get_property BEL $c]" }}
close $f
"""

BALLS = """A20 B19 B20 C20 D18 D19 D20 E17 E18 E19 F16 F19 F20 G14 G17 G18 G19
G20 H15 H16 H17 H18 H20 J14 J15 J16 J18 J19 J20 K14 K16 K17 K18 K19""".split()


def ports():
    """Every port bit and the ball it goes on (the clock on L16)."""
    names = ["ce"] + [f"tap[{i}]" for i in range(5)]
    for name, *_ in LINES:
        names += [f"{name}_in", f"{name}_out"]
    names += ["dyn_in", "dyn_out"]
    return [("clk", "L16")] + list(zip(names, BALLS))


def xdc():
    lines = ["create_clock -period 10 [get_ports clk]"]
    for p, ball in ports():
        lines.append(f"set_property PACKAGE_PIN {ball} [get_ports {{{p}}}]")
    lines.append("set_property IOSTANDARD LVCMOS33 [get_ports *]")
    return "\n".join(lines) + "\n"


def manifest():
    lines = ['[project]', 'name = "srl"', 'top = "top"', 'sources = ["top.v"]', '',
             '[target]', 'device = "xc7z010clg400-1"', '', '[pins]', 'clk = "L16"',
             'ce = "' + BALLS[0] + '"',
             'tap = [' + ", ".join(f'"{b}"' for b in BALLS[1:6]) + ']']
    for p, ball in ports()[7:]:
        lines.append(f'{p} = "{ball}"')
    return "\n".join(lines) + "\n"


def bit2fasm(bit):
    return subprocess.run(
        ["bit2fasm", "--db-root", str(DB), "--part", PART, str(bit)],
        capture_output=True, text=True, check=True).stdout


def main():
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=True)
    (out / "top.v").write_text(design_primitives() if "--primitives" in sys.argv else design())
    (out / "top.xdc").write_text(xdc())
    (out / "chipsmith.toml").write_text(manifest())
    (out / "build.tcl").write_text(TCL.replace("read_verilog top.v", "read_verilog top.v\nread_xdc top.xdc"))
    if not (out / "top.bit").exists():
        with open(out / "vivado.log", "w") as log:
            subprocess.run([XC7_RE / "tools/vivado.sh", "-mode", "batch", "-nojournal",
                            "-nolog", "-source", "build.tcl"],
                           cwd=out, stdout=log, stderr=subprocess.STDOUT, check=True)
    fasm = bit2fasm(out / "top.bit")
    (out / "top.fasm").write_text(fasm)
    feats = [l.split()[0] for l in fasm.splitlines() if l.strip() and not l.startswith("#")]
    rows = []
    for l in (out / "placed.tsv").read_text().splitlines():
        name, ref, loc, bel = l.split("\t")
        if not ref.startswith("SRL"):
            continue
        # The slice's features in its CLB tile: SLICE_X<2k>... is the tile's
        # X0 half; bit2fasm names halves SLICEM_X0 / SLICEL_X1.
        letter = bel.split(".")[-1][0]
        mine = sorted({f.split(".", 1)[1] for f in feats
                       if f.split(".")[1].startswith("SLICEM_X0")
                       and loc_matches(f, loc)
                       and (f.split(".")[2].startswith(letter + "LUT") or not f.split(".")[2][1:].startswith("LUT"))})
        rows.append([name, ref, loc, bel, " ".join(mine)])
    with open(out / "srl.tsv", "w") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n")
        w.writerow(["cell", "prim", "site", "bel", "features"])
        w.writerows(rows)


def loc_matches(feature, loc):
    """Whether a feature's CLB tile holds slice `loc` (SLICE_X<x>Y<y>):
    CLBLM_L_X<a>Y<y> holds slices X<2k> and X<2k+1> at the same Y; prjxray's
    tilegrid has the mapping, so read it from there."""
    return TILE_OF.get(loc) == feature.split(".")[0]


def tile_of_slices():
    import json
    grid = json.loads((DB / "xc7z010" / "tilegrid.json").read_text()) \
        if (DB / "xc7z010" / "tilegrid.json").exists() else {}
    out = {}
    for tile, t in grid.items():
        for site in t.get("sites", {}):
            if site.startswith("SLICE_"):
                out[site] = tile
    return out


TILE_OF = tile_of_slices()

if __name__ == "__main__":
    main()
