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
| EP2C35F672C6 (Cyclone II) | Terasic DE2 | yes: LUT/carry/register logic, M4K RAM blocks (below), global clocks from pins, 3.3-V LVTTL pins: input, output, tri-state, bidirectional, open drain | yes (2026-10-24: picorv32 built with chipsmith's LUT mapper by the nix package, flashed by chipsmith, counting on the LEDs; examples/bidir driving, releasing and reading back its pins) |
| xc7z010clg400-1 (Zynq-7010, PL only) | Digilent Zybo (original, "legacy") | yes: what yosys `synth_xilinx` and openXC7's nextpnr handle, LVCMOS33 pins; chipsmith routes the interconnect (below) | yes (2026-10-24: zybo-jtag-counter with chipsmith's routing, flashed by chipsmith, read back over JTAG at 125.01 MHz) |

`examples/picorv32` is a RISC-V core (PicoRV32, RV32I) counting on the
LEDs, about 1,200 LEs; a build takes about 85 s of CPU time.

`examples/bidir` is cyclone2-re's tri-state test: expansion-header and
SRAM data pins driven with their output enable from a switch, an
inverted switch, logic or a register, one open drain, all read back onto
the red LEDs, and a tri-state green LED. An inout must be written
`assign p = oe ? d : 1'bz` (yosys' `tribuf` and `iopadmap` make it one
`CY2_IOBUF`); one without a tri-state driver is an error, and so is a
driving pin on a dedicated clock input. With every switch down on the DE2
(2026-10-24, camera): LEDR[1] blinks with the counter it drives out,
LEDR[4] and LEDG[1] follow the open drain, the released pins read 1 and
LEDG[0] stays off. Don't load it with SW[4] up and the SRAM driving: the
FPGA then drives SRAM_DQ too.

M4K RAM blocks (4,608 bits each, 105 on the EP2C35) come from yosys'
`memory_libmap` with cyclone2-re's `synth/m4k.txt`: memories with
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
  value can be seen coming back. Built and validated, not yet run on the
  DE2.

Quartus assembles chipsmith's placement and routing into the same RBF bit
for bit (`bench/quartus-timing.sh`): checked 2026-10-24 on blinky-verilog
and blinky-vhdl (16.65 ns of slack at 50 MHz), on picorv32 (10.60 ns
since cyclone2-re 9cfe9be, see below; 11.08 ns before) and on bidir
(16.64 ns), all built through chipsmith's LUT mapper, and on ram
(16.21 ns), ram_widths (14.80 ns) and ram-leds (14.80 ns).
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

- `examples/zybo-blinky`: a counter on LD0-LD3. Builds in about 36 s.
- `examples/zybo-jtag-counter`: the Zybo's USB-UART is wired to the
  Zynq's processor (PS MIO 48/49, Digilent's Zybo reference manual,
  section 6), not to the FPGA fabric. So without software on the ARM
  cores, no PL design can print to it. This design answers over the JTAG
  cable instead. A BSCANE2 makes it a 64-bit USER1 register holding
  `"ZYBO"` and a Gray-coded 125 MHz counter.
  `cargo run --release --example xc7_user1` reads it twice, a second apart,
  with chipsmith's own cable code. Built in about 61 s with nextpnr's
  routing; with chipsmith's (two nextpnr runs, below) 3 min 45 s, of
  which 1 min 52 s CPU, on a heavily loaded machine. Two builds give the
  same FASM. Read back 2026-10-24:

  ```
  USER1 0x5a59424f07d1540d: tag "ZYBO", count 90281993
  USER1 0x5a59424f0abcdd7f: tag "ZYBO", count 215443882
  125161889 counts in 1.001 s: 125.01 MHz
  ```

  With zybo-blinky loaded instead, the same read returns all zeros.

Zybo limits, all of them inherited from the tools:

- Every pin is LVCMOS33. The manifest has no I/O standards yet, and the
  Zybo's PL banks (34, 35) are both 3.3 V.
- Pins are the package's 100 PL user I/O balls (prjxray-db's
  `package_pins.csv`). PS pins (MIO, DDR) are rejected in the manifest.
- The PS isn't configured. Nothing needs the ARM cores, DDR or the
  PS clocks, and a design instantiating PS7 is untested.
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

## Flow

Every stage is one `Step` in a `Plan` (`src/flow.rs`). The device in the
manifest picks the plan.

Cyclone II (EP2C35F672C6):

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | stays a translator in front of the flow |
| synthesis | yosys runs cyclone2-re's `cyclone2.ys` in two halves around the LUT mapping | to replace |
| LUT mapping | chipsmith: AIG plus priority cuts, in place of `abc -lut 4` (`src/synth/lutmap.rs`) | ours |
| packing | chipsmith: cyclone2-re's nextpnr packer, ported (`src/pack`) | ours |
| placement | chipsmith: simulated annealing over LAB slots, nextpnr's LAB rules (`src/place`) | ours |
| routing | chipsmith: PathFinder over the chip database (`src/route`) | ours |
| bitstream | `cram pack` (cyclone2-re) | ours already |
| JTAG stream | `cram svf` | ours already |
| cable | chipsmith: USB-Blaster and FT2232 (MPSSE) drivers, SVF player, 7-series JTAG loader (`src/jtag`) | ours |

Zynq-7010 PL (xc7z010clg400-1): shell out, except routing and the cable.

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | as for Cyclone II |
| synthesis | yosys `synth_xilinx -flatten -abc9 -arch xc7` | to replace |
| packing, placement, site routing | openXC7's nextpnr (`nextpnr-himbaechel`, xilinx uarch), chip database for xc7z010 built from prjxray-db in nix (2.5 min) | to replace: placer next, then packer |
| interconnect routing | chipsmith: PathFinder over the prjxray-db graph (`src/route/xray.rs`, `src/route/xc7.rs`) | ours |
| FASM | nextpnr's FASM writer, from chipsmith's routes (`-o preplaced`, `-o prerouted`) | to replace |
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
     compares top-level port directions too, since cram decides
     output vs bidir mode by them. Rechecked 2026-10-24 with cyclone2-re
     5eff911 (blinky, lfsr, uart_tx, picorv32 both mappers: identical),
     and for the M4K examples with 9cfe9be (`pack_m4k`: logical ports onto
     the block's pins, buffers in front of its inputs folded, its clock
     through a clock control).
     Parameters nextpnr writes as
     `0` (a flag toggled back) chipsmith leaves out; they read the same.
4. **Own synthesis**: techmap and LUT mapping on the IR. yosys's JSON
   netlist is the boundary until then.
   - LUT mapping (done, in the default Cyclone II flow). yosys runs
     `cyclone2.ys` up to `abc -lut 4` and writes `build/gates.json`.
     chipsmith maps that, and a second yosys runs the rest of the script
     from `opt -fast` on. chipsmith cuts the script at plan time, and the
     build fails if `abc -lut 4` isn't there exactly once. Measured
     2026-10-24, the same design through both flows; slack is from
     `bench/quartus-timing.sh` (byte-identical assembly in every row):

     | design | LUT mapper | mapped LUTs | packed LUTs (LEs) | Quartus slack @ 50 MHz |
     |---|---|---|---|---|
     | picorv32 | yosys `abc -lut 4` | 770 | 992 (1,105) | 11.23 ns |
     | picorv32 | chipsmith | 842 | 1,062 (1,175) | 11.08 ns |
     | blinky-verilog, blinky-vhdl | either | 0 (all carry) | 26 (26) | 16.65 ns, same RBF |

     `bench/lutmap-vs-abc.sh build/` maps one `gates.json` both ways
     (LUTs after `opt -fast`). On picorv32, abc's mapper alone
     (`strash; if -K 4`) gives 847 LUTs and chipsmith's 842, so the
     mapping itself is on par. The 9% gap comes from abc's logic
     optimisation around its mapper: choice nodes (`dch -f`, 810 LUTs)
     and don't-care resynthesis (`mfs2`, `lutpack`, 788). A choice-aware
     mapper is the next gain. More cuts per node (16, 32), more recovery
     passes, or relaxing depth by a level didn't help (844 to 847). An earlier comparison (842 vs 847) had
     counted abc's 75 one-input buffer LUTs, which `opt -fast` removes.
5. **Own Verilog frontend**: the synthesizable subset that GHDL and HDL
   generators emit.

6. **Persistent flashing**: `chipsmith flash --persist` writes the board's
   configuration flash so the design survives a power cycle. `flash` as it
   is loads configuration RAM over JTAG (fast, gone at power-off). DE2: the
   EPCS16, written through a JTAG-to-SPI bridge design loaded first. Zybo:
   a boot image (FSBL + bitstream) in QSPI flash, the same way, or on the
   SD card.

Next devices:

- **Zybo** (Zynq-7000, 7-series fabric): it builds and loads (above).
  Project X-Ray already documents the fabric, so owning the stages means
  importing prjxray-db, not reverse engineering. xc7-re holds the
  groundwork. Same order as for Cyclone II: router, placer, packer.
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
    yet. So the flow is: nextpnr packs, places and routes; chipsmith keeps
    everything of nextpnr's routing that isn't an interconnect pip (site
    pips, the LUT pin nextpnr permuted a signal onto, route-throughs) and
    reroutes the rest with the Cyclone II's PathFinder, between the site
    pins nextpnr chose. nextpnr then packs the netlist again with each cell
    pinned to its bel (`-o preplaced`) and every net locked to chipsmith's
    route (`-o prerouted`), and writes the FASM. On both designs below it
    bound all of chipsmith's routes unchanged (0 collided; the routes it
    writes back are pip for pip the ones given), and its router had
    nothing left to route. `route::xc7` also checks every route is a tree
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
slack after routing and doesn't fail on it: the estimate is not sign-off (picorv32: 11.90 ns estimated, 11.08 ns in Quartus). The
placer and router stay timing-driven against the critical path itself,
whatever the clock. Without `[clocks]`, nextpnr times against its default
12 MHz and Cyclone II reports no slack.

Unknown keys are errors, so a misspelt table can't be silently ignored.

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
