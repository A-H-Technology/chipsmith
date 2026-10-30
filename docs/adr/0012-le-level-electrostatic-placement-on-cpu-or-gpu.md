# Global placement at LE level by electrostatics, on CPU threads or a GPU

Placement takes most of a large build, and 91 to 98% of it is the refining anneal: 8 seeds, N^(4/3) moves per temperature (docs/gpu-pnr.md, "Baseline"; picorv32x24: 1,345 of 1,375 s). The anneal has that much to do because the analytic placement it starts from is coarse. LEs are clustered into LABs before anything is placed, and the quadratic solve then places about 2,000 clusters. On sha256 most clusters are a single register, and the anneal has to find their LUTs a LAB. A GPU can't help with that flow: the solve is too small to leave the launch-bound regime and is 0.4% of the time, and the anneal is sequential per seed (docs/fast-pnr-status.md, "GPU").

Global placement now works on every LE: an electrostatic placer (ePlace's density model, elfPlace's handling of FPGA resources) with one density field per resource. The resources are LE slots, register control sets and block sites. The control-set field is what lets an LE-level placer respect a LAB's single set of register controls. A greedy legaliser then puts each LE in a LAB, and a short, cold anneal refines the result. The placer's kernels have three implementations: serial, parallel on CPU threads, and on a GPU through CubeCL (CUDA or HIP). The backend is selected with `CHIPSMITH_ACCEL`, and the default is CPU.

## Considered options

- Keep the flow and run the 8 anneals faster. Parallel annealing on a GPU needs moves that don't share nets in flight, so it either serialises or changes the algorithm into something that isn't the measured anneal. It also leaves the coarse start.
- Move the quadratic (bound-to-bound) solve to LE level and keep recursive bisection for spreading. Its cost scales well, but bisection spreads by area alone. It can't express "one register control set per LAB", which is the binding constraint on sha256. Electrostatic density with per-resource fields can.
- Cluster first, then place clusters electrostatically: this is still the coarse start.
- Choose the backend automatically when a GPU is present. A GPU's floating point rounds differently from the CPU's, so the same binary would place the same design differently on two machines. Selecting the backend explicitly keeps a fixed binary, design, seed and setting deterministic, and the build cache keys on the setting.

## Consequences

- Below about 30,000 objects the GPU is launch-bound, so the gain from a GPU on these designs is bounded. Most of the speed-up comes from the algorithm, which runs the same way on CPU threads. docs/gpu-pnr.md reports serial CPU, parallel CPU and GPU separately.
- GPU and CPU placements differ, and each is repeatable on its own.
- The old cluster flow stays. It is the fallback when the LE legaliser can't fit a design, and `CHIPSMITH_PLACER=cluster` selects it for comparison.
- CubeCL is an optional dependency of `chipsmith-place` (the `gpu` feature). Builds without the feature don't compile it, but Cargo.lock lists it, so nix fetches its sources.
