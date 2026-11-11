# Cyclone II cell delays from silicon

Where the cell delays in `crates/chipsmith-route/src/timing.rs` come from
(measured 2026-11-10, branch `own-timing`). Until then they were the
values cyclone2-re decoded from Quartus' DMF database
(`ddb_magellan4-6.dmf.ddb`). Now chipsmith derives them itself:

- **Silicon.** Ring oscillators that chipsmith places and routes run on
  the DE2's EP2C35F672C6 and are counted over JTAG. This gives each LUT
  input, carry-chain element and multiplier its delay on this chip at
  room temperature.
- **Slow corner.** One scale per class of cell (LUT, carry chain,
  multiplier) takes those to speed grade 6's slow corner. The scales are
  fitted to quartus_sta's path totals on chipsmith's own placements, in
  aggregate (below).
- **Fitted.** Ring oscillators can't isolate register clock-to-output and
  setup, M4K or I/O-cell registers. Those constants come from the same
  aggregate fit.

Routing delays are unchanged: quartus_sta's median per element type
(cyclone2-re docs/timing-model.md). See docs/legal.md for why this matters.

## The bench

`bench/ring/` (`specs.py` the structures, `ring.py` the design,
`measure.sh` the board runs, `fit.py`, `derive.py`, `path_fit.py`,
`emit.py`):

1. `specs.py` writes up to eight rings per bitstream. `ring.py` turns
   them into a chipsmith project: LCELL_COMBs instantiated directly, a
   CLOCK_50 sequencer that runs one ring at a time for 16,776,960 cycles
   (0.34 s), a counter on each ring, and a USER1 register with each
   ring's count in its last window.
2. `ring.py place` moves the ring cells onto the LEs the spec names, after
   the placer has put everything else. Whole LABs are evicted, carry
   chains together. It marks the LUTs `CHIPSMITH_FIXED_INPUTS`, so the
   router keeps each one on the physical input it was wired to.
3. `run.sh` routes the design, loads it, and reads it with `de2_ring`.
   `measure.sh` does that for every spec, twice, with a fresh load each
   time.
4. `fit.py` solves per-element delays by least squares. A ring's half
   period is the sum over its stages of the LUT arc from the stage's
   input, plus the wires the router took into it
   (CHIPSMITH_DUMP_CONNECTIONS). The fit uses 816 reads of 204 ring
   loads.

The rings:

- **LUT rings.** 15-stage rings on each physical input, plus input D at
  7, 9 and 13 stages, so the tap's constant falls out.
- **Wire rings.** Stages alternate between two LABs: neighbours, four
  columns apart, one row apart, four rows apart.
- **Carry rings.** Chains of 16 to 40 LEs, and pairs of 8-LE chains that
  differ only in whether a chain crosses the LAB's half-way buffer or
  the LAB boundary.
- **Multiplier rings.** 18 × 18 product bits 35, 26, 19 and 0, and 9 × 9
  bits 17, 13, 11 and 10.

Each structure is built in up to four regions of the die.

### What didn't work, and what replaced it

| Hypothesis | Result |
|---|---|
| Every stage gated by the enable | Enabling a ring started an edge in every stage. One ring locked into three pulses at once (168 MHz instead of 56). Fixed: only stage 0 takes the enable; the others take a register that is 1 from the first clock. |
| One-input LUT stages | The packer folds a one-input LUT into its loads, as quartus_map does, and the ring disappears. Every stage is now a two-input NAND. |
| Short rings (carry chains of 4 to 16 LEs) | Above about 340 MHz the counter's clock path counts edges twice, depending on the load. A 16-LE ring read 351 MHz on some loads and 703 MHz on others. `fit.py` drops reads above 330 MHz. |
| Single-chain carry rings only | The link costs were collinear with head + tail (±19 and ±30 ps). The two-chain differential rings bring them to ±9 ps. |
| quartus_sta on the rings themselves | TimeQuest drops every arc of a combinational loop. `sta.sh` instead times an *open* build: the same placement with stage 0 fed by a register. |

### Repeatability

- **Reads of one load:** median spread 31 ppm, at most 104 ppm.
- **Loads of one bitstream:** median 29 ppm, at most 120 ppm.
- **Placements:** each LUT and carry structure's mean residual, over the
  regions it was built in, is at most 0.6% (LUT) and 1.5% (carry) of its
  period.
- **Wire rings:** they vary 1 to 4% between placements. That's per-wire
  variation (wire length, fanout) that a per-type delay can't capture,
  so only the routing derate check (below) uses them.

## The slow corner

This silicon is a typical part at room temperature; the model is the
slow corner. Four ways of scaling, compared by how well the model then
matches quartus_sta's path totals on 14 chipsmith-placed designs (residual
rms over 3,895 of their worst paths, `path_fit.py`; Quartus' DMF values
give 143 ps):

| Derate | Ratio | Path residual |
|---|---|---|
| Routing only: R4, C4 and LAB-line ratios of the open builds to silicon | 1.41 to 1.69 | not used: the routing model stays quartus_sta's |
| One factor: the open builds' segments against silicon | 1.65 to 1.74 (median of 88 segments 1.74) | 236 ps (K 1.70) |
| The largest LUT-input ratio on the open builds | 1.87 | 365 ps |
| One factor per class, fitted to the path totals | LUT 1.946, carry 1.516, multiplier 1.526 (±0.008) | **118 ps** |

The per-class fit is used. A single factor took carry chains and
multipliers to about 1.9 times this chip's delay, but quartus_sta's paths
want about 1.5. With every class at the LUTs' factor, a 32-bit carry chain
came out 0.5 ns slower than quartus_sta and a multiplier 1 ns slower. A
class's factor below 1 would be held at 1, since the slow corner can't be
faster than this chip at room temperature; none was.

## The values

Slow corner, in ps. "Old" is the DMF value (LUT arcs with timing.rs's
former 32 ps `LUT_INPUT_IC`). Uncertainties are one standard error:
silicon's from the ring fit times its class factor, the fitted constants'
from the path fit.

| Value | Old (DMF) | New | Source | ± |
|---|---|---|---|---|
| LUT input A to COMBOUT | 470 | 517 | silicon × 1.946 − LOCAL_LINE | 5 |
| LUT input B | 452 | 497 | silicon | 5 |
| LUT input C | 307 | 343 | silicon | 6 |
| LUT input D | 182 | 233 | silicon | 5 |
| Carry head, A to COUT | 446 | 579 | silicon (head + tail; tail as input C) | 25 |
| Carry head, B to COUT | 425 | 548 | silicon | 23 |
| CIN to COMBOUT | 410 | 343 | taken as input C's arc | 6 |
| CIN to COUT | 71 | 72 | silicon × 1.516 | 2 |
| + LAB half-way buffer (LE 7 to 8) | 88 | 78 | silicon × 1.516 | 14 |
| + next LAB (LE 15 to 0) | 75 | 66 | silicon × 1.516 | 13 |
| 18 × 18 multiplier, through | 2,903 | 3,024 | silicon × 1.526 (bit 35) | 21 |
| 9 × 9 multiplier, through | 2,903 | 2,018 | silicon × 1.526 (bit 10) | 11 |
| LE register TCO + TSU (DATAIN) | 386 | 287 | fitted (TSU held at 0) | 8 |
| TSU, SDATA | 330 | 434 | fitted | 6 |
| TSU, ENA | 624 | 471 | fitted | 8 |
| TSU, SCLR | 474 | 308 | fitted | 12 |
| TSU, SLOAD | 623 | 618 | fitted | 7 |
| M4K clock to data out | 3,326 | 3,085 | fitted | 11 |
| M4K, through its output register | 396 | 184 | fitted | 29 |
| M4K input register setup | −17 | 187 | fitted | 8 |
| I/O-cell register TCO | 533 | 391 | fitted | 16 |
| I/O-cell register TSU | 201 | 424 | fitted | 18 |

Silicon at room temperature, before scaling (`bench/ring/silicon.json`):

- **LUT ring stages** (arc plus a LOCAL_LINE hop): A 383, B 373, C 294,
  D 237 (±3).
- **Carry:** head + tail + hop 591 (A) and 576 (B) (±12). CIN to COUT 47.3
  (±1.0). The half-way buffer adds 51 (±9), the next LAB 43 (±9).
- **18 × 18 multiplier:** bit 35 1,982, bit 19 1,538, bit 0 836.
- **9 × 9 multiplier:** bit 10 1,323, bit 17 1,104.

The fits are too coarse to tell values apart beyond these:

- **Register TCO vs TSU.** Every register-to-register path has one start
  and one end, so only their sum is identifiable. DATAIN's setup is held
  at 0.
- **Multiplier registers.** No chipsmith design has enough paths through
  a multiplier's own registers, so they stay the LE register's.
- **Carry head vs tail.** They always come together on a path. The split
  takes CIN to COMBOUT as input C's arc, since CIN enters the LUT as C.

Each fitted constant also takes up what the model's routing misses on such
paths, against chipsmith's own wire medians, so it isn't a cell delay in
Quartus' sense. Reproduce with:

```
bench/ring/measure.sh /tmp/ring/m1          # on the DE2, about 30 min
bench/ring/sta.sh <spec> <dir>              # open builds, per spec
bench/ring/fit.py /tmp/ring/m1 <sta dirs>
bench/ring/derive.py /tmp/ring/m1 bench/ring/silicon.json
CALIB=1 bench/qor.sh <dir> <designs>; bench/ring/extra-paths.sh <dir>/*
bench/ring/path_fit.py bench/ring/silicon.json <dir>/*
bench/ring/emit.py bench/ring/silicon.json bench/ring/constants.json 0
```

## Against quartus_sta

`bench/de2-timing.sh` on 18 DE2 examples, chipsmith's estimate of its
critical path against quartus_sta's on the same placement and routing
(negative: chipsmith optimistic). Placement and routing change with the
model, so the "after" designs are placed anew.

| Design | Before | After |
|---|---|---|
| blinky-verilog | +1.4% | +0.2% |
| crc32 | +2.1% | −2.7% |
| de2-io | −5.2% | −3.0% |
| de2-ioregs | −0.0% | +1.2% |
| femtorv32 | −4.4% | +0.7% |
| fir | −0.7% | +0.2% |
| i2c-master | −0.1% | +0.9% |
| m4k-selftest | +0.4% | +0.1% |
| m4k-tdp | −6.2% | −3.1% |
| mult-selftest | −1.1% | −1.5% |
| picorv32 | −3.9% | −3.2% |
| ram-leds | +4.8% | +1.4% |
| sdram-selftest | −7.2% | +2.3% |
| serv | +0.1% | +0.5% |
| sha256 | −1.6% | −1.2% |
| sram-selftest | +2.1% | +0.3% |
| uart | +2.9% | −1.1% |
| vga-pattern | −2.1% | +4.8% |

The worst optimism went from −7.2% to −3.2%, and the spread from 12.0
to 8.0 points. crc32, uart and mult-selftest are more optimistic than
before, within the old range. A guardband (`emit.py`'s third argument, ps
on every path start) would buy that back at about 180 ps per path, which
would make every larger design pessimistic; it is 0.

`bench/qor.sh` on the five designs the README quotes, chipsmith's Fmax
estimate against quartus_sta's (positive: chipsmith optimistic):

| Design | Before | After |
|---|---|---|
| femtorv32 | +4.6% | −0.7% |
| picorv32 | +4.1% | +3.2% |
| sha256 | +1.6% | +1.3% |
| mult-selftest | +1.0% | +1.6% |
| sdram-selftest | +10.8% | +4.7% |

The model also steers placement and routing, so it moves what Quartus
times. Over placement seeds 1 to 3 (`bench/fmax-seeds.sh`, quartus_sta's
Fmax in MHz):

| Design | Before, seeds 1/2/3 | After, seeds 1/2/3 | Mean |
|---|---|---|---|
| sha256 | 70.6 / 70.5 / 70.2 | 69.0 / 68.0 / 68.4 | −2.8% |
| femtorv32 | 107.8 / 103.3 / 100.5 | 103.0 / 105.2 / 100.9 | −0.9%, within the seeds' spread |

On those seeds the estimate is within 1% of quartus_sta after, and was
0.5 to 3.7% optimistic before.

Three seeds understated sha256's spread. Seeds 4 to 6, the same way
(2026-11-11):

| Design | Before, seeds 4/5/6 | After, seeds 4/5/6 |
|---|---|---|
| sha256 | 68.6 / 70.8 / 67.5 | 67.2 / 68.6 / 68.9 |

Over all six seeds sha256 averages 69.7 MHz before and 68.3 after: −2.0%,
about two standard errors (the old model's seeds spread 67.5 to 70.8, the
new one's 67.2 to 69.0). Before, the estimate ran 2.4 to 6.0% optimistic
on seeds 4 to 6; after, 0.9% pessimistic to 0.9% optimistic. The
regression is smaller than the first three seeds showed but likely real.
The merge didn't recover it, and `route_quality`'s sha256 bound moved
from 15.0 to 15.5 ns for it.

**Recovered (2026-11-11): the placer keeps 410 ps for CIN to COMBOUT.**
With silicon's 343 ps, carry chains look cheaper and the placer stops
pulling them together. Same six seeds, quartus_sta:

| CIN to COMBOUT | seeds 1/2/3/4/5/6 | mean | estimate against quartus_sta |
|---|---|---|---|
| 343 ps everywhere (main) | 69.0 / 68.0 / 68.4 / 67.2 / 68.6 / 68.9 | 68.3 | −0.9..+0.9% |
| 410 ps everywhere | 68.3 / 71.1 / 70.9 / 71.8 / 69.5 / 70.4 | 70.3 | −3.5..−0.3% |
| 410 ps in the placer only | 67.4 / 70.0 / 70.0 / 70.9 / 70.9 / 69.2 | 69.7 | −0.7..+1.8% |

The placer-only variant is what ships (`timing::Graph::placement`,
`PLACER_CIN_COMBOUT`): +2.0% over 343 ps, about 2.3 standard errors, back to
the mean before the merge, with STA and routing still on silicon's value.
Its 0.6 MHz gap to 410 ps everywhere is within the seeds' spread, and that
variant's estimate turns pessimistic. All 12 new bitstreams were byte-identical
through Quartus. `route_quality`'s sha256 bound is back at 15.0 ns (the
estimate on its seed 14.41). The other two leads (the LUT class factor in
placement, the router's choice of LUT input) aren't measured.

The other carry-chain designs, seeds 1/2/3, 343 ps against placer-only
410 ps: femtorv32 103.0 / 105.2 / 100.9 against 106.0 / 100.3 / 105.9 MHz
(+1.0% mean), mult-selftest 77.0 / 76.3 / 76.8 against 77.8 / 74.0 / 77.1
(−0.5%), picorv32 145.0 / 146.9 / 130.6 against 142.6 / 144.4 / 155.1
(+4.7% mean, median −0.4%). None moves beyond its seeds' spread.

## On the board

`bench/de2-at-fmax.sh` runs a self-test from a PLL. On 2026-11-10:

- **mult-selftest:** estimate 74.7 MHz. At 75 MHz, about 65,300 passes a
  second and 0 errors. At 80 MHz, 7% above the estimate, also 0 errors.
- **m4k-selftest:** estimate 89.3 MHz. At 93.75 MHz, 11,478 passes a
  second and 0 errors.

This chip at room temperature is about 1.5 to 1.9 times faster than the
slow corner, so these runs show the estimate isn't optimistic for this
board. They don't show it holds at the slow corner.
