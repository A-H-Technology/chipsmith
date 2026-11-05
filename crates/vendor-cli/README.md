# chipsmith-vendor

The original chipsmith, renamed: the open toolchain took the `chipsmith` name
(see the [top-level README](../../README.md)). It stays for installing and
running the vendor tools the open flow is tested and reverse-engineered
against.

A command-line FPGA toolchain manager and build system. Handles downloading, installing, and orchestrating Intel Quartus tools so you can go from VHDL source to a programmed FPGA with a single manifest file.

## Quick start

```bash
# Create a new project
chipsmith-vendor init --family "Cyclone V" --device 5CSEBA6U23I7

# Run the testbenches (GHDL by default — no Quartus needed)
chipsmith-vendor test

# Build (downloads Quartus automatically on first run)
chipsmith-vendor build

# Flash to FPGA
chipsmith-vendor flash
```

## Installation

Requires a recent stable Rust — CI builds against `stable`, which is what the
`facet` dependencies track.

```bash
cargo install --path crates/vendor-cli
```

On NixOS, `nix build .#chipsmith-vendor` builds it with `unzip` wrapped in
and the Nix Compat paths baked in. The repo's `nix develop` shell (the open
toolchain's) has Rust, `just` and `ghdl`; add `unzip` with
`nix shell nixpkgs#unzip` to install Device Support from a `cargo run`.

## `chipsmith.toml`

Every project is defined by a single manifest:

```toml
[project]
name = "blinky"
top = "blinky"

[toolchain]
quartus-prime = "23.1"

[target]
family = "Cyclone V"
device = "5CSEBA6U23I7"
io_standard = "3.3-V LVTTL"    # optional, applied to every pin

[hdl]
standard = "VHDL_2008"         # optional, default; also VHDL_1993, VHDL_1987
sources = ["src/*.vhd"]

[pins]
clk = "PIN_V11"
led = ["PIN_W15", "PIN_AA24", "PIN_V16", "PIN_V15"]

[clocks]
clk = "50 MHz"

[sim]                          # optional; without it there is nothing to test
sources = ["tb/*.vhd"]
testbenches = ["blinky_tb"]
```

The `[toolchain]` key selects the backend. Supported backends:

| Key | Toolchain | Supported versions |
|-----|-----------|-------------------|
| `quartus-prime` | Intel Quartus Prime Lite | 22.1, 23.1, 24.1 |
| `quartus-ii-13` | Altera Quartus II | 13.0sp1 |
| `quartus-pro` | Altera Quartus Prime Pro (Agilex 5 device support) | 26.1.1 |

`quartus-pro` 26.1.1 is the newest Pro release and supports the DE25-Nano's
`A5EB013BB23BE4SCS` with final timing. Agilex 5 E-series costs nothing, but
the Fitter still refuses to run without Altera's no-cost node-locked licence
(`quartus_agilex5e`); synthesis runs without one. The base install is the
`.run` plus its `-part2-*.qdz` (9.9 GB together), Agilex 5 adds
`agilex5-*.qdz` and `agilex_common-*.qdz` (7.3 GB), and the result is 32 GB.

Altera's CDN answers chipsmith's own downloader with a 403 (Akamai
fingerprints the TLS client, so a browser User-Agent is not enough). Until
that is solved, put the files in chipsmith's cache yourself, e.g.
`nix shell nixpkgs#curl-impersonate -c curl_chrome146 -O <url>` into
`$XDG_CACHE_HOME/chipsmith/`, and `chipsmith-vendor install` uses them.

Pin mappings can be a single string for one pin or an array for a bus.

### I/O standards

`target.io_standard` sets the I/O standard for every assigned pin. Most boards run
everything at one voltage, so that's usually all you need. Mixed-voltage banks get a
per-signal override:

```toml
[io-standards]
hps_clk = "1.5 V"
```

Omit both and chipsmith emits no `IO_STANDARD` assignments, leaving Quartus on its
device defaults.

### Timing constraints

`[clocks]` maps a top-level port to its frequency, written the way it appears on the
board silkscreen — `Hz`, `kHz`, `MHz` and `GHz` are all accepted:

```toml
[clocks]
clk = "50 MHz"
```

Chipsmith turns that into a `.sdc` file (`create_clock` plus `derive_clock_uncertainty`)
and points the project at it, so `quartus_sta` reports real slack instead of analysing an
unconstrained design. Every port named here must also appear in `[pins]`.

Leave `[clocks]` out and the design still compiles — the timing report just won't mean
anything.

## Commands

### `chipsmith-vendor init`

Scaffold a new project with `chipsmith.toml` and a stub VHDL entity.

```bash
chipsmith-vendor init
chipsmith-vendor init --family "Cyclone IV E" --device EP4CE22F17C6
chipsmith-vendor init --backend quartus-ii-13 --family "Cyclone II" --device EP2C35F672C6
```

Defaults to Cyclone V and the `quartus-prime` backend. Omitting `--version`
picks the selected backend's latest, so `--backend quartus-ii-13` scaffolds
`13.0sp1` without being told.

### `chipsmith-vendor install`

Download and install a toolchain version. Happens automatically on first `build`, but can be done explicitly.

```bash
chipsmith-vendor install                          # latest quartus-prime (23.1)
chipsmith-vendor install 24.1
chipsmith-vendor install --backend quartus-ii-13  # latest for that backend (13.0sp1)
chipsmith-vendor install --backend quartus-pro    # Quartus Prime Pro 26.1.1 + Agilex 5
chipsmith-vendor install --installer ./QuartusLiteSetup.run   # from local file
```

The version defaults to the chosen backend's latest, not to a single global
default.

### `chipsmith-vendor build`

Run the full synthesis pipeline (map, fit, asm, timing analysis). Produces a `.sof` file in `build/output_files/`.

Afterwards chipsmith reads the `quartus_sta` summary and reports the worst-case slack.
A design that misses timing still produces a usable `.sof`, so this is a warning rather
than a build failure — but it won't pass silently. Pass `--require-timing` to make it
an error instead, which is what you want in CI.

```bash
chipsmith-vendor build
chipsmith-vendor build --project-dir path/to/project
chipsmith-vendor build --require-timing           # exit non-zero if timing is missed
```

### `chipsmith-vendor test`

Run the project's testbenches under a simulator and report a verdict for each.

```bash
chipsmith-vendor test
chipsmith-vendor test --testbench blinky_tb    # just one
chipsmith-vendor test --simulator ghdl         # override what chipsmith.toml says
```

Testbenches are declared in `[sim]`:

```toml
[sim]
simulator = "quartus"          # optional, default "ghdl"
sources = ["tb/*.vhd"]         # analysed only for simulation
testbenches = ["blinky_tb"]    # top-level entities to elaborate and run
```

`sim.sources` are compiled on top of `hdl.sources` and never reach the `.qsf`, so
a testbench can't accidentally end up in the synthesised design. Everything lands
in `build/sim/`, which is as disposable as the rest of `build/`.

A testbench fails when an assertion of severity `error` or worse fires. One
failing testbench doesn't stop the others — they all run, and `chipsmith-vendor test`
exits non-zero if any of them failed. Drive your testbench to a close with
`std.env.finish`; a free-running clock with no `finish` never terminates.

#### Simulators

| `simulator` | Tool | Needs |
|-------------|------|-------|
| `ghdl` (default) | GHDL | `ghdl` on `PATH`. No toolchain, no licence. |
| `quartus` | Whatever your Quartus bundles — Questa for Quartus Prime, ModelSim for Quartus II | An installed toolchain, and for Questa a licence |

GHDL is the default because it is the one that just works: a package manager
away, no toolchain, no licence.

`quartus` is one name because which simulator you get is a fact about the
version you build with rather than a choice — recent Quartus Prime Lite ships
Questa-Intel FPGA Starter Edition, older releases and Quartus II ship ModelSim.

**Questa needs a licence, ModelSim does not.** Per Intel's own
[licensing documentation](https://www.intel.com/content/www/us/en/docs/programmable/683472/22-4/and-software-license.html),
Questa-Intel FPGA Starter Edition requires a zero-cost licence while
ModelSim-Intel FPGA Starter Edition requires none. The Questa licence is free
but node-locked and expires yearly: generate it at Altera's Self-Service
Licensing Center and point `SALT_LICENSE_SERVER` at the `.dat` file. chipsmith
cannot obtain it for you; it says so when `vsim` fails.

A testbench means the same thing on both: chipsmith writes a `modelsim.ini` into
the work library setting `BreakOnAssertion = 2`, which is the severity GHDL's
`--assert-level=error` breaks at. ModelSim's own default is `failure`, so
without it a `severity error` assertion would pass under one simulator and fail
under the other.

### `chipsmith-vendor flash`

Program the FPGA via JTAG using `quartus_pgm`.

```bash
chipsmith-vendor flash                            # auto-detects .sof from build output
chipsmith-vendor flash --sof path/to/file.sof     # flash a specific file
chipsmith-vendor flash --cable "USB-Blaster"      # specify JTAG cable
```

### `chipsmith-vendor cables`

List connected JTAG cables and devices (runs `jtagconfig`).

```bash
chipsmith-vendor cables
```

### `chipsmith-vendor run`

Run any Quartus tool directly. Useful for operations not covered by the other commands.

```bash
chipsmith-vendor run quartus_sh -- --tcl_eval "puts hello"
chipsmith-vendor run quartus_pgm --version 24.1
chipsmith-vendor run jtagconfig --backend quartus-ii-13
```

### `chipsmith-vendor which`

Show the install directory for a toolchain version.

```bash
chipsmith-vendor which           # ~/intelFPGA_lite/23.1std
chipsmith-vendor which 24.1
```

## NixOS support

Chipsmith has first-class NixOS support. It automatically patches ELF binaries and shell scripts in the Quartus installation to work under NixOS's non-FHS filesystem layout. For Quartus II 13 (32-bit), it uses `bubblewrap` to provide the 32-bit dynamic linker.

No manual `nix-shell` or FHS wrappers needed.

An install remembers which nix store paths it was patched against. If the
chipsmith you run uses different ones — a nixpkgs bump, or switching between a
`cargo install`ed and a nix-built chipsmith — `build` refuses the install and
says so, and `chipsmith-vendor install` for that version patches it again.

`CHIPSMITH_INSTALL_ROOT=/some/dir` moves every install to
`/some/dir/<backend>/<version>`, e.g. `/some/dir/quartus-ii-13/13.0sp1`.

## Building with nix

The flake builds a Project as a derivation whose `$out` is the bitstream and the
timing report. Quartus stays out of the store: you install it with chipsmith as
usual, and the NixOS module tells nix which machines have it.

On the machine that has the toolchain:

```nix
{
  imports = [ chipsmith.nixosModules.chipsmith-vendor ];
  programs.chipsmith-vendor = {
    enable = true;
    toolchains = [
      { backend = "quartus-ii-13"; version = "13.0sp1"; path = "/home/alice/altera/13.0sp1"; }
    ];
  };
}
```

then, as the user who owns that path, `chipsmith-vendor install --backend quartus-ii-13`
with the chipsmith the module put on `PATH`, so the install is patched against
the same store paths the build sandbox will have. The path must be readable by
the nix build users.

Each entry adds the system feature `<backend>-<version>` and mounts the install
at `/chipsmith/<backend>/<version>` inside the build sandbox. A Project then
builds with:

```nix
chipsmith.vendorLib.buildProject {
  inherit pkgs;
  src = ./my-design;      # the directory holding chipsmith.toml
  requireTiming = true;   # optional: fail the derivation if timing is missed
}
```

The derivation requires the feature its Manifest's `[toolchain]` names, so nix
only runs it on a machine that declared that toolchain. To build from another
machine, list the feature in that builder's `nix.buildMachines.*.supportedFeatures`.

Outputs are not bit-reproducible — Quartus stamps dates into them — so
`nix build --rebuild` reports a difference. ModelSim/Questa are not run.

`nix build .#checks.x86_64-linux.vendor-e2e-quartus-ii-13` proves the whole path in a
VM: installs Quartus II 13.0sp1 as a user, declares it through the module, and
realises `buildProject ./crates/vendor-cli/example-de2` in the VM's own build sandbox. It fetches
the 7.2 GB Quartus II DVD image from archive.org, pinned to the SHA-1 Altera
publishes for it.

The same test for Quartus Prime 23.1 and `crates/vendor-cli/example/` is
`.#legacyPackages.x86_64-linux.vendor-e2e-quartus-prime`. Altera only serves Prime
Lite to logged-in users, so download `QuartusLiteSetup-23.1std.1.993-linux.run`
and `cyclonev-23.1std.1.993.qdz` yourself and `nix-store --add-fixed sha1` each
first. It has not been run yet.

## Project structure

```
crates/
  vendor-toolchain/       # Toolchain trait, manifest parsing, scaffolding, errors
  vendor-sim/             # Simulator trait, test verdicts, the GHDL simulator
  vendor-quartus-common/  # All Quartus behaviour: install, build, constraints, timing, ModelSim/Questa
  vendor-quartus-prime/   # Quartus Prime product description (23.1, 22.1, 24.1)
  vendor-quartus-pro/     # Quartus Prime Pro product description (26.1.1)
  vendor-quartus-ii-13/   # Quartus II product description (13.0sp1)
  vendor-core/            # Backend registry, manifest-driven commands
  vendor-cli/             # CLI binary, chipsmith-vendor
    example/              # Blinky on Cyclone V (Quartus Prime 23.1)
    example-de2/          # Blinky on DE2 board (Quartus II 13.0sp1)
```

Both examples carry a testbench and a `justfile`: `just testbench` runs it on
GHDL, `just testbench-quartus` on the simulator the toolchain bundles.

The backend crates are data, not code: each is one `QuartusProduct`
constant naming its versions, install root, installer flags and spawn
strategy. Everything they do lives once in `chipsmith-quartus-common`. See
[CONTEXT.md](CONTEXT.md) for the vocabulary.

## License

MIT — see [LICENSE](LICENSE).
