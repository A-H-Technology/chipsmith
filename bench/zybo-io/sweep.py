#!/usr/bin/env python3
"""Vivado's IOB33 bits for every pin setting the Zybo's banks can take.

    bench/zybo-io/sweep.py OUT_DIR [RUN ...]

Builds one design per run (a bank voltage's standards, drives, slews and
pulls on bank 35, a clock on bank 34) with Vivado through xc7-re, reads
each bitstream back with prjxray's bit2fasm, and writes OUT_DIR/iob.tsv:
one line per (run, pin) with its settings and the features of its IOB
(`IOB_Y<n>.*`), and OUT_DIR/bank.tsv with the HCLK_IOI features of each run.

chipsmith's feature table (crates/chipsmith-xc7/src/io.rs) is checked
against iob.tsv, copied to crates/chipsmith-xc7/tests/data/iostandard.tsv.
With $CHIPSMITH (a chipsmith binary) each run is also built by chipsmith
from the same pins and settings, and every I/O feature of the two
bitstreams (each IOB of the device, used or not, and each bank's
HCLK_IOI3) compared; a difference fails the script. Needs the devshell
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

CLOCK = "N18"  # bank 34

# Bank 35's user pins, minus its VREF pins (free, but kept clear).
BANK35 = """A20 B19 B20 C20 D18 D19 D20 E17 E18 E19 F16 F19 F20 G14 G17 G18 G19
G20 H15 H16 H17 H18 H20 J14 J15 J16 J18 J19 J20 K14 K16 K17 K18 K19 L14 L15
L16 L17 L19 L20 M14 M15 M17 M18 M19 M20 N15 N16""".split()

# Drives each standard takes on a high-range (IOB33) bank, UG471 table 1-56.
DRIVES = {
    "LVCMOS33": [4, 8, 12, 16],
    "LVTTL": [4, 8, 12, 16, 24],
    "LVCMOS25": [4, 8, 12, 16],
    "LVCMOS18": [4, 8, 12, 16, 24],
    "LVCMOS15": [4, 8, 12, 16],
    "LVCMOS12": [4, 8, 12],
}
RUNS = {
    "v33": ["LVCMOS33", "LVTTL"],
    "v25": ["LVCMOS25"],
    "v18": ["LVCMOS18"],
    "v15": ["LVCMOS15"],
    "v12": ["LVCMOS12"],
}
PULLS = ["NONE", "PULLUP", "PULLDOWN", "KEEPER"]


def cases(standards):
    """(direction, standard, drive, slew, pull) per pin."""
    out = []
    for s in standards:
        for d in DRIVES[s]:
            for slew in ["SLOW", "FAST"]:
                out.append(("out", s, d, slew, "NONE"))
        for pull in PULLS:
            out.append(("in", s, None, None, pull))
        out.append(("inout", s, 12, "SLOW", "PULLUP"))
        out.append(("inout", s, 8, "FAST", "NONE"))
    # An output with a pull: Vivado writes it like an input's.
    out.append(("out", standards[0], 12, "SLOW", "PULLDOWN"))
    assert len(out) <= len(BANK35), len(out)
    return list(zip(BANK35, out))


def design(pins):
    """A counter on the outputs, the inputs XORed into it, each inout
    driven by one counter bit and enabled by another."""
    ports = ["input clk"]
    body = ["reg [31:0] c = 0;", "wire [63:0] x;"]
    xs = []
    for i, (_, (d, *_)) in enumerate(pins):
        if d == "out":
            ports.append(f"output p{i}")
            body.append(f"assign p{i} = c[{i % 32}];")
        elif d == "in":
            ports.append(f"input p{i}")
            xs.append(f"p{i}")
        else:
            ports.append(f"inout p{i}")
            body.append(f"assign p{i} = c[{(i + 7) % 32}] ? c[{i % 32}] : 1'bz;")
            xs.append(f"p{i}")
    feed = " ^ ".join(xs) if xs else "1'b0"
    body.append(f"always @(posedge clk) c <= c + 1 + ({feed});")
    return "module top(" + ", ".join(ports) + ");\n" + "\n".join(body) + "\nendmodule\n"


def xdc(pins):
    lines = [
        f"set_property PACKAGE_PIN {CLOCK} [get_ports clk]",
        "set_property IOSTANDARD LVCMOS33 [get_ports clk]",
        "create_clock -period 10 [get_ports clk]",
    ]
    for i, (pin, (d, s, drive, slew, pull)) in enumerate(pins):
        p = f"[get_ports p{i}]"
        lines.append(f"set_property PACKAGE_PIN {pin} {p}")
        lines.append(f"set_property IOSTANDARD {s} {p}")
        if drive is not None:
            lines.append(f"set_property DRIVE {drive} {p}")
            lines.append(f"set_property SLEW {slew} {p}")
        if pull == "KEEPER":
            lines.append(f"set_property KEEPER TRUE {p}")
        elif pull != "NONE":
            lines.append(f"set_property {pull} TRUE {p}")
    return "\n".join(lines) + "\n"


TCL = f"""set_param general.maxThreads 1
read_verilog top.v
read_xdc top.xdc
synth_design -top top -part {PART}
opt_design
place_design
route_design
write_bitstream -force top.bit
"""


def sites():
    """Package pin -> (tile, IOB_Y index as FASM names it)."""
    out = {}
    with open(DB / PART / "package_pins.csv") as f:
        for row in csv.DictReader(f):
            if not row["pin_function"].startswith("IO_"):
                continue
            y = int(row["site"].rsplit("Y", 1)[1])
            tile = row["tile"]
            if "_SING_" in tile:
                iob = 1 if y % 50 == 49 else 0
            else:
                # A tile holds an odd site and the even one above it; the
                # lower (odd) one is relative Y0, which FASM calls IOB_Y1.
                iob = y % 2
            out[row["pin"]] = (tile, iob)
    return out


def manifest(pins):
    """The same pins and settings as a chipsmith.toml."""
    lines = ['[project]', 'name = "top"', 'top = "top"', 'sources = ["top.v"]', '',
             '[target]', 'device = "xc7z010clg400-1"', '', '[pins]', f'clk = "{CLOCK}"']
    for i, (pin, (d, s, drive, slew, pull)) in enumerate(pins):
        t = [f'pin = "{pin}"', f'standard = "{s}"']
        if drive is not None:
            t += [f'drive = "{drive} mA"', f'slew = "{slew.lower()}"']
        if pull != "NONE":
            t.append(f'pull = "{ {"PULLUP": "up", "PULLDOWN": "down", "KEEPER": "keeper"}[pull] }"')
        lines.append(f"p{i} = {{ {', '.join(t)} }}")
    return "\n".join(lines) + "\n"


# U13's IOB: PUDC_B, the configuration pull-up select. Vivado makes it an
# input with a pull-up whenever a design leaves it unused; chipsmith leaves
# an unused PUDC_B as it leaves every unused ball, unconfigured.
PUDC_B = "RIOB33_X31Y43.IOB_Y0."


def io_features(fasm):
    """Every IOB and bank feature, but the zero-encoded names bit2fasm
    reads on any IOB whose fields are clear, and PUDC_B's."""
    out = set()
    for l in fasm.splitlines():
        f = l.split()[0] if l.strip() and not l.startswith("#") else None
        if not f:
            continue
        if f.startswith(PUDC_B):
            continue
        if f.startswith("RIOB33") or f.startswith("LIOB33"):
            if f.endswith(".IN_TERM.NONE") or f.endswith(".PULLTYPE.PULLDOWN") \
                    or f.endswith(".SLEW.FAST"):
                continue
            out.add(f)
        elif f.startswith("HCLK_IOI3") and ".STEPDOWN" in f:
            out.add(f)
    return out


def bit2fasm(bit):
    return subprocess.run(
        ["bit2fasm", "--db-root", str(DB), "--part", PART, str(bit)],
        capture_output=True, text=True, check=True).stdout


def main():
    out = Path(sys.argv[1]).resolve()
    runs = sys.argv[2:] or list(RUNS)
    where = sites()
    iob_rows, bank_rows = [], []
    failed = []
    for run in runs:
        pins = cases(RUNS[run])
        d = out / run
        d.mkdir(parents=True, exist_ok=True)
        (d / "top.v").write_text(design(pins))
        (d / "top.xdc").write_text(xdc(pins))
        (d / "build.tcl").write_text(TCL)
        if not (d / "top.bit").exists():
            with open(d / "vivado.log", "w") as log:
                subprocess.run(
                    [XC7_RE / "tools/vivado.sh", "-mode", "batch", "-nojournal",
                     "-nolog", "-source", "build.tcl"],
                    cwd=d, stdout=log, stderr=subprocess.STDOUT, check=True)
        fasm = bit2fasm(d / "top.bit")
        (d / "top.fasm").write_text(fasm)
        if os.environ.get("CHIPSMITH"):
            c = d / "chipsmith"
            c.mkdir(exist_ok=True)
            (c / "top.v").write_text(design(pins))
            (c / "chipsmith.toml").write_text(manifest(pins))
            subprocess.run([os.environ["CHIPSMITH"], "-C", str(c), "build"],
                           stdout=subprocess.DEVNULL, check=True)
            ours = io_features(bit2fasm(c / "build/top.bit"))
            theirs = io_features(fasm)
            # Only the pins' own: the routing that reaches each differs.
            for name, diff in [("only Vivado", theirs - ours), ("only chipsmith", ours - theirs)]:
                for f in sorted(diff):
                    print(f"{run}: {name}: {f}")
                    failed.append(f)
            print(f"{run}: {len(theirs)} I/O features, {len(theirs ^ ours)} differ")
        feats = [l.split()[0] for l in fasm.splitlines() if l and not l.startswith("#")]
        for pin, (direction, s, drive, slew, pull) in pins:
            tile, y = where[pin]
            prefix = f"{tile}.IOB_Y{y}."
            mine = sorted(f[len(prefix):] for f in feats if f.startswith(prefix))
            iob_rows.append([run, pin, direction, s, drive or "", slew or "", pull,
                             " ".join(mine)])
        bank_rows += [[run, f] for f in feats if f.startswith("HCLK_IOI")]
    with open(out / "iob.tsv", "w") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n")
        w.writerow(["run", "pin", "dir", "standard", "drive", "slew", "pull", "features"])
        w.writerows(iob_rows)
    with open(out / "bank.tsv", "w") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n")
        w.writerow(["run", "feature"])
        w.writerows(bank_rows)
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
