#!/usr/bin/env python3
"""Vivado golden bitstreams of PLLE2_ADV and MMCME2_ADV configurations.

    bench/cmt/harvest.py <out-dir> [designs]

Writes <out-dir>/d<i>/top.v, pins.xdc and blocks.json, and <out-dir>/run.tcl,
which builds every design in one Vivado session (xc7-re's tools/vivado.sh):

    cd <out-dir> && /pool/dev/xc7-re/tools/vivado.sh -mode batch -source run.tcl

Each design has the device's four clock blocks, two PLLs and two MMCMs, each
with its own CLKFBOUT_MULT, counters, phases and duty cycles, so one
bitstream holds four configurations (`chipsmith_xc7::cmt` is checked against
them, bench/cmt/compare.sh). The multipliers sweep 2-64 across the designs;
the rest is pseudo-random from the design number, so a rerun is the same.

No clock is constrained on the pin: Vivado then takes each block's
CLKIN1_PERIOD as given, which sets a VCO inside its range for that
multiplier whatever the board's clock is.
"""

import json
import random
import sys
from pathlib import Path

# xc7z010-1 (DS187): PLL VCO 800-1600 MHz, MMCM 600-1200, input and phase
# detector 19-450 (PLL) / 10-450 (MMCM).
LIMITS = {
    "PLL": dict(vco=(800.0, 1600.0), pfd=(19.0, 450.0), outs=6, o_max=128, d_max=56),
    "MMCM": dict(vco=(600.0, 1200.0), pfd=(10.0, 450.0), outs=7, o_max=128, d_max=106),
}


def block(rng, prim, mult, fractional, bandwidth=None, zhold=False):
    lim = LIMITS[prim]
    lo, hi = lim["vco"]
    # D = 1: the input is the phase detector's.
    vco = max(lo + 50.0, lim["pfd"][0] * 1.1 * mult)
    vco = min(vco, hi - 20.0, lim["pfd"][1] * 0.95 * mult)
    assert lo < vco < hi, (prim, mult, vco)
    divclk = 1
    if rng.random() < 0.25:
        # A pre-divider too, where the phase detector allows it.
        d = rng.randint(2, 4)
        if vco * d / mult <= lim["pfd"][1] * 0.95:
            divclk = d
    fin = vco * divclk / mult
    period = round(1000.0 / fin, 3)
    outs = []
    n = rng.randint(1, lim["outs"])
    for k in range(lim["outs"]):
        if k >= n and k != 0:
            outs.append(None)
            continue
        if rng.random() < 0.15:
            o = 1
        else:
            o = rng.randint(2, lim["o_max"] if rng.random() < 0.2 else 24)
        if prim == "MMCM" and k == 0 and fractional:
            o = rng.randint(2, 30) + rng.randint(1, 7) / 8.0
        # Phase in eighths of the counter's VCO periods (the hardware's step),
        # up to the 63 whole periods DELAY_TIME holds.
        if rng.random() < 0.5 or (prim == "MMCM" and k == 0 and fractional):
            phase = 0.0
        else:
            steps = rng.randint(0, min(511, int(8 * o) - 1)) if o > 1 else rng.randint(0, 7)
            phase = round(360.0 * steps / (8 * o) if o > 1 else 45.0 * steps, 3)
            if rng.random() < 0.3:
                phase = round(phase - 360.0, 3)
        # Duty in half counts.
        if o > 1 and o == int(o) and rng.random() < 0.4:
            o_i = int(o)
            halves = rng.randint(max(2, o_i // 2), 2 * o_i - 2)
            duty = round(halves / (2 * o_i), 6)
        else:
            duty = 0.5
        outs.append(dict(divide=o, phase=phase, duty=duty))
    bw = bandwidth or rng.choice(["OPTIMIZED"] * 6 + ["LOW", "HIGH"])
    fb_phase = 0.0
    return dict(prim=prim, mult=mult, divclk=divclk, period=period, bandwidth=bw,
                fb_phase=fb_phase, outs=outs, zhold=zhold)


def fmt(v):
    return f"{v:.3f}"


def verilog(blocks):
    lines = ["module top(input clk, output [3:0] led);",
             "  wire clk_i;",
             "  IBUF ib (.I(clk), .O(clk_i));",
             "  wire clk_g;",
             "  BUFG bg (.I(clk_i), .O(clk_g));"]
    toggles = []
    for b, blk in enumerate(blocks):
        prim = blk["prim"]
        cell = "PLLE2_ADV" if prim == "PLL" else "MMCME2_ADV"
        # The first block (a PLL) is fed straight from the pin, with its
        # feedback through a BUFG (ZHOLD, the wizard's way); the others from
        # the BUFG, with internal feedback.
        zhold = b == 0 or blk.get("zhold", False)
        params = [f".CLKIN1_PERIOD({fmt(blk['period'])})",
                  f".DIVCLK_DIVIDE({blk['divclk']})",
                  f".BANDWIDTH(\"{blk['bandwidth']}\")",
                  f".COMPENSATION(\"{'ZHOLD' if zhold else 'INTERNAL'}\")",
                  f".CLKFBOUT_PHASE({fmt(blk['fb_phase'])})"]
        if prim == "PLL":
            params.append(f".CLKFBOUT_MULT({int(blk['mult'])})")
        else:
            params.append(f".CLKFBOUT_MULT_F({fmt(blk['mult'])})")
        conns = [f".CLKIN1({'clk_i' if b == 0 else 'clk_g'})", ".CLKIN2(1'b0)", ".CLKINSEL(1'b1)",
                 ".RST(1'b0)", ".PWRDWN(1'b0)", f".CLKFBOUT(fb{b})", f".CLKFBIN({'fbg' if zhold else 'fb'}{b})",
                 ".DADDR(7'd0)", ".DCLK(1'b0)", ".DEN(1'b0)", ".DI(16'd0)", ".DWE(1'b0)"]
        if prim == "MMCM":
            conns += [".PSCLK(1'b0)", ".PSEN(1'b0)", ".PSINCDEC(1'b0)"]
        decl = [f"  wire fb{b};"]
        if zhold:
            decl.append(f"  wire fbg{b};")
            decl.append(f"  BUFG bfb{b} (.I(fb{b}), .O(fbg{b}));")
        for k, o in enumerate(blk["outs"]):
            if o is None:
                continue
            name = "DIVIDE_F" if prim == "MMCM" and k == 0 else "DIVIDE"
            val = fmt(o["divide"]) if name == "DIVIDE_F" else str(int(o["divide"]))
            params += [f".CLKOUT{k}_{name}({val})", f".CLKOUT{k}_PHASE({fmt(o['phase'])})",
                       f".CLKOUT{k}_DUTY_CYCLE({o['duty']:.6f})"]
            conns.append(f".CLKOUT{k}(o{b}_{k})")
            decl += [f"  wire o{b}_{k}, g{b}_{k};",
                     f"  BUFG bo{b}_{k} (.I(o{b}_{k}), .O(g{b}_{k}));",
                     f"  (* DONT_TOUCH = \"yes\" *) reg t{b}_{k} = 0;",
                     f"  always @(posedge g{b}_{k}) t{b}_{k} <= ~t{b}_{k};"]
            toggles.append(f"t{b}_{k}")
        lines += decl
        lines.append(f"  (* DONT_TOUCH = \"yes\" *) {cell} #({', '.join(params)}) blk{b} ({', '.join(conns)});")
    for i in range(4):
        part = toggles[i::4] or ["1'b0"]
        lines.append(f"  assign led[{i}] = {' ^ '.join(part)};")
    lines.append("endmodule")
    return "\n".join(lines) + "\n"


XDC = """set_property PACKAGE_PIN L16 [get_ports clk]
set_property PACKAGE_PIN M14 [get_ports {led[0]}]
set_property PACKAGE_PIN M15 [get_ports {led[1]}]
set_property PACKAGE_PIN G14 [get_ports {led[2]}]
set_property PACKAGE_PIN D18 [get_ports {led[3]}]
set_property IOSTANDARD LVCMOS33 [get_ports *]
"""

TCL_HEAD = """set_param general.maxThreads 1
set root [pwd]
"""

TCL_DESIGN = """
if {![file exists $root/%(d)s/top.bit]} {
  cd $root/%(d)s
  if {[catch {
    create_project -in_memory -part xc7z010clg400-1
    read_verilog top.v
    read_xdc pins.xdc
    synth_design -top top -part xc7z010clg400-1 -flatten_hierarchy full
    opt_design
    place_design
    route_design
    set f [open placed.txt w]
    foreach c [get_cells -hier -filter {REF_NAME =~ PLLE2_ADV || REF_NAME =~ MMCME2_ADV}] { puts $f "$c [get_property LOC $c]" }
    close $f
    write_bitstream -force top.bit
  } err]} { puts "HARVEST %(d)s FAILED: [string map {"\\n" " "} $err]" } else { puts "HARVEST %(d)s OK" }
  close_project
}
"""


# The (kind, multiplier) pairs the 32 designs leave without a BANDWIDTH
# OPTIMIZED block, which `harvest.py <dir> fill` builds: two PLLs and two
# MMCMs a design, the first MMCMs with ZHOLD feedback (its bit is otherwise
# unseen).
FILL = {
    "PLL": [4, 6, 9, 13, 17, 19, 20, 21, 25, 26, 28, 36, 38, 41, 42, 43, 60, 63],
    "MMCM": [8, 12, 16, 17, 18, 19, 28, 31, 32, 39, 40, 46, 47, 49, 52, 53, 55, 57, 60, 62],
}


def fill(out):
    tcl = [TCL_HEAD]
    plls, mmcms = list(FILL["PLL"]), list(FILL["MMCM"])
    i = 0
    while plls or mmcms:
        rng = random.Random(5000 + i)
        take = lambda xs, default: xs.pop(0) if xs else default
        p = [take(plls, 7), take(plls, 8)]
        m = [take(mmcms, 8), take(mmcms, 10)]
        blocks = [block(rng, "PLL", p[0], False, "OPTIMIZED"), block(rng, "PLL", p[1], False, "OPTIMIZED"),
                  block(rng, "MMCM", float(m[0]), False, "OPTIMIZED", zhold=i < 3),
                  block(rng, "MMCM", float(m[1]), False, "OPTIMIZED")]
        d = out / f"f{i}"
        d.mkdir(parents=True, exist_ok=True)
        (d / "top.v").write_text(verilog(blocks))
        (d / "pins.xdc").write_text(XDC)
        (d / "blocks.json").write_text(json.dumps(blocks, indent=1))
        tcl.append(TCL_DESIGN % dict(d=f"f{i}"))
        i += 1
    (out / "fill.tcl").write_text("".join(tcl))


def main():
    out = Path(sys.argv[1])
    if len(sys.argv) > 2 and sys.argv[2] == "fill":
        return fill(out)
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 32
    out.mkdir(parents=True, exist_ok=True)
    tcl = [TCL_HEAD]
    for i in range(count):
        rng = random.Random(1000 + i)
        # Multipliers 2-64 across the designs: the PLLs from both ends, the
        # MMCMs the same, with fractional ones (eighths) in every third.
        m_lo, m_hi = 2 + i % 63, 64 - i % 63
        frac = i % 3 == 2
        mm = lambda m: m + (rng.randint(1, 7) / 8.0 if frac and m < 64 else 0.0)
        blocks = [block(rng, "PLL", m_lo, False), block(rng, "PLL", m_hi, False),
                  block(rng, "MMCM", mm(m_lo), frac), block(rng, "MMCM", mm(m_hi), frac)]
        d = out / f"d{i}"
        d.mkdir(exist_ok=True)
        (d / "top.v").write_text(verilog(blocks))
        (d / "pins.xdc").write_text(XDC)
        (d / "blocks.json").write_text(json.dumps(blocks, indent=1))
        tcl.append(TCL_DESIGN % dict(d=f"d{i}"))
    (out / "run.tcl").write_text("".join(tcl))


if __name__ == "__main__":
    main()
