# CLAUDE.md

## Project overview

chipsmith is a Rust CLI tool that manages Intel/Altera FPGA toolchains (Quartus
Prime, Quartus II) and builds FPGA projects from a declarative `chipsmith.toml`
manifest. It handles downloading, installing, patching (NixOS), and running
Quartus tools.

Read [CONTEXT.md](CONTEXT.md) first — it is the domain glossary, and the names
in it (Manifest, Product, Build Plan, Bitstream, Spawn Strategy, Timing Corner)
are the names used throughout the code.

## Build and test

```bash
cargo check                    # type check
cargo test --workspace         # run all tests (82 tests across 5 crates)
cargo fmt                      # format
cargo fmt --check              # verify formatting
cargo clippy --workspace --all-targets -- -D warnings
```

Clippy is enforced in CI. On NixOS, run anything missing through
`nix shell nixpkgs#<pkg>` or `nix run nixpkgs#<pkg>`.

## Architecture

**Crate dependency graph:**

```
chipsmith-cli
  -> chipsmith-core
       -> chipsmith-quartus-common          <- all Quartus behaviour
            -> chipsmith-toolchain
       -> chipsmith-quartus-prime  \  product
       -> chipsmith-quartus-ii-13  /  descriptions
```

- **chipsmith-toolchain**: Foundational crate, no Quartus knowledge. The
  `Toolchain` trait (async, object-safe via `async_trait`), `Manifest` parsing,
  `TimingSummary`, project scaffolding, and `ChipsmithError`.
- **chipsmith-quartus-common**: Every Quartus behaviour, once. Install and
  download, QSF/QPF/SDC generation, build planning, `.sta.summary` parsing,
  NixOS compatibility, the `ProcessHost` seam, and the single `QuartusToolchain`
  implementation of the trait.
- **chipsmith-quartus-prime** / **chipsmith-quartus-ii-13**: Data, not code.
  Each is one `QuartusProduct` constant. Adding a backend means adding a
  constant, not a crate's worth of logic.
- **chipsmith-core**: Backend registry (`resolve_backend`, `PRODUCTS`) plus the
  two commands driven by a Manifest rather than by flags (`build`, `flash`).
- **chipsmith-cli**: Binary. Uses `figue`/`facet` for arg parsing, owns
  presentation and exit-code policy.

## Key patterns

- **The variance lives in data.** `QuartusProduct` holds the four things that
  differ between Quartus Prime and Quartus II: version table, install root,
  installer flags, `SpawnStrategy`. Everything else is shared. If you find
  yourself adding a `match` on the backend name inside `quartus-common`, it
  probably wants to be a field on `QuartusProduct` instead.
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
- **Every subprocess goes through `ProcessHost`.** Including `is_nixos()` and
  `path_exists()`. `RealHost` in production, `RecordingHost` in tests — that is
  what makes the NixOS and non-NixOS paths both reachable, since no single
  machine can run both.

## Where things live

| What | Where |
|------|-------|
| Domain glossary | `CONTEXT.md` |
| Toolchain trait, BuildOutcome | `crates/chipsmith-toolchain/src/toolchain.rs` |
| Error types | `crates/chipsmith-toolchain/src/error.rs` |
| Manifest parsing | `crates/chipsmith-toolchain/src/manifest.rs` |
| Timing types | `crates/chipsmith-toolchain/src/timing.rs` |
| `chipsmith init` | `crates/chipsmith-toolchain/src/scaffold.rs` |
| Product descriptors | `crates/chipsmith-quartus-common/src/product.rs` |
| Process seam + test fake | `crates/chipsmith-quartus-common/src/process.rs` |
| Toolchain implementation | `crates/chipsmith-quartus-common/src/toolchain.rs` |
| Build plan + layout | `crates/chipsmith-quartus-common/src/build.rs` |
| QSF generation | `crates/chipsmith-quartus-common/src/qsf.rs` |
| SDC / timing constraints | `crates/chipsmith-quartus-common/src/sdc.rs` |
| `.sta.summary` parsing | `crates/chipsmith-quartus-common/src/timing.rs` |
| Download + caching | `crates/chipsmith-quartus-common/src/download.rs` |
| Install orchestration | `crates/chipsmith-quartus-common/src/install.rs` |
| Tool + installer spawning | `crates/chipsmith-quartus-common/src/runner.rs` |
| NixOS patching | `crates/chipsmith-quartus-common/src/nixos.rs` |
| Version tables | `crates/chipsmith-quartus-{prime,ii-13}/src/lib.rs` |
| Backend registry | `crates/chipsmith-core/src/lib.rs` |
| CLI commands | `crates/chipsmith-cli/src/main.rs` |

## Conventions

- Commit messages follow conventional commits: `feat:`, `fix:`, `refactor:`,
  `chore:`, etc. No Co-Authored-By lines.
- This is a NixOS machine. Use `nix shell nixpkgs#<pkg>` instead of apt/brew.
- Tests use `#[cfg(test)] mod tests` inline in each module.
- The `facet` ecosystem is used for serialization/deserialization (not serde).
- Prefer a test through the interface over a test of an extracted helper. If
  something can only be tested by reaching past the interface, the module is
  probably the wrong shape.
