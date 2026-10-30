# Incremental builds skip a step whose inputs' contents haven't changed

A `chipsmith build` runs its steps one after another, each writing a file the next one reads (`netlist.json`, `packed.json`, `placed.json` and `routed.json`, the `.rbf`). Every build ran every step, even when nothing had changed. On sha256 that is about 2 minutes of placement and routing again after an edit to a pin comment.

Each in-process step now leaves a stamp next to its first output, `<output>.stamp` (`crates/chipsmith/src/cache.rs`). The stamp holds:

- a key: this executable's contents, every `CHIPSMITH_*` variable, and the step as planned (its `Debug` form, which has its paths and settings). The variables include the databases' nix store paths and the placer's and router's tuning knobs.
- the content hash of every file the step read. For synthesis that is every file the Verilog front end opened: the sources, `` `include ``d files and `$readmem` data (`Frontend::files_read`). For the other steps it is their one input file.
- the lines the step reported.

When a later build plans the same step, the step is skipped if the key matches, all of its outputs exist and every recorded input still has the same hash. The build then prints `up to date` and replays the step's report. `CHIPSMITH_CACHE=0` runs every step.

Inputs are compared by content, not by date. A step whose input was rebuilt but came out byte-identical is therefore skipped, and so is everything after it. A comment edit re-runs synthesis, gives the same netlist, and skips pack, place-and-route and the bitstream. This works because every step is deterministic: the placer's parallel anneals and the router's searches give the same result whatever the thread timing, and the tests check that.

## Considered options

- File modification times, as make uses: a `touch` or a `git checkout` would rebuild everything, and a rebuilt but unchanged netlist would still re-run placement.
- One key over all of a build's inputs: any change would re-run everything, which is the case this ADR is for.
- Caching inside a step, for example reusing the placement of the parts of a netlist that didn't change (incremental placement, as Quartus' design partitions do). It saves time when part of a design changes, which is the common edit. It needs a netlist diff with stable cell names, which synthesis doesn't give yet: a one-line change renames `$auto$` cells all over the design. It is left for later, on top of this.
- A shared, content-addressed cache across projects (like ccache): no use case yet. The stamps work the same way and could move there.

## Consequences

- A build after a no-op edit costs reading and hashing the files: milliseconds plus about 30 ms to hash the executable once. The devshell's `nix develop` start-up costs more than that.
- The key covers the executable, so a rebuilt chipsmith re-runs every step even when the change didn't touch them. That is correct, but slower than keying on each stage's code version. Stage versions would be one more thing to keep in step by hand.
- External tools (GHDL for VHDL, the 7-series flow's yosys and nextpnr) are not cached: chipsmith doesn't know what they read. GHDL's Verilog output does feed synthesis, so the steps after it still skip when that output is unchanged.
- A stamp records 64-bit hashes. Two different inputs hashing the same is not a concern for files a build writes, but it is not a security boundary.
- Timings in a replayed report are those of the run that wrote the stamp.
