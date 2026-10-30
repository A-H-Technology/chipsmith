# fast-pnr: compile speed without losing quality

Branch `fast-pnr`, base `open` 385aa15. Roadmap item 3, "optimization":
an analytic placer, a faster router, incremental builds, and a measured
verdict on GPUs. This note records what changed, the measurements behind
each choice, and what is left.

All timings come from the shared 8-core machine while another agent ran
yosys proofs and Quartus (load average 8 to 26), so they are rough.
Routed critical paths are deterministic for a given binary, design and
seed, and the quality decisions rest on them.

## What changed

### Placement (`crates/chipsmith-place`, `crates/chipsmith-cyclone2/src/place.rs`)

1. LEs are clustered into LAB-sized groups of connected, compatible LEs
   (`cluster`), as VPR's packer does. When the clusters don't fit, small
   ones are merged (`merge_clusters`), topped up with unrelated LUT-only
   LEs (`cluster_with(.., fill = true)`) and packed first-fit
   (`pack_clusters`).
2. `chipsmith_place::place` places the clusters, carry chains and M4Ks
   analytically: a bound-to-bound quadratic wirelength model solved with
   Jacobi-preconditioned CG (rows in parallel with rayon), recursive
   bisection spreading, and anchors to the last legal placement. It is
   device-generic: a device implements the `Legalizer` trait (`legalize`,
   `reweight`) and describes a `Problem` (objects, fixed points, nets,
   bins). The xc7 back end doesn't use it yet.
3. `LabLegalizer` snaps the result onto LAB slots: chains first, then
   clusters largest first, each into the nearest LAB that `lab_ok`
   accepts. A small cluster may share a LAB with a carry chain's last LAB
   or with another small cluster. That is what lets picorv32x24 fit.
4. A refining anneal over LAB slots (`REFINE`: heat 0.5, range 3 LABs,
   1 x N^(4/3) moves per temperature) runs from 8 seeds in parallel
   (`best_of`). The one with the shortest estimated critical path wins,
   so the result doesn't depend on thread timing.

Knobs for sweeps, all read from the environment:
`CHIPSMITH_PLACE_{HEAT,RANGE,MOVES,SEEDS,SEED,DENSITY,ANCHOR,ROUNDS,PATIENCE,TRACE}`.
`SEED` picks another set of anneal seeds.

### Routing (`crates/chipsmith-route`)

Profiled first. On sha256, 77% of routing was the A* loop and 14% heap
pops, and 75% of the time went to nets with 32 or more sinks, mostly in
iterations 2 to 5. Every candidate change was measured on fixed
placements (route-only, `de2_pnr --route-only`), reporting CPU time,
search work and routed critical path:

| hypothesis | kept? | measured |
|---|---|---|
| Node data side by side (one cache line per node instead of four arrays; fixed-input check as a per-tile bitmask instead of a SipHash set) | yes | byte-identical output; sha256 about 15% less time (noisy) |
| Incremental rip-up: a congested or critical net keeps every branch on no node another net uses and reroutes only the sinks that lost their path, or whose connection is critical | yes | sha256 expansions 53M to 26M, picorv32 20 s to 10 s; critical paths within noise (20.61 to 20.44, 8.93 to 8.81, 13.18 to 13.28 ns) |
| Seed a sink's search only with tree nodes within a few hops of the best estimate (slack 2, 4, 8) | no | sha256 about 15% less CPU; picorv32 4 to 6% slower critical path, the smaller designs unchanged |
| First iteration in parallel (every net on an empty chip, deterministic at any thread count) | no | sha256 wall 85 to 66 s but CPU up; iteration 2 then reroutes 2,886 nets instead of 1,684; picorv32 and femtorv32 slower; picorv32 9.16 against 8.81 ns |
| Searches kept to the net's box plus a margin, VPR's `bb_factor`, unrestricted if that fails (margin 1, 3, 6) | yes, 3 | over 15 placements (3 seeds of 5 designs): geomean critical path x0.996, worst x1.06, CPU x0.96; sha256 CPU -16%, picorv32 -28%. Margin 1 sometimes needs 21 to 27 iterations |
| A* weight 1.2 or 2.0 instead of 1.5 | no | 2.0: CPU x0.79 but sha256 20.30 to 21.00 ns and picorv32 +5.5%; 1.2: CPU x1.08, no gain |

The late-iteration "flooding" that the bounding box cures: femtorv32's
10th iteration expanded 2M nodes for 98 nets, because a hard-to-resolve
node with a large present-cost factor made each search cover the chip.

`CHIPSMITH_ROUTE_TRACE=1` prints one line per iteration: nets rerouted,
time, time on wide nets, search work (seeded, pushed, expanded) and
overused nodes. `CHIPSMITH_ROUTE_{INCREMENTAL,BBOX_MARGIN,ASTAR}` are
the knobs.

### Flow (`crates/chipsmith/src/flow.rs`, `cache.rs`)

- The routing graph and the bitstream's device database load on
  threads of their own while synthesis and placement run (`Prefetch`).
  The device database alone took 3 to 6 s, more than everything else on
  small designs.
- Incremental builds (ADR-0011): each in-process step leaves a stamp of
  its key and its inputs' content hashes, and is skipped while they
  hold. A comment-only edit re-runs synthesis, gets the same netlist,
  and skips pack, place-and-route and the bitstream. `CHIPSMITH_CACHE=0`
  runs every step.
- `flake.nix`: the devshell's routing-graph cache is named after the
  database's store path. Before, a stale graph from before the PLL and
  IOE pins broke de2-io and de2-pll-counter in a fresh worktree.

## Choosing the placer's schedule

Rule: no example's routed critical path is worse than `open`'s beyond
noise. Noise was measured as the spread over three anneal seeds
(`CHIPSMITH_PLACE_SEED` 0, 1 and 2; `open` patched with the same knob,
in scratch only). Every DE2 example except sha256 and picorv32x24 was
run with each schedule, with routing as above. The table compares
medians over the three seeds against `open`'s median; "!" marks a
median above `open`'s worst seed by more than 2%.

| schedule | heat, range, moves | geomean vs open | worst | fails |
|---|---|---|---|---|
| S0 (the earlier default) | 0.1, 2, 1 | x0.986 | x1.137 | picorv32 9.11 against 8.10 ns, m4k-tdp 4.39 against 3.86 ns |
| MH | 0.3, 3, 1 | x0.977 | x1.074 | picorv32 8.70 ns (one seed) |
| **A (default)** | 0.5, 3, 1 | **x0.980** | **x1.018** | none: m4k-tdp 3.93 [3.80-4.14] against 3.86 [3.80-3.91] ns; its placement estimate is 3.80 ns on every seed, so routing alone moves it |
| M1 | 1, 4, 2 | x0.983 | x1.000 | none, but about twice A's placement time |

Searched on picorv32 and m4k-tdp only, three seeds each: heat 1, range 4,
moves 1 (picorv32 8.08 ns median, m4k-tdp 4.05); M1 with 4 seeds (as good
as M1); heat 0.3, range 4, moves 2 (as good, 3 times as slow).

## Results

Per design: `open` against this branch, three seeds each (median, and
the range in brackets). Times are the medians of the same runs, under
the load described at the top.

RESULTS_TABLE

## GPU

`experiments/gpu-bench` (CubeCL 0.11.3, CUDA and HIP) builds under nix
(`nix develop` provides the CUDA toolkit and ROCm's HIP runtime). Run on
this machine's GTX 1660, CPU numbers on the loaded 8-core host:

GPU_TABLE

Verdict: not worth pursuing yet, for either placement or routing.

- Placement: the analytic problem has about 2,000 objects (picorv32x24:
  1,733 clusters and 169 chains). At that size one CG iteration is
  launch-bound on the GPU: 413 us against 232 us on one CPU thread. The
  GPU wins from about 30,000 rows (412 us against 4.2 ms). And the
  solves are a small share of placement anyway: 1.15 s of sha256's 259 s
  (0.4%). The time is in the refining anneal (245 s), which is
  sequential per seed.
- Routing: the congestion update over every node is 3 ms a PathFinder
  iteration on one CPU thread. Iterations take seconds, nearly all of it
  A* searches, which are pointer-chasing and sequential per net. The GPU
  kernel itself is fast (2.8 ms against 15 ms at 4M nodes), but copying
  the occupancy each iteration makes it slower than the CPU (191 ms).
- Revisit when the analytic placer works at LE level on a large device
  (30K+ objects, an ePlace-style density gradient), or with a
  GPU-friendly router (batched, partitioned nets).
- HIP is not tested here (no AMD GPU on this machine). On the AMD
  desktop: `cd experiments/gpu-bench && nix develop ../.. --command
  cargo run --release -- hip`.

## Verification

VERIFICATION

## Left for later

- The xc7 back end doesn't use `chipsmith-place` yet. Its annealer could
  move into the crate as well.
- The refining anneal is now the largest share of placement on large
  designs (sha256: 245 of 259 s; picorv32x24 similar). Ideas: fewer
  seeds on large designs, an incremental timing update instead of the
  full `reweight` each temperature, and moving only critical clusters.
- picorv32x24's synthesis (58 s) and packing (10 s) are the next large
  costs on the biggest design. Not profiled.
- Incremental placement of the unchanged parts of a design (ADR-0011,
  considered options) needs stable cell names out of synthesis.
- With the parallel first iteration rejected, routing is
  single-threaded. Spatially partitioned batches of nets, with each
  search kept inside its box, could run side by side deterministically.

## Commands

```
# bench: one row per example (stage seconds, LEs, placed WL, pips,
# iterations, placed/routed critical ns, Fmax) plus <out>/<ex>.flow.log
nix develop --command bench/speed.sh target/release/chipsmith /tmp/out [example ...]

# place and route from a packed.json only (de2_pnr); settings via env
CHIPSMITH_PLACE_TRACE=1 target/release/examples/de2_pnr [--place-only | --route-only] examples/picorv32 /tmp/scratch-with-packed.json

# router trace on a fixed placement
CHIPSMITH_ROUTE_TRACE=1 target/release/examples/de2_pnr --route-only examples/sha256 <dir with placed.json>

# GPU bench (outside the workspace)
cd experiments/gpu-bench && nix develop ../.. --command cargo run --release -- cuda   # or hip
```

Heavy jobs run under
`systemd-run --user --scope -p MemoryMax=20G -p MemorySwapMax=0`.
