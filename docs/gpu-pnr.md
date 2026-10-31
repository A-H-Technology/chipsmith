# gpu-pnr: place and route rebuilt for parallel hardware

Branch `gpu-pnr`, from `open` 40a29cb. The goal was a place-and-route
flow whose heavy stages run as wide parallel kernels, so that a GPU
(CUDA, or HIP on AMD) makes it much faster, with the same kernels on CPU
threads (rayon) as the fallback. docs/fast-pnr-status.md, "GPU", had
measured that the flow then gave a GPU nothing: its analytic solve has
~2,000 rows and is 0.4% of placement, and its router is sequential A*.
This note records the baseline, what replaced those stages, the
measurements behind each choice, and the verdict.

In short: global placement now works on every LE (an electrostatic
placer with one density field per resource) and runs on one thread, on
rayon's threads or on a GPU. Its start is good enough that the
refining anneal can run cold, and routing converges in fewer
iterations. sha256 builds 2.3 times faster and picorv32x24 2.6 times,
with routed critical paths as good or better (picorv32x24 9.43 ns
against 10.65); the small designs build in about the same time. All of
that is the algorithm. The GPU makes global placement about twice as
fast as the CPU's threads (3 to 5 times per iteration, but it compiles
its kernels on every run), and global placement is now 2 to 5% of a
large build, so the GPU saves seconds.
A GPU router was prototyped and rejected: its searches lose to A* on 8
CPU threads.

## Baseline

`open` 40a29cb, `bench/speed.sh` with `CHIPSMITH_PLACE_TRACE=1
CHIPSMITH_ROUTE_TRACE=1`, one run, seed 0. The machine was shared:
load average 13 during sha256, 14 to 22 during picorv32x24 (Quartus and
yosys of other agents), so the times are high; Results has back-to-back
runs.

| design | LEs | place s | of it: cluster + analytic / anneal | route s | iterations | of it: iteration 1 | routed critical ns |
|---|---|---|---|---|---|---|---|
| femtorv32 | 788 | 7.8 | 0.1 / 7.6 | 4.3 | 5 | | 13.43 |
| mult-selftest | 1,008 | 13.7 | 0.2 / 13.3 | 12.1 | 7 | | 15.57 |
| picorv32 | 1,171 | 16.3 | 0.3 / 15.8 | 8.9 | 7 | | 8.19 |
| sha256 | 6,046 | 93.4 | 7.9 / 85.1 | 65.2 | 26 | 35.0 | 20.63 |
| picorv32x24 | 28,449 | 1,375.1 | 26.0 / 1,345.5 | 238.6 | 36 | 111.7 | 10.65 |

Where the time went:

- Placement was the larger stage on every design, and the refining
  anneal was 91 to 98% of it. It runs 8 seeds of N^(4/3) moves per
  temperature for 50 to 80 temperatures: 58 million moves on sha256,
  274 million on picorv32x24. The anneal did real work, not polish: it
  cut sha256's wirelength from 54,281 to 37,454 and picorv32x24's from
  146,170 to 123,209, and picorv32x24's critical path estimate from
  12.47 to 9.98 ns. The analytic start was coarse. LEs were clustered
  into LABs before anything was placed, and on sha256 1,543 of the 1,825
  clusters were a single register (1,804 distinct asynchronous clears,
  one per LAB). The analytic placer saw 1,543 one-LE "LABs" whose LUTs
  the anneal then had to fetch.
- Routing: the first PathFinder iteration was half of it (sha256 35 of
  65 s, picorv32x24 112 of 239 s), and nets of 32 or more sinks were
  two thirds of that iteration.

So the placer went first. The win had to come from replacing most of
the anneal's work, which a faster solve alone can't do.

## Placement

### Global placement: every LE, electrostatic (`chipsmith-place::electro`)

Every LE that isn't in a carry chain is its own object. A chain is one
object as tall as its LABs, and M4Ks and multipliers are objects of
their own kinds. That makes 5,400 objects on sha256 and 23,400 on
picorv32x24. The placer minimises

    W(x, y) + sum over resources r of lambda_r * N_r(x, y)

with Nesterov's method and ePlace's Lipschitz step (Lu et al., "ePlace",
TODAES 2015; the FPGA resources follow elfPlace, Li et al., ICCAD 2019):

- W is the weighted-average smooth wirelength of every net, each net
  weighted by the device's timing weight (1 + 8 crit^4, the anneal's).
- N_r is the electrostatic energy of resource r. Objects are charges
  spread over a footprint one bin wide and as tall as they are. Fillers
  (objects on no net) take each resource's free capacity at its target
  density, and bins without the resource hold a fixed charge. The
  potential solves Poisson's equation by a cosine transform over the
  bin grid (one bin per LAB, 67 x 37 on the EP2C35, as dense matrix
  products), and the force on an object is its charge times the field.
- The resources tell the density model what a LAB holds:
  - LE slots: an LE takes 1/16 of a LAB.
  - Register control sets: a LAB takes one set (clock edge, enable,
    clears; `lab_check`), and a register whose set has k members takes
    ceil(k / 16) / k of a LAB's. On sha256 this field says that 1,804
    registers need 1,804 LABs while their 4,000 LUTs can share them.
  - One field per block kind, capacity 1 at its sites.
- Timing: every 25 iterations once the overflow is below 0.5, the
  device reweights the nets from a timing analysis on the continuous
  positions (the anneal's delay model with distances in bins).
- It stops when every resource's overflow is below 0.1. A resource
  whose demand comes in whole bins (blocks, sha256's one-register
  control sets; demand-weighted share at least 0.9) gets twice that,
  and is spread to full bins rather than to the 0.9 target: its smoothed
  overflow stays up while its objects straddle bins, and waiting for it
  over-spreads everything else. If the overflow stops improving in the
  late phase, the run keeps the placement at its best overflow.

The objective and the kernels are device-generic, like the cluster
flow's `Problem`/`Legalizer` split. The device supplies objects, pins
with offsets, nets, the resources' capacities per bin and a timing hook.
The xc7 back end can use it with its own resources (LUTs, flip-flops,
control sets per slice).

Designs of 700 LEs or more are placed this way; smaller ones by the
cluster flow, as before. The global placement costs a few seconds
whatever the design's size: its fillers cover the chip (about 7,300 of
them against 100 real objects on m4k-bytes), and the cluster flow's
anneal only costs that much from about 700 LEs (see "Every DE2
example"). `CHIPSMITH_PLACER=electro` or `cluster` forces either.

### Legalisation (Cyclone II, `place/le.rs`)

Greedy and deterministic, per LE: blocks to the nearest free site of
their kind; carry chains, longest first, to the nearest free column of
LABs that `lab_check` accepts; then registers, then the other LEs, each
to the LAB that minimises the distance from its target plus 1.5 bins for
a register opening a new control set, among those with a free slot that
`lab_check` accepts. If any LE finds no LAB, the design is placed by the
cluster flow, which fits everything it fitted before.
`CHIPSMITH_PLACER=cluster` selects the cluster flow outright.

### Detailed placement

The existing timing-driven anneal (`best_of`, 8 seeds), started from the
legal placement with a colder schedule, `REFINE_LE`: heat 0.05, range 1
LAB, N^(4/3) / 2 moves per temperature. The cluster flow keeps `REFINE`
(heat 0.5, range 3, N^(4/3)).

## Choosing the settings

Every choice was made on routed critical paths, with several seeds per
design, against `open` 40a29cb with the same seeds. The machine was
shared throughout (load 10 to 26), which affects times but not critical
paths: those are deterministic for a binary, design and seed.

Baseline, routed critical path in ns, median of seeds 0 to 4 [range]:
picorv32 8.01 [7.61-8.62], femtorv32 12.81 [12.66-13.43], mult-selftest
15.15 [14.89-15.57], sha256 21.03 [20.63-21.28].

Anneal schedule after LE-level placement, seeds 0 to 2, ratio of each
design's median to the baseline's (picorv32, femtorv32, mult-selftest,
sha256):

| schedule | heat, range, moves | geomean | worst | placement (sha256, loaded) |
|---|---|---|---|---|
| A (`REFINE`) | 0.5, 3, 1 | x0.986 | x1.008 | 213 s |
| C1 | 0.1, 2, 1 | x0.971 | x0.993 | 133 s |
| C1 with 4 seeds | 0.1, 2, 1 | x0.981 | x0.994 | 131 s |
| C2 | 0.05, 1, 0.5 | x0.995 | x1.028 (sha256 21.44 [20.78-22.10]) | 25 s |

That chose C1. The global placer's density targets and stopping then
changed (below), and the two were measured again with five seeds each
against the baseline's five, plus picorv32x24 with three:

| schedule | geomean (4 designs) | worst | picorv32x24, routed ns | anneal moves, picorv32x24 |
|---|---|---|---|---|
| C1 | x0.989 | x1.011 (femtorv32) | 9.43, 9.63, 9.63 | 113 million |
| **C2 (`REFINE_LE`)** | **x0.990** | **x1.008 (femtorv32)** | **10.18, 9.34, 9.58** | **40 million** |

As good for a third of the moves, so C2 is the default.

Timing-driven global placement, with C2's anneal: weights that add up
over the reweights instead of replacing each other, reweighting every 10
iterations from an overflow of 0.8 (T3), took sha256 to 20.63 (x0.989)
but picorv32 to 8.26 (x1.031); with C1, picorv32 8.15 again. Rejected;
the knobs stay (`CHIPSMITH_ELECTRO_TIMING_{WEIGHT,ACCUMULATE,EVERY,FROM}`).

Density targets and stopping, each found on a design that broke:

- A 0.9 target for every resource: sha256's control-set field (one
  register per LAB) couldn't get below 0.12 overflow, the density weight
  grew without bound, and the placement ended in NaNs.
- The target raised to the largest single object's demand: a few
  one-register sets on picorv32x24 put its control field at 1.0, which
  left the legaliser no room (legal wirelength 140,897 against 88,220,
  estimate 21.05 ns against 10.37). The demand-weighted share replaced
  it.
- Overflow counted by the bin holding each object's centre: the field
  evens out the smoothed density, not centres, so every design stalled
  higher (mult-selftest 0.33, sha256 0.24) and picorv32x24 fell back to
  the cluster flow.
- Waiting for every resource: mult-selftest's multipliers held the run
  for 527 iterations; legal wirelength 7,943 against 4,206 with the
  whole-bin allowance.

The defaults, seeds 0 to 4 (picorv32x24 0 to 2), medians against the
baseline's [range]:

| design | baseline | this branch | ratio |
|---|---|---|---|
| picorv32 | 8.01 [7.61-8.62] | 7.78 [7.67-7.82] | x0.971 |
| femtorv32 | 12.81 [12.66-13.43] | 12.91 [12.90-13.19] | x1.008 |
| mult-selftest | 15.15 [14.89-15.57] | 14.98 [14.63-15.37] | x0.989 |
| sha256 | 21.03 [20.63-21.28] | 20.91 [20.82-21.11] | x0.994 |
| picorv32x24 | 10.65 (seed 0) | 9.58 [9.34-10.18] | x0.900 |

Geomean of the first four x0.990. Every median is inside the baseline's
range of seeds, and every design's worst seed is better than the
baseline's worst.

## Routing

### Batched PathFinder (`chipsmith-route`, opt-in)

`CHIPSMITH_ROUTE_BATCH=regions` routes the first iteration (more with
`CHIPSMITH_ROUTE_BATCH_ITERATIONS`) in batches whose nets' search
regions don't overlap, each batch's nets side by side against the
routing as it stood when the batch began, then committed in net order. A
net whose tree takes a node an earlier net of its batch just took is
routed again first, against what is committed. A net with 32 or more
sinks gets a batch of its own and searches its sinks 8 at a time side by
side, each from the tree as it was when the group began; the branches
are grafted in order. Group sizes don't depend on the thread count, so
the result is the same on any number of threads (a test checks 1
against 4 threads on a 12 x 12 grid with a 40-sink net).
`CHIPSMITH_ROUTE_BATCH=<n>` batches n nets in order instead.

What it took to make it converge, and why it stays off:

- Deciding once per iteration which nets reroute stalled picorv32 at 2
  overused nodes for 50 iterations: pairs of critical nets kept trading
  local lines. Asking again before each batch, as the one-at-a-time loop
  does, fixed it.
- Batching every iteration: sha256 needed 50 iterations instead of 21.
  So only the first iteration batches.
- On sha256 the first iteration batches poorly: the regions of a
  compact design overlap (picorv32: 1,672 nets in 901 batches, at most 4
  nets each), and its time goes to wide nets. Batched, sha256 routed in
  61.6 s and 14 iterations against 50.6 s and 9, critical path 21.03
  against 20.49 ns (load 17).

### A GPU router: prototyped and rejected

`experiments/gpu-bench route` takes the nets of a real routing
(`CHIPSMITH_ROUTE_DUMP`) and runs a first iteration's searches (every
node costs 1, every net split into its source-to-sink connections, each
kept to its net's region): A* as `chipsmith-route` runs it, on one
thread and on rayon's 8; and the GPU-friendly form, a level-synchronous
breadth-first search, 32 connections per launch, the 873k-node,
8.6M-edge graph resident on the device, distances stamped per batch so
nothing is reset. sha256, 19,962 connections, load 6 to 8:

| | time | work per connection |
|---|---|---|
| A*, 1 thread | 39.8 s | 1,081 nodes expanded |
| A*, rayon | 11.3 s | |
| GPU breadth-first, searching only | 20.1 s | 104,044 nodes visited (96 times A*'s), 5,481 launches |

The GPU's best case loses to A* on 8 threads: without A*'s pruning a
search covers its region. A real PathFinder iteration is less favourable
still: congestion and delay costs need a weighted shortest path, not
breadth-first levels, and a net's sinks are searched one after another
from its growing tree. With 128 connections per launch, frontiers
overflowed 8M entries.

## The GPU in placement

`chipsmith-place::gpu` (feature `gpu`, CubeCL 0.11.3) keeps the problem
and every vector on the device: pins and nets in CSR, each object's pins
in CSR, demands, the cosine tables, the positions and gradients. One
iteration is 17 kernel launches (pin positions, per-net sums, per-pin
gradients, clearing and scattering the density with integer atomics,
the charge, 6 matrix products and a scaling for the field, overflow,
per-object gradient, partial sums, the step) and one read of the
partial sums. The host supplies the step's scalars.

Global placement, seconds, from the Results runs (load 6 to 15). The
GPU's time includes compiling its kernels, which every run does: the
"of it" column is the time to the end of its first iteration.

| design | objects | iterations | 1 thread | rayon (8) | GTX 1660 | of it: first iteration |
|---|---|---|---|---|---|---|
| femtorv32 | 625 | 334 | 5.41 | 4.11 | 1.96 | 0.95 |
| mult-selftest | 759 | 402 | 4.96 | 5.68 | 2.33 | 1.16 |
| picorv32 | 958 | 281 | 4.87 | 4.88 | 2.56 | 1.40 |
| sha256 | 5,404 | 301 | 9.38 | 6.02 | 2.74 | 1.45 |
| picorv32x24 | 23,394 | 265 | 35.35 | 10.25 | 5.75 | 2.03 |

Per iteration after the first, picorv32x24: 133 ms on one thread, 39 ms
on rayon, 14 ms on the GPU; sha256: 31, 20 and 4 ms. On the small
designs most objects are fillers (about 7,300 against 600 to 1,000 real
ones), so they cost what sha256 does, and rayon gains nothing at that
size on a loaded machine. The GPU is 1.8 to 2.4 times faster than rayon
per run; per iteration, without the compile, 2.8 (picorv32x24) to 4.7
(sha256) times.

## Results

`de2_pnr` (place, route and bitstream from the packed netlist, as
`bench/speed.sh` times those stages), seed 0, `open` 40a29cb and this
branch back to back for each design. The machine was shared (load
average 6 to 17 at the end of each run, in the last column), so the
small designs' times move by a few seconds between runs. "This branch"
is the default (global placement on rayon, router one net at a time);
the next two columns change only the global placement's engine.

| design | `open`: place / route / total s | this branch: place / route / total s | global placement on 1 thread: total s | on the GPU: total s | routed critical ns, `open` → this branch (GPU) | load |
|---|---|---|---|---|---|---|
| femtorv32 | 5.2 / 6.7 / 18.8 | 7.4 / 4.1 / 17.2 | 21.6 | 12.5 | 13.43 → 12.95 (13.33) | 6 to 9 |
| mult-selftest | 9.3 / 9.6 / 23.5 | 15.2 / 9.1 / 30.9 | 24.2 | 21.0 | 15.57 → 14.91 (15.42) | 9 to 10 |
| picorv32 | 14.8 / 11.1 / 33.1 | 14.7 / 8.7 / 30.4 | 34.3 | 32.3 | 8.19 → 7.89 (7.94) | 12 to 15 |
| sha256 | 165.1 / 86.2 / 256.6 | 55.1 / 50.6 / 110.9 | 107.0 | 117.3 | 20.63 → 20.49 (21.07) | 8 to 13 |
| picorv32x24 | 1,281.8 / 242.1 / 1,531.3 | 413.2 / 160.1 / 580.3 | 554.7 | 692.3 | 10.65 → 9.43 (9.33) | 6 to 12 |

- sha256 builds 2.3 times faster, picorv32x24 2.6 times: placement 3.0
  and 3.1 times faster, routing 1.7 and 1.5 times, because the better
  placement routes in 9 and 15 iterations instead of 26 and 36.
- The small designs build in about the same time: the global placement
  (4 to 5 s) costs what the shorter anneal saves.
- Of picorv32x24's 413 s of placement, 10 s are global placement and
  about 400 s the anneal (112 million moves); of sha256's 55 s, 6 and
  45. The GPU's few seconds don't show in the totals; its picorv32x24
  run drew a higher load (12) than the CPU's.
- Routed critical paths are as good or better, on these single seeds as
  over five seeds (Choosing the settings).

## Every DE2 example

`bench/speed.sh` (synthesis to bitstream, no trace) over all 33 DE2
examples with `open` 40a29cb and with this branch placing everything LE
by LE, seed 0, at load 25 to 40: every example built. Routed critical
paths were equal (within 0.03 ns) on 17, better on 9 (femtorv32 13.97 → 13.19,
mult-selftest 15.80 → 14.63, picorv32 7.90 → 7.74, picorv32x24 10.65 →
10.18, negedge-selftest 9.44 → 9.09, vga-pattern 6.63 → 6.32,
sram-selftest 5.93 → 5.69, de2-io 3.71 → 3.45, m4k-tdp 3.93 → 3.80) and
worse on 7 (ram-leds 4.03 → 4.37, i2c-master 7.03 → 7.49, m4k-selftest
11.83 → 12.50, spi-master 4.22 → 4.45, sdram-selftest 6.48 → 6.65,
crc32 3.47 → 3.53, sha256 20.99 → 21.11), single seeds against seed
spreads of 2 to 20% on these designs (docs/fast-pnr-status.md).
Placement of the small examples went from under a second to 5 to 13 s
(m4k-bytes 0.8 → 7.2 s, ram 1.4 → 13.4 s, uart 0.7 → 5.9 s), the global
placement's fixed cost; fir (534 LEs) 7.4 → 14.1 s, femtorv32 (788)
14.5 → 11.2 s. That set the 700-LE threshold: below it the examples
place by clusters again, as on `open`.

## Determinism

- CPU: the serial and threaded engines are the same code and give
  identical placements (a unit test, and the bench: same wirelength,
  estimate and routed critical path at 1 and 8 threads). Every item
  (net, pin, object, bin) is computed in a fixed order, sums over many
  items are taken in chunks of 1,024 and the chunks in order, and
  density is accumulated in fixed point, which is exact in any order.
- GPU: the same structure, with no float atomics, so a run repeats
  exactly on the same GPU, driver and binary (a unit test runs twice and
  compares). It is not identical to the CPU: the GPU's `exp` and fused
  multiply-adds round differently, and a global placement turns an ulp
  into a different legal placement (sha256: legal wirelength 30,744
  against 30,665, routed 21.07 against 20.49 ns).
- So the engine is chosen, never detected: `CHIPSMITH_ACCEL=cpu` (the
  default), `serial`, `cuda` or `hip`. A fixed binary, design, seed and
  setting gives the same result, and the build cache keys on it (it keys
  on every `CHIPSMITH_*` variable, ADR-0011).
- The anneal (8 seeds on threads, the best by estimated critical path)
  and the router (batched or not) are deterministic as before.
- Found on the way, and older than this branch: the nets' timing weights
  before the anneal were computed only inside the placement trace's
  message, and the anneal sizes its first temperature with them, so
  `CHIPSMITH_PLACE_TRACE=1` changed the placement. They are computed
  always now (every measurement here ran with the trace on, so the
  numbers hold); picorv32 with and without the trace gives the same
  bitstream.
- Checked on the bench: global placement on 1 and on 8 threads gives
  byte-identical `placed.json`, `routed.json` and bitstreams for
  picorv32, sha256 and picorv32x24; two CUDA runs of picorv32 give the
  same bitstream.

## Building and running

- Without the `gpu` feature (the default, and the nix package) nothing
  of CubeCL is compiled. Cargo.lock lists it, so nix fetches its
  sources; `nix build` builds and tests the package as before.
- `cargo build --release -p chipsmith --features gpu` in the devshell
  (CUDA toolkit, ROCm's HIP runtime). CubeCL's bundled LLVM needs lld to
  link (GNU ld scans its archives in the wrong order), which
  `crates/chipsmith/build.rs` and `crates/chipsmith-place/build.rs` ask
  for when the feature is on. The devshell's `ld.lld` is nixpkgs'
  wrapped one, which writes the rpaths of what it links (libusb,
  libstdc++) as GNU ld's wrapper does; the bare `pkgs.lld` didn't, and
  the binaries didn't start outside the devshell.
- HIP builds but has not run: there is no AMD GPU here. On the AMD
  desktop: `CHIPSMITH_ACCEL=hip target/release/chipsmith -C examples/sha256
  build`, and `cd experiments/gpu-bench && nix develop ../.. --command
  cargo run --release -- hip`.

## Left

- The cold anneal is now most of placement (sha256 45 of 55 s,
  picorv32x24 about 400 of 413 s). C2's schedule is 5 times faster but cost sha256 3%;
  a timing-driven global placement good enough to need only C2 would
  cut placement again.
- Routing is the other half of a large build and stays sequential.
  Batches don't pay on compact designs; a router whose first iteration
  parallelises well needs a different decomposition (wide nets are most
  of it).
- On small designs the global placement takes longer than the old
  anneal it shortens (femtorv32: 4 s): most of its objects are fillers
  (7,000 against 800 real ones). Larger fillers or fewer bins for small
  designs would help.
- The GPU compiles its kernels on every run (~1.5 s). CubeCL can cache
  them (its `persistence` feature), not enabled here.
- The xc7 back end doesn't use `chipsmith-place` yet.
