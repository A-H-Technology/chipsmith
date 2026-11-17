#!/usr/bin/env python3
"""Vivado's slice latches (LDCE, LDPE) for every latch shape.

    bench/zybo-latch/sweep.py OUT_DIR

One design of latches (clear or preset, a gate enable or none, the gate
active high or low, power-up 0 or 1), each latching its own input pin onto
its own output pin, built by Vivado through xc7-re and read back by
bit2fasm. Writes OUT_DIR/top.fasm, OUT_DIR/placed.tsv (each latch's slice
and bel) and the chipsmith.toml of the same pins, so the bitstream decodes
against the design (crates/chipsmith-xc7/tests/designs/latch).
"""

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
XC7_RE = Path(os.environ.get("XC7_RE", ROOT.parent / "xc7-re"))
DB = Path(os.environ["CHIPSMITH_PRJXRAY_DB"]) / "zynq7"
PART = "xc7z010clg400-1"

# (primitive, gate enable, gate inverted, INIT)
LATCHES = [
    ("LDCE", False, False, 0),
    ("LDCE", True, False, 0),
    ("LDCE", False, True, 1),
    ("LDCE", True, True, 1),
    ("LDPE", False, False, 1),
    ("LDPE", True, False, 0),
    ("LDPE", False, True, 0),
    ("LDPE", True, True, 1),
]

BALLS = """A20 B19 B20 C20 D18 D19 D20 E17 E18 E19 F16 F19 F20 G14 G17 G18 G19
G20 H15 H16 H17 H18 H20 J14 J15 J16 J18 J19 J20 K14 K16 K17 K18 K19""".split()


def design():
    ports = ["input g", "input ge", "input rst"]
    body = []
    for i, (prim, ge, inv, init) in enumerate(LATCHES):
        ports += [f"input d{i}", f"output q{i}"]
        rst = "CLR" if prim == "LDCE" else "PRE"
        gate_enable = "ge" if ge else "1'b1"
        body.append(
            f"{prim} #(.INIT(1'b{init}), .IS_G_INVERTED(1'b{int(inv)})) l{i} "
            f"(.G(g), .GE({gate_enable}), .{rst}(rst), .D(d{i}), .Q(q{i}));")
    return "module top(" + ", ".join(ports) + ");\n" + "\n".join(body) + "\nendmodule\n"


def ports():
    """The gate on L16, a clock-capable ball (it becomes a clock net)."""
    names = ["ge", "rst"]
    for i in range(len(LATCHES)):
        names += [f"d{i}", f"q{i}"]
    return [("g", "L16")] + list(zip(names, BALLS))


def xdc():
    lines = [f"set_property PACKAGE_PIN {b} [get_ports {p}]" for p, b in ports()]
    lines.append("set_property IOSTANDARD LVCMOS33 [get_ports *]")
    return "\n".join(lines) + "\n"


def manifest():
    lines = ['[project]', 'name = "latch"', 'top = "top"', 'sources = ["top.v"]', '',
             '[target]', 'device = "xc7z010clg400-1"', '', '[pins]']
    lines += [f'{p} = "{b}"' for p, b in ports()]
    return "\n".join(lines) + "\n"


TCL = f"""set_param general.maxThreads 1
read_verilog top.v
read_xdc top.xdc
synth_design -top top -part {PART}
opt_design
place_design
route_design
write_bitstream -force top.bit
set f [open placed.tsv w]
foreach c [get_cells -hierarchical -filter {{IS_PRIMITIVE}}] {{ puts $f "[get_property NAME $c]\\t[get_property REF_NAME $c]\\t[get_property LOC $c]\\t[get_property BEL $c]" }}
close $f
"""


def main():
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=True)
    (out / "top.v").write_text(design())
    (out / "top.xdc").write_text(xdc())
    (out / "chipsmith.toml").write_text(manifest())
    (out / "build.tcl").write_text(TCL)
    if not (out / "top.bit").exists():
        with open(out / "vivado.log", "w") as log:
            subprocess.run([XC7_RE / "tools/vivado.sh", "-mode", "batch", "-nojournal",
                            "-nolog", "-source", "build.tcl"],
                           cwd=out, stdout=log, stderr=subprocess.STDOUT, check=True)
    fasm = subprocess.run(
        ["bit2fasm", "--db-root", str(DB), "--part", PART, str(out / "top.bit")],
        capture_output=True, text=True, check=True).stdout
    (out / "top.fasm").write_text(fasm)


if __name__ == "__main__":
    main()
