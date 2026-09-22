# chipsmith

A command-line FPGA toolchain manager and build system. Handles downloading, installing, and orchestrating Intel Quartus tools so you can go from VHDL source to a programmed FPGA with a single manifest file.

## Quick start

```bash
# Create a new project
chipsmith init --family "Cyclone V" --device 5CSEBA6U23I7

# Run the testbenches (GHDL by default — no Quartus needed)
chipsmith test

# Build (downloads Quartus automatically on first run)
chipsmith build

# Flash to FPGA
chipsmith flash
```

## Installation

Requires a recent stable Rust — CI builds against `stable`, which is what the
`facet` dependencies track.

```bash
cargo install --path crates/chipsmith-cli
```

On NixOS, `nix develop` gives you a shell with Rust, `just`, `ghdl` and the
tools chipsmith shells out to when it patches a Quartus install.

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

### `chipsmith init`

Scaffold a new project with `chipsmith.toml` and a stub VHDL entity.

```bash
chipsmith init
chipsmith init --family "Cyclone IV E" --device EP4CE22F17C6
chipsmith init --backend quartus-ii-13 --family "Cyclone II" --device EP2C35F672C6
```

Defaults to Cyclone V and the `quartus-prime` backend. Omitting `--version`
picks the selected backend's latest, so `--backend quartus-ii-13` scaffolds
`13.0sp1` without being told.

### `chipsmith install`

Download and install a toolchain version. Happens automatically on first `build`, but can be done explicitly.

```bash
chipsmith install                          # latest quartus-prime (23.1)
chipsmith install 24.1
chipsmith install --backend quartus-ii-13  # latest for that backend (13.0sp1)
chipsmith install --installer ./QuartusLiteSetup.run   # from local file
```

The version defaults to the chosen backend's latest, not to a single global
default.

### `chipsmith build`

Run the full synthesis pipeline (map, fit, asm, timing analysis). Produces a `.sof` file in `build/output_files/`.

Afterwards chipsmith reads the `quartus_sta` summary and reports the worst-case slack.
A design that misses timing still produces a usable `.sof`, so this is a warning rather
than a build failure — but it won't pass silently. Pass `--require-timing` to make it
an error instead, which is what you want in CI.

```bash
chipsmith build
chipsmith build --project-dir path/to/project
chipsmith build --require-timing           # exit non-zero if timing is missed
```

### `chipsmith test`

Run the project's testbenches under a simulator and report a verdict for each.

```bash
chipsmith test
chipsmith test --testbench blinky_tb    # just one
chipsmith test --simulator ghdl         # override what chipsmith.toml says
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
failing testbench doesn't stop the others — they all run, and `chipsmith test`
exits non-zero if any of them failed. Drive your testbench to a close with
`std.env.finish`; a free-running clock with no `finish` never terminates.

#### Simulators

| `simulator` | Tool | Needs |
|-------------|------|-------|
| `ghdl` (default) | GHDL | `ghdl` on `PATH`. No toolchain, no licence. |
| `quartus` | Whatever your Quartus bundles — Questa for Quartus Prime, ModelSim for Quartus II | An installed toolchain, and for Questa a licence |

`quartus` is one name because which simulator you get is a fact about the
version you build with, not a choice: Quartus Prime Lite 21.1 and later ship
Questa-Intel FPGA Starter Edition, everything older ships ModelSim.

**Questa needs a licence.** ModelSim Starter Edition never asked for one; Questa
Starter Edition refuses to elaborate without a free node-locked licence from
Altera's Self-Service Licensing Center, with `SALT_LICENSE_SERVER` pointing at
the `.dat` file. chipsmith can't obtain that for you — it just says so when
`vsim` fails. Use `--simulator ghdl` if you'd rather not.

A testbench means the same thing on both: chipsmith writes a `modelsim.ini` into
the work library setting `BreakOnAssertion = 2`, which is the severity GHDL's
`--assert-level=error` breaks at. ModelSim's own default is `failure`, so
without it a `severity error` assertion would pass under one simulator and fail
under the other.

### `chipsmith flash`

Program the FPGA via JTAG using `quartus_pgm`.

```bash
chipsmith flash                            # auto-detects .sof from build output
chipsmith flash --sof path/to/file.sof     # flash a specific file
chipsmith flash --cable "USB-Blaster"      # specify JTAG cable
```

### `chipsmith cables`

List connected JTAG cables and devices (runs `jtagconfig`).

```bash
chipsmith cables
```

### `chipsmith run`

Run any Quartus tool directly. Useful for operations not covered by the other commands.

```bash
chipsmith run quartus_sh -- --tcl_eval "puts hello"
chipsmith run quartus_pgm --version 24.1
chipsmith run jtagconfig --backend quartus-ii-13
```

### `chipsmith which`

Show the install directory for a toolchain version.

```bash
chipsmith which           # ~/intelFPGA_lite/23.1std
chipsmith which 24.1
```

## NixOS support

Chipsmith has first-class NixOS support. It automatically patches ELF binaries and shell scripts in the Quartus installation to work under NixOS's non-FHS filesystem layout. For Quartus II 13 (32-bit), it uses `bubblewrap` to provide the 32-bit dynamic linker.

No manual `nix-shell` or FHS wrappers needed.

## Project structure

```
crates/
  chipsmith-toolchain/       # Toolchain trait, manifest parsing, scaffolding, errors
  chipsmith-sim/             # Simulator trait, test verdicts, the GHDL simulator
  chipsmith-quartus-common/  # All Quartus behaviour: install, build, constraints, timing, ModelSim/Questa
  chipsmith-quartus-prime/   # Quartus Prime product description (23.1, 22.1, 24.1)
  chipsmith-quartus-ii-13/   # Quartus II product description (13.0sp1)
  chipsmith-core/            # Backend registry, manifest-driven commands
  chipsmith-cli/             # CLI binary
example/                     # Blinky on Cyclone V (Quartus Prime 23.1)
example-de2/                 # Blinky on DE2 board (Quartus II 13.0sp1)
```

Both examples carry a testbench and a `justfile`: `just testbench` runs it on the
simulator the manifest names, `just testbench-ghdl` on GHDL.

The two backend crates are data, not code: each is one `QuartusProduct`
constant naming its versions, install root, installer flags and spawn
strategy. Everything they do lives once in `chipsmith-quartus-common`. See
[CONTEXT.md](CONTEXT.md) for the vocabulary.

## License

MIT — see [LICENSE](LICENSE).
