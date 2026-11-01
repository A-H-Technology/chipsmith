# chipsmith against Quartus II 13.0sp1

Is chipsmith faster than Quartus? This note times both on the same
Verilog, to a bitstream, on the same machine, and puts each tool's timing
result next to its times. `bench/vs-quartus.sh` produces every number here
and can reproduce them; the raw rows are in `bench-out/vs1/runs.tsv` and
`incr.tsv` (not committed; the tables below are generated from them by
`bench/vs-quartus.sh table`).

## Verdict

**Up to about a thousand LEs, chipsmith is faster, by 3x to 26x in wall time.**
Quartus has a fixed floor: about 60 to 80 s for a 26-LE blinky (map 12 s,
fit 28 to 40 s, assembler 17 to 21 s), where chipsmith takes 5 s. On the
CPU cores (femtorv32, picorv32, mult-selftest, sdram-selftest; 470 to
1,200 LEs), `open`'s chipsmith takes 18 to 28 s of wall time to Quartus'
90 to 140 s, and 1.4x to 2.7x less CPU time.

**At sha256 and picorv32x24 sizes, `open` stops being clearly faster.** sha256
(3,303 LEs in Quartus, 6,046 in chipsmith): 151 s against 190 s of wall
time, but 2.3x Quartus' CPU time. picorv32x24 (20,240 / 28,449 LEs):
slower, a median of 1,516 s against 1,063 s, and 4x the CPU time (4,027 s
against 1,018 s). Its placement anneal is most of that: 644 to 1,807 s
of wall time, at 2.2 to 5.3 cores' worth of CPU time. At load 10, the best
run tied Quartus (900 s against 896 s). Under load, chipsmith's threads slow down far more than
single-threaded Quartus does.

**The gpu-pnr branch (61ae9d7) is faster than Quartus at every size.** picorv32x24
takes 393 to 505 s, 2.1x to 2.7x faster than Quartus, at about Quartus'
CPU time (823 to 910 s against 1,018 s). sha256 takes 100 s, 1.9x faster,
at 0.7x Quartus' CPU time. CUDA and its CPU fallback are within the noise
of each other: the gain comes from the branch's placer, not from the GPU.
That branch can't build sdram-selftest yet: it predates `open`'s negative
PLL phase shifts.

**Quartus wins on quality, most on the logic-deep designs.**

- **LEs:** Quartus' synthesis needs fewer of them: chipsmith uses 12%
  more on crc32, 21% on mult-selftest, 37% on sdram-selftest, 40% on
  picorv32 and picorv32x24, 67% on femtorv32 and 83% on sha256.
- **Fmax:** with both placements under quartus_sta, Quartus is ahead by
  13% on sdram-selftest (166 against 146 MHz), 9 to 21% on mult-selftest
  and 14 to 21% on femtorv32. On sha256 it is ahead by 43 to 50% (65
  against 43 to 45 MHz).
- **Even or chipsmith ahead:** blinky (298 against 285 MHz), crc32, and
  picorv32 (117 to 128 against 120 MHz). gpu-pnr's CUDA placement wins
  picorv32 at 128 MHz. On picorv32x24, chipsmith's own estimate is 95 to
  109 MHz against Quartus' 105.
- **chipsmith's own estimate:** it runs up to 12% above what quartus_sta
  finds for its placement (sdram-selftest 163 against 146, femtorv32 72
  against 67, picorv32 127 against 117).

**Edit and rebuild doesn't change the picture.** Neither tool rebuilds
incrementally after a change to logic. Each rebuild costs about the CPU
time of a cold build, in both tools (Edit and rebuild, below).

**Quartus' own speed settings help little.** `FAST FIT` saves up to a
quarter on the small designs and nothing on sha256 or picorv32x24, at
about the same Fmax. Parallel compilation is disabled in the Web Edition.

So chipsmith turns a bitstream around faster on the DE2's typical designs,
mostly because Quartus has a minute of fixed cost. Quartus' designs are
smaller, and on logic-deep designs faster. `open` doesn't win on large
designs; gpu-pnr's placer does.

## Method

**Machine.** Intel i7-4770K (4 cores, 8 threads, 3.5 GHz), 31 GB, no swap,
NVIDIA GTX 1660, NixOS. The machine is shared: other agents ran their own
builds during the runs. Those included Quartus fits, Quartus Pro fits,
yosys proofs and the gpu-pnr branch's placer benchmark. The `load` column
is the 1-minute load average, sampled every 5 s during each run and
averaged. It ranged from 4 to 41, with a median of 9 (Load, below). A quiet machine would give lower wall times for the
multi-threaded chipsmith and changes little for single-threaded Quartus;
see Caveats.

**Tools.**

| tool | what |
|---|---|
| `q` | Quartus II 13.0sp1 Web Edition (13.0.1 build 232): `quartus_map`, `quartus_fit` (Auto Fit), `quartus_asm` (SOF) |
| `q-fast` | the same with `FITTER_EFFORT "FAST FIT"` |
| `q-all` | the same with `NUM_PARALLEL_PROCESSORS ALL`: two designs only, see below |
| `cs` | `chipsmith build`, branch `open` at a288925 (bench-quartus' `target/release/chipsmith`) |
| `gpu-cpu` | `chipsmith build` of branch `gpu-pnr` at 61ae9d7, `CHIPSMITH_ACCEL=cpu` |
| `gpu-cuda` | the same binary, `CHIPSMITH_ACCEL=cuda` (GTX 1660) |

`q-all` is the same as `q`. The Web Edition prints *Warning (20028): Parallel
compilation is not licensed and has been disabled*, and the fit report
says one processor. It ran on blinky-verilog and crc32 (rep 1) to show
this, then was dropped from the tool list. Quartus 13.0 Web Edition is
single-threaded: its CPU time is its wall time (CPU/wall 0.96 to 0.99 below).

**What each tool does.** Both start from the example's Verilog and pins.
`bench/vs-quartus.py project` writes a Quartus project from the example's
`chipsmith.toml`. It sets the same sources, top, pins, 3.3-V LVTTL and the
same clock constraint (`create_clock` per manifest clock, plus
`derive_pll_clocks` when the design has a PLL). chipsmith's `CY2_JTAG` and
`CY2_PLL` become small shims onto `cycloneii_jtag` and `altpll`, and a
pin's I/O-cell `registers` become Quartus' `FAST_*_REGISTER` assignments.
Quartus' total is map + fit + asm. chipsmith's total is the whole
`chipsmith build`: elaboration, synthesis, packing, placement, routing,
its timing report and the RBF. Quartus' assembler writes a SOF, which is a
smaller job than chipsmith's RBF plus timing report, and quartus_sta is
timed but left out of Quartus' total.

**Timing.** Every run is its own `systemd-run --user --scope` (20 GB, no
swap). Its cgroup's `cpu.stat` gives the CPU time (user + sys) of
everything in it, including Quartus' bubblewrap sandbox. Quartus' stages
are timed by running its programs one at a time. chipsmith's report
lines are stamped with the wall clock and the cgroup's CPU counter as
they arrive, and the stages are the gaps between them. Runs are
interleaved: rep 1 builds design A with every tool, then design B, and so
on, and the tool order rotates with the rep. Each run starts from a fresh
copy of the example with no `build/` and no Quartus `db/`. Before timing,
each chipsmith binary does one untimed blinky build. That writes its
cached routing graph, as Quartus' installed device database already is.

**Quality.** Each row carries the tool's own critical path for its slowest clock
(chipsmith's routed estimate; Quartus' from quartus_sta, slow model, as
`report_clock_fmax_summary`). On rep 1, each chipsmith placement and
routing is also exported to Quartus (cyclone2-re's `cram quartus-export`),
assembled by Quartus with the routing fixed, and timed by the same
quartus_sta, untimed itself. That puts both tools' placements under one
timing model. chipsmith's placer is deterministic for a binary and
design, so its critical paths repeat exactly across reps.

**Edit and rebuild.** `bench/vs-quartus.sh incr` builds a design cold,
then rebuilds it three times, each after toggling one line of logic
(picorv32's loop counts by 2 instead of 1; sha256's seed steps by 3).
Quartus rebuilds in the same project directory, keeping `db/` and
`incremental_db/`. chipsmith rebuilds in the same `build/`, where it
skips every step whose inputs haven't changed (ADR-0011).

## Full builds

Medians over three interleaved reps with [min-max], from a fresh copy of
each example. "Speed-up" is Quartus' (`q`) median divided by chipsmith's.
LEs are each tool's own count after fitting. A run count in parentheses
means that row has fewer than three runs.

Wall time, s:

| design | LEs Quartus / chipsmith | q wall s | q-fast wall s | cs wall s | gpu-cpu wall s | gpu-cuda wall s | speed-up cs | speed-up gpu-cuda |
|---|---|---|---|---|---|---|---|---|
| blinky-verilog | 26 / 26 | 78.1 [70.8-78.2] | 58.5 [52.8-82.1] | 4.8 [4.5-7.7] | 8.9 [4.1-9.7] | 5.8 [4.7-8.3] | 16.4x | 13.5x |
| crc32 | 105 / 118 | 114.5 [84.8-115.0] | 87.5 [78.5-101.2] | 4.5 [4.3-9.5] | 5.1 [4.8-46.6] | 7.4 [6.3-8.4] | 25.6x | 15.5x |
| femtorv32 | 473 / 788 | 105.2 [85.7-112.6] | 86.5 [74.5-101.0] | 17.6 [15.1-18.7] | 12.7 [9.3-17.2] | 13.1 [9.4-13.2] | 6.0x | 8.0x |
| picorv32 | 839 / 1171 | 90.2 [87.6-121.7] | 121.8 [83.4-122.4] | 28.0 [18.3-31.9] | 18.1 [17.8-20.5] | 18.8 [11.0-20.8] | 3.2x | 4.8x |
| mult-selftest | 833 / 1008 | 140.5 [121.4-151.9] | 123.9 [82.4-126.5] | 22.0 [19.6-41.7] | 29.0 [20.8-37.6] | 21.0 [13.6-26.6] | 6.4x | 6.7x |
| m4k-selftest | 16296 / 405 | 796.3 (1) | 914.7 (1) | 9.4 [7.0-17.8] | 8.1 [6.7-17.7] | 7.6 [6.8-9.9] | 84.9x | 104.5x |
| sha256 | 3303 / 6046 | 189.6 [162.2-242.0] | 197.8 [156.4-202.1] | 151.0 [149.6-210.6] | 101.9 [92.0-104.3] | 100.5 [95.6-115.2] | 1.3x | 1.9x |
| picorv32x24 | 20240 / 28449 | 1063.3 [896.1-1191.0] | 1019.5 [905.7-1189.6] | 1515.9 [899.5-2172.4] | 504.9 [351.2-553.5] | 392.9 [382.8-619.9] | 0.7x | 2.7x |
| sdram-selftest | 567 / 777 | 104.3 [98.6-121.3] | 90.7 [78.6-91.1] | 22.1 [11.1-27.2] | fails | fails | 4.7x | - |

CPU time (user + sys of the run's cgroup), s:

| design | LEs Quartus / chipsmith | q CPU s | q-fast CPU s | cs CPU s | gpu-cpu CPU s | gpu-cuda CPU s | speed-up cs | speed-up gpu-cuda |
|---|---|---|---|---|---|---|---|---|
| blinky-verilog | 26 / 26 | 74.2 [68.6-75.2] | 56.6 [52.1-76.1] | 7.8 [7.2-9.1] | 10.2 [8.0-11.2] | 8.7 [7.9-9.5] | 9.5x | 8.6x |
| crc32 | 105 / 118 | 107.1 [83.3-109.9] | 84.4 [77.3-99.5] | 10.3 [9.4-12.5] | 11.1 [10.7-22.3] | 12.5 [12.4-13.0] | 10.4x | 8.6x |
| femtorv32 | 473 / 788 | 102.2 [82.3-105.1] | 83.8 [73.3-97.9] | 38.3 [38.3-38.7] | 25.2 [24.0-26.5] | 20.7 [16.9-21.0] | 2.7x | 4.9x |
| picorv32 | 839 / 1171 | 88.7 [86.2-116.7] | 115.1 [82.2-116.2] | 64.7 [58.5-65.8] | 31.7 [31.3-35.0] | 28.5 [21.0-28.6] | 1.4x | 3.1x |
| mult-selftest | 833 / 1008 | 129.0 [113.7-129.2] | 114.6 [81.3-118.0] | 56.1 [55.7-63.3] | 41.6 [41.0-44.6] | 32.0 [25.2-33.5] | 2.3x | 4.0x |
| m4k-selftest | 16296 / 405 | 781.1 (1) | 888.3 (1) | 23.9 [21.8-25.0] | 23.1 [21.5-23.3] | 22.7 [21.8-23.9] | 32.7x | 34.5x |
| sha256 | 3303 / 6046 | 184.5 [158.8-221.1] | 192.3 [153.2-195.5] | 427.5 [420.6-470.6] | 130.5 [128.2-133.2] | 133.5 [128.9-140.6] | 0.4x | 1.4x |
| picorv32x24 | 20240 / 28449 | 1017.8 [869.1-1066.4] | 963.4 [887.4-1102.8] | 4027.1 [3670.1-4326.1] | 909.8 [822.8-925.0] | 822.8 [817.3-958.4] | 0.3x | 1.2x |
| sdram-selftest | 567 / 777 | 98.1 [92.9-104.5] | 85.7 [76.2-87.3] | 38.8 [35.4-39.9] | fails | fails | 2.5x | - |

Notes on rows:

- **m4k-selftest** is not like for like. Quartus 13.0 infers 7 of its 9
  memories as M4Ks. It builds `w32` (four byte writes per cycle) and `c9`
  (a true dual port with two writes) from registers: 16,296 LEs to
  chipsmith's 405. One Quartus rep each; the default design list has
  sdram-selftest instead. Its Quartus rows also predate the JTAG fix
  below, so its readback was swept.
- **mult-selftest and sdram-selftest**' Quartus rows were rerun after a
  bench fix. Before it, quartus_map swept the `CY2_JTAG` readback because
  the atom didn't reach the dedicated JTAG pins (`runs-superseded.tsv`).
  Those reruns came after chipsmith's, not interleaved with them, at load
  7 to 22.
- **sdram-selftest on gpu-pnr fails:** *CY2_PLL CLK2_PHASE_SHIFT = -1500
  ps: negative phase shifts aren't supported* (the branch predates
  2026-10-31's PLL work on `open`).
- **picorv32x24's quartus_sta column is empty:** Quartus' assembly of
  chipsmith's placement (`quartus_fit` with chipsmith's routing fixed)
  ran for 50 minutes in *post-fitting delay annotation* without finishing,
  against 10 minutes for Quartus' whole fit of its own placement. It was
  stopped, and the gpu-pnr ones were stopped after 15 minutes.
  A retry of `open`'s placement, with nothing else of this bench running,
  hit its 3-hour limit in the same place. Another agent's Quartus assembly
  of the same design was past 3 hours at the time too.

Second window (`bench-out/vs1-quiet`, 2026-11-01 09:50 to 10:48, one rep,
no cross-check). It was meant to be quieter, but other agents kept the
load at 6 to 12. It agrees with the medians above:

| design | tool | wall s | CPU s | load |
|---|---|---|---|---|
| sha256 | q | 205.2 | 199.9 | 6.7 |
| sha256 | cs | 179.7 | 453.4 | 9.0 |
| sha256 | gpu-cpu | 76.0 | 117.3 | 7.6 |
| sha256 | gpu-cuda | 96.6 | 132.9 | 7.0 |
| picorv32x24 | q | 890.4 | 876.3 | 6.4 |
| picorv32x24 | cs | 1274.8 | 3975.0 | 11.8 |
| picorv32x24 | gpu-cpu | 410.6 | 866.1 | 10.1 |
| picorv32x24 | gpu-cuda | 420.0 | 860.6 | 11.0 |

## Quality

CLOCK_50's (sdram-selftest: the 100 MHz PLL clock's) Fmax, MHz.
"Own" is each tool's own analysis: quartus_sta (slow model) for Quartus,
the routed estimate for chipsmith. "quartus_sta" is chipsmith's
placement and routing assembled by Quartus and timed by the same
quartus_sta (rep 1). chipsmith's placement is deterministic, so its
values repeat across reps; gpu-pnr's too.

| design | q own | q-fast own | cs own | gpu-cpu own | gpu-cuda own | cs quartus_sta | gpu-cpu quartus_sta | gpu-cuda quartus_sta |
|---|---|---|---|---|---|---|---|---|
| blinky-verilog | 285.31 | 285.31 | 296.7 | 296.7 | 296.7 | 298.15 | 298.15 | 298.15 |
| crc32 | 267.95 | 267.95 | 288.2 | 298.5 | 298.5 | 260.96 | 301.2 | 301.2 |
| femtorv32 | 80.89 | 81.57 | 71.6 | 75.8 | 76.3 | 66.64 | 70.8 | 71.02 |
| picorv32 | 119.82 | 126.61 | 126.6 | 129.2 | 136.1 | 117.27 | 119.46 | 128.12 |
| mult-selftest | 70.99 | 70.33 | 63.3 | 68.4 | 66.7 | 58.68 | 65.22 | 62.83 |
| m4k-selftest | 71.22 | 69.7 | 84.5 | 84.5 | 84.5 | 85.41 | 85.41 | 85.41 |
| sha256 | 64.8 | 61.07 | 47.6 | 47.4 | 45.7 | 45.44 | 44.93 | 43.08 |
| picorv32x24 | 105.27 | 107.55 | 95.3 | 98.2 | 108.8 | - | - | - |
| sdram-selftest | 165.84 | 153.4 | 163.4 | fails | fails | 146.43 | fails | fails |

## Load

The 1-minute load average during each run (8 hardware threads):

| tool | runs | load, median [min-max] |
|---|---|---|
| q | 25 | 9.2 [4.6-22.6] |
| q-fast | 25 | 9.0 [5.4-21.6] |
| cs | 27 | 8.7 [4.3-23.3] |
| gpu-cpu | 24 | 9.1 [4.1-26.0] |
| gpu-cuda | 24 | 10.1 [4.8-41.3] |

## Edit and rebuild

`bench/vs-quartus.sh incr`, picorv32 and sha256: a cold build, then three
rebuilds, each after toggling one line. The load was higher than during
the full builds (median 15 to 17, peaks of 38), so compare the
rebuilds with the cold build in the same table, not with the tables
above.

| design | tool | cold wall s | rebuild wall s | cold CPU s | rebuild CPU s |
|---|---|---|---|---|---|
| picorv32 | q | 110.8 | 135.0 [96.5-148.1] | 106.2 | 117.4 [94.4-120.6] |
| picorv32 | cs | 29.6 | 58.1 [20.1-66.3] | 63.9 | 68.7 [57.0-68.9] |
| picorv32 | gpu-cpu | 13.3 | 32.0 [13.6-33.5] | 30.8 | 32.8 [30.8-35.1] |
| picorv32 | gpu-cuda | 11.3 | 28.4 [12.3-31.2] | 21.7 | 30.0 [21.9-30.3] |
| sha256 | q | 274.6 | 292.8 [267.3-336.3] | 232.5 | 240.7 [240.7-246.7] |
| sha256 | cs | 570.1 | 481.0 [310.4-655.0] | 533.4 | 525.9 [496.7-552.4] |
| sha256 | gpu-cpu | 170.1 | 201.1 [111.2-204.9] | 140.8 | 142.5 [132.7-152.3] |
| sha256 | gpu-cuda | 197.2 | 191.5 [141.6-228.6] | 151.2 | 151.0 [143.8-157.6] |

Neither tool rebuilds incrementally after a change to logic. Quartus,
run in the same project directory with no design partitions set up,
reruns quartus_map, quartus_fit and quartus_asm in full. chipsmith skips only
steps whose inputs are unchanged (ADR-0011). A changed netlist changes
everything from packing on, so its placement and routing restart from
scratch. Every rebuild costs about a cold build's CPU time in both tools.
picorv32x24 was left out: about 4 hours to show the same thing.

## Caveats

- **Shared machine.** The load average was 4 to 41 (median 9). chipsmith
  uses 1.3 to 3.4 cores and suffers more from load than single-threaded
  Quartus. picorv32x24 on `open` took 900 s at load 9.8 and 2,172 s at
  load 18.5. Quartus' CPU time also rises under load (hyper-threads
  sharing a core, lower turbo): picorv32x24 ranged from 869 to 1,066 s.
- **One machine, a 2013 4-core desktop CPU.** On more cores, chipsmith's
  wall times would drop and Quartus' wouldn't. Quartus' subscription
  edition, with parallel compilation, isn't measured.
- **What each total covers.** Quartus' total leaves out quartus_sta and
  writes a SOF. chipsmith's total includes its timing report and writes
  an RBF.
- **GPU time.** gpu-cuda's CPU time doesn't count the GPU's own time.
- **gpu-pnr is a different placer, not just an accelerator.** `gpu-cpu`
  against `cs` also compares two versions of the code (the branch places
  LE by LE from 700 LEs, ADR-0012). The branch has moved on since
  61ae9d7 (origin/gpu-pnr was at 72ef75d when this was written).
- **One placement seed per tool.** Quality differences of a few percent
  on one design are within what another seed would give
  (README, Compile speed: seed ranges of 2 to 8%).

## Appendix: every column

Full builds (`bench/vs-quartus.sh table`):

| design | tool | runs | LEs | wall s | CPU s | CPU/wall | load | own critical ns | quartus_sta Fmax MHz | stages, median wall/CPU s |
|---|---|---|---|---|---|---|---|---|---|---|
| blinky-verilog | q | 3 | 26 | 78.1 [70.8-78.2] | 74.2 [68.6-75.2] | 0.96 | 8.4 [7.1-10.5] | 3.50 [3.50-3.50] | 285.3 [285.3-285.3] | map 12.2/12.0, fit 39.4/38.0, asm 21.6/20.4, sta 8.8/8.4 |
| blinky-verilog | q-all | 1 | 26 | 65.0 | 63.5 | 0.98 | 8.3 | 3.50 | 285.3 | map 12.9/12.6, fit 35.0/34.0, asm 17.1/16.9, sta 7.0/6.9 |
| blinky-verilog | q-fast | 3 | 26 | 58.5 [52.8-82.1] | 56.6 [52.1-76.1] | 0.97 | 6.1 [5.4-8.0] | 3.50 [3.50-3.50] | 285.3 [285.3-285.3] | map 13.2/12.7, fit 28.1/27.3, asm 17.1/16.6, sta 7.0/6.8 |
| blinky-verilog | cs | 3 | 26 | 4.8 [4.5-7.7] | 7.8 [7.2-9.1] | 1.63 | 5.6 [5.1-8.9] | 3.37 [3.37-3.37] | 298.1 | elaborate 0.5/0.2, synth 0.0/0.0, pack 0.0/0.1, graph 1.8/2.7, place 0.5/2.4, route 0.3/0.7, timing 0.0/0.1, bitstream 1.5/1.5 |
| blinky-verilog | gpu-cpu | 3 | 26 | 8.9 [4.1-9.7] | 10.2 [8.0-11.2] | 1.15 | 8.9 [4.8-12.8] | 3.37 [3.37-3.37] | 298.1 | elaborate 5.2/4.9, synth 0.0/0.1, pack 0.0/0.1, graph 0.1/0.5, place 0.7/2.2, route 0.5/0.9, timing 0.1/0.1, bitstream 1.6/1.6 |
| blinky-verilog | gpu-cuda | 3 | 26 | 5.8 [4.7-8.3] | 8.7 [7.9-9.5] | 1.63 | 9.1 [4.8-31.2] | 3.37 [3.37-3.37] | 298.1 | elaborate 1.2/2.6, synth 0.0/0.1, pack 0.0/0.1, graph 0.1/0.2, place 0.5/2.3, route 0.3/0.6, timing 0.0/0.1, bitstream 2.8/2.8 |
| crc32 | q | 3 | 105 | 114.5 [84.8-115.0] | 107.1 [83.3-109.9] | 0.96 | 8.8 [8.7-20.7] | 3.73 [3.73-3.73] | 267.9 [267.9-267.9] | map 53.0/49.5, fit 39.4/37.9, asm 18.6/18.1, sta 8.6/8.4 |
| crc32 | q-all | 1 | 105 | 101.7 | 97.8 | 0.96 | 8.1 | 3.73 | 267.9 | map 45.4/43.2, fit 33.1/32.4, asm 23.2/22.1, sta 11.5/10.4 |
| crc32 | q-fast | 3 | 105 | 87.5 [78.5-101.2] | 84.4 [77.3-99.5] | 0.98 | 8.8 [7.7-11.2] | 3.73 [3.73-3.73] | 267.9 [267.9-267.9] | map 38.2/37.7, fit 29.4/28.9, asm 15.0/14.7, sta 6.7/6.6 |
| crc32 | cs | 3 | 118 | 4.5 [4.3-9.5] | 10.3 [9.4-12.5] | 2.18 | 7.2 [6.0-8.7] | 3.47 [3.47-3.47] | 261.0 | elaborate 1.4/3.6, synth 0.1/0.3, pack 0.1/0.1, graph 0.0/0.1, place 0.7/3.0, route 1.0/1.3, timing 0.0/0.1, bitstream 1.1/1.1 |
| crc32 | gpu-cpu | 3 | 118 | 5.1 [4.8-46.6] | 11.1 [10.7-22.3] | 2.18 | 6.0 [5.7-26.0] | 3.35 [3.35-3.35] | 301.2 | elaborate 1.7/4.4, synth 0.1/0.2, pack 0.1/0.1, graph 0.0/0.1, place 0.8/2.9, route 0.9/1.7, timing 0.1/0.1, bitstream 1.3/1.3 |
| crc32 | gpu-cuda | 3 | 118 | 7.4 [6.3-8.4] | 12.5 [12.4-13.0] | 1.68 | 14.9 [6.1-41.3] | 3.35 [3.35-3.35] | 301.2 | elaborate 2.6/5.2, synth 0.1/0.3, pack 0.1/0.1, graph 0.1/0.2, place 1.4/2.9, route 1.1/2.2, timing 0.1/0.1, bitstream 2.0/1.7 |
| femtorv32 | q | 3 | 473 | 105.2 [85.7-112.6] | 102.2 [82.3-105.1] | 0.96 | 10.7 [9.3-11.2] | 12.36 [12.36-12.36] | 80.9 [80.9-80.9] | map 28.2/26.8, fit 55.6/54.4, asm 21.4/21.0, sta 10.1/9.8 |
| femtorv32 | q-fast | 3 | 473 | 86.5 [74.5-101.0] | 83.8 [73.3-97.9] | 0.97 | 8.6 [7.4-9.0] | 12.26 [12.26-12.26] | 81.6 [81.6-81.6] | map 21.0/20.6, fit 47.7/45.7, asm 19.4/19.1, sta 9.3/9.1 |
| femtorv32 | cs | 3 | 788 | 17.6 [15.1-18.7] | 38.3 [38.3-38.7] | 2.20 | 8.1 [8.0-9.7] | 13.97 [13.97-13.97] | 66.6 | elaborate 0.2/0.4, synth 1.2/3.3, pack 0.3/0.6, graph 0.0/0.1, place 10.0/28.6, route 5.2/5.1, timing 0.0/0.1, bitstream 0.7/0.7 |
| femtorv32 | gpu-cpu | 3 | 788 | 12.7 [9.3-17.2] | 25.2 [24.0-26.5] | 1.89 | 8.5 [6.3-9.5] | 13.19 [13.19-13.19] | 70.8 | elaborate 1.0/2.8, synth 1.4/2.8, pack 0.3/0.5, graph 0.0/0.1, place 4.9/14.0, route 4.6/4.0, timing 0.1/0.1, bitstream 0.7/0.8 |
| femtorv32 | gpu-cuda | 3 | 788 | 13.1 [9.4-13.2] | 20.7 [16.9-21.0] | 1.59 | 9.0 [6.1-11.6] | 13.11 [13.11-13.11] | 71.0 | elaborate 0.8/2.5, synth 1.3/2.5, pack 0.3/0.5, graph 0.0/0.1, place 4.2/8.5, route 5.3/5.5, timing 0.1/0.1, bitstream 0.8/0.8 |
| picorv32 | q | 3 | 839 | 90.2 [87.6-121.7] | 88.7 [86.2-116.7] | 0.98 | 8.2 [4.6-8.7] | 8.35 [8.35-8.35] | 119.8 [119.8-119.8] | map 25.4/24.7, fit 48.2/47.6, asm 18.1/17.8, sta 7.0/6.9 |
| picorv32 | q-fast | 3 | 839 | 121.8 [83.4-122.4] | 115.1 [82.2-116.2] | 0.95 | 9.3 [5.5-13.0] | 7.90 [7.90-7.90] | 126.6 [126.6-126.6] | map 34.1/33.0, fit 63.2/60.3, asm 21.0/20.6, sta 8.6/8.4 |
| picorv32 | cs | 3 | 1171 | 28.0 [18.3-31.9] | 64.7 [58.5-65.8] | 2.31 | 9.2 [5.0-12.2] | 7.90 [7.90-7.90] | 117.3 | elaborate 0.2/0.6, synth 1.9/4.4, pack 0.4/0.8, graph 0.0/0.1, place 14.2/47.7, route 10.3/10.0, timing 0.0/0.1, bitstream 0.9/0.9 |
| picorv32 | gpu-cpu | 3 | 1171 | 18.1 [17.8-20.5] | 31.7 [31.3-35.0] | 1.76 | 9.2 [5.9-11.3] | 7.74 [7.74-7.74] | 119.5 | elaborate 1.2/2.9, synth 1.8/3.6, pack 0.4/0.8, graph 0.1/0.1, place 6.5/16.1, route 7.7/7.6, timing 0.1/0.1, bitstream 0.7/0.7 |
| picorv32 | gpu-cuda | 3 | 1171 | 18.8 [11.0-20.8] | 28.5 [21.0-28.6] | 1.52 | 9.2 [4.8-11.1] | 7.35 [7.35-7.35] | 128.1 | elaborate 1.4/2.8, synth 2.0/3.7, pack 0.5/0.8, graph 0.0/0.1, place 5.8/10.7, route 8.3/8.4, timing 0.1/0.1, bitstream 0.7/0.8 |
| mult-selftest | q | 3 | 833 | 140.5 [121.4-151.9] | 129.0 [113.7-129.2] | 0.92 | 20.0 [9.2-22.6] | 14.09 [14.09-14.09] | 71.0 [71.0-71.0] | map 36.0/33.1, fit 78.7/72.7, asm 25.8/22.9, sta 12.5/11.4 |
| mult-selftest | q-fast | 3 | 833 | 123.9 [82.4-126.5] | 114.6 [81.3-118.0] | 0.93 | 15.7 [7.0-21.6] | 14.22 [14.22-14.22] | 70.3 [70.3-70.3] | map 35.0/32.3, fit 70.5/65.8, asm 16.4/16.2, sta 8.9/8.7 |
| mult-selftest | cs | 3 | 1008 | 22.0 [19.6-41.7] | 56.1 [55.7-63.3] | 2.54 | 6.1 [4.5-10.8] | 15.80 [15.80-15.80] | 58.7 | elaborate 0.1/0.3, synth 1.1/3.2, pack 0.5/0.9, graph 0.1/0.1, place 11.1/43.2, route 7.9/7.8, timing 0.0/0.0, bitstream 0.6/0.6 |
| mult-selftest | gpu-cpu | 3 | 1008 | 29.0 [20.8-37.6] | 41.6 [41.0-44.6] | 1.41 | 8.0 [6.5-12.4] | 14.63 [14.63-14.63] | 65.2 | elaborate 1.0/2.4, synth 1.7/3.1, pack 0.5/0.9, graph 0.0/0.1, place 14.9/23.6, route 10.3/9.9, timing 0.0/0.1, bitstream 0.7/0.8 |
| mult-selftest | gpu-cuda | 3 | 1008 | 21.0 [13.6-26.6] | 32.0 [25.2-33.5] | 1.52 | 9.9 [5.6-12.1] | 14.99 [14.99-14.99] | 62.8 | elaborate 1.3/2.5, synth 1.7/3.1, pack 0.5/0.7, graph 0.0/0.1, place 7.1/14.0, route 9.6/10.1, timing 0.0/0.1, bitstream 0.8/0.9 |
| m4k-selftest | q | 1 | 16296 | 796.3 | 781.1 | 0.98 | 6.3 | 14.04 | 71.2 | map 235.4/230.2, fit 542.1/532.3, asm 18.8/18.6, sta 15.4/15.3 |
| m4k-selftest | q-fast | 1 | 16296 | 914.7 | 888.3 | 0.97 | 9.1 | 14.35 | 69.7 | map 196.3/193.7, fit 699.6/676.0, asm 18.8/18.6, sta 15.4/15.2 |
| m4k-selftest | cs | 3 | 405 | 9.4 [7.0-17.8] | 23.9 [21.8-25.0] | 2.55 | 7.4 [4.3-21.0] | 11.83 [11.83-11.83] | 85.4 | elaborate 0.3/0.5, synth 0.3/0.9, pack 0.3/0.7, graph 0.1/0.2, place 4.9/15.5, route 3.0/3.7, timing 0.0/0.1, bitstream 0.6/0.6 |
| m4k-selftest | gpu-cpu | 3 | 405 | 8.1 [6.7-17.7] | 23.1 [21.5-23.3] | 2.90 | 7.4 [4.1-18.4] | 11.83 [11.83-11.83] | 85.4 | elaborate 0.8/2.7, synth 0.3/0.4, pack 0.2/0.3, graph 0.0/0.1, place 3.4/13.8, route 2.7/4.1, timing 0.0/0.0, bitstream 0.5/0.6 |
| m4k-selftest | gpu-cuda | 3 | 405 | 7.6 [6.8-9.9] | 22.7 [21.8-23.9] | 2.98 | 7.5 [5.3-16.5] | 11.83 [11.83-11.83] | 85.4 | elaborate 0.9/2.6, synth 0.3/0.6, pack 0.2/0.4, graph 0.0/0.1, place 3.1/14.7, route 2.5/3.5, timing 0.0/0.1, bitstream 0.6/0.7 |
| sha256 | q | 3 | 3303 | 189.6 [162.2-242.0] | 184.5 [158.8-221.1] | 0.97 | 8.3 [7.5-13.1] | 15.43 [15.43-15.43] | 64.8 [64.8-64.8] | map 55.2/53.9, fit 110.8/108.0, asm 23.6/22.2, sta 13.5/12.9 |
| sha256 | q-fast | 3 | 3303 | 197.8 [156.4-202.1] | 192.3 [153.2-195.5] | 0.97 | 8.2 [5.6-9.4] | 16.37 [16.37-16.37] | 61.1 [61.1-61.1] | map 59.5/57.5, fit 117.4/114.4, asm 18.6/18.4, sta 8.9/8.8 |
| sha256 | cs | 3 | 6046 | 151.0 [149.6-210.6] | 427.5 [420.6-470.6] | 2.81 | 9.3 [8.4-14.5] | 20.99 [20.99-20.99] | 45.4 | elaborate 0.1/0.5, synth 4.9/9.1, pack 1.3/1.3, graph 0.0/0.0, place 96.6/369.1, route 49.1/48.4, timing 0.0/0.1, bitstream 1.5/1.5 |
| sha256 | gpu-cpu | 3 | 6046 | 101.9 [92.0-104.3] | 130.5 [128.2-133.2] | 1.31 | 12.2 [9.2-12.9] | 21.11 [21.11-21.11] | 44.9 | elaborate 1.5/3.0, synth 7.2/10.8, pack 1.9/1.9, graph 0.0/0.1, place 23.7/52.5, route 65.4/63.4, timing 0.1/0.1, bitstream 1.5/1.4 |
| sha256 | gpu-cuda | 3 | 6046 | 100.5 [95.6-115.2] | 133.5 [128.9-140.6] | 1.33 | 11.5 [8.7-12.6] | 21.86 [21.86-21.86] | 43.1 | elaborate 1.2/2.6, synth 7.7/11.7, pack 2.1/2.0, graph 0.0/0.1, place 15.8/40.4, route 75.2/77.2, timing 0.1/0.1, bitstream 1.8/1.8 |
| picorv32x24 | q | 3 | 20240 | 1063.3 [896.1-1191.0] | 1017.8 [869.1-1066.4] | 0.96 | 8.7 [7.6-13.4] | 9.50 [9.50-9.50] | 105.3 [105.3-105.3] | map 327.0/317.5, fit 707.3/672.6, asm 29.7/28.9, sta 26.1/25.6 |
| picorv32x24 | q-fast | 3 | 20240 | 1019.5 [905.7-1189.6] | 963.4 [887.4-1102.8] | 0.94 | 10.8 [8.2-12.0] | 9.30 [9.30-9.30] | 107.5 [107.5-107.5] | map 308.3/288.1, fit 678.8/645.0, asm 32.4/30.3, sta 26.4/25.2 |
| picorv32x24 | cs | 3 | 28449 | 1515.9 [899.5-2172.4] | 4027.1 [3670.1-4326.1] | 2.66 | 13.4 [9.8-18.5] | 10.49 [10.49-10.49] | - | elaborate 0.2/0.7, synth 38.8/43.7, pack 7.7/7.6, graph 0.1/0.1, place 1161.8/3683.2, route 294.7/281.4, timing 0.1/0.1, bitstream 5.2/5.0 |
| picorv32x24 | gpu-cpu | 3 | 28449 | 504.9 [351.2-553.5] | 909.8 [822.8-925.0] | 1.80 | 12.5 [8.3-13.5] | 10.18 [10.18-10.18] | - | elaborate 3.3/4.4, synth 44.2/45.7, pack 7.5/7.4, graph 0.1/0.1, place 207.6/626.9, route 214.8/209.1, timing 0.1/0.1, bitstream 5.6/5.4 |
| picorv32x24 | gpu-cuda | 3 | 28449 | 392.9 [382.8-619.9] | 822.8 [817.3-958.4] | 2.08 | 10.7 [10.2-12.6] | 9.19 [9.19-9.19] | - | elaborate 2.0/3.1, synth 41.4/41.4, pack 6.9/6.8, graph 0.1/0.1, place 189.6/601.0, route 169.8/177.9, timing 0.1/0.1, bitstream 3.9/4.1 |
| sdram-selftest | q | 3 | 567 | 104.3 [98.6-121.3] | 98.1 [92.9-104.5] | 0.94 | 13.4 [12.6-17.5] | 6.03 [6.03-6.03] | 165.8 [165.8-165.8] | map 27.4/24.5, fit 60.3/56.1, asm 24.1/22.6, sta 12.5/11.0 |
| sdram-selftest | q-fast | 3 | 567 | 90.7 [78.6-91.1] | 85.7 [76.2-87.3] | 0.96 | 12.2 [10.5-14.4] | 6.52 [6.52-6.52] | 153.4 [153.4-153.4] | map 25.8/24.3, fit 43.1/42.5, asm 16.2/15.9, sta 8.0/7.8 |
| sdram-selftest | cs | 3 | 777 | 22.1 [11.1-27.2] | 38.8 [35.4-39.9] | 1.76 | 13.2 [4.4-23.3] | 6.12 [6.12-6.12] | 146.4 | elaborate 0.3/0.3, synth 1.6/1.8, pack 0.9/1.2, graph 0.1/0.1, place 13.8/29.6, route 4.4/4.1, timing 0.0/0.1, bitstream 0.9/0.9 |
| sdram-selftest | gpu-cpu | 0 (3 failed) | | | | | | | | |
| sdram-selftest | gpu-cuda | 0 (3 failed) | | | | | | | | |

Edit and rebuild (cold builds, then rebuilds):

| design | tool | runs | LEs | wall s | CPU s | CPU/wall | load | own critical ns | quartus_sta Fmax MHz | stages, median wall/CPU s |
|---|---|---|---|---|---|---|---|---|---|---|
| picorv32 | q | 1 | 839 | 110.8 | 106.2 | 0.96 | 10.1 | 8.35 | 119.8 | map 28.5/27.5, fit 63.5/60.7, asm 18.8/18.0, sta 10.6/9.8 |
| picorv32 | cs | 1 | 1171 | 29.6 | 63.9 | 2.16 | 10.4 | 7.90 | - | elaborate 0.3/0.7, synth 1.7/4.0, pack 0.3/0.6, graph 0.0/0.1, place 18.4/49.7, route 8.3/8.1, timing 0.0/0.1, bitstream 0.5/0.6 |
| picorv32 | gpu-cpu | 1 | 1171 | 13.3 | 30.8 | 2.31 | 11.0 | 7.74 | - | elaborate 0.9/2.8, synth 1.8/3.6, pack 0.2/0.5, graph 0.0/0.1, place 4.0/17.5, route 5.8/5.7, timing 0.0/0.0, bitstream 0.5/0.5 |
| picorv32 | gpu-cuda | 1 | 1171 | 11.3 | 21.7 | 1.92 | 10.1 | 7.35 | - | elaborate 0.7/2.3, synth 1.1/2.3, pack 0.2/0.5, graph 0.0/0.1, place 3.4/10.3, route 5.4/5.7, timing 0.0/0.0, bitstream 0.5/0.6 |
| sha256 | q | 1 | 3303 | 274.6 | 232.5 | 0.85 | 13.3 | 15.43 | 64.8 | map 76.7/71.8, fit 165.2/136.9, asm 32.7/23.7, sta 19.6/13.3 |
| sha256 | cs | 1 | 6046 | 570.1 | 533.4 | 0.94 | 25.5 | 20.99 | - | elaborate 0.6/0.5, synth 15.8/13.4, pack 2.4/2.0, graph 0.1/0.1, place 448.0/443.4, route 100.7/72.3, timing 0.2/0.1, bitstream 2.3/1.8 |
| sha256 | gpu-cpu | 1 | 6046 | 170.1 | 140.8 | 0.83 | 18.5 | 21.11 | - | elaborate 3.0/2.3, synth 13.9/12.2, pack 2.1/2.0, graph 0.1/0.1, place 48.1/52.4, route 100.8/69.9, timing 0.1/0.1, bitstream 2.1/1.6 |
| sha256 | gpu-cuda | 1 | 6046 | 197.2 | 151.2 | 0.77 | 21.7 | 21.86 | - | elaborate 2.6/2.9, synth 17.5/12.0, pack 3.5/2.1, graph 0.1/0.1, place 54.2/46.8, route 116.4/85.3, timing 0.1/0.1, bitstream 2.8/1.9 |
| design | tool | runs | LEs | wall s | CPU s | CPU/wall | load | own critical ns | quartus_sta Fmax MHz | stages, median wall/CPU s |
|---|---|---|---|---|---|---|---|---|---|---|
| picorv32 | q | 3 | 835 | 135.0 [96.5-148.1] | 117.4 [94.4-120.6] | 0.87 | 10.4 [8.7-16.2] | 8.49 [8.35-8.49] | 117.7 [117.7-119.8] | map 29.8/29.0, fit 77.6/65.7, asm 25.4/21.9, sta 11.8/10.5 |
| picorv32 | cs | 3 | 1135 | 58.1 [20.1-66.3] | 68.7 [57.0-68.9] | 1.18 | 15.1 [9.3-16.8] | 9.04 [7.90-9.04] | - | elaborate 0.2/0.6, synth 2.8/4.8, pack 0.6/0.8, graph 0.1/0.1, place 38.8/49.0, route 14.4/10.4, timing 0.1/0.1, bitstream 1.1/0.8 |
| picorv32 | gpu-cpu | 3 | 1135 | 32.0 [13.6-33.5] | 32.8 [30.8-35.1] | 1.05 | 17.4 [8.6-17.7] | 8.10 [7.74-8.10] | - | elaborate 1.4/2.6, synth 2.4/3.9, pack 0.5/0.8, graph 0.1/0.1, place 14.1/15.2, route 10.9/8.9, timing 0.1/0.1, bitstream 0.9/0.8 |
| picorv32 | gpu-cuda | 3 | 1135 | 28.4 [12.3-31.2] | 30.0 [21.9-30.3] | 1.06 | 17.1 [8.0-17.3] | 7.82 [7.35-7.82] | - | elaborate 2.0/3.0, synth 2.7/3.7, pack 0.8/0.8, graph 0.1/0.1, place 9.6/11.7, route 10.5/8.8, timing 0.1/0.1, bitstream 1.0/0.9 |
| sha256 | q | 3 | 3303 | 292.8 [267.3-336.3] | 240.7 [240.7-246.7] | 0.82 | 17.1 [13.1-21.5] | 15.43 [15.43-15.43] | 64.8 [64.8-64.8] | map 93.6/77.3, fit 167.3/140.3, asm 28.8/24.3, sta 16.2/13.6 |
| sha256 | cs | 3 | 6046 | 481.0 [310.4-655.0] | 525.9 [496.7-552.4] | 1.09 | 25.0 [17.7-38.4] | 20.75 [20.75-20.99] | - | elaborate 0.9/0.6, synth 12.1/13.5, pack 2.4/2.1, graph 0.2/0.1, place 386.9/436.2, route 103.9/75.9, timing 0.1/0.1, bitstream 2.6/1.8 |
| sha256 | gpu-cpu | 3 | 6046 | 201.1 [111.2-204.9] | 142.5 [132.7-152.3] | 0.74 | 26.5 [12.7-30.8] | 21.11 [21.11-21.11] | - | elaborate 6.2/4.7, synth 9.9/10.8, pack 2.8/2.1, graph 0.1/0.1, place 66.7/54.4, route 105.1/68.6, timing 0.1/0.1, bitstream 2.5/1.7 |
| sha256 | gpu-cuda | 3 | 6046 | 191.5 [141.6-228.6] | 151.0 [143.8-157.6] | 0.79 | 26.9 [12.3-32.7] | 21.86 [21.86-21.86] | - | elaborate 3.8/2.7, synth 15.0/12.3, pack 2.9/2.1, graph 0.1/0.1, place 35.9/44.2, route 133.2/87.8, timing 0.2/0.1, bitstream 2.7/1.9 |
