# 7-series timing: fitted to Vivado's timer on chipsmith's own routing

chipsmith's Zybo estimate summed prjxray-db's intrinsic pip delays by wire class and the SDF's cell delays, and nothing checked it on chipsmith's own routing: Vivado was run on its own placement of the same Verilog (ADR-0013). A design's Fmax then compared two tools, not a model against a timer.

So:

- **Vivado times chipsmith's routing.** `chipsmith_xc7::vivado` writes a build's bitstream, decoded, as Vivado input: a netlist of unisim primitives (each LUT reduced to the pins it reads, with LOCK_PINS), LOC and BEL per cell, and every net's route as `FIXED_ROUTE`, node names in path order. xc7-re's `tools/import.sh` places by those, takes the routes (a net Vivado's site routing can't live with, such as a RAM32M's WE wanted on CE, goes to its router), and reports per endpoint and per connection. `bench/zybo-timing.sh` builds, exports, imports and compares. It is the DE2's `cram quartus-export` for the Zybo, short of a byte-identical bitstream.
- **Routing delay per node, fitted.** Each routing-graph node's delay is its class's (the pip into it: `NN2BEG`, `IMUX`, a slice's `M:A1`) from `data/xc7z010-1-routing.tsv`, least squares over 38,573 connections' Vivado net delays (`bench/zybo-timing-fit.py`), plus a site input's own delay. prjxray-db's intrinsic delays read 17-21% short connection by connection; the fit reads 1-2% (median) and 7-10% (p95), on builds it wasn't fitted on too. Classes the fit hasn't seen fall back to prjxray-db's intrinsic delay times 1.2.
- **The register's arcs are Vivado's.** prjxray-db's SDF leaves the site's routing out of the register (clock to Q 303 ps, Vivado's 456-518): clock to Q and setups are Vivado's medians, and every path end carries 140 ps for the clock tree (uncertainty and skew after pessimism removal), where chipsmith's clocks otherwise arrive at once.
- **Loops are cut.** A loop of LUTs synthesis leaves (no value takes it) stopped the topological order and left what follows untimed, picorv32's worst path among it; the 7-series graph cuts each loop as Vivado's timer does. The DE2's graph is unchanged.

## Considered options

- **prjxray's own Elmore model** (`prjxray/timing.py`: pip resistance into each node's capacitance): prjxray-db carries no capacitance for most INT wires, and Vivado's speed file has 0 for them in grade -1, so it adds little; the fit captures the loading Vivado's timer applies, whatever its source.
- **Placement-only import** (Vivado routing chipsmith's placement): it would time Vivado's routing, not chipsmith's. FIXED_ROUTE takes full node names, so the routing comes across.
- **Vivado's delay tables** (`delay{L,R}-<grade>.dst`): undocumented, and the build would need Vivado's installation; the fitted table is 130 numbers, and refitting is one bench run.

## Consequences

- The estimate, endpoint by endpoint, against Vivado on the same routing: 32.1% median, 44.1% p95 short before (7,715 endpoints over six designs); 6.5% median, 17.0% p95 after (8,632 over seven), 4.2% optimistic at the median. A subset of picorv32's CE paths runs through the loop above, which Vivado cuts elsewhere; there the estimate reads up to 57% long.
- Honest timing failed two examples' 125 MHz: bram-selftest's checker now registers each memory's compare (Vivado's own build of the old RTL made 118 MHz); riscv-bram's CPU runs at 62.5 MHz from a toggling register until chipsmith makes 125 on every seed (115 to 129 MHz over five; Vivado's own 143.5).
- The import found three things co-simulation can't: block RAM and DSP clocks routed beside the BUFG through general interconnect, a RAM32M's two write data pins swapped against Vivado's (and the board's) reading of the same bits, and the loop above.
- The placement-only import, rejected above as the timing method, is the diagnostic that splits placement from routing: `tools/import.sh` with an empty `route.tcl`. On sha256's seed 0 (2026-11-08) Vivado routed chipsmith's placement at 97.0 MHz (98.4 with the LUT pins free) where chipsmith's own routing of it made 71.8. That found the router timing only the connections it routes, every path broken inside a slice (branch xc7-qor2; 96.4 MHz after).
- A refit is due when the router or the placer changes what routes look like enough to move the classes' delays (bench/zybo-timing-fit.py on fresh imports, IMPORT_CONNECTIONS=1).
