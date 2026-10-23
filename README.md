# chipsmith

An open FPGA toolchain: HDL in, programmed chip out, no vendor software.

```
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl build
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl flash
```

The first run builds nextpnr's chip database (about 15 minutes).

## Status

| device | board | build | on hardware |
|---|---|---|---|
| EP2C35F672C6 (Cyclone II) | Terasic DE2 | yes: LUT/carry/register logic, global clocks from pins, 3.3-V LVTTL pins | yes (2026-10-23: blinky-vhdl built and flashed by chipsmith) |

Both examples produce RBFs that are byte-identical to the
[cyclone2-re](https://github.com/cramt/cyclone2-re) blinky, which Quartus
reproduces bit for bit from the same placement and routing.

The Cyclone II limits come from cyclone2-re: no timing model, no M4K RAM,
multipliers or PLLs, no bidirectional or registered I/O.

## Flow

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | stays a translator in front of the flow |
| synthesis | yosys + `cyclone2.ys` (cyclone2-re) | to replace |
| place and route | nextpnr-himbaechel, cyclone2 uarch (cyclone2-re) | to replace first |
| bitstream | `cram pack` (cyclone2-re) | ours already |
| JTAG stream | `cram svf` | ours already |
| cable | chipsmith: USB-Blaster driver + SVF player (`src/jtag`) | ours |

Every stage is one `Step` in a `Plan` (`src/flow.rs`), so replacing a tool
means replacing one step. The nix package pins every tool. The devshell
pins them too.

Verilog is the input language. Anything else, VHDL today and later Chisel,
Amaranth and the like, gets translated into it. Internally, the stages will
share chipsmith's own netlist IR, not Verilog.

## Roadmap

1. **Shell out** (done): `build` and `flash` over the existing open tools.
2. **Own cable** (done): a USB-Blaster driver and an SVF player in Rust.
   `chipsmith flash` loads the DE2 in 1.4 s, and fails on CONF_DONE low.
3. **Own place and route**: a placer and router over the decoded routing
   graph. cyclone2-re's `just validate` checks each result against Quartus.
   This stage brings the timing model.
4. **Own synthesis**: techmap and LUT mapping on the IR. yosys's JSON
   netlist is the boundary until then.
5. **Own Verilog frontend**: the synthesizable subset that GHDL and HDL
   generators emit.

Next devices:

- **Zybo Z7** (Zynq-7000, 7-series fabric): Project X-Ray already documents
  it, so the work is importing prjxray-db, not reverse engineering.
- **DE25-Nano** (Agilex 5): no public reverse engineering exists. First
  check whether the Secure Device Manager accepts a bitstream we write.

## Manifest

```toml
[project]
name = "blinky"            # build/blinky.rbf
top = "blinky"             # module or entity whose ports are pins
sources = ["src/*.vhd"]    # .v or .vhd/.vhdl, one language per design

[target]
device = "EP2C35F672C6"

[pins]
CLOCK_50 = "PIN_N2"
LEDR = ["PIN_AE23", "PIN_AF23", "PIN_AB21", "PIN_AC22"]  # LEDR[0], LEDR[1], ...
```

Unknown keys are errors: until there is a timing model, a `[clocks]` table
would be silently ignored, so it is rejected.

## Development

```
nix develop
cargo test
cargo clippy --all-targets -- -D warnings
cargo run -- -C examples/blinky-verilog build
```

The Quartus toolchain manager that used to be chipsmith lives on the `main`
branch.

## License

MIT
