# chipsmith against Quartus II 13.0sp1

Is chipsmith faster than Quartus, and are its results as good? This note
times both on the same Verilog, to a bitstream, on the same machine, and
puts each tool's timing result next to its times. `bench/vs-quartus.sh`
produces every number here and can reproduce them; the raw rows are in
`bench-out/vs5/runs.tsv` (not committed; the tables below come from it
through `bench/vs-quartus.sh table` and `bench/vs-quartus.py summary`).
This run (2026-11-05) is branch `qor4` at de70915. The fourth run
(2026-11-03, `qor3` f6d26ff), the third (2026-11-02/03, `qor2` 16c6b23),
the second (2026-11-02, `qor` 7b03ae2) and the first (2026-11-01, `open`
a288925 and gpu-pnr 61ae9d7) are kept below for comparison.

## Verdict

**chipsmith is 1.8x to 17x faster than Quartus in wall time, at every
size, and uses 1.4x to 7.4x less CPU time.** The machine was quiet
this time (load median 1.8, against 14 in the fourth run), which takes
most off Quartus' fixed floor: blinky took 51 s (128 s before) against
chipsmith's 3.8 s. On the CPU cores (femtorv32, picorv32, mult-selftest,
sdram-selftest; 500 to 1,000 LEs) chipsmith takes 7 to 22 s to Quartus'
64 to 77 s, sha256 78 s to 141 s, and picorv32x24 311 s to 673 s.

**Quartus still wins on LEs:** chipsmith uses 3.4% more on sha256, 4.4%
on picorv32, 4.9% on picorv32x24, 9.5% on femtorv32 (+5 LEs since the
fourth run, the early register file read's address holds), 11% on
sdram-selftest and 15% on mult-selftest (16% before). It uses 13% fewer
on crc32.

**Fmax, both placements under quartus_sta:**

- **chipsmith ahead:** femtorv32 (92.7 against 80.9 MHz; Quartus was 7%
  ahead in the fourth run), mult-selftest (75.6 against 71.0; Quartus was
  4% ahead), picorv32 (141 against 120), sha256 (69.5 against 64.8),
  crc32 (296 against 268) and blinky (298 against 285). picorv32x24 is
  even by chipsmith's estimate (105.4 against Quartus' 105.3).
- **Quartus ahead:** sdram-selftest at the default placement seed (165.8
  against 154.2; chipsmith was ahead with 180.9 in the fourth run). Over
  placement seeds 0-6, chipsmith's placement ranges from 153.7 to 176.1,
  mean 164.0 (157.8 to 180.9, mean 170.5, before Round 4's timing model
  change). The worst path at seed 0 is a register through five LUTs into
  the DRAM_CAS_N I/O-cell register, which chipsmith's estimate for that
  clock puts 13% faster than quartus_sta does (Round 4, What's left).
- **chipsmith's own estimate** is within -2.0% to +5.4% of what
  quartus_sta finds for its placement (sdram-selftest's +5.4%).

So chipsmith turns a bitstream around faster at every size, and its
placements are faster than Quartus' on six of the eight designs, even
on one, and on sdram-selftest depend on the placement seed. Quartus'
placements are 3% to 15% smaller.

## Full builds

Medians over three interleaved reps with [min-max], from a fresh copy of
each example. "Speed-up" is Quartus' median divided by chipsmith's. LEs
are each tool's own count after fitting.

Wall time, s:

| design | LEs Quartus / chipsmith | q wall s | cs wall s | speed-up cs |
|---|---|---|---|---|
| blinky-verilog | 26 / 26 | 50.7 [50.4-56.5] | 3.8 [3.8-4.6] | 13.4x |
| crc32 | 105 / 91 | 72.0 [71.5-74.4] | 4.2 [4.2-4.5] | 16.9x |
| femtorv32 | 473 / 518 | 65.5 [65.4-74.2] | 10.6 [10.3-17.9] | 6.2x |
| picorv32 | 839 / 876 | 75.0 [74.4-98.6] | 20.0 [20.0-23.6] | 3.8x |
| mult-selftest | 833 / 956 | 76.7 [76.4-81.0] | 21.7 [21.3-24.7] | 3.5x |
| sdram-selftest | 567 / 628 | 63.8 [63.7-67.1] | 7.3 [7.3-8.9] | 8.8x |
| sha256 | 3303 / 3417 | 140.9 [140.2-143.4] | 77.9 [77.0-79.4] | 1.8x |
| picorv32x24 | 20240 / 21241 | 673.1 [664.0-678.0] | 310.7 [308.6-339.3] | 2.2x |

CPU time (user + sys of the run's cgroup), s:

| design | LEs Quartus / chipsmith | q CPU s | cs CPU s | speed-up cs |
|---|---|---|---|---|
| blinky-verilog | 26 / 26 | 50.2 [49.8-55.8] | 7.0 [6.9-8.4] | 7.2x |
| crc32 | 105 / 91 | 71.1 [70.5-73.6] | 9.7 [9.5-10.3] | 7.4x |
| femtorv32 | 473 / 518 | 64.8 [64.7-72.9] | 24.2 [23.9-30.6] | 2.7x |
| picorv32 | 839 / 876 | 74.2 [73.7-97.0] | 39.1 [38.7-39.3] | 1.9x |
| mult-selftest | 833 / 956 | 75.8 [75.5-80.0] | 44.0 [43.8-44.5] | 1.7x |
| sdram-selftest | 567 / 628 | 63.1 [63.0-66.4] | 27.8 [27.4-29.0] | 2.3x |
| sha256 | 3303 / 3417 | 139.2 [138.5-141.9] | 98.1 [97.4-98.8] | 1.4x |
| picorv32x24 | 20240 / 21241 | 665.5 [655.4-669.1] | 426.4 [423.5-452.7] | 1.6x |

## Quality

CLOCK_50's (sdram-selftest: the 100 MHz PLL clock's) Fmax, MHz.
"Own" is each tool's own analysis: quartus_sta (slow model) for Quartus,
the routed estimate for chipsmith. "quartus_sta" is chipsmith's
placement and routing assembled by Quartus and timed by the same
quartus_sta (rep 1); every one of those assemblies came out byte-identical
to chipsmith's RBF. chipsmith's placement is deterministic, so its values
repeat across reps. They are one placement seed, the default; Round 4
gives each design's spread over seeds.

| design | q own | cs own | cs quartus_sta |
|---|---|---|---|
| blinky-verilog | 285.31 | 294.1 | 298.15 |
| crc32 | 267.95 | 289.9 | 295.95 |
| femtorv32 | 80.89 | 95.3 | 92.65 |
| picorv32 | 119.82 | 145.1 | 141.24 |
| mult-selftest | 70.99 | 75.5 | 75.58 |
| sdram-selftest | 165.84 | 162.6 | 154.23 |
| sha256 | 64.8 | 70.9 | 69.49 |
| picorv32x24 | 105.27 | 105.4 | - |

picorv32x24's chipsmith estimate fell from 109.6 to 105.4 MHz since the
fourth run (9.12 -> 9.49 ns). The model's register enable arc is now
measured, 660 ps where it was 366 ps (Round 4). picorv32x24 isn't
cross-checked (Method).

## Load

The 1-minute load average during each run (8 hardware threads):

| tool | runs | load, median [min-max] |
|---|---|---|
| q | 24 | 1.8 [1.4-8.2] |
| cs | 24 | 1.8 [1.3-8.2] |

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

### Where picorv32's LEs still go

Quartus' map report credits its multiplexer restructuring with 297 of
picorv32's LEs, most of them `reg_op1`'s 14:1 multiplexer (261 LEs before,
116 after). That is against Quartus' own unrestructured baseline, not
chipsmith: the LUTs that feed only `reg_op1`'s registers are 132 in
chipsmith for 32 bits, Quartus' 116 for 29. Comparing, per register, the
LUTs whose whole fanout ends in it (chipsmith's netlist against Quartus'
post-fit one):

| registers | chipsmith | Quartus |
|---|---|---|
| `reg_next_pc` (30) | 123 | 66 |
| `alu_out_q` (32) | 153 | 128 |
| `reg_out` (32) | 113 | 89 |
| shared by several | 168 | 83 |

Quartus builds `reg_next_pc` one LE a bit: one adder for both `current_pc
+ 4` and `current_pc + imm_j` (resource sharing, an operand multiplexed),
the register in the adder's LE, and `current_pc` on its synchronous load
(SDATA through input C, which arithmetic mode leaves free). chipsmith
builds two adders and a multiplexer LUT after them. Two attempts, neither
merged:

- **The LAB's synchronous load** under a multiplexer with an adder on one
  leg (branches `qor2-sload` in both repositories: chip database SLOAD
  pins, synthesis, packer, placer, timing, bitstream import, Quartus
  export, a proven and co-simulated test): real designs barely have the
  pattern as it stands (sha256 -6 LEs, the CPUs none), and the new pins
  change the routing graph, so every RBF would need revalidating. picorv32
  needs its adders shared first, and sha256's 540 synchronous loads in
  Quartus load constants (`digest_init ? H0 : H + a`).
- **Sharing two additions under a multiplexer:** picorv32's two have been
  narrowed differently by then (30 and 31 bits, bit 0 of one unread), so
  the multiplexer reads a slice of one; matching them needs the
  narrowing aligned.

femtorv32's worst path under quartus_sta starts at its register file's
M4K (3.0 ns clock to output) and crosses LUTs and a carry chain with
short routing (0.2 to 0.4 ns an element): the router has nothing to give
there, and its Fmax didn't move.

### Co-simulation

femtorv32's and picorv32's co-simulations compared no output bits: the
RTL's registers and memories without an initial value started x, so the
LED register loaded through an x enable at the first edge (and
femtorv32's x0, never written, read x), and their reset button, redrawn
every cycle, kept the CPUs in reset. The RTL's registers and memories now
power up 0 as the device's do (its x constants stay don't-cares), the
block model reads an undefined INIT bit as 0 as the bitstream writes it,
and the CPUs' KEY[0] is a button pressed one cycle in 256. Both compare
270,000 bits now, and swapping two of a register file's read address
pins fails both (`cpu_cosim_catches_a_wrong_register`); every M4K design
compares more (README, M4K table).

## Round 3 (branch `qor3`)

Each step below is its own commit, proven and co-simulated by
tests/synth.rs, and every DE2 example's RBF (33; picorv32x24 aside) is
byte-identical through Quartus (`bench/quartus-validate.sh`).
`crates/chipsmith/tests/route_quality.rs` fails if a gain slides back.

### sha256: the placement wasn't it

Round 2 left sha256's gap to the placement: its worst path crosses
adders feeding adders, and Quartus routing chipsmith's placement reached
only 55.5 MHz. On `open` 80351e1 the path (quartus_sta, 19.5 ns, 10.4 of
it routing) runs through eight carry chains, each entered a bit or two
up from where the last left it, with a hop of 0.8 to 1.5 ns between
them. A direct link to the LAB beside it costs 0.42 ns, and only 2 of
the 67 inter-LAB connections on the 300 worst paths were one. Quartus'
own path has four chains after its first LUT, side by side on one row.

Placer changes, measured on sha256 with five placement seeds (routed
estimate, MHz; - did not route in 60 iterations):

| placer | seed 0 | 1 | 2 | 3 | 4 |
|---|---|---|---|---|---|
| `open` | 51.9 | - | 50.5 | 51.2 | 52.0 |
| hop delays as whole wires, chains swapping LEs out of their way | 52.4 | 53.1 | 51.5 | 52.5 | 52.2 |
| the same, without the swaps | 52.1 | 52.0 | - | 52.1 | - |
| + chains legalised beside the chains they feed, timing cost 0.03/ps in the anneal | 52.5 | 52.2 | 52.0 | 52.2 | - |
| timing cost 0.1/ps | - | - | 52.0 | 52.2 | 52.9 |

The placer's model charged a quarter wire a LAB of distance, so a chain
one row off its neighbour cost 71 ps where the router pays a C4
(285 ps), and its anneal minimises weighted wirelength, which prefers a
chain below another (1 row) to one beside it (2 tile columns). With
both fixed it gains 1 to 3%, routes less often and the timing cost
doubles placement time. Rejected (parked on branch `qor3-placer`): the
chains were mostly side by side already (131 of about 260
chain-to-chain connections direct links on `open`), and what remained
was the adder tree's shape.

### Sums ordered by arrival

`alumacc` built a sum of three or more terms as a balanced tree,
pairing the terms as written. sha256's `a_new = t1 + sum0 + maj` (`t2`
folded in) became `(t1 + sum0) + maj` with `t1`, three adders deep,
going in first, and `w_data` (two adders and a multiplexer behind a
register) went into `t1`'s tree at the bottom: five adders from `w_data`
to `a_reg`. Now the two terms ready first are added first (Huffman on a
logic-level estimate: a gate 1, a LUT 2, a sum 4), so the late term goes
into the last adder. Cyclone II only; the 7-series flow keeps the order
as written.

### Shared sums

Quartus builds picorv32's `reg_next_pc` as one adder: `current_pc + 4`
and `current_pc + decoded_imm_j` under a multiplexer share
`current_pc`, so the multiplexer moves to the other operand. chipsmith
now does the same (`chipsmith_synth::share`) before `wreduce`, while
both sums are as wide as written: narrowed (30 and 31 bits, as in Round
2's attempt) they no longer match.

### The LAB's synchronous load

From the parked `qor2-sload` (chip database SLOAD pins, synthesis,
packer, placer, timing, bitstream import, Quartus export): a register
behind a multiplexer with a sum on one leg sits in the adder's LE, the
sum on DATAIN, the other leg on SDATA (input C, which arithmetic mode
leaves free) and the select on the LAB's SLOAD. What it needed to apply
to real designs:

- **A LAB budget.** A chain fills its LABs 16 LEs at a time, so 16 LEs'
  operands and SDATA legs, less what their own registers feed back, must
  fit a LAB's 34 inputs (30 with the controls). A load of a third signal
  a bit (48 for 16 LEs) left picorv32 with a chain no LAB took.
  picorv32's `reg_next_pc` loads `current_pc`, its own sum's operand.
- **Constant legs.** sha256's digest registers load `H0` (a constant, or
  `mode` or `!mode`, bit by bit) when not adding: a 0 on SDATA comes
  from the design's `~GND` LE, as M4K tied data does, and a 1 is input C
  left unrouted, which reads 1 (Quartus' `.sdata(1'b1)`; a constant-1 LE
  in the loads' own LAB came out of Quartus' assembly unrouted, so
  i2c-master differed in 11 bits), also where the gates have folded the
  multiplexer to an AND or OR. A synchronous reset to 1 on a
  sum stores its value the same way, for every reset of those 16 LEs
  (the clear takes the load with it and acts first).
- **One sync kind a LAB.** An LE takes the LAB's clear and load
  together, so where 16 LEs would mix a clear with a load, none takes
  the load (serv and i2c-master didn't fit without this).

The database's new pins are sinks nothing routes through. Built on the
new database, `qor3` 64c033b's binary, which doesn't use them, changed
no LE count, moved the routed estimates by the router's search order
(-2.9% to +0.5%) and stayed byte-identical through Quartus on all 33
examples.

### Results

LEs after each step (chipsmith / Quartus at the end):

| step | femtorv32 | picorv32 | sha256 | mult-selftest | sdram-selftest |
|---|---|---|---|---|---|
| `open` 80351e1 | 513 | 971 | 3,674 | 972 | 645 |
| sums by arrival | 513 | 971 | 3,672 | 971 | 645 |
| shared sums | 513 | 916 | 3,672 | 971 | 645 |
| synchronous load, signals | 513 | 875 | 3,538 | 970 | 645 |
| constant legs | 513 | 875 | 3,422 | 970 | 626 |
| Quartus | 473 | 839 | 3,303 | 833 | 567 |

quartus_sta's Fmax of chipsmith's placement and routing (seed 0), MHz,
every RBF byte-identical through Quartus:

| | femtorv32 | picorv32 | sha256 | mult-selftest | sdram-selftest |
|---|---|---|---|---|---|
| `open` 80351e1 (`qor2`) | 70.84 | 129.85 | 51.38 | 63.98 | 180.41 |
| sums by arrival, shared sums (64c033b) | 75.58 | 145.16 | 70.39 | 64.85 | 180.41 |
| synchronous load (new database) | 75.34 | 142.39 | 69.60 | 68.26 | 180.90 |
| Quartus' own | 80.89 | 119.82 | 64.80 | 70.99 | 165.84 |

sha256 crossed Quartus: its five seeds route to 69.9 to 73.2 MHz
(estimate) where they were 50.5 to 52.0, and all five route. On
picorv32 the load moves the estimate up (144.6 -> 151.1 on the same
database) and quartus_sta down (to 142.4): the model's SLOAD setup is a
placeholder (as SDATA's, unmeasured).

### femtorv32 and mult-selftest

**femtorv32.** Quartus' worst path from the register file's M4K is 5.3
ns, into `rs2`: Quartus keeps `rs1` and `rs2` as LE registers after the
block and moves the instruction register into the M4K's address
register. chipsmith merges `rs1` and `rs2` into the M4K's input
register instead, so the block's 3.0 ns clock to output starts its
worst path (M4K, an adder, a second adder, the write-back multiplexers,
the M4K's data input: 13.2 ns, against Quartus' worst of 12.4 ns
elsewhere). Doing what Quartus does changes the read-during-write
behaviour (the M4K's mixed ports read old data where the RTL's read
after the edge sees the new), so it needs either a proof that the CPU
never reads a register it writes in the same cycle or a bypass; not
done. LUT mapping isn't where it is: two LUT levels after the adders.

**mult-selftest.** Its worst path (15.5 ns) runs from a register through
a multiplier (2.7 ns) and three carry chains; Quartus' goes through the
multiplier and two. `p7 = a * b`, 64 bits of a 64x64 product, sums ten
18x18 limb products, all ready at once, so ordering by arrival can't
help; Quartus' adder tree for it is one chain shallower. It reached
68.26 MHz with the load (64.85 before it, on the old database; Quartus
70.99).

### What's left

- **LEs:** femtorv32 +8.5%, picorv32 +4.3%, sha256 +3.6%, mult-selftest
  +16%, sdram-selftest +10% against Quartus. sha256's remaining loads
  are `a_new`-style (a third signal a bit, `H0_reg`, beside `t1 + t2`),
  which no LAB of a chain can take; Quartus loads 448 registers.
- **femtorv32 Fmax:** the M4K's address register (above).
- **mult-selftest Fmax:** a shallower limb-sum tree.
- **picorv32's SLOAD timing:** measure the SLOAD setup arc against
  quartus_sta (bench/timing-calib.py) instead of SDATA's placeholder.

## Round 4 (branch `qor4`)

Each step is its own commit. Every DE2 example's RBF (33; picorv32x24
aside) is byte-identical through Quartus after each one
(`bench/quartus-validate.sh`). quartus_sta's Fmax of chipsmith's own
placement is measured over several placement seeds
(`bench/fmax-seeds.sh`), because one seed's routing moves a design by
up to 10%. `crates/chipsmith/tests/route_quality.rs` now also guards
mult-selftest, and femtorv32's bound is tighter.

### femtorv32: the register file read on the edge before

Quartus reads femtorv32's `rs1 <= registerFile[mem_rdata[19:15]]` (in
WAIT_INSTR) on the edge before: the M4K's address register takes
`mem_rdata`'s next value (`mem_rstrb ? rom : mem_rdata`) on FETCH_INSTR's
edge, and `rs1` and `rs2` stay registers after the block. chipsmith now
does the same (`memory::read_early`, Cyclone II only, before the M4K
mapping). It applies to a read port with its output register merged
whose address comes from registers on its clock. The block's 3.0 ns
clock to output then ends at a register instead of starting the ALU
path.

The early read takes the old data of a write on that edge, where the
RTL's read one edge later sees the new data. So a port only moves where
`hazard::never_after` proves that no write comes on the edge before one
the read register loads on, and that the register doesn't load on the
first edge after power-up. The proof evaluates both signals over every
value of the registers and inputs they depend on (at most 24 bits, 64
assignments at a time), over two frames, with the earlier frame's
registers free. femtorv32's write-back needs EXECUTE or WAIT_ALU_OR_MEM,
and WAIT_INSTR is only entered from FETCH_INSTR. The M4K mapping then
puts `rs1` and `rs2` into the block's output register.

The mapping is proven around (the RTL against the stage before it, the
stage after it against the result) and co-simulated through: femtorv32
compares 270,000 output bits, and `cpu_cosim_catches_a_wrong_register`
still fails a swapped address pin. `m4k_reads_early` co-simulates a
decode-execute register file where the move is safe (both ports moved)
and the same with a write in fetch (neither moved).

### mult-selftest: partial products that don't overlap share an operand

`p7 = a * b` is 32 x 32 -> 64 bits: four 18 x 18 blocks (limbs of 18
and 14 bits), not ten. Round 3 named the wrong count. chipsmith added
the partial products one by one, `((p00 + p01) + p10) + p11`, three
carry chains deep. Quartus' lpm_mult is two deep (`add9`, then `op_1`
on its worst path), because `p00` (bits 0-35) and `p11` (36-63) don't
overlap. chipsmith now packs the products whose bits can't overlap into
one operand and adds the operands as a balanced tree
(`mult::sum_parts`): `(p00 | p11) + p01 + p10`, two adders.

### Timing model: register control arcs and the M4K output register

The register's ENA, SCLR and SLOAD pins counted SDATA's 366 ps arc into
the register, a placeholder. The arcs were measured from quartus_sta's
`CYCLONEII_LCELL_FF` element after `|ena`, `|sclr` and `|sload` on
chipsmith's femtorv32 and picorv32, and every path gave the same value:

| pin | paths | arc, ps | was |
|---|---|---|---|
| ENA | 800 | 660 | 366 |
| SCLR | 451 | 510 | 366 |
| SLOAD | 43 | 659 | 366 |
| SDATA (check) | 418 | 366 | 366 (DMF) |

The routing into those pins already matched (`bench/timing-calib.py`:
quartus_sta's interconnect into them, LAB_CONTROL_MUX included, is 1.02x
the model's).

Every M4K's read data launched 3,326 ps after the clock, which is the
block's unregistered read. Behind its output register (`A_OUTREG`,
`B_OUTREG`), which femtorv32's register file uses since the early read,
quartus_sta gives 396 ps: the register's 209 ps uTco, 88 ps to
`portbdataout`, and the 99 ps by which the block's clock 1 arrives after
an LE register's. Without this, femtorv32's estimate was 9% to 16% below
quartus_sta, and the router chased a path that wasn't critical.

### Results

LEs (seed 0; chipsmith / Quartus at the end):

| step | femtorv32 | picorv32 | sha256 | mult-selftest | sdram-selftest |
|---|---|---|---|---|---|
| `open` ccbd6cc | 513 | 875 | 3,422 | 970 | 626 |
| early register file read | 518 | 875 | 3,422 | 970 | 626 |
| partial products packed | 518 | 875 | 3,422 | 957 | 626 |
| control arcs, M4K output register | 518 | 876 | 3,417 | 956 | 628 |
| Quartus | 473 | 839 | 3,303 | 833 | 567 |

Of the other 28 examples, only m4k-selftest changed, from 356 to 355
LEs. Its read moved early too, and its Fmax stays within its seed spread
(at its 50 MHz clock: 94.3 against 96.0 MHz mean).

quartus_sta's Fmax of chipsmith's placement and routing, MHz, mean over
seeds [min-max], every RBF byte-identical through Quartus:

| step | femtorv32 (0-4) | mult-selftest (0-4) | picorv32 (0-6) | sha256 (0-2) | sdram-selftest (0-6) |
|---|---|---|---|---|---|
| `open` ccbd6cc | 77.7 [75.3-79.6] | 67.2 [65.2-68.9] | - | - | - |
| early register file read | 87.4 [84.1-92.0] | - | - | - | - |
| partial products packed | = | 75.9 [75.0-76.7] | 141.4 [135.8-148.3] | 70.3 [69.6-70.9] | 170.5 [157.8-180.9] |
| control arcs | 86.7 [81.6-91.3] | 74.9 (0-2) | 141.2 [131.8-147.9] | 69.4 [69.1-69.7] | 164.0 [153.7-176.1] |
| M4K output register | 92.2 [87.8-95.7] | = | = | = | = |
| Quartus' own | 80.89 | 70.99 | 119.82 | 64.80 | 165.84 |

`=`: the step doesn't change the design's build (no product wider
than a block; no M4K output register). The first two steps don't change
picorv32, sha256 or sdram-selftest either, so for those designs the
"partial products packed" row is also the `open` baseline. The `open`
mult-selftest row was measured with the early-read binary, which doesn't
change mult-selftest. mult-selftest's control-arc row is seeds 0-2,
which were 76.4 the step before.

femtorv32 is now 14% faster than Quartus' own fit, and mult-selftest 7%
faster. The control arcs make the model more accurate without making
the designs faster. picorv32's estimate error against quartus_sta fell
from +5.8% to +1.9% on average over seven seeds, and its Fmax didn't
move. On the other designs Fmax moved within the seed spread: sha256
-1.2%, mult-selftest -2% and sdram-selftest -3.8%, where the spread is
153.7 to 180.9 MHz. Routing convergence is unchanged: all 33 examples x
5 seeds route on both binaries, in 10.0 iterations on average (10.4
before).

### Rejected or not done

- **LE gaps (task 4).** femtorv32 is +9.5% and mult-selftest +15%.
  Neither had a cheap fix. mult-selftest's extra LEs are normal-mode LUTs
  (679 against Quartus' 554, 487 of them 4-input against 306). Most sit
  in the 64-bit 9-way `want`/`ra`/`rb` selection by `mode`. femtorv32
  has 348 normal LUTs against 302, and its five new LUTs are the
  address holds (`mem_rstrb ? rom : mem_rdata`). Quartus' block has an
  address stall (`addressstall_b`) that would do this for free, but
  chipsmith's chip database doesn't model it.
- **Routing wire delays.** `timing-calib.py` on picorv32, femtorv32 and
  sha256 shows the model overestimating routes through R4s by about 20%:
  `LE_BUFFER R4 LOCAL_INTERCONNECT` is 687 ps in quartus_sta and 878 ps
  in the model. Not changed in this round.

### What's left

- **LEs:** femtorv32 +9.5%, picorv32 +4.4%, sha256 +3.4%, mult-selftest
  +15%, sdram-selftest +11% against Quartus.
- **The M4K address stall,** which would make femtorv32's hold LUTs
  unnecessary (chip database and bitstream support).
- **R4-heavy routes,** about 20% overestimated by the model.
- **sdram-selftest at the default seed:** 154.2 MHz against Quartus'
  165.8, but 153.7 to 176.1 over seeds 0-6. Its worst path at seed 0 runs
  from a register through five LUTs into the DRAM_CAS_N I/O-cell
  register on the 100 MHz PLL clock. chipsmith's estimate for that clock
  is 174.9 MHz (5.72 ns), 13% above quartus_sta's 154.2. The error is in
  the model's path into the I/O register, not in the control arcs, which
  that path doesn't use.

## Method

**Machine.** Intel i7-4770K (4 cores, 8 threads, 3.5 GHz), 31 GB, no swap,
NVIDIA GTX 1660, NixOS. The machine is shared: other agents ran their own
builds during the runs. Those included Quartus fits and programming-file
runs, another device's database builds and chipsmith's own proof suite.
The `load` column is the 1-minute load average, sampled every 5 s during
each run and averaged. In this run it ranged from 1.3 to 8.2, with a
median of 1.8 (Load, above). The end of this branch's own test suite and
Quartus validation runs overlapped the first rep. The fourth run's
median was 14, and both tools' wall times are lower on the quieter
machine (see Caveats).

**Tools.**

| tool | what |
|---|---|
| `q` | Quartus II 13.0sp1 Web Edition (13.0.1 build 232): `quartus_map`, `quartus_fit` (Auto Fit), `quartus_asm` (SOF) |
| `cs` | `chipsmith build`, branch `qor4` at de70915 (`open` ccbd6cc and the commits of Round 4), on CPU threads, chip database cyclone2-re c2baa4e (the fourth run's) |

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


## Fourth run (2026-11-03, `qor3` f6d26ff)

### Full builds

Medians over three interleaved reps with [min-max], from a fresh copy of
each example. "Speed-up" is Quartus' median divided by chipsmith's. LEs
are each tool's own count after fitting.

Wall time, s:

| design | LEs Quartus / chipsmith | q wall s | cs wall s | speed-up cs |
|---|---|---|---|---|
| blinky-verilog | 26 / 26 | 128.0 [86.2-137.4] | 14.1 [5.7-15.2] | 9.1x |
| crc32 | 105 / 91 | 168.4 [154.8-183.6] | 16.5 [9.9-17.9] | 10.2x |
| femtorv32 | 473 / 513 | 151.5 [126.3-152.3] | 29.3 [26.3-39.0] | 5.2x |
| picorv32 | 839 / 875 | 151.3 [145.3-203.1] | 50.2 [43.1-58.0] | 3.0x |
| mult-selftest | 833 / 970 | 153.8 [153.2-205.9] | 56.9 [52.6-79.7] | 2.7x |
| sdram-selftest | 567 / 626 | 128.0 [127.9-133.7] | 32.9 [31.6-36.0] | 3.9x |
| sha256 | 3303 / 3422 | 327.8 [293.1-337.7] | 199.5 [189.3-221.2] | 1.6x |
| picorv32x24 | 20240 / 21241 | 1318.2 [1288.2-1752.5] | 690.9 [526.9-707.3] | 1.9x |

CPU time (user + sys of the run's cgroup), s:

| design | LEs Quartus / chipsmith | q CPU s | cs CPU s | speed-up cs |
|---|---|---|---|---|
| blinky-verilog | 26 / 26 | 85.2 [80.0-85.2] | 10.0 [8.8-10.2] | 8.5x |
| crc32 | 105 / 91 | 121.3 [118.8-122.1] | 15.4 [14.8-15.4] | 7.9x |
| femtorv32 | 473 / 513 | 109.2 [108.2-112.0] | 33.4 [33.2-33.5] | 3.3x |
| picorv32 | 839 / 875 | 124.4 [122.3-125.1] | 42.6 [42.0-42.7] | 2.9x |
| mult-selftest | 833 / 970 | 127.2 [125.6-130.2] | 47.3 [46.8-49.1] | 2.7x |
| sdram-selftest | 567 / 626 | 106.2 [105.6-107.1] | 35.0 [34.9-35.5] | 3.0x |
| sha256 | 3303 / 3422 | 237.6 [237.5-243.2] | 163.4 [155.6-163.6] | 1.5x |
| picorv32x24 | 20240 / 21241 | 1113.0 [1109.1-1149.3] | 604.6 [569.8-609.6] | 1.8x |

### Quality

CLOCK_50's (sdram-selftest: the 100 MHz PLL clock's) Fmax, MHz.
"Own" is each tool's own analysis: quartus_sta (slow model) for Quartus,
the routed estimate for chipsmith. "quartus_sta" is chipsmith's
placement and routing assembled by Quartus and timed by the same
quartus_sta (rep 1); every one of those assemblies came out byte-identical
to chipsmith's RBF. chipsmith's placement is deterministic, so its values
repeat across reps.

| design | q own | cs own | cs quartus_sta |
|---|---|---|---|
| blinky-verilog | 285.31 | 294.1 | 298.15 |
| crc32 | 267.95 | 302.1 | 293.0 |
| femtorv32 | 80.89 | 77.0 | 75.34 |
| picorv32 | 119.82 | 151.1 | 142.39 |
| mult-selftest | 70.99 | 68.0 | 68.26 |
| sdram-selftest | 165.84 | 189.8 | 180.9 |
| sha256 | 64.8 | 72.7 | 69.6 |
| picorv32x24 | 105.27 | 109.6 | - |

picorv32x24's chipsmith estimate fell from 113.1 to 109.6 MHz since the
third run (8.84 -> 9.12 ns), at 2,286 fewer LEs; it isn't cross-checked
(Method).

### Load

The 1-minute load average during each run (8 hardware threads):

| tool | runs | load, median [min-max] |
|---|---|---|
| q | 24 | 13.7 [8.9-35.7] |
| cs | 24 | 14.1 [9.3-37.7] |

## Third run (2026-11-02/03, `qor2` 16c6b23)

### Full builds

Medians over three interleaved reps with [min-max], from a fresh copy of
each example. "Speed-up" is Quartus' median divided by chipsmith's. LEs
are each tool's own count after fitting.

Wall time, s:

| design | LEs Quartus / chipsmith | q wall s | cs wall s | speed-up cs |
|---|---|---|---|---|
| blinky-verilog | 26 / 26 | 64.9 [55.0-76.0] | 6.9 [6.0-7.2] | 9.4x |
| crc32 | 105 / 91 | 94.8 [88.7-104.1] | 5.0 [4.1-6.2] | 19.0x |
| femtorv32 | 473 / 513 | 72.2 [68.5-99.9] | 16.5 [10.1-17.8] | 4.4x |
| picorv32 | 839 / 971 | 107.3 [77.7-109.6] | 19.9 [16.1-22.9] | 5.4x |
| mult-selftest | 833 / 972 | 111.4 [92.8-122.5] | 38.6 [24.7-40.5] | 2.9x |
| sdram-selftest | 567 / 645 | 78.9 [66.0-98.5] | 13.2 [9.7-21.6] | 6.0x |
| sha256 | 3303 / 3674 | 165.5 [151.1-207.2] | 108.2 [106.2-142.9] | 1.5x |
| picorv32x24 | 20240 / 23527 | 1104.8 [949.6-1159.8] | 555.9 [486.7-662.1] | 2.0x |

CPU time (user + sys of the run's cgroup), s:

| design | LEs Quartus / chipsmith | q CPU s | cs CPU s | speed-up cs |
|---|---|---|---|---|
| blinky-verilog | 26 / 26 | 62.1 [54.2-65.3] | 9.9 [9.4-10.0] | 6.3x |
| crc32 | 105 / 91 | 92.2 [86.7-98.7] | 11.1 [9.9-12.4] | 8.3x |
| femtorv32 | 473 / 513 | 71.0 [67.6-94.0] | 29.1 [25.6-29.7] | 2.4x |
| picorv32 | 839 / 971 | 103.2 [76.8-107.5] | 34.6 [33.7-34.8] | 3.0x |
| mult-selftest | 833 / 972 | 105.5 [90.2-116.8] | 47.5 [45.7-53.3] | 2.2x |
| sdram-selftest | 567 / 645 | 76.4 [65.2-93.8] | 32.4 [30.6-36.4] | 2.4x |
| sha256 | 3303 / 3674 | 159.5 [148.9-199.2] | 127.0 [125.9-148.3] | 1.3x |
| picorv32x24 | 20240 / 23527 | 1023.3 [902.3-1037.7] | 585.8 [585.1-629.4] | 1.7x |

### Quality

CLOCK_50's (sdram-selftest: the 100 MHz PLL clock's) Fmax, MHz.
"Own" is each tool's own analysis: quartus_sta (slow model) for Quartus,
the routed estimate for chipsmith. "quartus_sta" is chipsmith's
placement and routing assembled by Quartus and timed by the same
quartus_sta (rep 1); every one of those assemblies came out byte-identical
to chipsmith's RBF. chipsmith's placement is deterministic, so its values
repeat across reps.

| design | q own | cs own | cs quartus_sta |
|---|---|---|---|
| blinky-verilog | 285.31 | 294.1 | 298.15 |
| crc32 | 267.95 | 302.1 | 293.0 |
| femtorv32 | 80.89 | 72.7 | 70.84 |
| picorv32 | 119.82 | 130.2 | 129.85 |
| mult-selftest | 70.99 | 65.1 | 63.98 |
| sdram-selftest | 165.84 | 189.8 | 180.41 |
| sha256 | 64.8 | 51.9 | 51.38 |
| picorv32x24 | 105.27 | 113.1 | - |

sha256's RBF differed from Quartus' assembly in 260 bits, one a LAB: the
LAB's clear-line inversion (`LAB_CLR_INVERT_PAIR`) for its active-low
reset. chipsmith was right: the cram that exported it to Quartus was the
nix build of cyclone2-re e6aa597, from before de2-arstn taught the export
the inversion (de3386a kept the previous outputHash for its chipsmith
dependency, so `nix build .#cram` failed on master and `result/` still
held the old build). With a current cram sha256 is byte-identical
(`qor2`), and cram now refuses to run from a checkout whose sources it
wasn't built from (cyclone2-re b5eb9e7).

### Load

The 1-minute load average during each run (8 hardware threads):

| tool | runs | load, median [min-max] |
|---|---|---|
| q | 24 | 8.6 [4.0-17.3] |
| cs | 24 | 9.3 [4.1-18.9] |

## Second run (2026-11-02, `qor` 7b03ae2)

At load 10 to 49 (median 17), `cs` the `qor` branch. LEs as in this run.

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

Fmax, MHz:

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

Full builds (`bench/vs-quartus.sh table bench-out/vs3`):

| design | tool | runs | LEs | wall s | CPU s | CPU/wall | load | own critical ns | quartus_sta Fmax MHz | stages, median wall/CPU s |
|---|---|---|---|---|---|---|---|---|---|---|
| blinky-verilog | q | 3 | 26 | 64.9 [55.0-76.0] | 62.1 [54.2-65.3] | 0.96 | 10.3 [4.0-11.9] | 3.50 [3.50-3.50] | 285.3 [285.3-285.3] | map 15.2/13.3, fit 30.5/29.9, asm 19.2/18.8, sta 9.9/9.6 |
| blinky-verilog | cs | 3 | 26 | 6.9 [6.0-7.2] | 9.9 [9.4-10.0] | 1.43 | 11.8 [5.3-11.9] | 3.40 [3.40-3.40] | 298.1 | elaborate 0.2/0.3, synth 0.1/0.1, pack 0.1/0.1, graph 0.8/1.7, place 1.0/2.7, route 0.6/1.0, timing 0.1/0.1, bitstream 3.9/3.6 |
| crc32 | q | 3 | 105 | 94.8 [88.7-104.1] | 92.2 [86.7-98.7] | 0.97 | 8.3 [6.6-11.4] | 3.73 [3.73-3.73] | 267.9 [267.9-267.9] | map 42.5/41.3, fit 39.1/38.3, asm 15.6/15.4, sta 6.7/6.7 |
| crc32 | cs | 3 | 91 | 5.0 [4.1-6.2] | 11.1 [9.9-12.4] | 2.22 | 8.4 [6.3-9.9] | 3.31 [3.31-3.31] | 293.0 | elaborate 1.2/3.5, synth 0.1/0.3, pack 0.1/0.1, graph 0.0/0.1, place 0.4/1.8, route 2.1/4.1, timing 0.0/0.1, bitstream 0.5/0.6 |
| femtorv32 | q | 3 | 473 | 72.2 [68.5-99.9] | 71.0 [67.6-94.0] | 0.98 | 6.3 [5.8-10.0] | 12.36 [12.36-12.36] | 80.9 [80.9-80.9] | map 18.5/18.3, fit 39.2/38.4, asm 14.6/14.4, sta 7.5/7.5 |
| femtorv32 | cs | 3 | 513 | 16.5 [10.1-17.8] | 29.1 [25.6-29.7] | 1.76 | 8.0 [5.1-8.4] | 13.76 [13.76-13.76] | 70.8 | elaborate 0.1/0.4, synth 0.7/2.3, pack 0.2/0.5, graph 0.1/0.1, place 4.5/15.2, route 8.4/9.5, timing 0.1/0.1, bitstream 0.8/0.8 |
| picorv32 | q | 3 | 839 | 107.3 [77.7-109.6] | 103.2 [76.8-107.5] | 0.98 | 8.2 [4.4-8.8] | 8.35 [8.35-8.35] | 119.8 [119.8-119.8] | map 32.2/31.5, fit 51.5/50.1, asm 18.6/18.0, sta 9.3/9.1 |
| picorv32 | cs | 3 | 971 | 19.9 [16.1-22.9] | 34.6 [33.7-34.8] | 1.74 | 9.2 [4.1-10.3] | 7.68 [7.68-7.68] | 129.8 | elaborate 0.2/0.6, synth 1.2/3.2, pack 0.2/0.5, graph 0.0/0.1, place 5.3/17.8, route 11.8/11.7, timing 0.1/0.1, bitstream 0.6/0.6 |
| mult-selftest | q | 3 | 833 | 111.4 [92.8-122.5] | 105.5 [90.2-116.8] | 0.95 | 8.4 [8.4-14.5] | 14.09 [14.09-14.09] | 71.0 [71.0-71.0] | map 35.3/32.8, fit 60.7/58.2, asm 17.1/16.8, sta 7.9/7.8 |
| mult-selftest | cs | 3 | 972 | 38.6 [24.7-40.5] | 47.5 [45.7-53.3] | 1.32 | 9.5 [7.3-12.1] | 15.35 [15.35-15.35] | 64.0 | elaborate 0.3/0.7, synth 1.9/3.1, pack 0.4/0.6, graph 0.0/0.1, place 7.2/22.4, route 21.2/20.9, timing 0.0/0.0, bitstream 0.6/0.7 |
| sdram-selftest | q | 3 | 567 | 78.9 [66.0-98.5] | 76.4 [65.2-93.8] | 0.97 | 10.2 [5.0-14.1] | 6.03 [6.03-6.03] | 165.8 [165.8-165.8] | map 25.5/23.8, fit 38.4/37.8, asm 15.1/14.9, sta 7.5/7.4 |
| sdram-selftest | cs | 3 | 645 | 13.2 [9.7-21.6] | 32.4 [30.6-36.4] | 2.45 | 10.8 [4.4-11.7] | 5.27 [5.27-5.27] | 180.4 | elaborate 0.1/0.4, synth 0.5/1.5, pack 0.3/1.0, graph 0.1/0.1, place 6.1/22.6, route 5.4/5.9, timing 0.0/0.1, bitstream 0.8/0.8 |
| sha256 | q | 3 | 3303 | 165.5 [151.1-207.2] | 159.5 [148.9-199.2] | 0.96 | 6.1 [5.6-11.5] | 15.43 [15.43-15.43] | 64.8 [64.8-64.8] | map 55.8/54.4, fit 94.5/90.1, asm 15.2/15.0, sta 8.7/8.6 |
| sha256 | cs | 3 | 3674 | 108.2 [106.2-142.9] | 127.0 [125.9-148.3] | 1.17 | 7.6 [7.4-12.4] | 19.27 [19.27-19.27] | 51.4 | elaborate 0.2/0.6, synth 5.9/11.8, pack 1.1/1.1, graph 0.0/0.0, place 5.5/21.0, route 96.2/93.2, timing 0.0/0.1, bitstream 0.8/0.9 |
| picorv32x24 | q | 3 | 20240 | 1104.8 [949.6-1159.8] | 1023.3 [902.3-1037.7] | 0.93 | 15.1 [10.5-17.3] | 9.50 [9.50-9.50] | 105.3 [105.3-105.3] | map 320.4/300.1, fit 751.1/693.9, asm 33.4/29.3, sta 35.3/29.2 |
| picorv32x24 | cs | 3 | 23527 | 555.9 [486.7-662.1] | 585.8 [585.1-629.4] | 1.05 | 17.8 [15.4-18.9] | 8.84 [8.84-8.84] | - | elaborate 0.8/0.9, synth 44.5/44.7, pack 10.6/9.4, graph 0.1/0.1, place 133.2/179.5, route 412.7/381.9, timing 0.1/0.1, bitstream 6.0/5.2 |
