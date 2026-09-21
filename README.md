# chipsmith

A command-line FPGA toolchain manager and build system. Handles downloading, installing, and orchestrating Intel Quartus tools so you can go from VHDL source to a programmed FPGA with a single manifest file.

## Quick start

```bash
# Create a new project
chipsmith init --family "Cyclone V" --device 5CSEBA6U23I7

# Build (downloads Quartus automatically on first run)
chipsmith build

# Flash to FPGA
chipsmith flash
```

## Installation

Requires Rust 1.75+.

```bash
cargo install --path crates/chipsmith-cli
```

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

[hdl]
standard = "VHDL_2008"         # optional, default
sources = ["src/*.vhd"]

[pins]
clk = "PIN_V11"
led = ["PIN_W15", "PIN_AA24", "PIN_V16", "PIN_V15"]
```

The `[toolchain]` key selects the backend. Supported backends:

| Key | Toolchain | Supported versions |
|-----|-----------|-------------------|
| `quartus-prime` | Intel Quartus Prime Lite | 22.1, 23.1, 24.1 |
| `quartus-ii-13` | Altera Quartus II | 13.0sp1 |

Pin mappings can be a single string for one pin or an array for a bus.

## Commands

### `chipsmith init`

Scaffold a new project with `chipsmith.toml` and a stub VHDL entity.

```bash
chipsmith init
chipsmith init --family "Cyclone IV E" --device EP4CE22F17C6
chipsmith init --backend quartus-ii-13 --version 13.0sp1 --family "Cyclone II" --device EP2C35F672C6
```

Defaults to Cyclone V / quartus-prime 23.1 when no flags are given.

### `chipsmith install`

Download and install a toolchain version. Happens automatically on first `build`, but can be done explicitly.

```bash
chipsmith install              # latest quartus-prime (23.1)
chipsmith install 24.1
chipsmith install 13.0sp1 --backend quartus-ii-13
chipsmith install --installer ./QuartusLiteSetup.run   # from local file
```

### `chipsmith build`

Run the full synthesis pipeline (map, fit, asm, timing analysis). Produces a `.sof` file in `build/output_files/`.

```bash
chipsmith build
chipsmith build --project-dir path/to/project
```

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
chipsmith run jtagconfig --backend quartus-ii-13 --version 13.0sp1
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
  chipsmith-toolchain/       # Core trait, manifest parsing, error types
  chipsmith-quartus-common/  # Shared Quartus logic (download, QSF gen, build)
  chipsmith-quartus-prime/   # Quartus Prime backend (23.1, 22.1, 24.1)
  chipsmith-quartus-ii-13/   # Quartus II 13.0sp1 backend
  chipsmith-core/            # Public API, backend routing
  chipsmith-cli/             # CLI binary
example/                     # Blinky on Cyclone V (Quartus Prime 23.1)
example-de2/                 # Blinky on DE2 board (Quartus II 13.0sp1)
```

## License

MIT — see [LICENSE](LICENSE).
