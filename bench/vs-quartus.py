#!/usr/bin/env python3
"""Helpers for bench/vs-quartus.sh: a Quartus II 13.0sp1 project from a
chipsmith manifest, and the numbers out of either tool's logs.

    bench/vs-quartus.py project <example dir> <out dir> <flavour>
    bench/vs-quartus.py quartus-row <quartus dir>
    bench/vs-quartus.py chipsmith-row <stamped flow log> <sta summary or ->
    bench/vs-quartus.py table <runs.tsv> [fresh|edit]
    bench/vs-quartus.py summary <runs.tsv> [fresh|edit]

`project` writes <out dir>/top.qpf, top.qsf and top.sdc: the manifest's
sources, top module, pins (3.3-V LVTTL, chipsmith's default) and clocks,
so both tools compile the same Verilog to the same pins against the same
constraint. Flavours: `default` (Quartus' own defaults: Auto Fit, its own
processor count), `all` (NUM_PARALLEL_PROCESSORS ALL), `fast` (Fast Fit).

`quartus-row` and `chipsmith-row` print key=value pairs (LEs, critical path
and Fmax by each tool's own timing analysis, processors Quartus used);
`table` turns runs.tsv into the full Markdown table of docs/vs-quartus.md,
`summary` into its compact wall, CPU and Fmax tables.
"""
import glob
import os
import re
import statistics
import sys
import tomllib

FLAVOURS = {
    "default": [],
    "all": ["set_global_assignment -name NUM_PARALLEL_PROCESSORS ALL"],
    "fast": ['set_global_assignment -name FITTER_EFFORT "FAST FIT"'],
}

# chipsmith's CY2_JTAG (crates/chipsmith-cyclone2/src/synth/cells.v) as the
# Quartus atom it becomes. The atom has to reach the dedicated JTAG pins
# as top-level ports named altera_reserved_*, or quartus_map sweeps it and
# everything that only feeds it: jtag_ports() adds them.
JTAG_SHIM = """\
module CY2_JTAG (input TMS, input TCK, input TDI, output TDO,
                 input TDOUSER, output TMSUTAP, output TCKUTAP, output TDIUTAP,
                 output SHIFTUSER, output CLKDRUSER, output UPDATEUSER,
                 output RUNIDLEUSER, output USR1USER);
  cycloneii_jtag jtag (.tms(TMS), .tck(TCK), .tdi(TDI), .tdo(TDO),
    .tdouser(TDOUSER), .tmsutap(TMSUTAP), .tckutap(TCKUTAP),
    .tdiutap(TDIUTAP), .shiftuser(SHIFTUSER), .clkdruser(CLKDRUSER),
    .updateuser(UPDATEUSER), .runidleuser(RUNIDLEUSER), .usr1user(USR1USER));
endmodule
"""

# chipsmith's CY2_PLL as the altpll Quartus infers it from. Its ARESET and
# PFDENA stay unconnected (no benchmarked design drives them): an undriven
# pfdena would hold the PLL's phase detector off.
PLL_SHIM = """\
module CY2_PLL (input INCLK, input ARESET, input PFDENA, output CLK0, output CLK1, output CLK2,
                output LOCKED);
  parameter INCLK0_INPUT_FREQUENCY = 20000;
  parameter OPERATION_MODE = "NORMAL";
""" + "".join(
    f"  parameter CLK{i}_MULTIPLY_BY = 1;\n  parameter CLK{i}_DIVIDE_BY = 1;\n"
    f"  parameter CLK{i}_PHASE_SHIFT = 0;\n  parameter CLK{i}_DUTY_CYCLE = 50;\n"
    for i in range(3)) + """\
  wire [5:0] c;
  altpll #(.intended_device_family("Cyclone II"), .lpm_type("altpll"),
    .operation_mode(OPERATION_MODE), .compensate_clock("CLK0"),
    .inclk0_input_frequency(INCLK0_INPUT_FREQUENCY),
    .port_inclk0("PORT_USED"), .port_inclk1("PORT_UNUSED"),
    .port_clk0("PORT_USED"), .port_clk1("PORT_USED"), .port_clk2("PORT_USED"),
    .port_locked("PORT_USED"), .port_areset("PORT_UNUSED"), .port_pfdena("PORT_UNUSED"),
""" + "".join(
    f"    .clk{i}_multiply_by(CLK{i}_MULTIPLY_BY), .clk{i}_divide_by(CLK{i}_DIVIDE_BY),\n"
    f"    .clk{i}_phase_shift(CLK{i}_PHASE_SHIFT), .clk{i}_duty_cycle(CLK{i}_DUTY_CYCLE),\n"
    for i in range(3)).rstrip(",\n") + """)
    pll (.inclk({1'b0, INCLK}), .clk(c), .locked(LOCKED));
  assign {CLK2, CLK1, CLK0} = c[2:0];
endmodule
"""

# A manifest pin's `registers` as Quartus' fast I/O register assignments.
FAST_REGISTERS = {
    "input": "FAST_INPUT_REGISTER",
    "output": "FAST_OUTPUT_REGISTER",
    "output-enable": "FAST_OUTPUT_ENABLE_REGISTER",
}


def jtag_ports(src, top):
    """`src` with the dedicated JTAG pins added to module `top`'s ports and
    connected to its CY2_JTAG instance (which must be in `top`)."""
    jtag = ["tms", "tck", "tdi", "tdo"]
    src, n = re.subn(rf"(\bmodule\s+{top}\s*\()",
                     r"\1" + "".join(f"{'output' if p == 'tdo' else 'input'} altera_reserved_{p}, " for p in jtag),
                     src, count=1)
    src, m = re.subn(r"(\bCY2_JTAG\s+\w+\s*\()",
                     r"\1" + "".join(f".{p.upper()}(altera_reserved_{p}), " for p in jtag), src)
    if n != 1 or m != 1:
        sys.exit(f"CY2_JTAG: expected one instance in module {top} (ANSI ports)")
    return src


def pins(entries):
    """(port, pin, I/O-cell registers) from a manifest's [pins] table."""
    for port, v in entries.items():
        regs = []
        if isinstance(v, dict):
            if set(v) - {"pin", "pins", "registers"}:
                # Other pin settings (standards, pulls) have Quartus
                # spellings, but no design benchmarked here uses them.
                sys.exit(f"{port}: pin settings {sorted(set(v) - {'pin', 'pins', 'registers'})} not supported")
            regs = v.get("registers", [])
            v = v.get("pins", v.get("pin"))
        if isinstance(v, str):
            yield port, v, regs
        else:
            for i, p in enumerate(v):
                yield f"{port}[{i}]", p, regs


def mhz(s):
    m = re.fullmatch(r"\s*([0-9.]+)\s*MHz\s*", s)
    if not m:
        sys.exit(f"clock {s!r}: expected '<n> MHz'")
    return float(m.group(1))


def project(example, out, flavour):
    with open(os.path.join(example, "chipsmith.toml"), "rb") as f:
        m = tomllib.load(f)
    if m["target"]["device"] != "EP2C35F672C6":
        sys.exit(f"{example}: not an EP2C35F672C6 design")
    sources = []
    for pat in m["project"]["sources"]:
        found = sorted(glob.glob(os.path.join(example, pat)))
        if not found:
            sys.exit(f"{example}: no source matches {pat}")
        sources += [os.path.realpath(s) for s in found]
    if any(not s.endswith(".v") for s in sources):
        sys.exit(f"{example}: Verilog sources only")
    os.makedirs(out, exist_ok=True)
    jtag = [s for s in sources if "CY2_JTAG" in open(s).read()]
    if jtag:
        # A copy of the one source that instantiates it, with the JTAG pins
        # (`incr` edits both).
        if len(jtag) != 1:
            sys.exit(f"{example}: CY2_JTAG in more than one source")
        copy = os.path.join(out, os.path.basename(jtag[0]))
        with open(copy, "w") as f:
            f.write(jtag_ports(open(jtag[0]).read(), m["project"]["top"]))
        sources[sources.index(jtag[0])] = copy
        with open(os.path.join(out, "cy2_jtag.v"), "w") as f:
            f.write(JTAG_SHIM)
        sources.append(os.path.join(out, "cy2_jtag.v"))
    has_pll = any("CY2_PLL" in open(s).read() for s in sources)
    if has_pll:
        with open(os.path.join(out, "cy2_pll.v"), "w") as f:
            f.write(PLL_SHIM)
        sources.append(os.path.join(out, "cy2_pll.v"))
    with open(os.path.join(out, "top.qpf"), "w") as f:
        f.write('QUARTUS_VERSION = "13.0sp1"\nPROJECT_REVISION = "top"\n')
    q = [
        'set_global_assignment -name FAMILY "Cyclone II"',
        "set_global_assignment -name DEVICE EP2C35F672C6",
        f"set_global_assignment -name TOP_LEVEL_ENTITY {m['project']['top']}",
        "set_global_assignment -name PROJECT_OUTPUT_DIRECTORY output_files",
        'set_global_assignment -name STRATIX_DEVICE_IO_STANDARD "3.3-V LVTTL"',
        'set_global_assignment -name RESERVE_ALL_UNUSED_PINS "AS INPUT TRI-STATED"',
        "set_global_assignment -name SDC_FILE top.sdc",
    ]
    q += [f"set_global_assignment -name VERILOG_FILE {s}" for s in sources]
    for port, pin, regs in pins(m["pins"]):
        q.append(f"set_location_assignment {pin} -to {port}")
        q += [f"set_instance_assignment -name {FAST_REGISTERS[r]} ON -to {port}" for r in regs]
    q += FLAVOURS[flavour]
    with open(os.path.join(out, "top.qsf"), "w") as f:
        f.write("\n".join(q) + "\n")
    with open(os.path.join(out, "top.sdc"), "w") as f:
        for port, freq in m.get("clocks", {}).items():
            f.write(f"create_clock -name {port} -period {1000 / mhz(freq):.3f} [get_ports {port}]\n")
        if has_pll:
            f.write("derive_pll_clocks\n")


def read(path):
    try:
        with open(path, errors="replace") as f:
            return f.read()
    except FileNotFoundError:
        return ""


def fmax_row(sta):
    """The slowest clock's restricted Fmax (MHz) in a
    report_clock_fmax_summary panel, and its period: CLOCK_50's, or a PLL
    output's when the logic runs on one (sdram-selftest at 100 MHz).
    Unconstrained clocks (JTAG's TCK) aren't in the panel."""
    rows = re.findall(r"^;\s*[0-9.]+ MHz\s*;\s*([0-9.]+) MHz\s*;\s*(\S+)", sta, re.M)
    if not rows:
        return {"q_fmax": "-", "q_crit": "-"}
    fmax = min((f for f, _ in rows), key=float)
    return {"q_fmax": fmax, "q_crit": f"{1000 / float(fmax):.2f}"}


def quartus_row(qdir):
    fit = read(os.path.join(qdir, "output_files", "top.fit.rpt"))
    les = re.search(r"Total logic elements\s*;\s*([0-9,]+)", fit)
    allowed = re.search(r"Maximum allowed\s*;\s*(\d+)", fit)
    used = re.search(r"Average used\s*;\s*([0-9.]+)", fit)
    effort = re.search(r"^; Fitter Effort\s*;\s*([^;]+?)\s*;", fit, re.M)
    row = {
        "les": les.group(1).replace(",", "") if les else "-",
        "procs": allowed.group(1) if allowed else "-",
        "procs_avg": used.group(1) if used else "-",
        "effort": effort.group(1).replace(" ", "_") if effort else "-",
    }
    row.update(fmax_row(read(os.path.join(qdir, "fmax.txt"))))
    return row


def chipsmith_row(log, sta):
    text = read(log)
    placed = re.search(r"placed (\d+) LEs.*?critical path ([0-9.]+) ns", text)
    routed = re.search(r"routed \d+ nets.*?critical path ([0-9.]+) ns", text)
    row = {
        "les": placed.group(1) if placed else "-",
        "cs_crit": routed.group(1) if routed else "-",
        "cs_fmax": f"{1000 / float(routed.group(1)):.1f}" if routed else "-",
    }
    row.update(fmax_row(read(sta)) if sta != "-" else {"q_fmax": "-", "q_crit": "-"})
    return row


def spread(xs, digits=1):
    xs = [float(x) for x in xs if x not in ("-", "")]
    if not xs:
        return "-"
    med = statistics.median(xs)
    if len(xs) == 1:
        return f"{med:.{digits}f}"
    return f"{med:.{digits}f} [{min(xs):.{digits}f}-{max(xs):.{digits}f}]"


def load_rows(path):
    rows = []
    with open(path) as f:
        header = f.readline().rstrip("\n").split("\t")
        for line in f:
            r = dict(zip(header, line.rstrip("\n").split("\t")))
            for kv in r.pop("quality", "").split():
                k, _, v = kv.partition("=")
                r[k] = v
            r["stages"] = {
                st.split("=")[0]: tuple(map(float, st.split("=")[1].split("/")))
                for st in r.get("stages", "-").split(",") if "=" in st
            }
            rows.append(r)
    return rows


def table(path, kinds="fresh"):
    """Per design and tool: medians [min-max] of the runs whose kind
    matches (`fresh` for full builds, `edit` for incr's rebuilds)."""
    rows = [r for r in load_rows(path) if r["kind"].startswith(kinds)]
    designs = list(dict.fromkeys(r["design"] for r in rows))
    tools = list(dict.fromkeys(r["tool"] for r in rows))
    print("| design | tool | runs | LEs | wall s | CPU s | CPU/wall | load | own critical ns | "
          "quartus_sta Fmax MHz | stages, median wall/CPU s |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for d in designs:
        for t in tools:
            rs = [r for r in rows if r["design"] == d and r["tool"] == t and r["status"] == "ok"]
            failed = sum(1 for r in rows if r["design"] == d and r["tool"] == t and r["status"] != "ok")
            if not rs:
                if failed:
                    print(f"| {d} | {t} | 0 ({failed} failed) | | | | | | | | |")
                continue
            wall = [float(r["wall"]) for r in rs]
            cpu = [float(r["cpu"]) for r in rs]
            ratio = statistics.median(c / w for c, w in zip(cpu, wall))
            own = [r["cs_crit"] if "cs_crit" in r else r.get("q_crit", "-") for r in rs]
            names = list(dict.fromkeys(k for r in rs for k in r["stages"]))
            stages = ", ".join(
                f"{n} {statistics.median(r['stages'][n][0] for r in rs if n in r['stages']):.1f}/"
                f"{statistics.median(r['stages'][n][1] for r in rs if n in r['stages']):.1f}"
                for n in names)
            runs = f"{len(rs)}" + (f" ({failed} failed)" if failed else "")
            print(f"| {d} | {t} | {runs} | {rs[0].get('les', '-')} | {spread(wall)} | {spread(cpu)} | "
                  f"{ratio:.2f} | {spread([r['load'] for r in rs])} | {spread(own, 2)} | "
                  f"{spread([r.get('q_fmax', '-') for r in rs])} | {stages} |")


def summary(path, kinds="fresh"):
    """The compact tables of docs/vs-quartus.md: wall and CPU per design
    and tool (median [min-max]), chipsmith's speed-up over `q` (ratio of
    medians), and each tool's Fmax by its own analysis and by quartus_sta."""
    rows = [r for r in load_rows(path) if r["kind"].startswith(kinds) and r["status"] == "ok"]
    failed = {(r["design"], r["tool"]) for r in load_rows(path)
              if r["kind"].startswith(kinds) and r["status"] != "ok"}
    designs = list(dict.fromkeys(r["design"] for r in rows))
    tools = [t for t in ("q", "q-fast", "cs", "gpu-cpu", "gpu-cuda") if any(r["tool"] == t for r in rows)]

    def of(d, t):
        return [r for r in rows if r["design"] == d and r["tool"] == t]

    def med(d, t, k):
        xs = [float(r[k]) for r in of(d, t)]
        return statistics.median(xs) if xs else None

    def cell(d, t, k):
        rs = of(d, t)
        if not rs:
            return "fails" if (d, t) in failed else "-"
        return spread([r[k] for r in rs]) + (f" ({len(rs)})" if len(rs) != 3 else "")

    les = lambda d, t: next((r["les"] for r in of(d, t)), "-")
    for k, what in (("wall", "wall s"), ("cpu", "CPU s")):
        print(f"| design | LEs Quartus / chipsmith | " + " | ".join(f"{t} {what}" for t in tools)
              + " | speed-up cs | speed-up gpu-cuda |")
        print("|---" * (len(tools) + 4) + "|")
        for d in designs:
            ratios = []
            for t in ("cs", "gpu-cuda"):
                q, c = med(d, "q", k), med(d, t, k)
                ratios.append(f"{q / c:.1f}x" if q and c else "-")
            print(f"| {d} | {les(d, 'q')} / {les(d, 'cs')} | " + " | ".join(cell(d, t, k) for t in tools)
                  + " | " + " | ".join(ratios) + " |")
        print()
    def same(xs):
        # Deterministic placements repeat their critical path exactly.
        xs = [x for x in xs if x not in ("-", "")]
        return spread(xs) if len(set(xs)) > 1 else (xs[0] if xs else "-")

    print("| design | " + " | ".join(f"{t} own" for t in tools) + " | "
          + " | ".join(f"{t} quartus_sta" for t in tools if t.startswith(("cs", "gpu"))) + " |")
    print("|---" * (1 + len(tools) + sum(t.startswith(("cs", "gpu")) for t in tools)) + "|")
    for d in designs:
        own = []
        for t in tools:
            rs = of(d, t)
            k = "q_fmax" if t.startswith("q") else "cs_fmax"
            own.append(same([r.get(k, "-") for r in rs]) if rs else ("fails" if (d, t) in failed else "-"))
        sta = [same([r.get("q_fmax", "-") for r in of(d, t)]) if of(d, t) else ("fails" if (d, t) in failed else "-")
               for t in tools if t.startswith(("cs", "gpu"))]
        print(f"| {d} | " + " | ".join(own) + " | " + " | ".join(sta) + " |")
    print()
    print("| tool | runs | load, median [min-max] |")
    print("|---|---|---|")
    for t in tools:
        rs = [r for r in rows if r["tool"] == t]
        print(f"| {t} | {len(rs)} | {spread([r['load'] for r in rs])} |")


def main():
    cmd, *args = sys.argv[1:]
    if cmd == "project":
        project(*args)
    elif cmd in ("quartus-row", "chipsmith-row"):
        row = quartus_row(*args) if cmd == "quartus-row" else chipsmith_row(*args)
        print(" ".join(f"{k}={v}" for k, v in row.items()))
    elif cmd == "table":
        table(*args)
    elif cmd == "summary":
        summary(*args)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
