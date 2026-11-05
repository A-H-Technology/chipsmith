# CLAUDE.md

## Project overview

chipsmith-vendor (this crate and the other `crates/vendor-*`) is the
original chipsmith, kept for installing and running the vendor tools that the
open toolchain is tested and reverse-engineered against. It is a Rust CLI tool
that manages Intel/Altera FPGA toolchains (Quartus
Prime, Quartus II) and builds FPGA projects from a declarative `chipsmith.toml`
manifest. It handles downloading, installing, patching (NixOS), and running
Quartus tools.

Read [CONTEXT.md](CONTEXT.md) first — it is the domain glossary, and the names
in it (Manifest, Product, Build Plan, Bitstream, Spawn Strategy, Timing Corner)
are the names used throughout the code.

## Build and test

```bash
cargo check                    # type check
cargo test --workspace         # run all tests 
cargo fmt                      # format
cargo fmt --check              # verify formatting
cargo clippy --workspace --all-targets -- -D warnings
```

Clippy is enforced in CI. On NixOS, `nix develop` provides Rust, `just`, `ghdl`
and the tools chipsmith shells out to; run anything else missing through
`nix shell nixpkgs#<pkg>` or `nix run nixpkgs#<pkg>`.

The examples are runnable: `cd crates/vendor-cli/example && just testbench` exercises the whole
simulation path on GHDL, without a Quartus install.

`nix build .#checks.x86_64-linux.vendor-e2e-quartus-ii-13` is the end-to-end proof of
the nix integration: a NixOS VM installs Quartus II 13.0sp1, declares it via
the module and builds `crates/vendor-cli/example-de2` as a derivation. It needs KVM, ~10 GB of
store and well over half an hour; run it when touching `nix/vendor/`, `nixos.rs`,
`runner.rs` or the install path.

## Architecture

**Crate dependency graph:**

```
chipsmith-vendor (crates/vendor-cli)
  -> chipsmith-core
       -> chipsmith-quartus-common          <- all Quartus behaviour
            -> chipsmith-sim                <- ModelSim/Questa is a Quartus component
            -> chipsmith-toolchain
       -> chipsmith-sim                     <- simulator-agnostic test running
       -> chipsmith-quartus-prime  \
       -> chipsmith-quartus-pro     > product descriptions
       -> chipsmith-quartus-ii-13  /
```

- **chipsmith-toolchain**: Foundational crate, no Quartus knowledge. The
  `Toolchain` trait (async, object-safe via `async_trait`), `Manifest` parsing,
  `TimingSummary`, project scaffolding, the `ProcessHost` seam, and
  `ChipsmithError`.
- **chipsmith-sim**: The `Simulator` trait, `SimPlan`, the shared `execute` that
  turns exit codes into Test Verdicts, and the GHDL implementation. No vendor
  knowledge.
- **chipsmith-quartus-common**: Every Quartus behaviour, once. Install and
  download, QSF/QPF/SDC generation, build planning, `.sta.summary` parsing,
  NixOS compatibility, the single `QuartusToolchain` implementation of the
  trait, and `QuartusSimulator` — which lives here rather than in
  `chipsmith-sim` because `vsim` is a vendor binary and needs the same NixOS
  dressing as every other one.
- **chipsmith-quartus-prime** / **-pro** / **-ii-13**: Data, not code.
  Each is one `QuartusProduct` constant. Adding a backend means adding a
  constant, not a crate's worth of logic.
- **chipsmith-core**: Backend and Simulator registries (`resolve_backend`,
  `PRODUCTS`) plus the three commands driven by a Manifest rather than by flags
  (`build`, `test`, `flash`).
- **chipsmith-vendor** (`crates/vendor-cli`): Binary. Uses `figue`/`facet` for arg parsing, owns
  presentation and exit-code policy.

## Key patterns

- **The variance lives in data.** `QuartusProduct` holds the things that
  differ between Quartus Prime Lite, Prime Pro and Quartus II: version table
  (including archives the installer reads beside itself), install root,
  installer flags, `SpawnStrategy`, synthesis tool, and the
  `BundledSimulator` it ships.
  Everything else is shared. If you find yourself adding a `match` on the
  backend name inside `quartus-common`, it probably wants to be a field on
  `QuartusProduct` instead.
- **Parse, don't validate.** `Manifest::parse` is the only place manifest rules
  are enforced. A `Manifest` that exists is valid: `ToolchainSpec` is a
  `{ backend, version }` pair that cannot hold anything else, and `clocks` are
  already resolved to periods. Downstream code does not re-check.
- **Plan, then do.** `build::plan_build` is pure and returns every file with its
  contents; `materialize` writes them. Assert on the plan, not on a scratch
  directory. `BuildLayout` is the single owner of where artifacts live — never
  re-derive `build/output_files/<name>.sof` anywhere else.
- **Verdicts travel out.** `build` returns `BuildOutcome { bitstream, timing }`.
  The library reports; the CLI decides what to print and what to exit with.
- **Every subprocess goes through `ProcessHost`.** Including `nix_compat()` and
  `path_exists()`. `RealHost` in production, `RecordingHost` in tests — that is
  what makes the NixOS and non-NixOS paths both reachable, since no single
  machine can run both.

## Where things live

| What | Where |
|------|-------|
| Domain glossary | `CONTEXT.md` |
| Toolchain trait, BuildOutcome | `crates/vendor-toolchain/src/toolchain.rs` |
| Error types | `crates/vendor-toolchain/src/error.rs` |
| Manifest parsing | `crates/vendor-toolchain/src/manifest.rs` |
| Timing types | `crates/vendor-toolchain/src/timing.rs` |
| `chipsmith-vendor init` | `crates/vendor-toolchain/src/scaffold.rs` |
| Product descriptors | `crates/vendor-quartus-common/src/product.rs` |
| Process seam + test fake | `crates/vendor-toolchain/src/process.rs` |
| Simulator trait, verdicts, `execute` | `crates/vendor-sim/src/lib.rs` |
| GHDL simulator | `crates/vendor-sim/src/ghdl.rs` |
| ModelSim/Questa simulator | `crates/vendor-quartus-common/src/simulator.rs` |
| Simulator selection | `SimulatorChoice` in `crates/vendor-toolchain/src/manifest.rs` |
| Example testbenches + `just` recipes | `crates/vendor-cli/example{,-de2}/tb/`, `.../justfile` |
| Toolchain implementation | `crates/vendor-quartus-common/src/toolchain.rs` |
| Build plan + layout | `crates/vendor-quartus-common/src/build.rs` |
| QSF generation | `crates/vendor-quartus-common/src/qsf.rs` |
| SDC / timing constraints | `crates/vendor-quartus-common/src/sdc.rs` |
| `.sta.summary` parsing | `crates/vendor-quartus-common/src/timing.rs` |
| Download + caching | `crates/vendor-quartus-common/src/download.rs` |
| Install orchestration | `crates/vendor-quartus-common/src/install.rs` |
| Tool + installer spawning | `crates/vendor-quartus-common/src/runner.rs` |
| NixOS patching | `crates/vendor-quartus-common/src/nixos.rs` |
| Version tables | `crates/vendor-quartus-{prime,pro,ii-13}/src/lib.rs` |
| Backend registry | `crates/vendor-core/src/lib.rs` |
| CLI commands | `crates/vendor-cli/src/main.rs` |
| Nix Compat store paths (one list, two readers) | `crates/vendor-quartus-common/compat.nix` |
| Flake package, NixOS module, `lib.buildProject` | `nix/package.nix`, `nix/module.nix`, `nix/build-project.nix` |
| VM end-to-end test | `nix/tests/default.nix` |

## Conventions

- Commit messages follow conventional commits: `feat:`, `fix:`, `refactor:`,
  `chore:`, etc. No Co-Authored-By lines.
- This is a NixOS machine. Use `nix shell nixpkgs#<pkg>` instead of apt/brew.
- Tests use `#[cfg(test)] mod tests` inline in each module.
- The `facet` ecosystem is used for serialization/deserialization (not serde).
- Prefer a test through the interface over a test of an extracted helper. If
  something can only be tested by reaching past the interface, the module is
  probably the wrong shape.
