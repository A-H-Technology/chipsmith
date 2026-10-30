# gpu-pnr: place and route rebuilt for parallel hardware

Branch `gpu-pnr`, from `open` 40a29cb. The goal: a place-and-route flow
whose heavy stages run as wide parallel kernels, so a GPU (CUDA, or HIP
on AMD) makes it much faster, and the same kernels on CPU threads
(rayon) remain the fallback. docs/fast-pnr-status.md, "GPU", measured
that today's flow gives a GPU nothing: its analytic solve has ~2,000
rows and is 0.4% of placement, and its router is sequential A*. This
note says what replaces them, records the baseline, and (later) the
results.

## Baseline

`open` 40a29cb, `bench/speed.sh` with `CHIPSMITH_PLACE_TRACE=1
CHIPSMITH_ROUTE_TRACE=1`, one run, seed 0. The machine was shared:
load average 13 during sha256, 14 to 22 during picorv32x24 (Quartus and
yosys of other agents), so the times are high. A quiet rerun replaces
them in the results.

| design | LEs | place s | of it: cluster + analytic / anneal | route s | iterations | of it: iteration 1 | routed critical ns |
|---|---|---|---|---|---|---|---|
| femtorv32 | 788 | 7.8 | 0.1 / 7.6 | 4.3 | 5 | | 13.43 |
| mult-selftest | 1,008 | 13.7 | 0.2 / 13.3 | 12.1 | 7 | | 15.57 |
| picorv32 | 1,171 | 16.3 | 0.3 / 15.8 | 8.9 | 7 | | 8.19 |
| sha256 | 6,046 | 93.4 | 7.9 / 85.1 | 65.2 | 26 | 35.0 | 20.63 |
| picorv32x24 | 28,449 | 1,375.1 | 26.0 / 1,345.5 | 238.6 | 36 | 111.7 | 10.65 |

Where the time goes:

- Placement is the larger stage on every design, and the refining
  anneal is 91 to 98% of it. It runs 8 seeds of N^(4/3) moves per
  temperature for 50 to 80 temperatures: 58 million moves on sha256, 274
  million on picorv32x24. The anneal does real work, not polish: it cuts
  sha256's wirelength from 54,281 to 37,454 and picorv32x24's from
  146,170 to 123,209, and its critical path estimate from 12.47 to
  9.98 ns. The analytic start it refines is coarse: LEs are clustered
  into LABs before anything is placed, and on sha256 1,543 of the 1,825
  clusters are a single register (1,804 distinct asynchronous clears,
  one per LAB), so the analytic placer sees 1,543 one-LE "LABs" whose
  LUTs the anneal then has to fetch.
- Routing: the first PathFinder iteration is half of it (sha256 35 of
  65 s, picorv32x24 112 of 239 s), and nets of 32 or more sinks are two
  thirds of that iteration. Later iterations reroute a few hundred nets
  each.

So the placer goes first, as expected. The win has to come from
replacing most of the anneal's work, which a faster solve alone can't do.

## Placement

### Global placement: LE-level, electrostatic (`chipsmith-place::electro`)

Every LE that isn't in a carry chain is its own object; a chain is one
object as tall as its LABs; M4Ks and multipliers are objects of their own
kinds. That is 6,000 objects on sha256 and 28,000 on picorv32x24, the
size where the GPU bench found the GPU starts winning. The placer
minimises

    W(x, y) + sum over resources r of lambda_r * N_r(x, y)

with Nesterov's method and ePlace's Lipschitz step estimate (Lu et al.,
"ePlace", TODAES 2015; the FPGA form follows elfPlace, Li et al.,
ICCAD 2019):

- W is the weighted-average (WA) smooth wirelength of every net, each
  weighted by the device's timing weight (1 + 8 crit^4, as now).
- N_r is the electrostatic energy of resource r: objects are charges,
  each bin holds its capacity as negative charge, the potential solves
  Poisson's equation by a discrete cosine transform over the bin grid
  (one bin per LAB: 66 x 37 on the EP2C35), and the force on an object
  is its charge times the field. Bins without a site of r (M4K and
  multiplier columns, I/O) have capacity 0, so they repel.
- Resources, so the density model knows what a LAB can hold:
  - LE slots: capacity 16 a LAB, every LE takes 1.
  - Register control sets: a LAB takes one set of register controls
    (clock edge, enable, clears; `lab_check`), so capacity 1 a LAB, and a
    register whose set has k members takes ceil(k / 16) / k of it. On
    sha256 this is what says 1,804 registers need 1,804 LABs while their
    4,000 LUTs can share them.
  - One resource per block kind, capacity 1 at its sites.
- Timing: every few dozen iterations, once the placement has spread,
  the device reweights the nets from a timing analysis on the continuous
  positions (the anneal's delay model, distances not snapped to LABs).
- It stops when every resource's overflow is below 10%.

The objective and the kernels are device-generic, like the current
`Problem`/`Legalizer` split: the device supplies objects, pins with
offsets, nets, the resources' capacities per bin, and a timing hook. The
xc7 back end can use it later with its own resources (LUTs, FFs, control
sets per slice).

### Legalisation (Cyclone II)

Deterministic and greedy, as now, but per LE:

1. blocks to the nearest free site of their kind;
2. carry chains, longest first, to the nearest free column of LABs
   (`chain_labs`) that `lab_check` accepts;
3. LEs, registers first, each to the LAB that minimises distance from
   its target plus a penalty for opening a new control set there, among
   those with a free slot that `lab_check` accepts with it. Searched
   outwards bin by bin until the distance alone exceeds the best cost.

If any LE finds no LAB, the design is placed by today's flow (clusters,
quadratic solve, LAB legaliser), so a design that fits today still fits.

### Detailed placement

The existing timing-driven anneal (`best_of`), started cold from the
legal placement: a lower starting temperature and range, which also
means fewer temperatures. How cold, how many moves and how many seeds
is a measured choice (several schedules, routed critical paths over the
benchmark set, three seeds each, against the baseline). The quality bar
is the baseline's routed critical path within noise.

## Routing

### Batched, partitioned PathFinder (`chipsmith-route`)

Each PathFinder iteration, the nets to (re)route are taken in today's
order and cut into batches greedily: a net joins the current batch if
its search region (its box grown by the bounding-box margin) overlaps
no other net's region in the batch. A batch's nets are routed side by
side against the routing state as it was when the batch began
(occupancy, history, present-cost factor); their trees are then added
to the occupancy in net order before the next batch starts.

- Deterministic at any thread count: a net's tree depends only on the
  state at the start of its batch, and batches are formed and committed
  in a fixed order.
- PathFinder's convergence argument is unchanged. It only needs every
  net to be rerouted against present and history costs that grow on
  overused nodes until no node is overused. Within a batch two nets
  can't see each other's choices, which is a Jacobi step instead of a
  Gauss-Seidel step. With disjoint regions they can only collide on a
  long wire (C16, R24) that reaches into both regions; that is an overuse
  the next iteration negotiates away like any other. The doc's earlier
  experiment, the whole first iteration in parallel with no regions
  (docs/fast-pnr-status.md), shows the cost of collisions without
  partitioning: 2,886 nets rerouted in iteration 2 instead of 1,684.
  Disjoint regions keep collisions to the long wires.
- A net whose region search fails, and so searches the whole chip, is
  routed alone after its batch.
- Nets whose region covers most of the chip (wide nets, clock-like
  fan-outs) end up in batches of one, which is today's behaviour.

### On the GPU

The routing graph is uploaded once and stays on the device for the
whole routing: the downhill edges in CSR (`edge_start`, `edge_dst`), each node's delay and box, the LUT-input permutation
mask per edge, and the routing state: occupancy (u32), history (f32),
present-cost factor. A batch is one kernel launch: one thread block per
net, each running a parallel shortest-path search (frontier relaxation,
Bellman-Ford style, costs in fixed point so the minimum doesn't depend
on the order of relaxations) inside its region from the net's current
tree to its next sink. Trees and occupancy updates stay on the device;
what comes back per iteration is each rerouted net's sink delays (for
timing, which stays on the CPU) and the count of overused nodes, and at
the end the trees. Uploads per iteration are the new criticalities
(one f32 per sink).

This is built after the placer, and only if the CPU-parallel router
leaves routing as a large share of the flow.

## Determinism

- CPU, serial and parallel: identical results whatever the thread count.
  Work is split over independent items (nets, objects, bins) each
  computed in a fixed order; every sum over many items is summed in
  fixed-size chunks, then the chunks in order; density is accumulated in
  fixed point (exact, so order-independent).
- GPU: the same structure (no float atomics, fixed-order reductions,
  fixed-point density), so a GPU run is repeatable on that GPU and
  driver. It is not bit-identical to the CPU: the GPU's `exp`, its fused
  multiply-adds and the compiler's contraction choices round
  differently, and a global placement amplifies an ulp into a different
  legal placement.
- So the backend is chosen explicitly, never detected:
  `CHIPSMITH_ACCEL=cpu` (default), `cuda` or `hip`. A fixed binary,
  design, seed and setting gives the same result, and the incremental
  build cache keys on the setting (it keys on every `CHIPSMITH_*`
  variable, ADR-0011). A machine without a GPU builds and runs the
  default; the GPU code is behind the `gpu` cargo feature.

## Measuring

Bench: `bench/speed.sh` over femtorv32, mult-selftest, picorv32,
sha256 and picorv32x24, plus every DE2 example for the quality check.
Each stage is timed with CPU-serial (`RAYON_NUM_THREADS=1`),
CPU-parallel (8 threads) and the GPU, so the table separates what the
algorithm gains from what the GPU gains. Machine load is noted for each
run; runs that decide a choice are repeated when the load is low.

HIP is built but not run: there is no AMD GPU here. On the AMD desktop:
`CHIPSMITH_ACCEL=hip bench/speed.sh ...` with a binary built with
`--features gpu`.
