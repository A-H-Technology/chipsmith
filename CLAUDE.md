# CLAUDE.md

## Project overview

chipsmith is a Rust CLI tool that manages Intel FPGA toolchains (Quartus Prime, Quartus II) and builds FPGA projects from a declarative `chipsmith.toml` manifest. It handles downloading, installing, patching (NixOS), and running Quartus tools.

## Build and test

```bash
cargo check                    # type check
cargo test --workspace         # run all tests (40 tests across 4 crates)
cargo fmt                      # format
cargo fmt --check              # verify formatting
```

Clippy is enforced in CI but may not be installed locally (NixOS):
```bash
cargo clippy --workspace -- -D warnings
```

## Architecture

**Crate dependency graph:**
```
chipsmith-cli
  -> chipsmith-core
       -> chipsmith-quartus-prime  -> chipsmith-quartus-common -> chipsmith-toolchain
       -> chipsmith-quartus-ii-13  -> chipsmith-quartus-common -> chipsmith-toolchain
```

- **chipsmith-toolchain**: Foundational crate. Defines the `Toolchain` trait (async, object-safe via `async_trait`), `Manifest` struct (parsed from `chipsmith.toml` via `facet_toml`), `ChipsmithError` enum, and `PinMapping`.
- **chipsmith-quartus-common**: Shared Quartus-family logic. Download with caching (`~/.cache/chipsmith/`), QSF/QPF/SDC file generation, build step orchestration (`prepare_build` returns a `BuildPlan`), `quartus_sta` summary parsing, version types (`KnownVersion`), and device support installation.
- **chipsmith-quartus-prime**: Quartus Prime backend. Version table (22.1, 23.1, 24.1), install to `~/intelFPGA_lite/<ver>/`, NixOS compat via `patchelf`.
- **chipsmith-quartus-ii-13**: Quartus II 13.0sp1 backend. Version table (13.0sp1), install to `~/altera/13.0sp1/`, NixOS compat via `bubblewrap` (32-bit).
- **chipsmith-core**: Public API. Routes backend name strings to `Box<dyn Toolchain>` via `resolve_backend()`. All CLI commands go through here.
- **chipsmith-cli**: Binary crate. Uses `figue`/`facet` for arg parsing. Thin dispatch to `chipsmith-core` functions.

## Key patterns

- **Backend dispatch**: `chipsmith-core::resolve_backend("quartus-prime")` returns `Box<dyn Toolchain>`. Adding a new backend means: create a crate, implement `Toolchain`, add one match arm.
- **Build orchestration**: `chipsmith_quartus_common::build::prepare_build()` creates the build directory, writes QSF/QPF files, and returns a `BuildPlan` with tool steps. Each backend runs the steps through its own runner (for NixOS compat differences).
- **NixOS compat**: `nixos.rs` in each backend resolves nix store paths via `nix eval`, patches ELF interpreters with `patchelf`, and fixes `#!/bin/bash` shebangs. The quartus-ii-13 backend additionally uses `bubblewrap` for 32-bit binaries.
- **Default trait methods**: `flash` is implemented as a default method on `Toolchain` — it delegates to `run_tool("quartus_pgm", ...)` so backends get it for free.

## Where things live

| What | Where |
|------|-------|
| Toolchain trait | `crates/chipsmith-toolchain/src/toolchain.rs` |
| Error types | `crates/chipsmith-toolchain/src/error.rs` |
| Manifest parsing | `crates/chipsmith-toolchain/src/manifest.rs` |
| QSF generation | `crates/chipsmith-quartus-common/src/qsf.rs` |
| SDC / timing constraints | `crates/chipsmith-quartus-common/src/sdc.rs` |
| Timing report parsing | `crates/chipsmith-quartus-common/src/timing.rs` |
| Download + caching | `crates/chipsmith-quartus-common/src/download.rs` |
| Build plan | `crates/chipsmith-quartus-common/src/build.rs` |
| Shared install types | `crates/chipsmith-quartus-common/src/install.rs` |
| Version tables | `crates/chipsmith-quartus-prime/src/install.rs`, `crates/chipsmith-quartus-ii-13/src/install.rs` |
| NixOS patching | `crates/chipsmith-quartus-prime/src/nixos.rs`, `crates/chipsmith-quartus-ii-13/src/nixos.rs` |
| CLI commands | `crates/chipsmith-cli/src/main.rs` |
| Public API | `crates/chipsmith-core/src/lib.rs` |

## Conventions

- Commit messages follow conventional commits: `feat:`, `fix:`, `refactor:`, `chore:`, etc.
- No Co-Authored-By lines in commits.
- This is a NixOS machine. Use `nix shell nixpkgs#<pkg>` instead of apt/brew.
- Tests use `#[cfg(test)] mod tests` inline in each module.
- The `facet` ecosystem is used for serialization/deserialization (not serde).
