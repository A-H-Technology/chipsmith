# fast-pnr: compile speed without losing quality

Branch `fast-pnr`, from `open` 385aa15, with `open` 33fb8a4 (multipliers,
falling-edge registers) merged in. Roadmap item 3, "optimization":
an analytic placer, a faster router, incremental builds, and a measured
verdict on GPUs. This note records what changed, the measurements behind
each choice, and what is left.

The sweeps that chose the router's and the placer's settings ran on the
shared 8-core machine while another agent ran yosys proofs and Quartus
(load average 8 to 26), so their times are rough. The final speed table
(Results) comes from back-to-back runs on a quiet machine. Routed critical
paths are deterministic for a given binary, design and seed, and the
quality decisions rest on them.

## What changed

### Placement (`crates/chipsmith-place`, `crates/chipsmith-cyclone2/src/place.rs`)

1. LEs are clustered into LAB-sized groups of connected, compatible LEs
   (`cluster`), as VPR's packer does. When the clusters don't fit, small
   ones are merged (`merge_clusters`), topped up with unrelated LUT-only
   LEs (`cluster_with(.., fill = true)`) and packed first-fit
   (`pack_clusters`).
2. `chipsmith_place::place` places the clusters, carry chains and blocks
   (M4Ks, multipliers) analytically: a bound-to-bound quadratic wirelength model solved with
   Jacobi-preconditioned CG (rows in parallel with rayon), recursive
   bisection spreading, and anchors to the last legal placement. It is
   device-generic: a device implements the `Legalizer` trait (`legalize`,
   `reweight`) and describes a `Problem` (objects, fixed points, nets,
   bins). The xc7 back end doesn't use it yet.
3. `LabLegalizer` snaps the result onto LAB slots: chains first, then
   clusters largest first, each into the nearest LAB that `lab_ok`
   accepts. A small cluster may share a LAB with a carry chain's last LAB
   or with another small cluster. That is what lets picorv32x24 fit.
   Blocks go to the nearest free site of their own kind.
4. A LAB's register domain includes the clock's edge (`FfCtl::clk`), so
   clustering, legalising and annealing, which all go through `lab_check`,
   keep rising- and falling-edge registers in separate LABs.
5. A refining anneal over LAB slots (`REFINE`: heat 0.5, range 3 LABs,
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

Chosen before the merge, against `open` 385aa15, and re-checked after it
against 33fb8a4 (Results). Rule: no example's routed critical path is
worse than `open`'s beyond noise. Noise was measured as the spread over three anneal seeds
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

Per design, `open` 33fb8a4 (the multipliers and small-gaps branches
merged; patched with `CHIPSMITH_PLACE_SEED` in scratch only) against this
branch merged with it. Times are seed 0, the two binaries run back to
back on a quiet machine (load 4 to 11 from other services). Critical
paths are the median over seeds 0, 1 and 2, with the range in brackets.
Geomean of the medians x0.974; the worst are m4k-tdp x1.018 (see above),
mult-selftest x1.014 (15.80 ns against `open`'s worst seed 15.70) and fir
x1.008.

| design | LEs | place s | route s | build s | critical path ns, median of 3 seeds [range] |
|---|---|---|---|---|---|
| bidir | 29 | 0.1 → 0.3 | 0.3 → 0.3 | 5.1 → 4.4 | 3.37 [3.37-3.37] → 3.37 [3.37-3.37] |
| blinky-verilog | 26 | 0.4 → 0.4 | 0.2 → 0.3 | 4.8 → 4.2 | 3.37 [3.37-3.37] → 3.37 [3.37-3.37] |
| blinky-vhdl | 26 | 0.4 → 0.4 | 0.2 → 0.3 | 5.0 → 4.2 | 3.37 [3.37-3.37] → 3.37 [3.37-3.37] |
| crc32 | 118 | 0.8 → 0.6 | 1.3 → 0.6 | 7.3 → 4.4 | 3.87 [3.35-3.87] → 3.36 [3.24-3.47] |
| de2-io | 33 | 0.1 → 0.1 | 0.3 → 0.3 | 4.7 → 3.9 | 3.71 [3.71-3.71] → 3.71 [3.71-3.71] |
| de2-jtag-counter | 129 | 0.5 → 0.4 | 0.7 → 0.5 | 5.5 → 4.1 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
| de2-pll-counter | 321 | 2.0 → 1.0 | 1.4 → 0.8 | 7.8 → 4.3 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
| femtorv32 | 788 | 10.9 → 6.1 | 10.4 → 4.1 | 26.5 → 11.8 | 13.18 [12.97-13.74] → 13.15 [12.74-13.97] |
| fir | 534 | 3.6 → 2.2 | 5.8 → 4.2 | 13.8 → 7.6 | 13.44 [12.93-13.45] → 13.55 [13.24-13.57] |
| i2c-master | 214 | 1.6 → 1.0 | 1.7 → 0.9 | 7.9 → 4.5 | 7.87 [6.81-7.89] → 7.16 [7.03-7.77] |
| m4k-bytes | 98 | 0.3 → 0.3 | 0.8 → 0.9 | 5.3 → 4.2 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
| m4k-deep | 128 | 0.7 → 0.5 | 1.3 → 1.0 | 6.5 → 4.3 | 3.80 [3.80-3.98] → 3.80 [3.80-3.80] |
| m4k-dualclock | 112 | 0.3 → 0.3 | 0.8 → 0.8 | 5.4 → 4.1 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
| m4k-enables | 73 | 0.2 → 0.2 | 0.8 → 0.7 | 5.5 → 4.0 | 3.59 [3.59-3.59] → 3.59 [3.59-3.59] |
| m4k-mixed | 90 | 0.3 → 0.2 | 0.8 → 0.7 | 5.5 → 4.2 | 3.73 [3.73-3.73] → 3.73 [3.73-3.73] |
| m4k-outreg | 83 | 0.3 → 0.2 | 0.9 → 0.7 | 5.6 → 4.0 | 3.59 [3.59-3.79] → 3.59 [3.59-3.59] |
| m4k-selftest | 405 | 4.4 → 2.6 | 63.7 → 2.5 | 72.7 → 6.2 | 12.54 [12.26-12.73] → 11.60 [11.45-11.83] |
| m4k-shallow | 95 | 0.3 → 0.2 | 0.8 → 0.7 | 5.3 → 4.0 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
| m4k-tdp | 97 | 0.3 → 0.2 | 1.0 → 0.8 | 5.5 → 3.9 | 3.86 [3.80-3.91] → 3.93 [3.80-4.14] |
| mult-selftest | 1008 | 17.2 → 8.0 | 16.4 → 7.9 | 39.0 → 17.9 | 15.58 [15.03-15.70] → 15.80 [15.41-15.86] |
| negedge-selftest | 481 | 3.2 → 2.0 | 4.3 → 1.8 | 11.9 → 5.1 | 10.12 [9.51-10.85] → 9.72 [9.44-10.18] |
| picorv32 | 1171 | 20.2 → 8.9 | 19.0 → 6.5 | 45.6 → 17.5 | 8.17 [8.12-8.72] → 7.90 [7.89-8.12] |
| ram | 151 | 0.8 → 0.6 | 2.1 → 1.4 | 7.9 → 4.3 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
| ram-leds | 42 | 0.2 → 0.2 | 0.8 → 0.4 | 5.9 → 4.2 | 5.04 [4.77-5.15] → 4.03 [3.78-4.70] |
| ram_widths | 78 | 0.5 → 0.3 | 1.1 → 0.8 | 6.1 → 4.1 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
| serv | 345 | 3.8 → 1.5 | 2.8 → 1.2 | 13.2 → 4.4 | 10.37 [10.23-10.47] → 10.19 [10.19-10.61] |
| spi-master | 173 | 1.1 → 0.5 | 1.9 → 0.8 | 9.4 → 4.2 | 4.50 [4.36-4.69] → 4.00 [3.89-4.22] |
| uart | 164 | 0.9 → 0.5 | 1.1 → 0.6 | 6.4 → 4.1 | 5.85 [5.76-6.49] → 5.70 [5.53-5.83] |

The larger designs, one seed, same conditions:

| design | LEs | place s | route s | build s | critical path ns |
|---|---|---|---|---|---|
| sha256 | 6,046 | 608.5 → 63.1 | 149.3 → 45.0 | 769.3 → 115.3 | 24.18 → 20.99 |
| picorv32x24 | 28,449 | about 720 → 593.6 | did not finish in 7 h → 196.3 (36 iterations) | → 827.8 | → 10.65 |

picorv32x24's `open` numbers are from 385aa15 on a loaded machine
(placement about 720 s; the router ran for more than 7 h before it was
stopped). Under load (average about 25), this branch placed it in
1,870 s: 8 parallel anneals compete for the cores.

Schedules measured before the merge, against 385aa15 (all DE2
examples then, three seeds): S0 x0.986 with picorv32 x1.137; A x0.980,
worst x1.018; A with 4 parallel seeds instead of 8: x0.991, picorv32
x1.098, so 8 stay.

## GPU

`experiments/gpu-bench` (CubeCL 0.11.3, CUDA and HIP) builds under nix
(`nix develop` provides the CUDA toolkit and ROCm's HIP runtime). Run on
this machine's GTX 1660, CPU numbers on the loaded 8-core host:

One CG iteration (sparse matrix-vector product, 3 dot products, 3 vector
updates; 12 entries a row), microseconds:

| rows | CPU, 1 thread | CPU, rayon | GTX 1660, data on the device (10 kernel launches) |
|---|---|---|---|
| 2,000 | 231.9 | 1,635.4 | 413.4 |
| 30,000 | 4,209.5 | 2,869.2 | 412.6 |
| 300,000 | 31,501.9 | 17,941.7 | 703.4 |
| 3,000,000 | 349,914.9 | 123,863.1 | 12,129.5 |

PathFinder's congestion update over every routing node, microseconds:

| nodes | CPU, 1 thread | CPU, rayon | GPU kernel only | GPU with the occupancy upload and the count download |
|---|---|---|---|---|
| 300,000 | 762.1 | 1,055.8 | 168.5 | 3,370.8 |
| 1,000,000 | 3,211.1 | 2,690.0 | 2,365.2 | 11,227.3 |
| 4,000,000 | 15,391.2 | 14,518.7 | 2,779.5 | 191,411.8 |

The CPU numbers are on a loaded machine, so rayon loses to one thread at
the small sizes. The comparison that matters doesn't depend on that: at
chipsmith's sizes the GPU is launch-bound.

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

All on the merged tree (this branch with `open` 33fb8a4) unless noted.

- Tests: `cargo test --release --workspace --lib --bins --examples` pass;
  the integration tests of chipsmith-verilog (semantics 23, yosys 15),
  chipsmith-netlist and chipsmith-synth pass; the cyclone2 synthesis
  suite (`--test synth`, 129 tests): SYNTH_RESULT. The suite doesn't
  place or route; it covers the Verilog front end's change
  (`Frontend::files_read`) and the merge.
- clippy on chipsmith-route, chipsmith-place, chipsmith-cyclone2,
  chipsmith and chipsmith-verilog (all targets): clean.
- Determinism: route-only output is byte-identical with the node-layout
  change, with incremental rip-up off, and at 1 and 8 router threads
  (when the parallel first iteration was tried); the default placement
  and bitstream are identical on a rebuild (before the merge
  de2-pll-counter, de2-jtag-counter, m4k-selftest, picorv32, m4k-tdp and
  uart; after it fir, mult-selftest, negedge-selftest and crc32).
- Quartus byte-identical (`bench/quartus-validate.sh` with cyclone2-re
  a0cba39's `cram`, the locked revision): 26 of the 28 DE2 examples.
  i2c-master (6 I/O bits) and uart (2 I/O bits) differ, as they do on
  `open` (README, "not investigated"); in i2c-master Quartus drops two
  constant-1 LUTs that feed register data inputs. sha256: SHA_RESULT.
  Placement changed every bitstream, so this is a check of new
  placements and routes, not of unchanged ones.
- On the DE2, over JTAG (`de2_user1`), merged builds: negedge-selftest
  "NEG!" with 0 errors, its counter at 50.00 MHz; mult-selftest "MUL!"
  with 0 errors, passes rising at about 43,600 a second; m4k-selftest
  "M4K!" with 0 errors; de2-pll-counter at 50.00, 100.0000 and
  40.0000 MHz. Before the merge: de2-pll-counter and m4k-selftest the
  same.
- Incremental builds: a second build of i2c-master skips every step; a
  comment added to a source re-runs synthesis only (same netlist), and
  the cache's unit tests cover a changed input, a changed key, a missing
  output and a failed step.

## Left for later

- The xc7 back end doesn't use `chipsmith-place` yet. Its annealer could
  move into the crate as well.
- The refining anneal is now the largest share of placement on large
  designs (sha256: 245 of 259 s; picorv32x24 similar). Ideas: fewer
  seeds on large designs, an incremental timing update instead of the
  full `reweight` each temperature, and moving only critical clusters.
- With 8 anneals in parallel, placement uses all cores: on a loaded
  machine the small designs place no faster than with `open`'s single
  anneal, and picorv32x24 took 1,870 s. A design-size-dependent seed
  count needs its own quality sweep (4 seeds failed picorv32).
- picorv32x24's synthesis (28 s quiet, 58 s loaded) and packing (6 s) are the next large
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
