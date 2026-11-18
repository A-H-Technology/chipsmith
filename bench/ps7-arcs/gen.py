#!/usr/bin/env python3
"""The PS7's timing arcs as Vivado has them, for every fabric port:
crates/chipsmith-xc7/src/ps7_timing.tsv.

Vivado gives a PS7 pin an arc only when the design connects it, so this
writes a top with every PS7 fabric port (chipsmith's PS7 blackbox,
crates/chipsmith-xc7/src/synth/cells.v) connected: clock pins on the board
clock, every other input from its own register of one long shift register,
every output folded into one pin. Vivado places and routes it at 100 MHz
and reports the path to each PS7 input and from each output; the setup
(`Setup_ps7_*`) and clock-to-out (`Prop_ps7_*`) its timer charges there are
the table. Not the arcs' DELAY_* properties: for setup those read 0.175 ns
over what the timer uses (MAXIGP0ARREADY 1.013 against 0.838 ns).

    bench/ps7-arcs/gen.py OUTDIR      # needs ../xc7-re/tools/vivado.sh
"""

import os
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
XC7_RE = Path(os.environ.get("XC7_RE", ROOT.parent / "xc7-re"))
CELLS = ROOT / "crates/chipsmith-xc7/src/synth/cells.v"
PART = "xc7z010clg400-1"


def ps7_ports():
    body = re.search(r"module PS7 \((.*?)\);", CELLS.read_text(), re.S).group(1)
    return [(d, int(w or 0) + 1, n)
            for d, w, n in re.findall(r"(input|output)\s*(?:\[(\d+):0\])?\s*(\w+)", body)]


def is_clock(name):
    # The PS7's clock inputs: the AXI ports' ACLKs and the other ports'
    # fabric-side clocks (DMA, EMIO Ethernet, trace, ...).
    return name.endswith("ACLK") or name.endswith("CLK") or re.search(r"CLK(FB)?$", name)


def top(ports):
    ins = [(w, n) for d, w, n in ports if d == "input" and not is_clock(n)]
    clocks = [n for d, w, n in ports if d == "input" and is_clock(n)]
    outs = [(w, n) for d, w, n in ports if d == "output"]
    total = sum(w for w, _ in ins)
    lines = ["module top(input clk, input d, output q);",
             f"  reg [{total - 1}:0] s = 0;",
             f"  always @(posedge clk) s <= {{s[{total - 2}:0], d}};"]
    conns, at = [], 0
    for w, n in ins:
        conns.append(f".{n}(s[{at + w - 1}:{at}])")
        at += w
    conns += [f".{n}(clk)" for n in clocks]
    for w, n in outs:
        lines.append(f"  wire [{w - 1}:0] o_{n};")
        conns.append(f".{n}(o_{n})")
    lines.append(f"  PS7 ps7 ({', '.join(conns)});")
    fold = " ^ ".join(f"^o_{n}" for _, n in outs)
    lines.append(f"  reg r = 0;\n  always @(posedge clk) r <= {fold};\n  assign q = r;")
    lines.append("endmodule")
    return "\n".join(lines) + "\n"


XDC = """set_property PACKAGE_PIN L16 [get_ports clk]
set_property PACKAGE_PIN T14 [get_ports d]
set_property PACKAGE_PIN M14 [get_ports q]
set_property IOSTANDARD LVCMOS33 [get_ports {clk d q}]
create_clock -period 10.000 [get_ports clk]
"""

TCL = f"""set_param general.maxThreads 1
read_verilog top.v
read_xdc top.xdc
synth_design -top top -part {PART}
opt_design
place_design
route_design
set c [get_cells ps7]
set f [open arcs.tsv w]
foreach a [get_timing_arcs -of_objects $c] {{
    puts $f "[get_property REF_PIN_NAME [get_property FROM_PIN $a]]\t[get_property REF_PIN_NAME [get_property TO_PIN $a]]\t[get_property TYPE $a]\t[get_property DELAY_SLOW_MAX_RISE $a]"
}}
close $f
file delete timing.txt
foreach a [get_timing_arcs -of_objects $c -filter {{TYPE == setup}}] {{
    report_timing -to [get_property TO_PIN $a] -max_paths 1 -file timing.txt -append
}}
foreach a [get_timing_arcs -of_objects $c -filter {{TYPE == "Reg Clk to Q"}}] {{
    report_timing -from [get_property TO_PIN $a] -max_paths 1 -file timing.txt -append
}}
"""

TERM = re.compile(r"PS7 \((Setup|Prop)_ps7_([A-Z0-9]+)_([A-Z0-9\[\]]+)\)\s*\n\s*(-?[\d.]+)")


def table(timing_txt):
    """Vivado's reports -> {site pin: (clock pin, clock_to_out|setup, ps)}."""
    rows = {}
    for kind, clk, pin, ns in TERM.findall(timing_txt.read_text()):
        ps = round(abs(float(ns)) * 1000)
        key = re.sub(r"\[(\d+)\]$", r"\1", pin)
        rows[key] = (clk, "setup" if kind == "Setup" else "clock_to_out", ps)
    return rows


def main():
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    (out / "arcs.tcl").write_text(TCL)
    (out / "top.xdc").write_text(XDC)
    ports = ps7_ports()
    # chipsmith's PS7 is the site's, which also has test pins Vivado's PS7
    # primitive lacks (DEBUGSELECT, TESTA9MBIST*, ...): Vivado names each
    # one it doesn't know, and the second run leaves them out.
    for attempt in range(2):
        (out / "top.v").write_text(top(ports))
        with open(out / "vivado.log", "w") as log:
            rc = subprocess.run([XC7_RE / "tools/vivado.sh", "-mode", "batch", "-nojournal",
                                 "-nolog", "-source", "arcs.tcl"],
                                cwd=out, stdout=log, stderr=subprocess.STDOUT).returncode
        unknown = set(re.findall(r"named port connection '(\w+)' does not exist",
                                 (out / "vivado.log").read_text()))
        if rc == 0:
            break
        if attempt or not unknown:
            sys.exit(f"Vivado failed: {out / 'vivado.log'}")
        print(f"leaving out {len(unknown)} ports Vivado's PS7 lacks", file=sys.stderr)
        ports = [p for p in ports if p[2] not in unknown]
    rows = table(out / "timing.txt")
    kinds = defaultdict(int)
    for clk, kind, _ in rows.values():
        kinds[(clk, kind)] += 1
    for (clk, kind), n in sorted(kinds.items()):
        print(f"{clk}\t{kind}\t{n}", file=sys.stderr)
    with open(out / "ps7_timing.tsv", "w") as f:
        for pin, (clk, kind, ps) in sorted(rows.items()):
            f.write(f"{pin}\t{clk}\t{kind}\t{ps}\n")


if __name__ == "__main__":
    main()
