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
rayon's threads or on a GPU. Its start is good enough that the refining
anneal does a thirty-fifth of `open`'s moves, and the routes converge in
fewer iterations. picorv32x24 places 16 times faster and builds 4.2
times faster (835 → 201 s), sha256 2.0 times (129 → 64 s), with routed
critical paths within `open`'s range over five seeds (picorv32x24 9.64
against 10.65). That is the algorithm: it holds on one thread. The GPU
makes global placement up to 3.8 times faster than 8 threads, and runs
the router's first-iteration batches 7 times faster than 8 threads with
the same routes; together they take another 8% (picorv32x24 185 s)
and 21% (sha256 51 s) off, because routing's wide nets and later
iterations are still sequential A*. A batched detailed placer that
would replace the anneal was built and measured; it shortens wires as
well but not critical paths, and stays opt-in.

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
legal placement with a colder, shorter schedule, `REFINE_LE`: heat 0.05,
range 1 LAB, N^(4/3) / 10 moves per temperature (picorv32x24: 8 million
moves against `open`'s 274 million). The cluster flow keeps `REFINE`
(heat 0.5, range 3, N^(4/3)).

A batched detailed placer (`place/detail.rs`, `CHIPSMITH_PLACE_DETAIL=1`
before the anneal, `2` after) was built to replace the anneal and
measured against it; it stays opt-in. Each round, every LE scores moves
towards the middle of its nets (into a free slot of the nearest LABs, or
swapping with an LE there) against the placement as it stands, on rayon
or one thread, bit-identical; the best are taken greedily when they
share no LAB and no net whose box one of them changes (moves that leave
a box alone can share its net: together they only shrink it). After the
first round only LEs whose proposal lost a clash, or that sit on a net a
taken move changed, propose again, and a full round ends the run.

| picorv32x24, from the same legal placement | wirelength | estimated critical ns | anneal moves |
|---|---|---|---|
| detailed placement alone | 79,160 | 10.44 | 0 |
| detailed placement, then the anneal at 0.1 or 0.2 of C2's moves | 78,182 / 77,528 | 10.44 / 10.44 | 8.0 / 16.1 million |
| the anneal at 0.1 of C2's moves (`REFINE_LE`) | 79,931 | **10.16** | 7.9 million |
| C2's anneal | 76,839 | 10.10 | 40 million |

Greedy moves shorten wires as well as the anneal does but leave its
timing behind: the anneal's best of 8 seeds, chosen by estimated
critical path, is what the single greedy descent can't match, and an
anneal started from the greedy result stays in its basin. Run after the
short anneal instead, five seeds each, routed (medians against `open`
untraced): picorv32 8.17, femtorv32 12.97, mult-selftest 14.91, sha256
21.17, picorv32x24 9.51 (two seeds); geomean x1.004 against the short
anneal's x1.011, sha256 worse, for 140 s more on picorv32x24 (300
rounds of mostly small gains, a timing reweight each). Scoring is
lab_check and box arithmetic per move; porting it to the GPU would speed
the rounds up, not the descent's result.

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
| C2 | x0.990 | x1.008 (femtorv32) | 10.18, 9.34, 9.58 | 40 million |

C2 was as good for a third of the moves. Then the anneal itself was cut
again: C3 (heat 0.05, range 1, a tenth of N^(4/3) moves), against
`open` without the placement trace (which `open` lets change its
placement, see Determinism), five seeds:

| design | `open` | C2 | **C3 (`REFINE_LE`)** |
|---|---|---|---|
| picorv32 | 7.90 [7.69-8.38] | 7.78 [7.67-7.82] | 8.15 [7.92-8.41] |
| femtorv32 | 13.15 [12.74-13.97] | 12.91 [12.90-13.19] | 13.26 [12.97-13.92] |
| mult-selftest | 15.41 [15.07-15.86] | 14.98 [14.63-15.37] | 15.38 [14.76-15.41] |
| sha256 | 20.59 [20.17-20.99] | 20.91 [20.82-21.11] | 20.74 [20.24-21.23] |
| geomean of medians | | x0.989 | x1.011 |
| picorv32x24 (`open`: seed 0) | 10.65 | 9.58 [9.34-10.18] | 9.64 [9.37-9.90] |

C3's medians are all inside `open`'s ranges of seeds, its worst seeds
within 1.1% of `open`'s worst; it does a fifth of C2's anneal (8 million
moves on picorv32x24), which is most of a large design's placement.
C2 is about 2% better on these four designs; `CHIPSMITH_PLACE_MOVES=0.5`
gets it back.

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

The traced `open` (the earlier baseline runs, with the trace's reweight)
and C2, for the record: picorv32 8.01 against 7.78, femtorv32 12.81
against 12.91, mult-selftest 15.15 against 14.98, sha256 21.03 against
20.91 (geomean x0.990).

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

### The GPU router (`chipsmith-route`, feature `gpu`)

Built from the design above, after the breadth-first prototype below
lost. The search the GPU runs is A* in a form that parallelises and is
deterministic (`bucket.rs`): label-correcting rounds, each relaxing
every frontier node within a bucket (one node cost) of the frontier's
best estimate, from the cost the node had when the round began; costs,
delays and the estimate in fixed point (1/1024 of an R4); a node's label
is the least (cost, edge) offered, taken by an atomic minimum; the
search stops when the sink's cost is no more than the best estimate
left. The CPU runs the same search (`CHIPSMITH_ROUTE_SEARCH=bucket`), so
the GPU's trees can be checked against the CPU's: they are the same,
byte for byte (unit tests on a grid; picorv32, sha256 and picorv32x24's
`routed.json`).

On the GPU (`CHIPSMITH_ROUTE_ACCEL=cuda`, implies the integer search):
the routing graph (CSR, edge sources, usable-edge mask, node boxes and
delays), occupancy and history stay on the device; a batch of up to 64
nets is one kernel launch, one cube (256 threads) per net looping over
its rounds and sinks inside the kernel: thread 0 ends a search (walks
the branch back, grafts it where it meets the tree, picks the next
sink), all threads seed, take the frontier's minimum, relax and compact.
Up goes the batch (sources, sinks, criticalities, regions: one buffer)
and the committed trees as occupancy deltas; down comes each net's tree.
Labels are stamped with a per-slot generation, so nothing is cleared
between searches.

The integer search runs for the nets of batches of more than one net in
the first iteration; nets alone in a batch (wide ones: 32 sinks or
more), collision repairs and later iterations use A*, the same in every
mode. Batches of 64 nets in order (`CHIPSMITH_ROUTE_BATCH=64`): region
batches are too small on compact designs (picorv32: 884 batches, at most
5 nets) to fill a GPU.

Routing from the same placement (load 3 to 10):

| design | router | route s | iteration 1 s | of it on the GPU | iterations | routed ns |
|---|---|---|---|---|---|---|
| picorv32 | A*, one net at a time | 9.3 | 4.05 | | 10 | 7.89 |
| | integer, 1 thread | 10.2 | 6.78 | | 7 | 7.84 |
| | integer, rayon (8) | 7.8 | 3.31 | | 7 | 7.84 |
| | integer, GPU | 24.3 | 7.17 | 2.49 | 7 | 7.84 |
| sha256 | A*, one net at a time | 49.6 | 29.1 | | 9 | 20.49 |
| | integer, 1 thread | 68.7 | 52.9 | | 7 | 20.50 |
| | integer, rayon | 41.4 | 25.0 | | 7 | 20.50 |
| | integer, GPU | 39.0 | 13.9 | 2.04 (122 launches) | 7 | 20.50 |
| picorv32x24 | A*, one net at a time | 144.2 | 66.0 | | 15 | 9.43 |
| | integer, rayon | 160.7 | 76.9 | | 27 | 9.52 |
| | integer, GPU | 139.6 | 44.8 | 4.77 (621 launches) | 27 | 9.52 |

- The GPU earns its keep on what it runs: the batched nets of sha256's
  first iteration take 2.0 s on the GTX 1660 against about 14 s on 8
  threads and about 40 s on one (the rest of the iteration is the wide
  nets, about 11 s of A*); picorv32x24's, 4.8 s against about 37 s on 8
  threads. Same trees.
- It doesn't make routing much faster yet: wide nets and every later
  iteration run sequential A* on the CPU, and on picorv32x24 the 64-net
  batches (whose nets don't see each other) leave more overuse, 27
  iterations instead of 15. sha256 routes 1.27 times faster than with
  A* alone; picorv32x24 3% faster.
- On the CPU the integer search is slower than A* (no heap: every
  round passes over the frontier), so batching it on rayon doesn't beat
  A* one net at a time; the GPU's gain over rayon is the GPU's.
- Rounds dominate a batch: a net's sinks are searched one after
  another, about 20 rounds each. A wider bucket (4, 16 node costs) makes
  fewer rounds but more relaxations: picorv32's batches 2.49 → 1.38 s,
  sha256's 2.04 → 5.84 s, and changes the routes (both still the CPU's).
  The default stays 1.

The first version made 6 launches and a host round-trip per round
(picorv32: 52 s for 11,698 rounds); one launch per batch with the rounds
inside the kernel took that to 2.5 s.

### The GPU router's prototype: breadth-first, rejected

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

`de2_pnr` (place, route and bitstream from the packed netlist), seed 0,
`open` 40a29cb and this branch back to back per design, load 4 to 7 (in
the last column). "rayon" is the default: global placement on rayon's
threads, the router one net at a time with A*. The other columns change
one engine each: global placement on one thread, or on the GPU, and the
GPU also routing the first iteration's batches.

| design | `open`: place / route / total s | rayon: place / route / total s | global placement 1 thread / rayon / GPU s | anneal s | GPU placement and router: route / total s | routed ns: `open` → rayon (GPU, GPU+router) | load |
|---|---|---|---|---|---|---|---|
| femtorv32 | 5.7 / 4.4 / 15.4 | 2.8 / 4.3 / 12.4 | 3.32 / 2.37 / 2.25 | 0.3 | 11.9 / 18.6 | 13.43 → 13.92 (13.06, 13.56) | 5 to 7 |
| picorv32 | 8.9 / 7.3 / 20.7 | 3.5 / 11.0 / 21.2 | 2.74 / 2.65 / 1.93 | 0.7 | 14.8 / 21.8 | 8.19 → 8.39 (7.57, 7.89) | 5 to 8 |
| mult-selftest | 8.1 / 8.7 / 21.1 | 4.0 / 6.3 / 14.7 | 4.23 / 2.93 / 1.93 | 0.9 | 15.1 / 22.6 | 15.57 → 14.76 (15.25, 15.50) | 4 to 5 |
| sha256 | 67.4 / 56.8 / 129.0 | 6.4 / 52.8 / 64.0 | 6.02 / 3.21 / 2.02 | 2.6 | 40.2 / 50.7 | 20.63 → 20.86 (21.99, 21.49) | 4 to 6 |
| picorv32x24 | 636.6 / 190.5 / 835.3 | 39.7 / 153.8 / 200.8 | 21.76 / 10.42 / 2.71 | 26.0 | 140.3 / 185.1 | 10.65 → 9.64 (9.90, 9.90) | 4 to 7 |

- Placement is 10 times faster on sha256 and 16 times on picorv32x24
  (636.6 → 39.7 s); builds 2.0 and 4.2 times. Routing is now most of a
  large build, and the routes converge in fewer iterations (21 against
  36) than `open`'s.
- What the GPU adds: global placement 1.05 (femtorv32) to 3.8
  (picorv32x24: 10.4 → 2.7 s) times faster than rayon's 8 threads,
  including ~1.5 s compiling kernels, and the first iteration's batched nets 7 times
  faster than the same search on 8 threads. With both, sha256 builds in
  50.7 s and picorv32x24 in 185.1 s, against 64.0 and 200.8 s on the
  CPU: the GPU's share of the remaining time is small because routing's
  wide nets and later iterations stay sequential A*.
- The algorithm accounts for the rest: the whole gain over `open` is
  there on one thread (picorv32x24 225.9 s, sha256 65.9 s).
- Critical paths here are single seeds; over five seeds the defaults
  are within `open`'s range (Choosing the settings). The GPU's
  placements differ from the CPU's; sha256 came out 4% slower on the
  GPU in all three runs measured (21.99, 21.07, 21.86 against the CPU's
  20.86, 20.49, 21.11), picorv32x24 either way (9.90, 9.33, 9.19 against
  9.64, 9.43, 10.18). Not investigated.
- The small designs build in about the same time as on `open` (they are
  placed LE by LE from 700 LEs; femtorv32 and up here).

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

- Routing is now most of a large build. On the GPU it needs wide nets
  (a net's sinks one after another make a long slot) and the later,
  incremental iterations (their kept branches would be the slot's
  starting tree); and batches whose nets see each other less: 64-net
  batches cost picorv32x24 more iterations (20 against 17 above, 27
  against 15 from another placement).
- The anneal is down to 26 s on picorv32x24 (8 million moves on 8
  threads). The batched detailed placer matches it on wirelength but not
  on critical path; a timing-aware selection (best of several rounds, as
  the anneal's best of 8 seeds) is the next thing to try.
- sha256 places 4% slower on the GPU than on the CPU in three runs.
- The global placement's fixed cost (fillers over the whole chip) keeps
  designs under 700 LEs on the cluster flow.
- The GPU compiles its kernels on every run (~1.5 s); CubeCL's
  `persistence` feature would cache them.
- HIP builds but has not run (no AMD GPU here).
- The xc7 back end doesn't use `chipsmith-place` yet.
