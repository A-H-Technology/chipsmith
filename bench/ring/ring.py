#!/usr/bin/env python3
"""Ring oscillators on the DE2, for measuring Cyclone II delays on silicon.

    bench/ring/ring.py gen <spec.json> <project dir>
    bench/ring/ring.py place <spec.json> <placed.json>

A spec is a list of rings, each {"name", "pin", "les": [[x, y, le], ...],
"tap": [x, y, le]} plus optional "kind" ("lut", the default, or "carry").
`gen` writes a chipsmith project: every ring's LCELL_COMBs instantiated
directly, a CLOCK_50 sequencer that runs one ring at a time for a fixed
window, a counter on each ring's tap, and a USER1 register with each
ring's count in its last window (bench/ring/run.sh, `de2_ring` reads it).
`place` moves the ring's cells onto the LEs the spec names, after the
placer has put everything else somewhere, and marks them so the router
keeps each LUT's inputs where they were wired (`CHIPSMITH_FIXED_INPUTS`).

A "lut" ring of N stages (N odd): stage 0 is NAND(ring, enable), the others
NAND(ring, armed), the ring on physical input `pin` (A to D), the second
input on A (B for a ring on A). `armed` is a register that is 1 from the
first clock on: the packer folds a one-input LUT into its loads (as
quartus_map does), and with the enable on every stage, enabling a ring
started an edge in each and one ring locked into three pulses at once. A
"carry" ring: a head LE (input `pin`, A or B, to COUT), `len(les) - 2` LEs that pass
CIN to COUT, a tail LE (CIN to COMBOUT, inverting, enable on input A) whose
COMBOUT is routed back to the head.
"""
import json
import os
import sys

PINS = "ABCD"
# Window and sequencing: each ring runs for ENABLED cycles of CLOCK_50 out
# of every 2^WINDOW_BITS; its counter is sampled SAMPLE cycles before the
# window ends, long after the ring stopped.
WINDOW_BITS = 24
ENABLED = (1 << WINDOW_BITS) - 256
OPEN = os.environ.get("RING_OPEN") == "1"
SAMPLE = (1 << WINDOW_BITS) - 16


def mask(f):
    """LUT_MASK of f(a, b, c, d), bit index {D, C, B, A}."""
    m = 0
    for i in range(16):
        a, b, c, d = i & 1, i >> 1 & 1, i >> 2 & 1, i >> 3 & 1
        if f(a, b, c, d):
            m |= 1 << i
    return m


def pick(pin):
    return lambda a, b, c, d: (a, b, c, d)[PINS.index(pin)]


def lut(name, m, conns):
    ports = ", ".join(f".{p}({n})" for p, n in conns)
    return f"  LCELL_COMB #(.LUT_MASK(16'h{m:04x})) {name} ({ports});\n"


def feedback(r, n):
    """What stage 0 takes: the last stage's output, or for an open build
    (`OPEN`, timed by quartus_sta) a register, so there is no loop."""
    return f"r{r}_launch" if OPEN else f"r{r}_n[{n - 1}]"


def lut_ring(r, ring):
    pin, n = ring["pin"], len(ring["les"])
    assert n % 2 == 1, f"{ring['name']}: a ring of inverters needs an odd length"
    en_pin = "A" if pin != "A" else "B"
    x, e = pick(pin), pick(en_pin)
    v = ""
    v += lut(f"r{r}_s0", mask(lambda *i: not (x(*i) and e(*i))),
             [(f"DATA{pin}", feedback(r, n)), (f"DATA{en_pin}", f"en[{r}]"), ("COMBOUT", f"r{r}_n[0]")])
    for s in range(1, n):
        v += lut(f"r{r}_s{s}", mask(lambda *i: not (x(*i) and e(*i))),
                 [(f"DATA{pin}", f"r{r}_n[{s - 1}]"), (f"DATA{en_pin}", "armed"), ("COMBOUT", f"r{r}_n[{s}]")])
    v += lut(f"r{r}_tap", mask(lambda *i: x(*i) and e(*i)),
             [(f"DATA{pin}", f"r{r}_n[{n - 1 if OPEN else 0}]"), (f"DATA{en_pin}", "armed"), ("COMBOUT", f"r{r}_clk")])
    return n, v


def chains(ring):
    """A carry ring's chains as (first stage, length): `chains` in the spec,
    else one chain of every LE."""
    lens = ring.get("chains", [len(ring["les"])])
    out, first = [], 0
    for n in lens:
        out.append((first, n))
        first += n
    assert first == len(ring["les"])
    return out


def carry_ring(r, ring):
    pin, n = ring["pin"], len(ring["les"])
    assert pin in "AB", "a carry chain's head takes its operand on A or B"
    x = pick(pin)
    v = ""
    cs = chains(ring)
    for j, (first, m) in enumerate(cs):
        last = first + m - 1
        prev = feedback(r, n) if j == 0 else f"r{r}_n[{first - 1}]"
        # COUT is the LUT with C = CIN and D = 0: the head's COUT = its
        # input, the middle LEs' COUT = CIN; the last chain's tail inverts
        # and takes the enable, the others pass CIN (A is `armed`).
        v += lut(f"r{r}_s{first}", mask(lambda *i: x(*i)),
                 [(f"DATA{pin}", prev), ("COUT", f"r{r}_c[{first}]")])
        for s in range(first + 1, last):
            v += lut(f"r{r}_s{s}", mask(lambda a, b, c, d: c),
                     [("CIN", f"r{r}_c[{s - 1}]"), ("COUT", f"r{r}_c[{s}]")])
        if j == len(cs) - 1:
            f, a_net = (lambda a, b, c, d: not (c and a)), f"en[{r}]"
        else:
            f, a_net = (lambda a, b, c, d: c and a), "armed"
        v += lut(f"r{r}_s{last}", mask(f),
                 [("CIN", f"r{r}_c[{last - 1}]"), ("DATAA", a_net), ("COMBOUT", f"r{r}_n[{last}]")])
    v += lut(f"r{r}_tap", mask(lambda a, b, c, d: a and b),
             [("DATAA", f"r{r}_n[{n - 1}]"), ("DATAB", "armed"), ("COMBOUT", f"r{r}_clk")])
    return n, v


def mult_ring(r, ring):
    """Stage 0 (NAND on `pin`) drives every bit of an unsigned 18 x 18
    multiplier's A, B is all ones, product bit `out` closes the ring: A is
    0 or 2^18 - 1 and (2^18 - 1)^2 = 0xffffc0001, so bit `out` follows A
    for out 0 and 19 to 35 (the others stay 0). With "width": 9, the same
    9 x 9: bits 0 and 10 to 17."""
    pin, out = ring["pin"], ring["out"]
    en_pin = "A" if pin != "A" else "B"
    x, e = pick(pin), pick(en_pin)
    v = f"  wire [35:0] r{r}_p;\n"
    v += lut(f"r{r}_s0", mask(lambda *i: not (x(*i) and e(*i))),
             [(f"DATA{pin}", f"r{r}_launch" if OPEN else f"r{r}_p[{out}]"), (f"DATA{en_pin}", f"en[{r}]"),
              ("COMBOUT", f"r{r}_n[0]")])
    if ring.get("width", 18) == 18:
        v += (f"  CY2_MULT #(.A_WIDTH(18), .B_WIDTH(18), .A_SIGNED(0), .B_SIGNED(0), .A_REG(0), .B_REG(0), .P_REG(0))"
              f" r{r}_mult (.A({{18{{r{r}_n[0]}}}}), .B(18'h3ffff), .P(r{r}_p));\n")
    else:
        # Inferred: an instantiated 9 x 9 still has 18-bit ports, whose top
        # bits the packer refuses tied. B is `armed` (1 from the first
        # clock), so synthesis can't take it for a constant.
        v += f"  assign r{r}_p = {{18'd0, {{9'd0, {{9{{r{r}_n[0]}}}}}} * {{9'd0, {{9{{armed}}}}}}}};\n"
    v += lut(f"r{r}_tap", mask(lambda *i: x(*i) and e(*i)),
             [(f"DATA{pin}", f"r{r}_n[0]"), (f"DATA{en_pin}", "armed"), ("COMBOUT", f"r{r}_clk")])
    return 1, v


def gen(spec, out):
    import os
    os.makedirs(f"{out}/src", exist_ok=True)
    k = len(spec)
    v = "// Generated by bench/ring/ring.py.\n"
    v += "module top(input CLOCK_50, output [3:0] LEDR);\n"
    v += f"  reg [{WINDOW_BITS - 1}:0] timer = 0;\n  reg [7:0] sel = 0;\n  reg [31:0] round = 0;\n"
    v += f"  wire [{k - 1}:0] en;\n"
    v += "  reg armed = 0;\n  always @(posedge CLOCK_50) armed <= 1'b1;\n"
    for r, ring in enumerate(spec):
        n, body = {"carry": carry_ring, "mult": mult_ring}.get(ring.get("kind"), lut_ring)(r, ring)
        v += f"  wire [{n - 1}:0] r{r}_n;\n"
        if ring.get("kind") == "carry":
            v += f"  wire [{n - 1}:0] r{r}_c;\n"
        v += f"  wire r{r}_clk;\n" + body
        v += f"  reg r{r}_launch = 0;\n  always @(posedge CLOCK_50) r{r}_launch <= ~r{r}_launch;\n"
        # Four fast bits and the rest enabled once every 16 edges: only a
        # 4-bit carry has to keep up with the ring.
        v += f"  reg [3:0] r{r}_lo = 0;\n  reg [27:0] r{r}_hi = 0;\n"
        v += f"  always @(posedge r{r}_clk) begin r{r}_lo <= r{r}_lo + 1'b1; if (&r{r}_lo) r{r}_hi <= r{r}_hi + 1'b1; end\n"
        v += f"  wire [31:0] r{r}_count = {{r{r}_hi, r{r}_lo}};\n"
        v += f"  reg [31:0] r{r}_prev = 0, r{r}_delta = 0;\n"
        v += f"  reg r{r}_en = 0;\n  assign en[{r}] = r{r}_en;\n"
    v += "  always @(posedge CLOCK_50) begin\n"
    v += "    timer <= timer + 1'b1;\n"
    v += f"    if (&timer) begin round <= round + 1'b1; sel <= (sel == {k - 1}) ? 8'd0 : sel + 1'b1; end\n"
    for r in range(k):
        v += f"    r{r}_en <= (sel == {r}) && (timer < {WINDOW_BITS}'d{ENABLED});\n"
        v += f"    if (sel == {r} && timer == {WINDOW_BITS}'d{SAMPLE}) begin r{r}_delta <= r{r}_count - r{r}_prev; r{r}_prev <= r{r}_count; end\n"
    v += "  end\n  assign LEDR = round[3:0];\n"
    words = 3 + k
    v += f"""  wire tck, tdi, shift, usr1;
  reg [{32 * words - 1}:0] dr = 0;
  CY2_JTAG jtag (.TDOUSER(dr[0]), .TCKUTAP(tck), .TDIUTAP(tdi), .SHIFTUSER(shift), .USR1USER(usr1));
  always @(posedge tck)
    if (shift & usr1) dr <= {{tdi, dr[{32 * words - 1}:1]}};
    else dr <= {{"RING", {", ".join(f"r{r}_delta" for r in reversed(range(k)))}, round, 32'd0}};
endmodule
"""
    open(f"{out}/src/top.v", "w").write(v)
    open(f"{out}/chipsmith.toml", "w").write("""[project]
name = "ring"
top = "top"
sources = ["src/*.v"]

[target]
device = "EP2C35F672C6"

[pins]
CLOCK_50 = "PIN_N2"
LEDR = ["PIN_AE23", "PIN_AF23", "PIN_AB21", "PIN_AC22"]

[clocks]
CLOCK_50 = "50 MHz"
""")
    json.dump({"window": ENABLED, "rings": spec}, open(f"{out}/ring.json", "w"), indent=1)


def lab_tiles():
    """Every LAB's (x, y), from the chip database (cached next to the rings)."""
    import lzma
    import os
    cache = "/tmp/ring/labs.txt"
    if not os.path.exists(cache):
        os.makedirs(os.path.dirname(cache), exist_ok=True)
        db = os.environ["CHIPSMITH_CYCLONE2_DB"] + "/EP2C35F672C6/chipdb.txt.xz"
        with lzma.open(db, "rt") as f, open(cache, "w") as out:
            for line in f:
                if line.startswith("t ") and line.split()[3] == "LAB":
                    out.write(" ".join(line.split()[1:3]) + "\n")
    return [tuple(map(int, l.split())) for l in open(cache)]


def place(spec, placed):
    import re
    j = json.load(open(placed))
    cells = j["modules"]["top"]["cells"]
    want = {}
    for r, ring in enumerate(spec):
        if ring.get("kind") == "mult":
            if f"r{r}_mult" not in cells:
                # Inferred (9 x 9): the multiplier stage 0 drives, renamed.
                net = cells[f"r{r}_s0"]["connections"]["COMBOUT"][0]
                name = next(n for n, c in cells.items() if c["type"] == "CY2_MULT"
                            and any(b == [net] for b in c["connections"].values()))
                cells[f"r{r}_mult"] = cells.pop(name)
            # Next to wherever the placer put the multiplier, in the first
            # LAB column to its right.
            bel = cells[f"r{r}_mult"]["attributes"]["NEXTPNR_BEL"]
            mx, my = map(int, re.match(r"X(\d+)Y(\d+)/", bel).groups())
            ring["les"], ring["tap"] = [[mx + 2, my, 0]], [mx + 2, my, 15]
        for s, (x, y, le) in enumerate(ring["les"]):
            want[f"r{r}_s{s}"] = (x, y, le)
        want[f"r{r}_tap"] = tuple(ring["tap"])
    found = {n: n for n in cells if n in want}
    missing = set(want) - set(found)
    assert not missing, f"ring cells not in the netlist: {sorted(missing)[:5]}"
    # Every other cell in a LAB a ring needs moves, with its whole LAB, to
    # the nearest LAB nothing uses: a ring's LAB holds nothing else, so no
    # other LAB-wide signal or line loads the ring's.
    by_lab = {}
    for name, c in cells.items():
        m = re.match(r"X(\d+)Y(\d+)/LE\d+\.", c["attributes"].get("NEXTPNR_BEL", ""))
        if m and name not in want:
            by_lab.setdefault((int(m[1]), int(m[2])), []).append(name)
    targets = {(x, y) for x, y, _ in want.values()}
    lab_of = {}
    for lab, names in by_lab.items():
        for n in names:
            lab_of[n] = lab
    # A carry chain across LABs moves as one: LABs linked by a COUT->CIN
    # go together, by the same offset.
    group = {lab: {lab} for lab in by_lab}
    drivers = {}
    for name in lab_of:
        co = cells[name]["connections"].get("COUT")
        if co:
            drivers[co[0]] = name
    for name in lab_of:
        ci = cells[name]["connections"].get("CIN")
        d = drivers.get(ci[0]) if ci else None
        if d and lab_of[d] != lab_of[name]:
            merged = group[lab_of[d]] | group[lab_of[name]]
            for lab in merged:
                group[lab] = merged
    labs = set(lab_tiles())
    used = set(by_lab) | targets
    moved = set()
    for lab in sorted(targets & set(by_lab)):
        g = group[lab]
        if lab in moved:
            continue
        offsets = sorted(((dx, dy) for dx in range(-60, 61, 2) for dy in range(-20, 21)),
                         key=lambda o: abs(o[0]) + 2 * abs(o[1]))
        for dx, dy in offsets:
            dest = {(x + dx, y + dy) for x, y in g}
            if (dx, dy) != (0, 0) and dest <= labs and not dest & used:
                break
        else:
            raise SystemExit(f"nowhere to move LABs {sorted(g)}")
        used |= dest
        for src in g:
            moved.add(src)
            for name in by_lab[src]:
                a = cells[name]["attributes"]
                a["NEXTPNR_BEL"] = re.sub(r"^X\d+Y\d+/", f"X{src[0] + dx}Y{src[1] + dy}/", a["NEXTPNR_BEL"])
    for base, (x, y, le) in want.items():
        c = cells[found[base]]
        c["attributes"]["NEXTPNR_BEL"] = f"X{x}Y{y}/LE{le}.LUT"
        if spec[int(base[1:].split("_")[0])].get("kind") in (None, "lut", "mult"):
            c["attributes"]["CHIPSMITH_FIXED_INPUTS"] = "1"
    json.dump(j, open(placed, "w"))
    json.dump(spec, open(os.path.join(os.path.dirname(placed), "placed-spec.json"), "w"), indent=1)


def qname(bel, ff=False):
    """Quartus' name for the LE half chipsmith's export puts at `bel`."""
    import re
    x, y, le, half = re.match(r"X(\d+)Y(\d+)/LE(\d+)\.(LUT|FF)", bel).groups()
    return f"{'ff' if half == 'FF' else 'lc'}_x{int(x) // 2}_y{y}_n{2 * int(le) + (half == 'FF')}"


def sta_tcl(spec, routed, tcl):
    """quartus_sta script: per ring of an open build, report_path from the
    launch register to the last stage's output, printed as `RING k ns`."""
    cells = json.load(open(routed))["modules"]["top"]["cells"]
    drivers = {}
    for name, c in cells.items():
        for port, bits in c["connections"].items():
            if c["port_directions"].get(port) == "output" and bits:
                drivers[bits[0]] = name
    out = ["project_open top", "create_timing_netlist", "read_sdc", "update_timing_netlist"]
    for k, ring in enumerate(spec):
        n = len(ring["les"])
        s0 = cells[f"r{k}_s0"]
        launch = drivers[s0["connections"][f"DATA{ring['pin']}"][0]]
        frm = qname(cells[launch]["attributes"]["NEXTPNR_BEL"]) + "|regout"
        to = qname(cells[f"r{k}_s{n - 1}"]["attributes"]["NEXTPNR_BEL"]) + "|combout"
        out.append(f'puts "RING {k} {frm} [lindex [report_path -from {{{frm}}} -to {{{to}}} -npaths 1] 1]"')
    open(tcl, "w").write("\n".join(out) + "\n")


if __name__ == "__main__":
    cmd, spec_path, target = sys.argv[1:4]
    spec = json.load(open(spec_path))
    if cmd == "sta-tcl":
        sta_tcl(spec, target, sys.argv[4])
    else:
        {"gen": gen, "place": place}[cmd](spec, target)
