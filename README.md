# chipsmith

An open FPGA toolchain: HDL in, programmed chip out, no vendor software.

```
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl build
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl flash
```

The first run builds cram and flattens its chip database into the router's
graph. After that a build of blinky-vhdl takes about 6 s; chipsmith runs no
nextpnr.

## Status

| device | board | build | on hardware |
|---|---|---|---|
| EP2C35F672C6 (Cyclone II) | Terasic DE2 | yes: LUT/carry/register logic, global clocks from pins, 3.3-V LVTTL pins | yes (2026-10-23: blinky-vhdl built and flashed by chipsmith) |
| xc7z010clg400-1 (Zynq-7010, PL only) | Digilent Zybo (original, "legacy") | yes, all external tools (below): what yosys `synth_xilinx` and openXC7's nextpnr handle, LVCMOS33 pins | yes (2026-10-23: zybo-jtag-counter built by the nix package, flashed by chipsmith, read back over JTAG at 125.00 MHz) |

`examples/picorv32` is a RISC-V core (PicoRV32, RV32I) counting on the
LEDs, about 1,100 LEs; it builds in about 70 s.

Quartus assembles chipsmith's placement and routing into the same RBF bit
for bit (`bench/quartus-timing.sh`): checked 2026-10-23 on blinky-vhdl
(16.65 ns of slack at 50 MHz) and on picorv32. Since chipsmith places
designs itself, the RBFs no longer match the
[cyclone2-re](https://github.com/cramt/cyclone2-re) examples built with
nextpnr's placement.

The Cyclone II limits come from cyclone2-re: no M4K RAM, multipliers or
PLLs, no bidirectional or registered I/O. The timing model covers LE and
register delays exactly and routing by per-element-type averages.

The Zybo examples:

- `examples/zybo-blinky`: a counter on LD0-LD3. Builds in about 36 s.
- `examples/zybo-jtag-counter`: the Zybo's USB-UART is wired to the
  Zynq's processor (PS MIO 48/49, Digilent's Zybo reference manual,
  section 6), not to the FPGA fabric. So without software on the ARM
  cores, no PL design can print to it. This design answers over the JTAG
  cable instead. A BSCANE2 makes it a 64-bit USER1 register holding
  `"ZYBO"` and a Gray-coded 125 MHz counter.
  `cargo run --release --example xc7_user1` reads it twice, a second apart,
  with chipsmith's own cable code. Builds in about 61 s.

  ```
  USER1 0x5a59424f0dc63866: tag "ZYBO", count 159109188
  USER1 0x5a59424f18982cdd: tag "ZYBO", count 284149910
  125040722 counts in 1.000 s: 125.00 MHz
  ```

  With zybo-blinky loaded instead, the same read returns all zeros.

Zybo limits, all of them inherited from the tools:

- Every pin is LVCMOS33. The manifest has no I/O standards yet, and the
  Zybo's PL banks (34, 35) are both 3.3 V.
- Pins are the package's 100 PL user I/O balls (prjxray-db's
  `package_pins.csv`). PS pins (MIO, DDR) are rejected in the manifest.
- The PS isn't configured. Nothing needs the ARM cores, DDR or the
  PS clocks, and a design instantiating PS7 is untested.
- No clock constraints reach nextpnr, so it checks timing against its
  default 12 MHz. Its estimate still tracks Vivado's. zybo-blinky through
  the Vivado 2026.1 oracle (xc7-re `tools/oracle.sh`, same Verilog, 8 ns
  clock): 27 registers, 1 LUT, 7 CARRY4, WNS 5.438 ns, so a 2.562 ns
  critical path (390.3 MHz) through the 7-CARRY4 chain. nextpnr reports
  391.5 MHz, 0.3% apart. nextpnr reports 56 LUT BELs, against Vivado's 1
  LUT. That is most likely route-through LUTs, not checked.
- The flow is deterministic: two builds give the same FASM. The `.bit`
  header still carries xc7frames2bit's build date and time.
- The Zybo Z7 has its clock on K17. It needs only another pin map; the
  Z7-20 (xc7z020) needs another `Device`.

## Flow

Every stage is one `Step` in a `Plan` (`src/flow.rs`). The device in the
manifest picks the plan.

Cyclone II (EP2C35F672C6):

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | stays a translator in front of the flow |
| synthesis | yosys + `cyclone2.ys` (cyclone2-re) | to replace |
| packing | chipsmith: cyclone2-re's nextpnr packer, ported (`src/pack`) | ours |
| placement | chipsmith: simulated annealing over LAB slots, nextpnr's LAB rules (`src/place`) | ours |
| routing | chipsmith: PathFinder over the chip database (`src/route`) | ours |
| bitstream | `cram pack` (cyclone2-re) | ours already |
| JTAG stream | `cram svf` | ours already |
| cable | chipsmith: USB-Blaster and FT2232 (MPSSE) drivers, SVF player, 7-series JTAG loader (`src/jtag`) | ours |

Zynq-7010 PL (xc7z010clg400-1), stage 1 of the roadmap: shell out.

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | as for Cyclone II |
| synthesis | yosys `synth_xilinx -flatten -abc9 -arch xc7` | to replace |
| packing, placement, routing | openXC7's nextpnr (`nextpnr-himbaechel`, xilinx uarch), chip database for xc7z010 built from prjxray-db in nix (2.5 min) | to replace |
| FASM → frames | prjxray `fasm2frames` | to replace |
| frames → `.bit` | prjxray `xc7frames2bit` | to replace |
| cable | chipsmith: FT2232 (MPSSE) driver and 7-series JTAG loader (`src/jtag`), loads in 1.3 s with DONE checked | ours |

openXC7's nextpnr, prjxray, fasm and prjxray-db come from one pinned
[openXC7/toolchain-nix](https://github.com/openXC7/toolchain-nix)
revision (`flake.nix`). That revision is the first whose nextpnr places
BSCANE2. Replacing a tool means replacing one step. The nix package pins
every tool. The devshell pins them too.

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
     | picorv32 | chipsmith, wirelength only | 12.2 s | 9.10 ns |
     | picorv32 | chipsmith, timing-driven (below) | 22.3 s | 11.23 ns |
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
   - Timing-driven placement. The placer builds the same timing graph,
     estimates each connection's delay from the placement (LE buffer and
     LAB line plus R4/C4 delay per LAB crossed), reruns STA every
     temperature and weights each net's wirelength by
     `1 + 8 * crit^4`. picorv32 critical path in quartus_sta: 9.82 ->
     8.79 ns (11.23 ns of slack at 50 MHz; was 2.25 ns with nextpnr).
     Weight/exponent sweep, one seed each: 8/4 8.79 ns, 32/8 8.81 ns,
     16/4 8.94 ns, 8/2 9.35 ns.
   - Packer (done). `src/pack` is the `pack()` of cyclone2-re's nextpnr
     uarch in Rust: pin buffers named after their port bits, constants
     folded into LUT masks and pins, stuck-at-GND registers and input-less
     LUTs removed, one-input LUTs folded into their loads (inversions into
     masks, `DATA_INVERT`, `ENA_INV`/`SCLR_INV`; pushed back into the driver
     or made an inverted copy of it in front of a register), registers
     paired with their LUT or put on the sync-load path, carry chains in
     Quartus' polarity, a clock control per clock (the placer picks which).
     Some rules depend on visiting order (folding one buffer can make
     another LUT a buffer; the first register to claim a LUT gets it), so
     the netlist iterates the way nextpnr's containers do and imports yosys'
     JSON the way its frontend does; net ids come out in nextpnr's order,
     which is the order the router routes in.
     `bench/pack-equiv.sh` packs a netlist with both and compares
     (`chipsmith pack-diff`: cells by name, type, parameters, every
     connection by net name, net id order):

     | design | cells | nextpnr `--pack-only` vs chipsmith | full-flow RBF vs nextpnr-packed flow | Quartus (`bench/quartus-timing.sh`) |
     |---|---|---|---|---|
     | blinky | 58 | identical, same net order | identical | byte-identical; slack 16.65 ns, data delay 3.39 ns |
     | lfsr | 173 | identical, same net order | identical | byte-identical; slack 13.39 ns, data delay 6.68 ns |
     | uart_tx | 152 | identical, same net order | identical | byte-identical; slack 14.46 ns, data delay 5.56 ns |
     | picorv32 | 1,459 | identical, same net order | identical | byte-identical; slack 11.23 ns, data delay 8.79 ns |
     | examples/blinky-verilog, blinky-vhdl | 58 | identical, same net order | identical | (same design as blinky) |

     Packing picorv32 takes 0.4 s. A design exercising the rarer rules
     (register inits of 1, active-low enables and clears, constant and
     inverted outputs) packs identically too. Parameters nextpnr writes as
     `0` (a flag toggled back) chipsmith leaves out; they read the same.
4. **Own synthesis**: techmap and LUT mapping on the IR. yosys's JSON
   netlist is the boundary until then.
5. **Own Verilog frontend**: the synthesizable subset that GHDL and HDL
   generators emit.

6. **Persistent flashing**: `chipsmith flash --persist` writes the board's
   configuration flash so the design survives a power cycle. `flash` as it
   is loads configuration RAM over JTAG (fast, gone at power-off). DE2: the
   EPCS16, written through a JTAG-to-SPI bridge design loaded first. Zybo:
   a boot image (FSBL + bitstream) in QSPI flash, the same way, or on the
   SD card.

Next devices:

- **Zybo** (Zynq-7000, 7-series fabric): it builds and loads (above), on
  external tools. Project X-Ray already documents the fabric, so owning
  the stages means importing prjxray-db, not reverse engineering:
  bitstream writer first, then packing, placement and routing. xc7-re
  holds the groundwork.
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

The device decides how pins are written: `PIN_N2` on Cyclone II, the
package ball (`L16`) on the Zynq. A pin of the wrong form, or one the
package doesn't have as user I/O, is an error when the manifest loads.
The Zynq builds `build/<name>.bit` instead of `.rbf`.

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
