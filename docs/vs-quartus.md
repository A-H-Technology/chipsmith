# chipsmith against Quartus II 13.0sp1

Is chipsmith faster than Quartus, and are its results as good? This note
times both on the same Verilog, to a bitstream, on the same machine, and
puts each tool's timing result next to its times. `bench/vs-quartus.sh`
produces every number here and can reproduce them; the raw rows are in
`bench-out/vs2/runs.tsv` (not committed; the tables below come from it
through `bench/vs-quartus.sh table` and `bench/vs-quartus.py summary`).
This run (2026-11-02) is branch `qor` at 7b03ae2. The first run
(2026-11-01, `open` a288925 and gpu-pnr 61ae9d7) is kept below for
comparison.

## Verdict

**chipsmith is 2x to 16x faster than Quartus in wall time, at every
size, and uses 2.2x to 10x less CPU time.** Quartus has a fixed floor:
about 100 s for a 26-LE blinky (map, fit, assembler), where chipsmith
takes 9 s. On the CPU cores (femtorv32, picorv32, mult-selftest,
sdram-selftest; 500 to 1,000 LEs) chipsmith takes 25 to 40 s to
Quartus' 123 to 162 s. sha256 takes 133 s against 296 s, and
picorv32x24 (20,240 LEs in Quartus) 416 s against 1,247 s. In the first
run `open` was slower than Quartus on picorv32x24 (1,516 s): since then
`open` has merged the gpu-pnr branch's LE-level placer.

**Quartus still wins on LEs, by much less.** chipsmith uses 8.5% more on
femtorv32 (was 67%), 11% on sha256 (83% on a288925, 28% on 61762d1),
14% on sdram-selftest (37%), 16% on picorv32 and picorv32x24 (40% each),
17% on mult-selftest (21%), and 13% fewer on crc32 (it used 12% more).
Closing the gap, below, has what was lost where and what each fix gave
back.

**Fmax, both placements under quartus_sta:**

- **chipsmith ahead:** picorv32 (125 against 120 MHz), sdram-selftest
  (170 against 166), blinky (298 against 285); crc32 even (269 against
  268).
- **Quartus ahead:** by 14% on femtorv32 (81 against 71 MHz) and on
  mult-selftest (71 against 62), and by 48% on sha256 (65 against 44).
- **picorv32x24:** chipsmith's own estimate is 83 MHz against Quartus'
  105. Its register files are in 48 M4Ks now, as in Quartus, and pull the
  24 cores' logic toward the M4K columns; chipsmith's placement handles
  that worse than Quartus' (`open` 61762d1, register files in logic:
  105 MHz by the earlier timing model; with them in M4Ks and nothing else
  changed, 77).
- **chipsmith's own estimate** is within -1.4% to +3.4% of what
  quartus_sta finds for its placement (it ran up to 12% above in the
  first run).

So chipsmith turns a bitstream around faster at every size in this run.
Quartus' designs are 8% to 16% smaller, and faster on the deepest logic
(sha256) and two of the CPUs.

## Full builds

Medians over three interleaved reps with [min-max], from a fresh copy of
each example. "Speed-up" is Quartus' median divided by chipsmith's. LEs
are each tool's own count after fitting.

Wall time, s:

| design | LEs Quartus / chipsmith | q wall s | cs wall s | speed-up cs |
|---|---|---|---|---|
| blinky-verilog | 26 / 26 | 99.8 [87.0-128.5] | 8.7 [8.1-17.4] | 11.5x |
| crc32 | 105 / 91 | 131.9 [125.1-338.2] | 8.1 [6.7-23.6] | 16.2x |
| femtorv32 | 473 / 513 | 138.3 [117.4-152.9] | 25.3 [12.9-28.7] | 5.5x |
| picorv32 | 839 / 971 | 138.2 [115.2-160.3] | 40.0 [26.9-47.0] | 3.5x |
| mult-selftest | 833 / 972 | 162.2 [128.9-165.9] | 38.8 [38.7-52.3] | 4.2x |
| sdram-selftest | 567 / 645 | 123.3 [119.1-133.3] | 29.8 [16.3-31.5] | 4.1x |
| sha256 | 3303 / 3674 | 295.5 [279.1-328.4] | 132.5 [107.7-137.8] | 2.2x |
| picorv32x24 | 20240 / 23527 | 1247.3 [1239.6-1288.9] | 416.2 [341.9-424.6] | 3.0x |

CPU time (user + sys of the run's cgroup), s:

| design | LEs Quartus / chipsmith | q CPU s | cs CPU s | speed-up cs |
|---|---|---|---|---|
| blinky-verilog | 26 / 26 | 83.3 [80.9-86.0] | 9.8 [9.8-10.0] | 8.5x |
| crc32 | 105 / 91 | 119.8 [118.7-131.9] | 12.0 [11.4-12.6] | 10.0x |
| femtorv32 | 473 / 513 | 110.3 [110.2-111.0] | 27.9 [26.1-28.2] | 3.9x |
| picorv32 | 839 / 971 | 125.4 [112.0-126.0] | 37.9 [35.9-38.1] | 3.3x |
| mult-selftest | 833 / 972 | 129.8 [120.4-130.1] | 40.9 [40.6-42.5] | 3.2x |
| sdram-selftest | 567 / 645 | 105.9 [105.7-107.6] | 33.3 [31.7-34.4] | 3.2x |
| sha256 | 3303 / 3674 | 242.0 [239.0-242.1] | 111.2 [111.0-115.2] | 2.2x |
| picorv32x24 | 20240 / 23527 | 1108.0 [1107.8-1116.7] | 466.6 [443.9-473.9] | 2.4x |

## Quality

CLOCK_50's (sdram-selftest: the 100 MHz PLL clock's) Fmax, MHz.
"Own" is each tool's own analysis: quartus_sta (slow model) for Quartus,
the routed estimate for chipsmith. "quartus_sta" is chipsmith's
placement and routing assembled by Quartus and timed by the same
quartus_sta (rep 1); every one of those assemblies came out byte-identical
to chipsmith's RBF but sha256's (below). chipsmith's placement is
deterministic, so its values repeat across reps.

| design | q own | cs own | cs quartus_sta |
|---|---|---|---|
| blinky-verilog | 285.31 | 294.1 | 298.15 |
| crc32 | 267.95 | 274.7 | 268.6 |
| femtorv32 | 80.89 | 72.2 | 70.91 |
| picorv32 | 119.82 | 128.5 | 125.28 |
| mult-selftest | 70.99 | 64.2 | 62.34 |
| sdram-selftest | 165.84 | 176.1 | 170.3 |
| sha256 | 64.8 | 44.3 | 43.65 |
| picorv32x24 | 105.27 | 83.1 | - |

sha256's RBF differed from Quartus' assembly in 260 bits, one a LAB: the
LAB's clear-line inversion (`LAB_CLR_INVERT_PAIR`) for its active-low
reset. chipsmith was right: the cram that exported it to Quartus was the
nix build of cyclone2-re e6aa597, from before de2-arstn taught the export
the inversion (de3386a kept the previous outputHash for its chipsmith
dependency, so `nix build .#cram` failed on master and `result/` still
held the old build). With a current cram sha256 is byte-identical
(`qor2`), and cram now refuses to run from a checkout whose sources it
wasn't built from (cyclone2-re b5eb9e7).

## Load

The 1-minute load average during each run (8 hardware threads):

| tool | runs | load, median [min-max] |
|---|---|---|
| q | 24 | 17.1 [10.8-46.2] |
| cs | 24 | 16.4 [9.6-49.4] |

## Closing the gap (branch `qor`)

What follows took chipsmith's results from `open` 61762d1 toward
Quartus'. Each step is its own commit, proven or co-simulated by
tests/synth.rs and byte-identical through Quartus (`bench/qor.sh`; sha256
excepted, as on `open`), with a test that fails if the gain regresses.

### Where the LEs went

`bench/qor-stats.py` splits a packed design into the categories of
Quartus' fit report. On `open`, against Quartus:

| design | LEs | normal-mode LUTs | arithmetic LUTs | register-only LEs | M4Ks |
|---|---|---|---|---|---|
| femtorv32 | 788 / 473 | 536 / 302 | 170 / 163 | 82 / 8 | 0 / 2 |
| picorv32 | 1,171 / 839 | 837 / 620 | 221 / 180 | 113 / 39 | 0 / 2 |
| sha256 | 4,242 / 3,303 | 2,830 / 2,153 | 673 / 598 | 739 / 552 | 0 / 0 |
| sdram-selftest | 777 / 567 | 573 / 441 | 150 / 116 | 54 / 10 | 0 / 0 |

(chipsmith / Quartus.) The causes, ranked by the LEs each fix gave
back (measured by building with and without it):

1. **Register files in logic** (femtorv32 258 LEs, picorv32 175). Quartus
   puts a CPU's register file, read at two addresses, into two M4Ks.
   chipsmith left it in registers and multiplexers: the read registers
   sat behind nested enables its register merge didn't follow, the
   memory had more read ports than a block, picorv32 reads through an
   enabled address register with new data on a write, and femtorv32's
   ROM-fed read address has constant bits.
2. **Register-only LEs** (sha256 315, femtorv32 80, picorv32 57,
   mult-selftest 36, sdram-selftest 18). A register on the sync load path
   (SDATA) took an LE of its own; Quartus packs it beside an unrelated LUT
   that leaves the LE's input C free.
3. **A constant table as a sum of minterms** (sha256 212). sha256's 64
   round constants of 32 bits took 372 LUTs, Quartus 189: per bit an OR of
   decoded addresses, which a 4-input mapper can't take apart.
4. **Multiplexer trees** (sdram-selftest 107 to 111). Quartus' map report
   credits its "multiplexer restructuring" with 45 (femtorv32) to 297
   (picorv32) LEs. chipsmith now restructures the trees whose leaves
   repeat (one word or the default); trees of mostly distinct words, the
   bulk of picorv32's and mult-selftest's, cost more as one-hot selects and
   stay as they are.
5. **Small comparisons on carry chains** (sha256 32, sdram-selftest 9).
   `step >= 3 && step <= 10` took two chains of five LEs; on 4 bits it is
   one LUT.

Ruled out, by measurement:

- **The LUT mapper.** On the same gates chipsmith's mapper beats abc's
  `if -K 4` and its full `abc -lut 4` script (sdram-selftest 375 LUTs
  against 474 and 389); relaxing its depth bound by 1 to 3 levels and
  keeping 16 or 32 priority cuts instead of 8 change the LUT count by
  under 0.5%.
- **The LAB's synchronous load for multiplexers in front of registers.**
  Quartus loads 540 of sha256's registers so. Taken out before mapping,
  the multiplexer's other leg still needs its own LUT where it is logic,
  and the register's LUT may then use only three inputs (input C carries
  the load data): picorv32 729 -> 731 LUTs, sha256 4,548 -> 4,553.

### Fixes

LEs after each step, in commit order (one-hop register packing in the
first row; the timing commit doesn't change LEs):

| step | femtorv32 | picorv32 | sha256 | mult-selftest | sdram-selftest | picorv32x24 |
|---|---|---|---|---|---|---|
| `open` 61762d1 | 788 | 1,171 | 4,242 | 1,008 | 777 | 28,449 |
| register files into M4Ks | 514 | 976 | | | | |
| constant tables | | | (4,718 -> 4,551 mapped LUTs) | | | |
| register packing | 513 | 971 | 3,715 | 972 | 759 | |
| small comparisons as LUTs | 513 | 971 | 3,674 | 972 | 752 | |
| multiplexer trees (`qor` 7b03ae2) | 513 | 971 | 3,674 | 972 | 645 | 23,527 |
| Quartus | 473 | 839 | 3,303 | 833 | 567 | 20,240 |

quartus_sta's Fmax of chipsmith's placement (seed 0), MHz:

| | femtorv32 | picorv32 | sha256 | mult-selftest | sdram-selftest |
|---|---|---|---|---|---|
| `open` 61762d1 | 68.6 | 113.7 | 47.0 | 64.1 | 128.3 |
| `qor` 7b03ae2 | 70.9 | 125.3 | 43.7 | 62.3 | 170.3 |
| Quartus' own | 80.9 | 119.8 | 64.8 | 71.0 | 165.8 |

### Timing

**Why the estimate was optimistic.** The router puts a LUT's logical
inputs on any of the LE's physical inputs A to D (the mask permuted to
match), but the timing model charged each logical input's delay. The
physical input decides the delay (DMF: A 438 ps, B 420, C 275, D 150):
on the 443 LUT inputs of quartus_sta's 300 worst paths of five designs,
quartus_sta was slower than the model by 53 ps a LUT on average, a
logical D on physical A by 288 ps. quartus_sta also has an
interconnect element into every LUT input that the model didn't (mean
32 ps over 9,000 of them). Routing delays per element matched
(quartus_sta's medians at fanout 1: LAB line 354 ps against the model's
362, C4 284 / 285, R4 235 / 235).

**Calibration.** Every LUT arc is now input D's plus 32 ps, and what a
pin adds sits on the LE input's routing node, so a route pays for the pin
it ends on and the timing-driven router puts critical signals on fast
pins, as Quartus does. The placer charges the mean pin. The routed
estimate against quartus_sta on the same placement, before -> after:

| design | before | after |
|---|---|---|
| femtorv32 | +10.8% | +1.8 to +2.8% |
| picorv32 | +8.7% | +0.1 to +2.6% |
| sha256 | +5.7% | +2.5 to +3.7% |
| mult-selftest | +4.7% | -0.3 to +1.6% |
| sdram-selftest | +16.5% | -0.7 to +9.1% |

(three placement seeds after.) quartus_sta's Fmax rose 5% over the five
(geometric mean of the seeds' means), most on sdram-selftest (132 ->
146 MHz) and picorv32 (111 -> 123); sha256 lost 3.5%.

**Routability.** Register packing made LABs fuller. An LE has one local
line, so a second of its outputs read in the LAB comes back in on a LAB
line: the placer counts it now, and sha256 routes in 16 iterations where
two of three seeds didn't route in 60. At most 8 LEs of a LAB pair a LUT
with an unrelated register.

## Round 2 (branch `qor2`)

### Routing

**Where the time went.** quartus_sta's worst path of sha256 on `open`
3ba072c (43.7 MHz) runs through eight carry chains, adder into adder; of
its 22.9 ns, about 14 are routing between the chains, 1.1 to 2.9 ns a hop,
where Quartus' own placement has 0.44 to 1.0. The hops took detours: two
LABs side by side, whose LE output reaches the next LAB's lines directly,
were joined by five R4/C4 wires (1.8 ns); another net went down nine rows
and back. With chipsmith's placement and no routing, Quartus routed
sha256 to 55.5 MHz: the router accounted for more of the gap than the
placement. picorv32x24 showed it directly: its critical path was 9.58 ns
at the fourth routing iteration (1,713 nodes still overused) and 12.08 ns
once legal, its worst connection a register into an adder in the next LAB
over 19 wires (5.5 ns).

**Why.** Present congestion's cost grew 1.8x an iteration. By the last
iterations a node another net used cost even a 0.95-critical connection
more than a detour of ten or twenty wires, so clearing the last few
overused nodes pushed critical connections the long way round.

**Fix.** The growth is 1.2 now (VPR's default is 1.3), and once the
routing is legal, the nets of connections at least 0.9 critical are
rerouted for delay alone, taking other nets' wires at the cost of an R4;
the nets they displaced reroute around them, and a repair stays only if
the routing is legal, the net got faster and the critical path no longer.
Picked on femtorv32, picorv32, mult-selftest, sdram-selftest and sha256,
three placement seeds each (routed critical paths, geometric mean against
1.8): the repair alone x0.981; growth 1.5 x0.981, 1.3 x0.966; with the
repair, 1.4 x0.941, 1.3 x0.955, 1.2 x0.938, 1.1 as good in half again the
iterations. Capping the present factor at 100 or 1000 instead didn't move
picorv32x24 (12.08 to 13.10 ns).

quartus_sta's Fmax of chipsmith's placement and routing (seed 0), MHz,
every RBF byte-identical through Quartus (`bench/qor.sh`), as are the 26
other DE2 examples (`bench/quartus-validate.sh`):

| design | `open` 3ba072c | `qor2` | Quartus' own |
|---|---|---|---|
| sha256 | 43.65 | 51.38 | 64.80 |
| crc32 | 268.60 | 293.00 | 267.95 |
| sdram-selftest | 170.30 | 180.41 | 165.84 |
| picorv32 | 125.28 | 129.85 | 119.82 |
| mult-selftest | 62.34 | 63.98 | 70.99 |
| femtorv32 | 70.91 | 70.84 | 80.89 |
| blinky-verilog | 298.15 | 298.15 | 285.31 |

picorv32x24, chipsmith's estimate (Quartus' assembly of it doesn't
finish): 12.08 -> 8.84 ns, 82.8 -> 113 MHz, against Quartus' 105.3. Routing
takes more iterations (sha256 19 against 31 at seed 0, femtorv32 16
against 10) and the repair 5 to 40 s; the build times are in the rerun
above. `crates/chipsmith/tests/route_quality.rs` fails if femtorv32's or
sha256's routed critical path goes back over 14.0 or 19.6 ns.

## Method

**Machine.** Intel i7-4770K (4 cores, 8 threads, 3.5 GHz), 31 GB, no swap,
NVIDIA GTX 1660, NixOS. The machine is shared: other agents ran their own
builds during the runs. Those included Quartus fits and programming-file
runs, another device's database builds and chipsmith's own proof suite.
The `load` column is the 1-minute load average, sampled every 5 s during
each run and averaged. In this run it ranged from 10 to 49, with a median
of 17 (Load, below), about twice the first run's. A quiet machine would
give lower wall times for both tools; see Caveats.

**Tools.**

| tool | what |
|---|---|
| `q` | Quartus II 13.0sp1 Web Edition (13.0.1 build 232): `quartus_map`, `quartus_fit` (Auto Fit), `quartus_asm` (SOF) |
| `cs` | `chipsmith build`, branch `qor` at 7b03ae2 (`open` 61762d1, which has gpu-pnr merged, and the commits of Closing the gap), on CPU threads |

The first run also had `q-fast` (`FITTER_EFFORT "FAST FIT"`), `q-all`
(`NUM_PARALLEL_PROCESSORS ALL`, which the Web Edition disables: warning
20028) and the gpu-pnr branch on CPU and CUDA (First run, below). Quartus
13.0 Web Edition is single-threaded: its CPU time is its wall time.

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
design, so its critical paths repeat exactly across reps. picorv32x24
isn't cross-checked: Quartus' assembly of chipsmith's placement of it
hadn't finished after 3 hours in the first run (`XCHECK=0`).

**Edit and rebuild.** `bench/vs-quartus.sh incr` builds a design cold,
then rebuilds it three times, each after toggling one line of logic
(picorv32's loop counts by 2 instead of 1; sha256's seed steps by 3).
Quartus rebuilds in the same project directory, keeping `db/` and
`incremental_db/`. chipsmith rebuilds in the same `build/`, where it
skips every step whose inputs haven't changed (ADR-0011).


## First run (2026-11-01)

`open` a288925 and the gpu-pnr branch at 61ae9d7, at load 4 to 41
(median 9). `cs` is `open`; `gpu-cpu` and `gpu-cuda` the gpu-pnr
branch's binary, on CPU threads and on the GTX 1660.

### Full builds

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

### Quality

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

### Load

The 1-minute load average during each run (8 hardware threads):

| tool | runs | load, median [min-max] |
|---|---|---|
| q | 25 | 9.2 [4.6-22.6] |
| q-fast | 25 | 9.0 [5.4-21.6] |
| cs | 27 | 8.7 [4.3-23.3] |
| gpu-cpu | 24 | 9.1 [4.1-26.0] |
| gpu-cuda | 24 | 10.1 [4.8-41.3] |


### Edit and rebuild

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

- **Shared machine.** The load average was 10 to 49 (median 17) in this
  run and 4 to 41 (median 9) in the first. Both tools slow down under
  load: Quartus' CPU time on picorv32x24 was 1,108 s here against 869 to
  1,066 s in the first run, and chipsmith's wall time runs about 10%
  above its CPU time.
- **One machine, a 2013 4-core desktop CPU.** Quartus' subscription
  edition, with parallel compilation, isn't measured.
- **What each total covers.** Quartus' total leaves out quartus_sta and
  writes a SOF. chipsmith's total includes its timing report and writes
  an RBF.
- **One placement seed per tool.** Quality differences of a few percent
  on one design are within what another seed would give: on sha256,
  mult-selftest and sdram-selftest three seeds of `qor` spread 5% to 7%
  under quartus_sta.

## Appendix: every column

Full builds (`bench/vs-quartus.sh table bench-out/vs2`):

| design | tool | runs | LEs | wall s | CPU s | CPU/wall | load | own critical ns | quartus_sta Fmax MHz | stages, median wall/CPU s |
|---|---|---|---|---|---|---|---|---|---|---|
| blinky-verilog | q | 3 | 26 | 99.8 [87.0-128.5] | 83.3 [80.9-86.0] | 0.83 | 19.0 [16.2-46.2] | 3.50 [3.50-3.50] | 285.3 [285.3-285.3] | map 23.1/18.0, fit 50.6/42.4, asm 26.2/22.9, sta 11.5/10.0 |
| blinky-verilog | cs | 3 | 26 | 8.7 [8.1-17.4] | 9.8 [9.8-10.0] | 1.13 | 22.3 [13.3-49.4] | 3.40 [3.40-3.40] | 298.1 | elaborate 0.2/0.3, synth 0.1/0.1, pack 0.1/0.1, graph 1.5/1.4, place 1.7/2.6, route 0.9/1.2, timing 0.1/0.1, bitstream 4.5/3.9 |
| crc32 | q | 3 | 105 | 131.9 [125.1-338.2] | 119.8 [118.7-131.9] | 0.91 | 18.5 [10.8-37.4] | 3.73 [3.73-3.73] | 267.9 [267.9-267.9] | map 55.9/51.2, fit 51.2/46.2, asm 24.9/22.5, sta 15.1/10.4 |
| crc32 | cs | 3 | 91 | 8.1 [6.7-23.6] | 12.0 [11.4-12.6] | 1.47 | 17.8 [9.6-34.5] | 3.64 [3.64-3.64] | 268.6 | elaborate 2.3/3.8, synth 0.2/0.3, pack 0.1/0.1, graph 0.1/0.1, place 1.1/1.9, route 1.9/3.3, timing 0.1/0.1, bitstream 2.5/2.3 |
| femtorv32 | q | 3 | 473 | 138.3 [117.4-152.9] | 110.3 [110.2-111.0] | 0.80 | 17.5 [11.1-21.0] | 12.36 [12.36-12.36] | 80.9 [80.9-80.9] | map 36.4/28.9, fit 73.6/59.0, asm 26.2/23.1, sta 12.6/10.7 |
| femtorv32 | cs | 3 | 513 | 25.3 [12.9-28.7] | 27.9 [26.1-28.2] | 1.10 | 20.9 [12.3-22.0] | 13.86 [13.86-13.86] | 70.9 | elaborate 0.3/0.5, synth 1.6/2.4, pack 0.2/0.5, graph 0.1/0.1, place 13.7/15.6, route 7.5/7.8, timing 0.1/0.1, bitstream 0.9/0.8 |
| picorv32 | q | 3 | 839 | 138.2 [115.2-160.3] | 125.4 [112.0-126.0] | 0.91 | 14.3 [11.4-18.0] | 8.35 [8.35-8.35] | 119.8 [119.8-119.8] | map 44.1/35.6, fit 70.6/66.9, asm 23.5/22.3, sta 11.5/10.8 |
| picorv32 | cs | 3 | 971 | 40.0 [26.9-47.0] | 37.9 [35.9-38.1] | 0.95 | 17.7 [11.1-23.1] | 7.78 [7.78-7.78] | 125.3 | elaborate 0.6/0.7, synth 3.3/4.5, pack 0.8/0.9, graph 0.1/0.1, place 13.3/14.8, route 19.8/15.9, timing 0.1/0.1, bitstream 1.1/0.9 |
| mult-selftest | q | 3 | 833 | 162.2 [128.9-165.9] | 129.8 [120.4-130.1] | 0.80 | 14.7 [10.9-26.7] | 14.09 [14.09-14.09] | 71.0 [71.0-71.0] | map 42.8/33.9, fit 87.8/73.2, asm 27.2/22.6, sta 14.6/11.6 |
| mult-selftest | cs | 3 | 972 | 38.8 [38.7-52.3] | 40.9 [40.6-42.5] | 1.05 | 12.9 [12.2-29.1] | 15.57 [15.57-15.57] | 62.3 | elaborate 0.3/0.6, synth 2.3/4.0, pack 0.6/0.8, graph 0.1/0.1, place 17.1/19.1, route 18.6/15.6, timing 0.1/0.1, bitstream 1.1/0.9 |
| sdram-selftest | q | 3 | 567 | 123.3 [119.1-133.3] | 105.9 [105.7-107.6] | 0.86 | 14.0 [13.6-18.7] | 6.03 [6.03-6.03] | 165.8 [165.8-165.8] | map 30.6/26.0, fit 64.4/57.7, asm 26.9/22.6, sta 13.4/10.9 |
| sdram-selftest | cs | 3 | 645 | 29.8 [16.3-31.5] | 33.3 [31.7-34.4] | 1.12 | 17.1 [13.2-23.6] | 5.68 [5.68-5.68] | 170.3 | elaborate 0.3/0.5, synth 1.3/1.8, pack 0.9/1.0, graph 0.1/0.1, place 20.2/25.5, route 4.8/3.5, timing 0.1/0.1, bitstream 1.0/0.9 |
| sha256 | q | 3 | 3303 | 295.5 [279.1-328.4] | 242.0 [239.0-242.1] | 0.82 | 19.2 [19.0-20.0] | 15.43 [15.43-15.43] | 64.8 [64.8-64.8] | map 101.1/76.7, fit 166.1/140.7, asm 28.8/23.9, sta 15.8/13.5 |
| sha256 | cs | 3 | 3674 | 132.5 [107.7-137.8] | 111.2 [111.0-115.2] | 0.87 | 15.6 [15.5-18.3] | 22.58 [22.58-22.58] | 43.6 | elaborate 0.5/0.6, synth 14.2/15.4, pack 2.3/1.8, graph 0.1/0.1, place 19.3/21.3, route 86.2/72.2, timing 0.1/0.1, bitstream 1.6/1.3 |
| picorv32x24 | q | 3 | 20240 | 1247.3 [1239.6-1288.9] | 1108.0 [1107.8-1116.7] | 0.89 | 16.2 [14.3-16.6] | 9.50 [9.50-9.50] | 105.3 [105.3-105.3] | map 401.6/341.4, fit 837.3/737.4, asm 33.6/30.5, sta 32.7/29.2 |
| picorv32x24 | cs | 3 | 23527 | 416.2 [341.9-424.6] | 466.6 [443.9-473.9] | 1.12 | 12.0 [10.6-14.3] | 12.03 [12.03-12.03] | - | elaborate 0.7/0.9, synth 43.0/43.8, pack 9.6/9.0, graph 0.1/0.1, place 104.0/176.6, route 250.2/229.1, timing 0.1/0.1, bitstream 5.3/5.0 |
