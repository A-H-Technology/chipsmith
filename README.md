# chipsmith

An open FPGA toolchain: HDL in, programmed chip out, no vendor software.

```
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl build
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl flash
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl flash --persist  # DE2: into the EPCS
nix run github:A-H-Technology/chipsmith/open -- -C examples/zybo-jtag-counter flash --ssh venus  # Zybo, through its Linux
```

The first run builds cyclone2-re's device database package and flattens
its chip database into the router's graph. After that a build of blinky-vhdl takes about 6 s. A DE2 build
runs no program but chipsmith between the Verilog and the chip: no yosys,
no nextpnr, no cram. GHDL translates VHDL in front of it.

## Status

| device | board | build | on hardware |
|---|---|---|---|
| EP2C35F672C6 (Cyclone II) | Terasic DE2 | yes, with no external tool but GHDL for VHDL (2026-10-25: chipsmith's own Verilog front end and synthesis): LUT/carry/register logic, M4K RAM blocks (below), global clocks from pins, 3.3-V LVTTL pins: input, output, tri-state, bidirectional, open drain | yes (2026-10-25, chipsmith's own synthesis end to end, checked over JTAG: examples/de2-jtag-counter reads back `"DE2!"` at 50.00 MHz after a JTAG load and after `flash --persist` (loader synthesized by chipsmith too, EPCS written, verified and reloaded from; 2026-10-26 also after a power cycle); picorv32 with the same readback added runs its loop at 3.33 MHz. 2026-10-24, synthesized by yosys: picorv32 built with chipsmith's LUT mapper by the nix package, flashed by chipsmith, counting on the LEDs; examples/bidir driving, releasing and reading back its pins; examples/ram-leds stepping its ROM and RAM on the LEDs; `flash --persist` writes the EPCS16 and the design runs from it, roadmap step 6) |
| xc7z010clg400-1 (Zynq-7010, PL only) | Digilent Zybo (original, "legacy") | yes: what yosys `synth_xilinx` and openXC7's nextpnr's packer handle, LVCMOS33 pins; chipsmith places LUT/register/carry logic, RAM32Ms, pins, BUFGs, BSCANs and the PS7 and routes the interconnect (below) | yes (2026-10-24: zybo-jtag-counter placed and routed by chipsmith, read back over JTAG at 125.00 MHz, loaded over JTAG and with `flash --ssh` through Linux on the board; a bitstream loaded at every boot through nixos-fpga) |

`examples/picorv32` is a RISC-V core (PicoRV32, RV32I) counting on the
LEDs, 1,171 LEs; a build takes about 42 s, half of it placement.

`examples/de2-jtag-counter` answers over the JTAG cable, so a load can be
checked without watching the board: the Cyclone II's JTAG block makes it a
64-bit USER1 register holding `"DE2!"` and a Gray-coded 50 MHz counter.
`cargo run --release -p chipsmith-cyclone2 --example de2_user1` reads it
twice, a second apart:

```
USER1 0x44453221633f8c74: tag "DE2!", count 1110112167
USER1 0x444532216744cde8: tag "DE2!", count 1166505648
56393481 counts in 1.128 s: 50.00 MHz
```

The register is clocked by CLOCK_50, not TCK, which samples TCK instead:
the DE2 back end only clocks registers from pins, since driving a global
clock from the core (cycloneii_clkctrl fed by logic) isn't decoded yet. So
the reader bit-bangs its shift, slow enough for that sampling, and the
first bit it sees is the register's bit 1.

`examples/bidir` is cyclone2-re's tri-state test: expansion-header and
SRAM data pins driven with their output enable from a switch, an
inverted switch, logic or a register, one open drain, all read back onto
the red LEDs, and a tri-state green LED. An inout must be written
`assign p = oe ? d : 1'bz` (synthesis' `tribuf` and pin buffers make it
one `CY2_IOBUF`); one without a tri-state driver is an error, and so is a
driving pin on a dedicated clock input. With every switch down on the DE2
(2026-10-24, camera): LEDR[1] blinks with the counter it drives out,
LEDR[4] and LEDG[1] follow the open drain, the released pins read 1 and
LEDG[0] stays off. Don't load it with SW[4] up and the SRAM driving: the
FPGA then drives SRAM_DQ too.

M4K RAM blocks (4,608 bits each, 105 on the EP2C35) come from chipsmith's
synthesis (`crates/chipsmith-cyclone2/src/synth/m4k.rs`, the rules and costs
of cyclone2-re's `synth/m4k.txt` for yosys' `memory_libmap`): memories with
synchronous reads on one clock, single port x1 to x18 (read-during-write
returns the new data), simple dual port x1 to x36 (one write port, one
read port, old data), ROMs, initial contents. A memory has to fill its
block at its width (e.g. 512 x 9, 256 x 18): shallower ones stay in logic,
and so do asynchronous reads. Not yet: byte enables, clock enables, output
registers, two clocks, true dual port, mixed widths, a single-port x36.
The packer moves a `CY2_M4K` onto the block's physical pins (some address
bits enter on data pins), the placer swaps blocks between M4K sites, and
the router reaches their inputs through the block's LAB lines, the clock
through a row clock and the read data from the M4K buffers.

- `examples/ram`: one memory of each shape (7 blocks), written from a
  counter and the switches, read data folded onto the LEDs.
- `examples/ram_widths`: the widths `ram` doesn't use (10 blocks).
- `examples/ram-leds`: a hardware demo. A 256 x 18 ROM steps a bouncing
  light and a filling bar across LEDR about 12 times a second. A 512 x 9
  RAM, initialised to a Gray code, is read at the same step onto LEDG.
  KEY[0] writes SW[8:0] at the address shown, KEY[1] holds the step so the
  value can be seen coming back. Runs on the DE2, from JTAG and from the
  EPCS.

Quartus assembles chipsmith's placement and routing into the same RBF bit
for bit (`bench/quartus-timing.sh`): checked 2026-10-24 on blinky-verilog
and blinky-vhdl (16.65 ns of slack at 50 MHz), on picorv32 (10.60 ns
since cyclone2-re 9cfe9be, see below; 11.08 ns before) and on bidir
(16.64 ns), all built through chipsmith's LUT mapper, and on ram
(16.21 ns), ram_widths (14.80 ns) and ram-leds (14.80 ns). Those were
yosys-synthesized. chipsmith's synthesis gives the same RBF only for the
blinkies and the flash loader (the others have other cell and net names,
so another placement) and hasn't been through Quartus yet.
picorv32 has no M4K; its slack moved because cyclone2-re's synthesis
script gained `memory_libmap`, which renames yosys' cells and nets. Same
logic, same 1,175 LEs, but other net ids and so another placement. The
same chipsmith with the previous script places it at 11.08 ns again.
An inverter from a pin straight into an M4K's write enable, or into a
register, doesn't validate: quartus_map removes the one-input LUT and
folds it into the block (cram doesn't model that bit yet) or re-creates
it elsewhere. The bitstream still works; `ram-leds` registers its key. Since chipsmith places
designs itself, the RBFs no longer match the
[cyclone2-re](https://github.com/cramt/cyclone2-re) examples built with
nextpnr's placement.

The Cyclone II limits come from cyclone2-re's open flow: M4K RAM only in
the shapes above, no multipliers or PLLs, no registered I/O, LVTTL only.
The timing model covers LE and register delays exactly and routing by
per-element-type averages. M4K clock-to-output (3.33 ns) and setup are
from one quartus_sta path each, and so is the M4K buffer's routing delay.

The Zybo examples:

- `examples/zybo-blinky`: a counter on LD0-LD3. Builds in about 70 s.
- `examples/zybo-jtag-counter`: the Zybo's USB-UART is wired to the
  Zynq's processor (PS MIO 48/49, Digilent's Zybo reference manual,
  section 6), not to the FPGA fabric. So without software on the ARM
  cores, no PL design can print to it. This design answers over the JTAG
  cable instead. A BSCANE2 makes it a 64-bit USER1 register holding
  `"ZYBO"` and a Gray-coded 125 MHz counter.
  `cargo run --release --example xc7_user1` reads it twice, a second apart,
  with chipsmith's own cable code. Builds in about 87 s with chipsmith's
  placement and routing (three nextpnr runs, below). Two builds give the
  same placement and the same FASM. Read back 2026-10-24, chipsmith's
  placement and routing, loaded with `flash --ssh`:

  ```
  USER1 0x5a59424fd795d8f0: tag "ZYBO", count 2598801568
  USER1 0x5a59424ff37745fb: tag "ZYBO", count 2723838290
  125036722 counts in 1.000 s: 125.00 MHz
  ```

  With zybo-blinky loaded instead, the same read returns all zeros.
- `examples/zybo-picorv32`: `examples/picorv32`'s sources with its ports
  on arbitrary Zybo balls, a placement and routing bench, not for
  hardware. Builds in about 110 s.

Zybo limits, all of them inherited from the tools:

- Every pin is LVCMOS33. The manifest has no I/O standards yet, and the
  Zybo's PL banks (34, 35) are both 3.3 V.
- Pins are the package's 100 PL user I/O balls (prjxray-db's
  `package_pins.csv`). PS pins (MIO, DDR) are rejected in the manifest.
- The PS isn't configured from the fabric side beyond one thing: every
  design gets a PS7 whose IRQF2P inputs are tied to 0 (the build's "PS7"
  step, or the design's own PS7 if it leaves IRQF2P open). IRQF2P[19:16]
  are the ARM cores' FIQ and IRQ, active high, and an interconnect input
  nothing drives reads as 1. So a design without a PS7, Vivado's
  PL-only blinky included, holds FIQ on both cores, and the moment the
  PL->PS level shifters come on under Linux (Linux loading it, or a JTAG
  load while U-Boot runs) both cores spin in the FIQ handler. Observed on
  venus, halted over JTAG: both cores in `__fiq_svc`/`ct_nmi_enter`; with
  IRQF2P tied to 0 Linux stays up. Designs that use the PS beyond that
  (AXI, PS clocks, interrupts to Linux) are untested.
- Both Zybo examples declare their 125 MHz clock in `[clocks]`, which
  reaches nextpnr as an XDC `create_clock`. nextpnr's estimate tracks
  Vivado's. zybo-blinky through the Vivado 2026.1 oracle (xc7-re
  `tools/oracle.sh`, same Verilog, 8 ns clock): 27 registers, 1 LUT,
  7 CARRY4, WNS 5.438 ns, so a 2.562 ns critical path (390.3 MHz) through
  the 7-CARRY4 chain. chipsmith's build prints nextpnr's 391.5 MHz, slack
  5.446 ns at 125 MHz, 0.3% apart. zybo-jtag-counter: 317.2 MHz,
  slack 4.847 ns (no Vivado number yet). Its BSCANE2's `tck` has no pin,
  so `[clocks]` can't name it, and nextpnr times it at its default
  12 MHz. nextpnr reports 56 LUT BELs, against Vivado's 1 LUT. That is
  most likely route-through LUTs, not checked.
- The flow is deterministic: two builds give the same FASM. The `.bit`
  header still carries xc7frames2bit's build date and time.
- The Zybo Z7 has its clock on K17. It needs only another pin map; the
  Z7-20 (xc7z020) needs another `Device`.

### Loading a Zybo from its own Linux

`chipsmith flash --ssh HOST` loads the PL from Linux on the Zybo's ARM
cores instead of over the JTAG cable: no cable or cable drivers on the
host, any board on the network. HOST runs NixOS with
[nixos-fpga](https://github.com/A-H-Technology/nixos-fpga)'s
`digilent-zybo` module, whose `/dev/fpga-load` hands a bitstream to the
kernel's fpga-manager (the Zynq's devcfg/PCAP), and the ssh user must be
in its `fpga` group. chipsmith sends the `.bit`'s payload with every
32-bit word byte-swapped, the format the Zynq driver takes, then checks
the manager reports `operating`. The transfer and load of zybo-jtag-counter's
2 MB take about 0.5 s; the rest is the ssh login.

Every Zybo build also writes that format next to the `.bit`
(`build/<name>.bin`). Set nixos-fpga's `hardware.fpga.bitstream` to it and
the board loads it at every boot, which is the Zybo's persistent flashing
(roadmap step 6). Observed 2026-10-24 on venus: zybo-blinky and
zybo-jtag-counter loaded with `flash --ssh`, the JTAG readback following
each (zeros, then `"ZYBO"` at 125.00 MHz), Linux up throughout; blinky
loaded by the boot service on a reboot.

## Flow

Every stage is one `Step` in a `Plan` (`crates/chipsmith/src/flow.rs`). The device in the
manifest picks the plan.

Cyclone II (EP2C35F672C6):

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | stays a translator in front of the flow |
| Verilog front end | chipsmith: preprocessor, parser, elaboration to yosys' `proc` level, with the Cyclone II cell library (`crates/chipsmith-verilog`, `crates/chipsmith-cyclone2/src/synth/cells.v`) | ours |
| synthesis | chipsmith: what cyclone2-re's `cyclone2.ys` does, on chipsmith's netlist IR (`crates/chipsmith-synth`, `crates/chipsmith-cyclone2/src/synth`) | ours |
| LUT mapping | chipsmith: AIG plus priority cuts, in place of `abc -lut 4` (`crates/chipsmith-synth/src/lutmap.rs`) | ours |
| packing | chipsmith: cyclone2-re's nextpnr packer, ported (`crates/chipsmith-cyclone2/src/pack`) | ours |
| placement | chipsmith: simulated annealing over LAB slots, nextpnr's LAB rules (`crates/chipsmith-cyclone2/src/place.rs`) | ours |
| routing | chipsmith: PathFinder over the chip database (`crates/chipsmith-route`, `crates/chipsmith-cyclone2/src/route.rs`) | ours |
| bitstream | chipsmith: cyclone2-re's assembler (`cram pack`), moved here and still checked against Quartus by cyclone2-re's `cram` (`crates/chipsmith-cyclone2/src/bitstream`, ADR-0003) | ours |
| JTAG stream | chipsmith: the SVF `cram svf` wrote, CONF_DONE checked (`crates/chipsmith-cyclone2/src/bitstream/jtag.rs`) | ours |
| cable | chipsmith: USB-Blaster and FT2232 (MPSSE) drivers, SVF player, 7-series JTAG loader (`crates/chipsmith-jtag`) | ours |
| configuration flash | chipsmith: serial flash loader (built by chipsmith) and EPCS16 driver (`crates/chipsmith-cyclone2/src/persist`) | ours |

The plan is `[ghdl,] synthesis, pack, place and route, bitstream`. yosys
and cyclone2-re's techmap are test oracles now (ADR-0004): the devshell has
them, the build doesn't run them.

Zynq-7010 PL (xc7z010clg400-1): shell out, except placement, routing and
the cable.

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | as for Cyclone II |
| synthesis | yosys `synth_xilinx -flatten -abc9 -arch xc7` | to replace |
| packing | openXC7's nextpnr (`nextpnr-himbaechel --pack-only`, xilinx uarch), chip database for xc7z010 built from prjxray-db in nix (2.5 min) | to replace next |
| placement | chipsmith: simulated annealing over slices, timing-driven with prjxray-db's delays (`crates/chipsmith-xc7/src/place.rs`, `crates/chipsmith-xc7/src/delay.rs`) | ours |
| site routing | nextpnr, routing chipsmith's placement (`-o preplaced`) | to replace |
| interconnect routing | chipsmith: PathFinder over the prjxray-db graph (`crates/chipsmith-xc7/src/xray.rs`, `crates/chipsmith-xc7/src/route.rs`) | ours |
| FASM | nextpnr's FASM writer, from chipsmith's routes (`-o preplaced`, `-o prerouted`) | to replace |
| FASM → frames | prjxray `fasm2frames` | to replace |
| frames → `.bit` | prjxray `xc7frames2bit` | to replace |
| cable | chipsmith: FT2232 (MPSSE) driver and 7-series JTAG loader (`crates/chipsmith-jtag`), loads in 1.3 s with DONE checked | ours |

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
   - Timing model (`crates/chipsmith-route/src/timing.rs`) and timing-driven routing. Cell delays
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
   - Packer (done). `crates/chipsmith-cyclone2/src/pack` is the `pack()` of cyclone2-re's nextpnr
     uarch in Rust: pin buffers named after their port bits, constants
     folded into LUT masks and pins, stuck-at-GND registers and input-less
     LUTs removed, one-input LUTs folded into their loads (inversions into
     masks, `DATA_INVERT`, `ENA_INV`/`SCLR_INV`; pushed back into the driver
     or made an inverted copy of it in front of a register), registers
     paired with their LUT or put on the sync-load path, carry chains in
     Quartus' polarity, a clock control per clock (the placer picks which),
     M4K blocks on their physical pins.
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
     | bidir (yosys `abc -lut 4`, and chipsmith's LUT mapper) | 99 | identical, same net order, same ports | not run | byte-identical; slack 16.64 ns, data delay 3.39 ns (examples/bidir) |
     | examples/ram (7 M4Ks) | 241 | identical, same net order | not run | byte-identical; slack 16.21 ns, data delay 3.83 ns (estimate 16.20 ns) |
     | examples/ram_widths (10 M4Ks) | 171 | identical, same net order | not run | byte-identical; slack 14.80 ns, data delay 5.22 ns (estimate 15.24 ns) |
     | examples/ram-leds (2 M4Ks) | 128 | identical, same net order | not run | byte-identical; slack 14.80 ns, data delay 5.28 ns (estimate 15.11 ns) |

     Packing picorv32 takes 0.4 s. A design exercising the rarer rules
     (register inits of 1, active-low enables and clears, constant and
     inverted outputs) packs identically too, and so does bidir (10
     `CY2_IOBUF`s: constant data and enables, inverters folded into
     `DATA_INVERT`/`OE_INVERT`, unread inputs switched off). `pack-diff`
     compares top-level port directions too, since the bitstream decides
     output vs bidir mode by them. Rechecked 2026-10-24 with cyclone2-re
     5eff911 (blinky, lfsr, uart_tx, picorv32 both mappers: identical),
     and for the M4K examples with 9cfe9be (`pack_m4k`: logical ports onto
     the block's pins, buffers in front of its inputs folded, its clock
     through a clock control).
     Parameters nextpnr writes as
     `0` (a flag toggled back) chipsmith leaves out; they read the same.
4. **Own synthesis** (done for the DE2, in the build since 2026-10-25):
   techmap and LUT mapping on the IR. yosys' JSON stays the netlist format
   between stages (ADR-0004).
   - Netlist IR (done, 2026-10-25): `crates/chipsmith-netlist`, in
     yosys' RTLIL cell vocabulary, with typed cells for the families
     synthesis matches on (ADR-0004, ADR-0005). It reads and writes
     yosys' JSON. The tests run 19 designs (the examples, the flash
     loader, eight cyclone2-re specimens, one instance of every typed
     RTLIL cell, every cell in yosys' `simcells.v`) through up to four
     levels (`proc`, `synth -run :fine`, `techmap; opt`, `cyclone2.ys`),
     64 netlists in all. For each one, read-write-read gives the same IR,
     yosys reads our file to the same design as its own, and yosys'
     `equiv_*` passes prove the two equivalent. picorv32 after
     `techmap; opt` (8,417 cells, 5.6 MB) reads in 137 ms and writes in
     73 ms.
   - LUT mapping (done, 2026-10-24). First switched on between two yosys
     runs: yosys ran `cyclone2.ys` up to `abc -lut 4` and wrote
     `gates.json`, chipsmith mapped that, and a second yosys ran the rest
     of the script from `opt -fast` on. Now it runs inside chipsmith's
     synthesis (below). Measured
     2026-10-24, the same design through both flows; slack is from
     `bench/quartus-timing.sh` (byte-identical assembly in every row):

     | design | LUT mapper | mapped LUTs | packed LUTs (LEs) | Quartus slack @ 50 MHz |
     |---|---|---|---|---|
     | picorv32 | yosys `abc -lut 4` | 770 | 992 (1,105) | 11.23 ns |
     | picorv32 | chipsmith | 842 | 1,062 (1,175) | 11.08 ns |
     | blinky-verilog, blinky-vhdl | either | 0 (all carry) | 26 (26) | 16.65 ns, same RBF |

     `bench/lutmap-vs-abc.sh <dir>` maps one `gates.json` (from
     `de2_synth --yosys`) both ways
     (LUTs after `opt -fast`). On picorv32, abc's mapper alone
     (`strash; if -K 4`) gives 847 LUTs and chipsmith's 842, so the
     mapping itself is on par. The 9% gap comes from abc's logic
     optimisation around its mapper: choice nodes (`dch -f`, 810 LUTs)
     and don't-care resynthesis (`mfs2`, `lutpack`, 788). A choice-aware
     mapper is the next gain. More cuts per node (16, 32), more recovery
     passes, or relaxing depth by a level didn't help (844 to 847). An earlier comparison (842 vs 847) had
     counted abc's 75 one-input buffer LUTs, which `opt -fast` removes.
   - Synthesis (done, 2026-10-25, the DE2 build's since then):
     `chipsmith_cyclone2::synth::synthesize` takes the front end's netlist
     (or yosys' `proc; flatten`) to the cells `cyclone2.ys` makes, in place
     of both yosys halves: flatten, tri-states (`chipsmith_synth::tribuf`),
     memories collected and their read registers merged where an M4K takes
     them, undefined values resolved once (ADR-0007), state registers
     compared by the bits that tell their values apart (`fsm`), the `opt`
     loop (constants and merging on gates, `$mux`/`$pmux` folding,
     `opt_muxtree`, `opt_dff`), registers stuck at their power-up value
     found as a fixed point (`opt::seqconst`), `wreduce`, `alumacc`, M4Ks
     (`m4k.txt`'s rules and costs, `m4k_map.v`), the other memories to
     registers, carry chains (`arith_map.v`), lowering to gates, the LE
     register (`dfflegalize` to the six types, `cells_map.v`), the LUT
     mapper on the IR (same LUTs and depth as on yosys' JSON), pin buffers
     (`iopadmap`). Measured with chipsmith's front end, packer, placer and
     router (`cargo run --release --example de2_synth`, `--yosys` for the
     yosys flow as the build ran it before):

     | design | LUTs | registers | LEs | carry chains | M4Ks | Fmax (estimate) |
     |---|---|---|---|---|---|---|
     | picorv32 | 1,058 (yosys flow 1,062) | 431 (434) | 1,171 (1,175) | 8 (8) | 0 | 123.4 MHz (114.4) |
     | ram | 151 (151) | 32 (32) | 151 (151) | 1 (1) | 7 (7) | 263.2 MHz (263.2) |
     | ram_widths | 78 (78) | 32 (32) | 78 (78) | 1 (1) | 10 (10) | 263.2 MHz (210.0) |
     | ram-leds | 40 (40) | 34 (34) | 42 (42) | 2 (2) | 2 (2) | 198.5 MHz (204.3) |
     | bidir | 28 (28) | 27 (27) | 29 (29) | 1 (1) | 0 | 296.5 MHz (296.5) |
     | blinky-verilog, blinky-vhdl | 26 (26) | 26 (26) | 26 (26) | 1 (1) | 0 | 296.5 MHz (296.5) |
     | persist loader | 3 (3) | 0 (0) | 3 (3) | 0 | 0 | no clock |

     The M4K cells (mode, width, contents, which pins are tied) are the
     yosys flow's, cell for cell. picorv32 synthesizes in 1 s (yosys 14 s).
     `crates/chipsmith-cyclone2/tests/synth.rs` proves every example, the
     loader and five specimens sequentially equivalent to yosys' RTL of the
     same Verilog, every stage on three of them, and that a flipped LUT
     bit fails the proof. The check is chipsmith-netlist's `equiv_*`
     script in two steps (ADR-0007): the RTL against the netlist with x
     resolved, then that against the result, with the registers synthesis
     made constant (989 on picorv32) and its state registers' value sets
     first proven invariant by `sat -tempinduct`, and M4Ks compared at
     their ports. All 174 plain-Verilog cyclone2-re specimens pass it and
     the packer.
   - In the build (2026-10-25): one `synthesis` step (front end, the cell
     library `crates/chipsmith-cyclone2/src/synth/cells.v`, synthesis,
     netlist JSON) replaced both yosys runs and the LUT mapping step. Every
     DE2 example and the flash loader build to the same RBF as `de2_synth`,
     byte for byte; for blinky-verilog, blinky-vhdl and the loader that is
     also the RBF the yosys flow built. picorv32 builds in 42 s (41.6 s CPU), against 50 s
     (50.0 s CPU) through yosys. The nix
     package carries no cyclone2-re techmap any more, and yosys only for
     the Zybo.
5. **Own Verilog frontend** (done for the DE2, in the build since
   2026-10-25): the synthesizable subset that GHDL and HDL generators emit.
   - Elaboration (done, 2026-10-25, the DE2 build's front end since then): `crates/chipsmith-verilog`
     reads Verilog to the netlist yosys has after `read_verilog; hierarchy;
     proc`, and `chipsmith_synth::flatten` flattens it. Every example
     (picorv32, the RAMs, bidir, the Zybo designs, the persist loader),
     GHDL's translation of the VHDL ones and all 250 of cyclone2-re's
     specimens are proven sequentially equivalent to yosys' front end
     (`crates/chipsmith-verilog/tests`). Through `cyclone2.ys` the DE2
     examples give the same cells as with yosys' front end, except
     picorv32: 989 LUTs instead of 993 (ADR-0006). picorv32 elaborates in
     0.1 s, against 1.2 s for yosys.

6. **Persistent flashing**: `chipsmith flash --persist` writes the board's
   configuration flash so the design survives a power cycle. `flash` as it
   is loads configuration RAM over JTAG (fast, gone at power-off).
   - DE2 (done, 2026-10-24). The EP2C35 configures itself from its EPCS16
     at power-up (active serial, RUN/PROG switch in RUN, where it normally
     is; PROG isn't needed). chipsmith writes the EPCS over the JTAG cable
     through a serial flash loader of its own: `crates/chipsmith-cyclone2/src/persist/loader`, a
     JTAG-to-SPI bridge on the FPGA's flash pins, built by chipsmith (the
     nix package prebuilds it). It uses the Cyclone II's JTAG and
     serial-flash hard blocks (`CY2_JTAG`, `CY2_ASMI`, `crates/chipsmith-cyclone2/src/hard.rs`),
     which cyclone2-re's chip database and cram gained for this; Quartus
     assembles chipsmith's loader byte-identical. With USER0 in the IR each
     Shift-DR is one SPI transaction, so the loader is three LUTs and no
     clock. The image is what Quartus writes for an EPCS16 (the RBF with
     the passive-serial option bit cleared, each byte bit-reversed; the
     factory image is stored the same way). After writing, chipsmith reads
     the image back, then sends `PULSE_NCONFIG` (IR 0x001), which makes the
     FPGA reconfigure from the EPCS as at power-up, and checks CONF_DONE
     and the USERCODE. So `flash --persist` ends with the board running the
     design from flash.

     Observed on the DE2: blinky-verilog and ram-leds written (856,076
     bytes, about 50 s, half of it erase and page-program polling),
     verified, and running from the EPCS after `PULSE_NCONFIG` (camera:
     counter, then the bouncing light). A dump of the whole flash, read
     twice and compared (`chipsmith flash-dump`, hidden, 2 MB in 134 s),
     was saved before anything was erased. Writing it back
     (`chipsmith flash-restore`, hidden) brought the factory design up
     again from flash (USERCODE 0x0042b391), so the factory image is
     restorable. After a real power cycle (2026-10-26) the board came up
     from the EPCS by itself: examples/de2-jtag-counter read back `"DE2!"`
     counting at 50.00 MHz over JTAG.
   - Zybo (done, 2026-10-24, through Linux). With NixOS on the board,
     nixos-fpga's `hardware.fpga.bitstream` loads the build's `.bin` at
     every boot ("Loading a Zybo from its own Linux", above), so
     `--persist` with `--ssh` points there instead of writing flash.
     Without Linux, persistence means writing a boot image to QSPI, which
     is still not done:

     The QSPI flash hangs off the Zynq's processor (PS
     MIO 1-6); the PL can't reach it, so a PL bridge like the DE2's can't
     work. openFPGALoader refuses to write Zynq flash for that reason;
     Vivado's `program_flash` runs an FSBL plus a U-Boot helper on the ARM
     core over the debug port (traced for the ZynqMP in openFPGALoader
     issue 654). The PS BootROM loads only a first-stage
     loader from QSPI, never a bitstream, so a PL-only design still needs
     a boot image: a BootROM header, a first-stage loader in OCM, the
     bitstream. It doesn't need Xilinx's FSBL. The honest minimum is three
     pieces chipsmith doesn't have yet:
     1. ARM debug access over JTAG: the ADIv5 DAP on the Zybo's chain
        (IR 4, beside the xc7z010), DPACC/APACC scans, AHB-AP memory reads
        and writes. With that the host can drive the PS's QSPI controller
        (0xE000D000; MIO and LQSPI clock set up in the SLCR) in I/O mode
        and program the 128 Mbit QSPI flash itself, with no code on the ARM.
     2. A first-stage loader of our own, built with an open ARM toolchain
        from nixpkgs: about 40 instructions that start a PCAP DMA from the
        QSPI linear window (0xFC000000 + offset) into the PL and wait for
        DONE. No DDR or PLL setup for a PL-only design.
     3. The BootROM header (UG585 section 6.3: width word, `XLNX`, loader
        offset, length, checksum), written by chipsmith; bootgen isn't
        needed for an unencrypted image.
     The board then also has to boot from QSPI: the boot-mode jumper JP5
     must be on QSPI, which only someone at the board can set (chipsmith
     can't read it until piece 1 exists: it's SLCR BOOT_MODE,
     0xF800025C). A PS soft reset over the DAP (SLCR PSS_RST_CTRL) would
     then rerun the BootROM without a power cycle. On the Zybo JPROGRAM
     only clears the PL; the PL is the PS's slave, so it doesn't reload
     from flash the way a standalone 7-series in master mode does.
     The SD card is the other path (BOOT.bin on FAT32, same boot image
     and jumper).

Next devices:

- **Zybo** (Zynq-7000, 7-series fabric): it builds and loads (above).
  Project X-Ray already documents the fabric, so owning the stages means
  importing prjxray-db, not reverse engineering. xc7-re holds the
  groundwork. Same order as for Cyclone II: router, placer, packer.
  The flow: yosys; nextpnr packs (`--pack-only`); chipsmith places;
  nextpnr routes that placement (`-o preplaced`, nothing left to place)
  for the site routing; chipsmith reroutes the interconnect; nextpnr
  writes the FASM and times it (`--report`).
  - Router (done, interconnect only). The routing graph comes from
    prjxray-db (`tilegrid.json`, `tile_type_*.json`, `tileconn.json`, and
    `segbits`/`ppips` for which pips a bitstream can express), into the
    same `ChipDb` as the Cyclone II graph. A nix derivation
    (`xc7-routing-graph`) prebuilds it: xc7z010 has 13,440 tiles of 90
    types, 3,607,758 tile wires in 1,200,334 nodes and 12,346,252 pips.
    The import takes 7.5 s in the nix build (15 s on a busy run), the
    180 MB cache 2.0 s to load at a load
    average of 18 (up to 8 s on the busier runs). Pips are
    taken as nextpnr's generator takes them: bidirectional ones become one
    pip each way, and pips with no FASM feature are dropped. Pseudo pips
    (route-throughs of a LUT, IOB or OLOGIC) are dropped too, since they
    need a free bel and site configuration the router doesn't model.

    nextpnr's xilinx graph has the sites inside it, and chipsmith's doesn't
    yet. So the flow is: nextpnr routes (chipsmith's placement, below); chipsmith keeps
    everything of nextpnr's routing that isn't an interconnect pip (site
    pips, the LUT pin nextpnr permuted a signal onto, route-throughs) and
    reroutes the rest with the Cyclone II's PathFinder, between the site
    pins nextpnr chose. nextpnr then packs the netlist again with each cell
    pinned to its bel (`-o preplaced`) and every net locked to chipsmith's
    route (`-o prerouted`), and writes the FASM. On both designs below it
    bound all of chipsmith's routes unchanged (0 collided; the routes it
    writes back are pip for pip the ones given), and its router had
    nothing left to route. `chipsmith_xc7::route` also checks every route is a tree
    of graph pips reaching all its sinks, with no node shared.

    Same placement, nextpnr's router2 against chipsmith's (routing is
    congestion-driven only: no 7-series delay model yet). Pips are
    interconnect pips of the prjxray graph; Fmax is nextpnr's own STA of
    each routing:

    | design | router | time | pips | iterations | Fmax (nextpnr STA) |
    |---|---|---|---|---|---|
    | zybo-jtag-counter | nextpnr router2 | 142 s | 1,790 | 6 | 317.2 MHz |
    | zybo-jtag-counter | chipsmith | 4.6 s | 1,764 | 2 | 312.0 MHz |
    | picorv32 (Zybo pins, not for hardware) | nextpnr router2, in the build | 154 s | 16,443 | 8 | 181.3 MHz |
    | picorv32 | nextpnr router2, again on that placement | 37.8 s | 16,356 | 9 | 166.3 MHz |
    | picorv32 | chipsmith | 39.7 s | 16,259 | 4 | 143.5 MHz |

    The machine was shared and loaded throughout (load averages of 10 to
    70 on 8 cores), so times are rough: chipsmith's picorv32 routing took
    39.7 s to 69 s across runs. The second nextpnr picorv32 row and
    chipsmith's ran back to back at a load average near 10. nextpnr
    spends most of the counter's 142 s on the four nets from the BSCANE2
    in the configuration tile (24 to 38 s each). Its two picorv32 routings
    of one placement differ by 9% in Fmax. The A*
    distance weight is device data (`ChipDb::tile_distance`); on
    picorv32, per tile in x/y:

    | weight | time | pips | Fmax |
    |---|---|---|---|
    | 0.125/0.25 | 69.6 s | 16,150 | |
    | 0.25/0.5 (default) | 39.7 s | 16,259 | 143.5 MHz |
    | 0.5/1 | 19.3 s | 16,856 | 127.7 MHz |
    | 1/2 | 10.9 s | 17,885 | |

    Both designs' bitstreams build (fasm2frames, xc7frames2bit), and
    zybo-jtag-counter runs on the Zybo (above). Vivado 2026.1 (xc7-re
    `tools/oracle.sh`, its own placement and routing of the same Verilog,
    8 ns clock) has the counter's clock domain at WNS 5.353 ns, a 2.647 ns
    critical path (377.8 MHz), where nextpnr's STA estimates 317 MHz on
    nextpnr's placement. Vivado can't take chipsmith's routing (that needs
    LOC/BEL constraints and `FIXED_ROUTE` per net), so this compares
    tools, not routings.

    Open: timing-driven routing (prjxray-db's pip and wire delays into
    `timing.rs`), which picorv32's Fmax gap calls for; the site graph,
    so chipsmith can choose site pins and LUT pin permutations itself and
    no longer needs nextpnr's routing to start from; and FASM from
    chipsmith's own writer.
  - Placer (done). chipsmith places nextpnr's packed netlist (the cells
    its packer writes with `--pack-only`: SLICE_LUTX, SLICE_FFX, CARRY4,
    RAMD32, PAD, IOB33 buffers, BUFGCTRL, BSCAN) and hands every cell's
    bel to nextpnr as `-o preplaced` lines. nextpnr binds all of them and
    places nothing itself; `chipsmith_xc7::route` checks that each cell is on the
    bel chipsmith gave it before rerouting. Packing names are
    deterministic, so the packer's own cells (split carries, feed-through
    LUTs, pin buffers) match by name in every later nextpnr run.

    Slices are filled in shapes that are legal by construction, a subset
    of nextpnr's `xc7_logic_tile_valid`: a BLE (a LUT in an `x6LUT` and
    the register in the `xFF` beside it, or either alone) goes anywhere,
    and the only per-slice rule left is one control set (clock, CE, SR
    and their modes) per slice; a carry chain owns whole slices up its
    column, following COUT to the next CIN through the prjxray graph (and
    so across the HCLK row), with its S LUTs in the `x6LUT`s, its DI LUTs
    in the `x5LUT`s and registers fed by a sum output in that bit's
    `xFF`; a RAM32M owns a SLICEM. Chains and RAMs take the tile's X0
    slice, as nextpnr's packer constrains them. Pins stay where the XDC
    put them, the BSCAN for USER<n> goes to BSCAN_X0Y<n-1>, each BUFGCTRL
    to the free one nearest its source. Anything else (block RAM, DSPs,
    SRLs, wide muxes, I/O logic, other RAM shapes) leaves the placement
    empty and nextpnr places the design as before. The 5LUTs and 5FFs
    stay unused outside carry chains, so chipsmith fills more slices than
    nextpnr does.

    The annealer is the Cyclone II one's (VPR schedule, range limit,
    half-perimeter wirelength with x at half weight, since grid x counts
    site columns), with moves that swap BLEs, or move a chain or RAM and
    hand the BLEs in its new slices to the slices it left, slice for
    slice, so control sets never mix. Eight anneals run on eight threads
    from one start with fixed seeds, and the cheapest wins: the result
    doesn't depend on the machine.

    Timing-driven: prjxray-db's timing data loads in a few lines. Cell
    delays come from `timings/slicel.sdf` (slow corner, max: LUT 124 ps,
    clock-to-Q 303 ps, every CARRY4 arc), routing from distance, hop by
    hop, with the mean pip delay per wire class of `tile_type_INT_L.json`
    (hexes up and down, quads across, then doubles and singles, plus the
    IMUX pip). Every temperature the placer runs the STA of `timing.rs`
    and weights each net by `1 + 20 * criticality^4`. prjxray's pip delays
    have no wire RC, and the estimate is rough either way: picorv32's
    critical path comes out at 5.93 ns against 5.89 ns by nextpnr's STA
    of chipsmith's routing, jtag-counter's at 4.07 ns against 2.84 ns.
    Only its proportions matter.

    nextpnr's placement (HeAP and SA, in the old flow) against
    chipsmith's, each routed by chipsmith's router. Fmax is nextpnr's STA
    (`--report`) of that routing; wirelength is chipsmith's measure
    (half-perimeter, x halved) of both placements of the same packed
    netlist. Release builds, 8 cores; another job shared the machine.

    | design | placer | wirelength | place time | Fmax, chipsmith's routing | Fmax, nextpnr's router2 |
    |---|---|---|---|---|---|
    | zybo-blinky | nextpnr | 165.5 | 0.7 s | 391.5 MHz | 391.5 MHz |
    | zybo-blinky | chipsmith | 172 | 0.5 s | 391.5 MHz | 391.5 MHz |
    | zybo-jtag-counter | nextpnr | 569 | 1.0 s | 312.0 MHz | 317.2 MHz |
    | zybo-jtag-counter | chipsmith | 523 | 0.7 s | 352.5 MHz | 352.5 MHz |
    | zybo-picorv32 | nextpnr | 7,562.5 | 10.0 s | 143.5 MHz | 181.3 MHz |
    | zybo-picorv32 | chipsmith | 7,484 | 9.2 s | 169.8 MHz | 172.6 MHz |

    blinky is its 7-CARRY4 chain either way. Whole builds, at a load
    average of 5 to 8: blinky 71 s against 55 s, jtag-counter 87 s
    against 66 s, picorv32 110 s against 101 s. The steps that changed
    account for about 3 s of that (the `--pack-only` run takes 2 s, and
    chipsmith's placing about what nextpnr's took); the rest is the load
    (yosys alone took 38 s instead of 17 s on the counter).

    Picking the weights, nextpnr's Fmax on chipsmith's routing (the
    placement changes with the seeds, and one placement's Fmax moves by
    about 10% with it):

    | placer | picorv32 | jtag-counter |
    |---|---|---|
    | wirelength only | 132.6 MHz | 352.5 MHz |
    | timing weight 8 (the Cyclone II's) | 137.3, 125.2 MHz | 366.2 MHz |
    | 8, criticality^2 | 156.6 MHz | 366.2 MHz |
    | 8, wire hops doubled | 112.1 MHz | 351.3 MHz |
    | 20 (default) | 169.8, 156.7, 146.3 MHz | 352.5, 366.2, 366.2 MHz |
    | 20, wire hops doubled | 160.6 MHz | 366.2 MHz |
    | 40 | 148.3, 130.4 MHz | 366.2, 351.3 MHz |
    | 40, criticality^2 | 152.0 MHz | 351.3 MHz |
    | 80 | 163.4 MHz | 352.5 MHz |

    Wirelength alone gives the shortest picorv32 (5,484.5, 27% under
    nextpnr's) and the slowest; its critical path is the reset counter
    into the high-fanout enable of most registers, which only the timing
    weight pulls together. Open: the 5LUTs and 5FFs, a delay model with
    the wires' RC, and chipsmith's own packer, so nextpnr's `--pack-only`
    run goes.
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

A `[clocks]` table gives the frequency of a clock entering at a pin:

```toml
[clocks]
CLOCK_50 = "50 MHz"        # a port bit with a pin in [pins]
```

On the Zynq each clock becomes an XDC `create_clock` for nextpnr; the
build prints nextpnr's Fmax and slack per clock and fails if one misses
its constraint. Cyclone II takes at most one clock, because chipsmith's
timing model times one clock domain. The build prints the estimated
slack after routing and doesn't fail on it: the estimate is not sign-off (yosys-synthesized picorv32: 11.26 ns estimated, 10.60 ns in Quartus). The
placer and router stay timing-driven against the critical path itself,
whatever the clock. Without `[clocks]`, nextpnr times against its default
12 MHz and Cyclone II reports no slack.

Unknown keys are errors, so a misspelt table can't be silently ignored.

## Development

```
nix develop
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -- -C examples/blinky-verilog build
```

A Cargo workspace, one crate per concern
([ADR-0002](docs/adr/0002-cargo-workspace-by-concern.md)):

| crate | |
|---|---|
| `crates/chipsmith` | the binary: manifest, devices, the flow (`Plan`, `Step`), CLI |
| `crates/chipsmith-synth` | device-independent synthesis: flattening, RTL optimisation, arithmetic, memories, lowering to gates, LUT mapping |
| `crates/chipsmith-route` | what both back ends share: the routing graph (`ChipDb`), the PathFinder router, the timing graph |
| `crates/chipsmith-cyclone2` | Cyclone II: synthesis to its cells, packing, placement, routing (nextpnr JSON in and out), M4K, hard blocks, EPCS and its serial flash loader |
| `crates/chipsmith-xc7` | 7-series: prjxray-db import, placement, delays, routing, Zynq `.bin` |
| `crates/chipsmith-jtag` | cables (USB-Blaster, FT2232), SVF player, 7-series JTAG loader |

Cargo examples sit with their crate and run from the root by name, e.g.
`cargo run --release --example xc7_user1` (`chipsmith-jtag`).

The Quartus toolchain manager that used to be chipsmith lives on the `main`
branch.

## License

MIT
