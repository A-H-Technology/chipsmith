#!/usr/bin/env python3
"""Vivado's I/O-tile registers (ILOGICE3/OLOGICE3) for every register shape.

    bench/zybo-ioreg/sweep.py OUT_DIR

One design: per pin of bank 35 a register of one shape (FDRE, FDSE, FDCE,
FDPE; clock enable, set/reset, falling edge) as the pin's input, output or
output-enable register, `IOB TRUE`, built by Vivado through xc7-re and
read back by bit2fasm. Writes OUT_DIR/ioreg.tsv: per pin its shape and
the site features of its IOI tile's ILOGIC/OLOGIC and IOB (not the pips:
how the fabric reaches the tile is the router's business).

With $CHIPSMITH (a chipsmith binary) the same design is built by
chipsmith from the same pins (`registers` in [pins]) and the two compared
feature for feature; a difference fails the script. Needs the devshell
(bit2fasm, CHIPSMITH_PRJXRAY_DB) and Vivado ($XC7_RE, default ../xc7-re).
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
CLOCK = "L16"  # bank 35's clock-capable pin; the Zybo's 125 MHz

PINS = """A20 B19 B20 C20 D18 D19 D20 E17 E18 E19 F16 F19 F20 G14 G17 G18 G19
G20 H15 H16 H17 H18 H20 J14 J15 J16 J18 J19 J20 K14 K16 K17 K18 K19 L14 L15
L17 L19 L20 M14 M15 M17 M18 M19 M20 N15 N16""".split()

# (primitive, its reset pin or None, INIT, clock enable used, falling edge)
SHAPES = [
    ("FDRE", None, 0, False, False),
    ("FDRE", "R", 0, True, False),
    ("FDRE", "R", 1, False, False),
    ("FDSE", "S", 1, True, False),
    ("FDSE", "S", 0, False, False),
    ("FDCE", "CLR", 0, True, False),
    ("FDCE", "CLR", 1, False, False),
    ("FDPE", "PRE", 1, True, False),
    ("FDPE", "PRE", 0, False, False),
    ("FDRE", None, 0, False, True),
    ("FDRE", "R", 1, True, True),
]
ROLES = ["in", "out", "oe"]


def cases():
    out = [(role, shape) for role in ROLES for shape in SHAPES]
    # A pin with all three, as an inout's would be.
    out.append(("all", SHAPES[1]))
    assert len(out) <= len(PINS)
    return list(zip(PINS, out))


def ff(name, shape, d, q):
    prim, rst, init, ce, neg = shape
    ports = [f".C(clk)", f".D({d})", f".Q({q})", f".CE({'ce' if ce else "1'b1"})"]
    if rst:
        ports.append(f".{rst}(rst)")
    else:
        ports.append(".R(1'b0)")
        prim = "FDRE"
    params = [f".INIT(1'b{init})"]
    if neg:
        params.append(".IS_C_INVERTED(1'b1)")
    return f"(* IOB = \"TRUE\" *) {prim} #({', '.join(params)}) {name} ({', '.join(ports)});"


def design(pins):
    """Input registers XORed into a counter; output and OE registers fed
    from it, an OE register's data from a counter bit."""
    ports = ["input clk", "input ce", "input rst"]
    body = ["reg [31:0] c = 0;"]
    xs = []
    for i, (_, (role, shape)) in enumerate(pins):
        if role == "in":
            ports.append(f"input p{i}")
            body.append(f"wire q{i};")
            body.append(ff(f"r{i}", shape, f"p{i}", f"q{i}"))
            xs.append(f"q{i}")
        elif role == "out":
            ports.append(f"output p{i}")
            body.append(ff(f"r{i}", shape, f"c[{i % 32}]", f"p{i}"))
        elif role == "oe":
            ports.append(f"output p{i}")
            body.append(f"wire t{i};")
            body.append(ff(f"r{i}", shape, f"c[{(i + 5) % 32}]", f"t{i}"))
            body.append(f"OBUFT b{i} (.I(c[{i % 32}]), .T(t{i}), .O(p{i}));")
        else:
            ports.append(f"inout p{i}")
            body += [f"wire i{i}, o{i}, t{i}, q{i};"]
            body.append(ff(f"ri{i}", shape, f"i{i}", f"q{i}"))
            body.append(ff(f"ro{i}", shape, f"c[{i % 32}]", f"o{i}"))
            body.append(ff(f"rt{i}", shape, f"c[{(i + 5) % 32}]", f"t{i}"))
            body.append(f"IOBUF b{i} (.I(o{i}), .T(t{i}), .O(i{i}), .IO(p{i}));")
            xs.append(f"q{i}")
    body.append(f"always @(posedge clk) c <= c + 1 + ({' ^ '.join(xs)});")
    return "module top(" + ", ".join(ports) + ");\n" + "\n".join(body) + "\nendmodule\n"


CTRL = {"ce": "N18", "rst": "N20"}  # bank 34


def xdc(pins):
    lines = [f"set_property PACKAGE_PIN {CLOCK} [get_ports clk]",
             "create_clock -period 10 [get_ports clk]"]
    for p, ball in CTRL.items():
        lines.append(f"set_property PACKAGE_PIN {ball} [get_ports {p}]")
    for i, (pin, _) in enumerate(pins):
        lines.append(f"set_property PACKAGE_PIN {pin} [get_ports p{i}]")
    lines.append("set_property IOSTANDARD LVCMOS33 [get_ports *]")
    return "\n".join(lines) + "\n"


def manifest(pins):
    regs = {"in": '["input"]', "out": '["output"]', "oe": '["output-enable"]',
            "all": '["input", "output", "output-enable"]'}
    lines = ['[project]', 'name = "top"', 'top = "top"', 'sources = ["top.v"]', '',
             '[target]', 'device = "xc7z010clg400-1"', '', '[pins]', f'clk = "{CLOCK}"']
    lines += [f'{p} = "{b}"' for p, b in CTRL.items()]
    for i, (pin, (role, _)) in enumerate(pins):
        lines.append(f'p{i} = {{ pin = "{pin}", registers = {regs[role]} }}')
    return "\n".join(lines) + "\n"


TCL = f"""set_param general.maxThreads 1
read_verilog top.v
read_xdc top.xdc
synth_design -top top -part {PART}
opt_design
place_design
route_design
write_checkpoint -force route.dcp
write_bitstream -force top.bit
# Where each register went: a register Vivado wouldn't put in its I/O tile
# stays in a slice.
set f [open placed.tsv w]
foreach c [get_cells -filter {{REF_NAME =~ FD*}}] {{ puts $f "[get_property NAME $c]\t[get_property BEL $c]\t[get_property LOC $c]" }}
close $f
# Each I/O-tile register's arcs as Vivado's timer has them, the slower of
# rise and fall (ns), for crates/chipsmith-xc7/src/ioi_timing.tsv.
set f [open arcs.tsv w]
foreach c [get_cells -hierarchical -filter {{IOB == TRUE}}] {{
    foreach a [get_timing_arcs -of_objects $c] {{
        set d ""
        foreach p {{DELAY_SLOW_MAX_RISE DELAY_SLOW_MAX_FALL DELAY_SLOW_MAX}} {{
            if {{![catch {{get_property $p $a}} x] && $x ne "" && ($d eq "" || $x > $d)}} {{ set d $x }}
        }}
        puts $f "[get_property REF_NAME $c]\t[get_property BEL $c]\t[get_property REF_PIN_NAME [get_property FROM_PIN $a]]\t[get_property REF_PIN_NAME [get_property TO_PIN $a]]\t[get_property TYPE $a]\t$d"
    }}
}}
close $f
"""


def ioi_tile(iob_tile):
    return iob_tile.replace("RIOB33", "RIOI3").replace("LIOB33", "LIOI3")


def sites():
    """Package pin -> (IOB tile, site index as FASM names it)."""
    out = {}
    with open(DB / PART / "package_pins.csv") as f:
        for row in csv.DictReader(f):
            if not row["pin_function"].startswith("IO_"):
                continue
            y = int(row["site"].rsplit("Y", 1)[1])
            tile = row["tile"]
            if "_SING_" in tile:
                n = 1 if y % 50 == 49 else 0
            else:
                n = y % 2
            out[row["pin"]] = (tile, n)
    return out


def bit2fasm(bit):
    return subprocess.run(
        ["bit2fasm", "--db-root", str(DB), "--part", PART, str(bit)],
        capture_output=True, text=True, check=True).stdout


def zero_features():
    """Per tile type, the features whose bits are all clear: bit2fasm names
    them on any tile with bits set, used or not, so they say nothing."""
    out = {}
    for path in DB.glob("segbits_*.db"):
        for l in path.read_text().splitlines():
            name, *bits = l.split()
            if bits and all(b.startswith("!") for b in bits):
                ty, feat = name.split(".", 1)
                out.setdefault(ty, set()).add(feat)
    return out


ZERO = zero_features()


def tile_type(tile):
    return tile.rsplit("_X", 1)[0]


def pin_features(fasm, where, pin):
    """The site features (not pips) of a pin's IOB and its IOI tile's
    ILOGIC/OLOGIC, without the tile name."""
    tile, n = where[pin]
    prefixes = [f"{tile}.IOB_Y{n}.", f"{ioi_tile(tile)}.ILOGIC_Y{n}.",
                f"{ioi_tile(tile)}.OLOGIC_Y{n}."]
    out = []
    for l in fasm.splitlines():
        if not l.strip() or l.startswith("#"):
            continue
        f = l.split()[0]
        for p in prefixes:
            if f.startswith(p):
                t, feat = f.split(".", 1)
                if feat not in ZERO.get(tile_type(t), ()):
                    out.append(feat)
    return sorted(out)


def main():
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=True)
    pins = cases()
    where = sites()
    (out / "top.v").write_text(design(pins))
    (out / "top.xdc").write_text(xdc(pins))
    (out / "build.tcl").write_text(TCL)
    if not (out / "top.bit").exists():
        with open(out / "vivado.log", "w") as log:
            subprocess.run([XC7_RE / "tools/vivado.sh", "-mode", "batch", "-nojournal",
                            "-nolog", "-source", "build.tcl"],
                           cwd=out, stdout=log, stderr=subprocess.STDOUT, check=True)
    # Every register in its I/O tile, or the comparison means nothing.
    stray = [l.split("\t")[0] for l in (out / "placed.tsv").read_text().splitlines()
             if l.startswith("r") and "LOGIC" not in l.split("\t")[1]]
    if stray:
        sys.exit(f"Vivado left {', '.join(stray)} out of the I/O tiles")
    fasm = bit2fasm(out / "top.bit")
    (out / "top.fasm").write_text(fasm)
    rows = []
    for pin, (role, (prim, rst, init, ce, neg)) in pins:
        rows.append([pin, role, prim if rst else "FDRE", rst or "", init, int(ce), int(neg),
                     " ".join(pin_features(fasm, where, pin))])
    with open(out / "ioreg.tsv", "w") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n")
        w.writerow(["pin", "role", "prim", "reset", "init", "ce", "negedge", "features"])
        w.writerows(rows)

    if os.environ.get("CHIPSMITH"):
        c = out / "chipsmith"
        c.mkdir(exist_ok=True)
        (c / "top.v").write_text(design(pins))
        (c / "chipsmith.toml").write_text(manifest(pins))
        subprocess.run([os.environ["CHIPSMITH"], "-C", str(c), "build"],
                       stdout=subprocess.DEVNULL, check=True)
        ours = bit2fasm(c / "build/top.bit")
        bad = 0
        for pin, case in pins:
            a, b = pin_features(fasm, where, pin), pin_features(ours, where, pin)
            if a != b:
                bad += 1
                print(f"{pin} {case}:\n  Vivado    {' '.join(sorted(set(a) - set(b)))}"
                      f"\n  chipsmith {' '.join(sorted(set(b) - set(a)))}")
        print(f"{len(pins) - bad} of {len(pins)} pins as Vivado has them")
        if bad:
            sys.exit(1)


if __name__ == "__main__":
    main()
