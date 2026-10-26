#!/usr/bin/env python3
"""A random DE2 pin-settings project, for checking chipsmith's I/O cells
against Quartus (bench/quartus-validate.sh).

    bench/io-fuzz.py <out-dir> <seed> [pads.txt]

Every user pin of the EP2C35F672C6 is independently used with probability
1/4 as an input, an output or a bidir, each its own scalar port so it gets
its own [pins] entry: a random I/O standard (3.3-V LVTTL at a random
current, 3.3-V LVCMOS, 2.5 V on inputs), pull-up or bus hold, and I/O-cell
registers where the design has the register to move (some powering up
high, some behind an inverter). One dedicated clock input clocks them. The
unused-pin mode is random too. Outputs are driven from inputs, so the core
holds little but routing. Not for hardware: the pins are random.

pads.txt is the chip database's `pad` lines (default: read from
$CHIPSMITH_CYCLONE2_DB/EP2C35F672C6/chipdb.txt.xz).
"""
import lzma
import os
import pathlib
import random
import re
import sys

CURRENTS = [4, 8, 12, 16, 20, 24]
UNUSED = ["tri-stated", "pull-up", "bus-hold"]
DUAL_PURPOSE = {"E3", "D3", "AE24"}


def pads(path):
    if path:
        lines = pathlib.Path(path).read_text().splitlines()
    else:
        db = pathlib.Path(os.environ["CHIPSMITH_CYCLONE2_DB"]) / "EP2C35F672C6/chipdb.txt.xz"
        with lzma.open(db, "rt") as f:
            lines = [l for l in f if l.startswith("pad ")]
    out = []
    for l in lines:
        _, ball, _tile, bel, _bank = l.split()
        clock = int(bel[3:]) >= 8
        out.append((ball, clock))
    return out


def settings(rng, role, clock):
    s = {}
    if clock:
        return s
    r = rng.random()
    if r < 0.15:
        s["standard"] = "3.3-V LVCMOS"
    elif r < 0.25 and role == "in":
        s["standard"] = "2.5 V"
    elif role != "in" and rng.random() < 0.6:
        s["drive"] = f"{rng.choice(CURRENTS)} mA"
    p = rng.random()
    if p < 0.2:
        s["pull"] = "up"
    elif p < 0.4:
        s["pull"] = "bus-hold"
    return s


def toml_entry(name, pin, s, regs):
    fields = [f'pin = "PIN_{pin}"']
    for k, v in s.items():
        fields.append(f'{k} = "{v}"')
    if regs:
        fields.append("registers = [" + ", ".join(f'"{r}"' for r in regs) + "]")
    return f"{name} = {{ {', '.join(fields)} }}"


def main():
    out = pathlib.Path(sys.argv[1])
    seed = int(sys.argv[2])
    rng = random.Random(seed)
    allp = pads(sys.argv[3] if len(sys.argv) > 3 else None)
    clk = rng.choice([p for p in allp if p[1]])
    # Dedicated clock inputs other than the clock stay unused: as data
    # they would ride the clock network.
    # Neither do the dual-purpose configuration pins (ASDO, nCSO, nCEO),
    # which Quartus only lets a design have with device options set.
    used = [p for p in allp if not p[1] and p[0] not in DUAL_PURPOSE and rng.random() < 0.25]
    ins, outs, bidirs = [], [], []
    for ball, clock in used:
        role = rng.choice(["in", "in", "out", "out", "bidir"])
        {"in": ins, "out": outs, "bidir": bidirs}[role].append((ball, clock))
    if not ins:
        ins.append(outs.pop())

    ports, body, entries = ["input clk"], [], [f'clk = "PIN_{clk[0]}"']
    srcs = []
    # Registered pins whose register something must read, or synthesis
    # drops it: (source name, its [pins] entry index, register line index).
    pending = {}
    for k, (ball, clock) in enumerate(ins):
        ports.append(f"input i{k}")
        s = settings(rng, "in", clock)
        regs = []
        if rng.random() < 0.4:
            pending[f"ri{k}"] = (len(entries), (f"i{k}", ball, s), len(body))
            body.append(f"  reg ri{k}; always @(posedge clk) ri{k} <= i{k};")
            regs.append("input")
            srcs.append(f"ri{k}")
        else:
            srcs.append(f"i{k}")
        entries.append(toml_entry(f"i{k}", ball, s, regs))

    read = set()

    def src():
        unread = [n for n in srcs if n not in read]
        n = rng.choice(unread or srcs)
        read.add(n)
        return f"~{n}" if rng.random() < 0.2 else n

    def reg(name, d):
        init = " = 1'b1" if rng.random() < 0.3 else ""
        body.append(f"  reg {name}{init}; always @(posedge clk) {name} <= {d};")
        return name

    for k, (ball, _) in enumerate(outs):
        ports.append(f"output o{k}")
        s = settings(rng, "out", False)
        regs = []
        d = src()
        if rng.random() < 0.4:
            d = reg(f"ro{k}", d)
            regs.append("output")
        body.append(f"  assign o{k} = {d};")
        entries.append(toml_entry(f"o{k}", ball, s, regs))
    for k, (ball, _) in enumerate(bidirs):
        ports.append(f"inout b{k}")
        s = settings(rng, "bidir", False)
        regs = []
        d, oe = src(), src()
        if rng.random() < 0.4:
            d = reg(f"rd{k}", d)
            regs.append("output")
        if rng.random() < 0.4:
            oe = reg(f"re{k}", oe)
            regs.append("output-enable")
        body.append(f"  assign b{k} = {oe} ? {d} : 1'bz;")
        # The input side, registered or not, feeds later outputs.
        if rng.random() < 0.4:
            pending[f"rb{k}"] = (len(entries), (f"b{k}", ball, s), len(body))
            body.append(f"  reg rb{k}; always @(posedge clk) rb{k} <= b{k};")
            regs.append("input")
            srcs.append(f"rb{k}")
        else:
            srcs.append(f"b{k}")
        entries.append(toml_entry(f"b{k}", ball, s, regs))
    # An input register nothing reads isn't asked for.
    for n, (e, (name, ball, s), line) in pending.items():
        if n not in read:
            regs = re.findall(r'"(output|output-enable)"', entries[e].split("registers", 1)[-1]) if "registers" in entries[e] else []
            entries[e] = toml_entry(name, ball, s, regs)
            body[line] = ""

    out.mkdir(parents=True, exist_ok=True)
    (out / "src").mkdir(exist_ok=True)
    (out / "src/top.v").write_text(
        f"module top({', '.join(ports)});\n" + "\n".join(body) + "\nendmodule\n"
    )
    (out / "chipsmith.toml").write_text(
        f"""# bench/io-fuzz.py {seed}: random pins and settings, not for hardware.
[project]
name = "top"
top = "top"
sources = ["src/top.v"]

[target]
device = "EP2C35F672C6"
unused_pins = "{rng.choice(UNUSED)}"

[pins]
"""
        + "\n".join(entries)
        + "\n"
    )


if __name__ == "__main__":
    main()
