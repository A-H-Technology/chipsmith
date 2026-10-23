# chipsmith

An open FPGA toolchain: HDL in, programmed chip out, no vendor software.

```
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl build
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl flash
```

The first run builds nextpnr's chip database (about 15 minutes). A build of
blinky takes about 9 s.

## Status

| device | board | build | on hardware |
|---|---|---|---|
| EP2C35F672C6 (Cyclone II) | Terasic DE2 | yes: LUT/carry/register logic, global clocks from pins, 3.3-V LVTTL pins | yes (2026-10-23: blinky-vhdl built and flashed by chipsmith) |

Both examples produce RBFs that are byte-identical to the
[cyclone2-re](https://github.com/cramt/cyclone2-re) blinky, which Quartus
reproduces bit for bit from the same placement and routing.

The Cyclone II limits come from cyclone2-re: no M4K RAM, multipliers or
PLLs, no bidirectional or registered I/O. The timing model covers LE and
register delays exactly and routing by per-element-type averages.

## Flow

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | stays a translator in front of the flow |
| synthesis | yosys + `cyclone2.ys` (cyclone2-re) | to replace |
| packing | nextpnr-himbaechel `--pack-only`, cyclone2 uarch (cyclone2-re) | to replace next |
| placement | chipsmith: simulated annealing over LAB slots, nextpnr's LAB rules (`src/place`) | ours |
| routing | chipsmith: PathFinder over the chip database (`src/route`) | ours |
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
   - Router (done). Quartus keeps 100% of its routes and assembles RBFs
     byte-identical to ours for blinky, lfsr, uart_tx and picorv32. Same
     placement, nextpnr's router2 vs ours:

     | design | router | time | routing bits | setup slack @ 50 MHz (Quartus STA) |
     |---|---|---|---|---|
     | picorv32 | nextpnr router2 | 24.2 s | 17,209 | 2.25 ns |
     | picorv32 | chipsmith | 9.1 s | 10,127 | 9.30 ns |
     | lfsr | chipsmith | 0.7 s | 1,127 | 10.93 ns |
     | uart_tx | chipsmith | 0.3 s | 528 | 13.39 ns |

     The routing graph (861,427 nodes, 8,325,848 pips) is flattened from
     cyclone2-re's chip database by a nix derivation. Loading it took 2.4 s;
     now 0.45 s: the name index is built only while parsing (1.05 s), the
     2.3M wire names are one string per tile type, the large arrays are raw
     little-endian words instead of postcard varints, and those decode on
     parallel threads (the cost left is first-touch page faults).
     Routing nets of one PathFinder iteration in parallel batches (each net
     blind to the others in its batch) didn't converge on picorv32 within
     5 minutes at batch sizes 8 and 32, against 6.6 s sequentially: parallel
     routing needs nets that can't compete, e.g. disjoint bounding boxes.
   - Placer (done). nextpnr only packs; chipsmith places and routes. Quartus
     keeps 100% of the routes and assembles byte-identical RBFs for lfsr,
     uart_tx and picorv32:

     | design | placer + router | place time | setup slack @ 50 MHz (Quartus STA) |
     |---|---|---|---|
     | picorv32 | nextpnr (HeAP + SA, router2) | 50.8 s | 2.25 ns |
     | picorv32 | chipsmith | 12.2 s | 9.10 ns |
     | lfsr | chipsmith | 1.5 s | 12.22 ns |
     | uart_tx | chipsmith | 0.7 s | 13.16 ns |

     Placer speed on picorv32, same result each time: 31.8 s with std
     `HashMap`, 19.8 s with FxHash, 12.2 s with an enum of ports and dense
     net ids (no hashing per move). Recomputing high-fanout nets' bounding
     boxes is the largest cost left (26%). Annealing 4x longer (8 moves
     per temperature, 139 s) ends at lower wirelength (3,903 vs 4,261) but
     less slack (8.84 ns): wirelength is not timing, so the next gain is a
     timing model, not more annealing.
   - Timing model (`src/timing.rs`) and timing-driven routing. Cell delays
     are Quartus' DMF values (cyclone2-re docs/timing-model.md), routing
     delays quartus_sta's median per element type; register-to-register
     paths only, like Quartus with just a clock constraint. After each
     PathFinder iteration the router runs STA and costs each connection
     `(1 - crit) * congestion + crit * delay`. `bench/quartus-timing.sh`
     has Quartus time a chipsmith routing.

     picorv32, same placement, Quartus' data delay on the critical path:

     | routing | chipsmith estimate | Quartus |
     |---|---|---|
     | congestion only | 9.12 ns (before the carry fixes below) | 10.71 ns |
     | timing-driven, carry chains missing from the model | 7.62 ns | 10.68 ns |
     | timing-driven, carry chains in, carry delay by LE position | 9.45 ns | 9.98 ns |
     | + A* distance from each wire's bounding box | 9.45 ns | 9.82 ns |

     Per connection, Quartus' routing delay is 1.07x the model's (median
     over 564 connections). The critical path is now a 27-LE carry chain
     (2.5 ns), 5 LUTs (1.8 ns) and 5 routed connections (4.7 ns, about
     0.95 ns each), so the next gain is timing-driven placement. Routing
     picorv32 takes 16.7 s, up from 6.6 s: critical nets are rerouted every
     iteration, and the box distance needs a stronger A* weight (1.5) to
     search as narrowly as the old single-tile one.
   - Packer: next.
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
