# fast-pnr: status and handoff (2026-10-29, WIP)

Branch `fast-pnr`, base `open` 385aa15. Goal: compile speed without losing
quality (see the brief: profile first, analytic + annealer placement,
rayon, router speed, GPU verdict with numbers, ADR on incremental
compilation, quality table in the README).

## What is in the tree

- `crates/chipsmith-place`: device-generic analytic global placer
  (bound-to-bound quadratic model, Jacobi-preconditioned CG with rayon over
  rows, recursive-bisection spreading, anchors). The device interface is the
  `Legalizer` trait (`legalize`, `reweight`) plus a `Problem` (objects,
  fixed points, nets, bins). Has 3 unit tests (pass). The xc7 back end does
  not use it yet.
- `crates/chipsmith-cyclone2/src/place.rs`: the placer is now cluster
  LEs into LAB-sized clusters, run analytic placement, legalise into LAB
  slots (`LabLegalizer`), then a short refining anneal (`REFINE`: heat 0.1,
  range 2, moves 1). `best_of` runs 8 refining anneals from different seeds
  side by side and keeps the one with the shortest estimated critical
  path (`CHIPSMITH_PLACE_SEEDS` overrides the count). The knobs read from
  the environment for sweeping: `CHIPSMITH_PLACE_{HEAT,RANGE,MOVES,SEEDS,
  DENSITY,ANCHOR,ROUNDS,PATIENCE,TRACE}`. The old from-scratch initial
  placement (`State::initial`) is gone.
- Clustering fallbacks for near-full designs: `merge_clusters` (connected
  small clusters) always runs; `pack_clusters` (first-fit) and
  `cluster_with(.., fill = true)` (top clusters up with unrelated LUT-only
  LEs) run only when the clusters don't fit. **The fill/pack code was
  written but its build was interrupted: it has not been compiled.**
- `crates/chipsmith/src/flow.rs`: the routing graph and the bitstream
  device database load on threads of their own (`Prefetch`) while
  synthesis and placement run. The device database alone took 3-6 s,
  more than everything else together for small designs. Built, not
  benchmarked.
- `flake.nix`: the devshell's routing-graph cache is named after the
  database store path. The cache only checked its format, so this
  worktree had a graph from before the PLL/IOE pins: de2-io and
  de2-pll-counter failed to route ("bel X131Y30/IOE0 has no such pin",
  "no free PLL") until it was regenerated.
- `bench/speed.sh`: the bench (below).
- `examples/picorv32x24`: 24 picorv32 cores, about 28K LEs, the large
  benchmark.
- `experiments/gpu-bench`: a CubeCL (CUDA/HIP) microbench of one CG
  iteration (SpMV + dots + axpys, scalars kept on the device) and of
  PathFinder's congestion update, against 1-thread and rayon CPU, at
  2K-3M rows and 0.3-4M nodes. **Never compiled** (the first release
  build of CubeCL was interrupted). No GPU numbers yet.

## Measured so far

Loads on the machine were high (load average 15-23, other agent's yosys
proofs), so the times are rough. Quality numbers are deterministic.
"base" is 385aa15 (`/tmp/fast-pnr2/bench-base`). "WIP" is the analytic
placer with a single-seed refine, before `best_of`
(`/tmp/fast-pnr2/bench-wip`).

| design | base place s | base route s | base crit ns | WIP place s | WIP route s | WIP crit ns |
|---|---|---|---|---|---|---|
| picorv32 | 41.1 | 43.9 | 8.10 | 4.3 | 29.9 | 8.93 |
| femtorv32 | 21.1 | 21.5 | 13.74 | 2.0 | 14.3 | 13.18 |
| i2c-master | 3.5 | 2.9 | 7.90 | 0.6 | 2.6 | 7.38 |
| serv | 8.0 | 4.4 | 10.47 | 0.6 | 3.4 | 10.32 |
| m4k-tdp | 0.5 | 2.3 | 3.80 | 0.1 | 1.4 | 4.26 |
| sha256 | 1049 | 227 | 25.29 | 52.4 | 333.1 | 20.61 |
| picorv32x24 | about 720 | did not finish in more than 6 h | - | fails: doesn't fit | - | - |

The sha256 base row comes from the previous agent's log (`/tmp/fast-pnr/base-sha256-pnr.log`).
The picorv32x24 base row is the previous agent's run (pid 2532668, still routing after 7 h when this note was written).

Other findings:

- Most small examples are now bound by fixed costs: device database 3-6 s,
  routing graph 0.5-1 s (the prefetch above targets this).
- picorv32x24 synthesis takes 72.5 s and packing 14.6 s. Both are worth
  profiling.
- sha256 routing (333 s) is now the largest single stage.
- Refine-schedule sweep (`/tmp/fast-pnr2/sweep/results{1,2}.tsv`): the
  routed critical path moves about ±10% per design with the schedule and
  seed, and no schedule wins everywhere. Single-seed geomean against base
  is +1% (S0, the default) to +4% (worse). With 8 seeds:
  - M0 (default schedule): picorv32 8.95 (base 8.10), i2c 6.99 (7.90),
    uart 5.99 (6.05), m4k-tdp 4.36 (3.80), serv 10.32 (10.47),
    femtorv32 13.18 (13.74), spi-master 4.90.
  - M1 (heat 1, range 4, moves 2): picorv32 7.94, m4k-tdp 3.80, i2c 8.19,
    uart 6.15, serv 10.38, femtorv32 13.05, spi-master 4.71, with about
    5x the placement time.
  - The picorv32 and m4k-tdp regressions under the default schedule
    block making it the default (quality gate). Either M1 becomes the
    default, or the analytic path is opt-in.
- picorv32x24 under the WIP: 1,794 clusters + 361 chain LABs > 2,076
  LABs. A LAB takes one enable and the design has 674 enables, so
  singleton register clusters pile up. `merge_clusters` alone got it to
  1,779. The fill fallback above is meant to fix it but has not been run.

## Not verified

- Release tests: lib/bins/examples passed (`LIBEXIT=0`). The integration
  tests (`--tests --test-threads=1`) were stopped partway through the
  m4k shape proofs; everything up to them passed. They have not been run
  on the current tree.
- clippy: not run.
- Quartus byte-identical checks: not run. Setup that works: a copy of
  cyclone2-re's `tools/` in `/tmp/fast-pnr2/c2re`, with
  `tools/cram/target/release/cram` symlinked to the nix-built cram of
  the locked cyclone2-re (`/nix/store/92wwpsb78rsxw8g366z8q4rfn6ljk7p9-cram-0.1.0`).
  The cram in /pool/dev/cyclone2-re is from Oct 26, from before M4K.
  Run `CYCLONE2_RE=/tmp/fast-pnr2/c2re bench/quartus-validate.sh <bench out>/examples/<name>`.
- README quality table, ADR: not written.

## Not started

- Router speedups (profile first: sha256 and picorv32x24). Ideas: cache
  the timing graph's topological order (`timing::Graph::analyse`
  rebuilds fanout lists and Kahn's order every call); for high-fanout
  nets, seed the A* from tree nodes near the sink only; route spatially
  disjoint nets in parallel.
- Profiling with perf: `nix shell nixpkgs#perf`. Build with
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` into a separate target dir.
- GPU verdict. Expectation, not measured: the analytic problem has
  2-3K objects, so a CG iteration is microseconds on the CPU and
  kernel-launch-bound on a GPU. The bench is there to show this with
  numbers.
- ADR 0011 (0010 is taken on the `mult` branch) on incremental
  compilation: what each stage can cache and its key (netlist, packed,
  placement keyed on inputs plus settings; routing graph keyed on the
  database store path, done here).
- Moving the generic anneal into chipsmith-place for xc7.

## Next concrete step

1. Build and fix compile errors in the fill/pack clustering. Then get
   picorv32x24 to place and route with `de2_pnr`.
2. Settle the default placer by quality: run `bench/speed.sh` over all
   examples with M0 and M1 (8 seeds). If neither matches base on picorv32
   and m4k-tdp, restore the old initial placement plus the full anneal
   as the default and make the analytic path opt-in.
3. Run the release tests and clippy, commit, push.
4. Profile the router, then go on to the router, GPU and ADR items in
   order.

## Commands

```
# bench: one row per example (stage seconds, LEs, placed WL, pips,
# iterations, placed/routed critical ns, Fmax) plus <out>/<ex>.flow.log
nix develop --command bench/speed.sh target/release/chipsmith /tmp/out [example ...]

# place and route from a packed.json only (de2_pnr); settings via env
CHIPSMITH_PLACE_TRACE=1 CHIPSMITH_PLACE_SEEDS=8 \
  target/release/examples/de2_pnr [--place-only] examples/picorv32 /tmp/scratch-with-packed.json

# baseline binary (385aa15), already built
/tmp/fast-pnr2/base-src/target/release/chipsmith

# GPU bench (outside the workspace)
cd experiments/gpu-bench && CARGO_TARGET_DIR=/tmp/fast-pnr2/gpu-target \
  nix develop ../.. --command cargo run --release -- cuda   # or hip
```

Heavy jobs run under
`systemd-run --user --scope -p MemoryMax=20G -p MemorySwapMax=0`. Run
integration tests with `--test-threads=1`, so only one yosys runs at a time.
