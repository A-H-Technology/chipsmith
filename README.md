# chipsmith

An open FPGA toolchain: HDL in, programmed chip out, no vendor software.

```
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl build
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl flash
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl flash --persist  # DE2: into the EPCS
nix run github:A-H-Technology/chipsmith/open -- -C examples/zybo-jtag-counter flash --persist  # Zybo: into the QSPI
nix run github:A-H-Technology/chipsmith/open -- -C examples/zybo-jtag-counter flash --ssh venus  # Zybo, through its Linux
```

The first run builds cyclone2-re's device database package and flattens
its chip database into the router's graph. After that a build of blinky-vhdl takes about 6 s. A DE2 build
runs no program but chipsmith between the Verilog and the chip: no yosys,
no nextpnr, no cram. GHDL translates VHDL in front of it. Since
2026-11-02 a Zybo build runs no other program either: no yosys, no
nextpnr, no prjxray tool, only prjxray-db's data.

## Status

| device | board | build | on hardware |
|---|---|---|---|
| EP2C35F672C6 (Cyclone II) | Terasic DE2 | yes, with no external tool but GHDL for VHDL (2026-10-25: chipsmith's own Verilog front end and synthesis): LUT/carry/register logic, M4K RAM blocks and embedded multipliers (below), global clocks from pins and from logic (a register, the JTAG block's TCK; one clock per LAB), PLLs (`CY2_PLL`; both on the board 2026-10-26, examples/de2-jtag-counter on TCK and examples/de2-pll-counter at 100 and 40 MHz from CLOCK_50; 2026-10-30 a PLL clock on its PLL's clock output pin, 2026-10-31 on any pin or into logic as data, the DE2's SDRAM run at 100 MHz with 0 errors by examples/sdram-selftest, its SRAM by examples/sram-selftest); pins input, output, tri-state, bidirectional, open drain, at 3.3-V LVTTL (any current) or LVCMOS or 2.5-V inputs, with pull-up or bus hold and I/O-cell registers (with the cell's clock enables, asynchronous clears and presets and falling-edge clocks as Quartus packs them since 2026-10-31, examples/de2-ioregs; an output or OE register's synchronous reset as logic on its D; Manifest, below) | yes (2026-10-25, chipsmith's own synthesis end to end, checked over JTAG: examples/de2-jtag-counter (its earlier CLOCK_50-sampled version) reads back `"DE2!"` at 50.00 MHz after a JTAG load and after `flash --persist` (loader synthesized by chipsmith too, EPCS written, verified and reloaded from; 2026-10-26 also after a power cycle); picorv32 with the same readback added runs its loop at 3.33 MHz; 2026-11-09 examples/spade-jtag-counter, the same design in Spade, at 50.00 MHz ("Spade", below). 2026-10-24, synthesized by yosys: picorv32 built with chipsmith's LUT mapper by the nix package, flashed by chipsmith, counting on the LEDs; examples/bidir driving, releasing and reading back its pins; examples/ram-leds stepping its ROM and RAM on the LEDs; `flash --persist` writes the EPCS16 and the design runs from it, roadmap step 6) |
| xc7z010clg400-1 (Zynq-7010, PL only) | Digilent Zybo (original, "legacy") | yes, with no external tool but GHDL for VHDL (2026-11-01: chipsmith's own Verilog front end, synthesis, packing, placement, routing and FASM; 2026-11-02: frames and `.bit`; "The Zybo flow", below): LUT1-LUT6, CARRY4, FDRE/FDSE/FDCE/FDPE, RAM32M, block RAM (RAMB18E1, since 2026-11-02; "7-series block RAM", below), multipliers (DSP48E1, since 2026-11-02; "7-series multipliers", below), LUT6_2 pairs, LVCMOS33 pins, BUFGs, BSCANs, PLLs and MMCMs (PLLE2_ADV, MMCME2_ADV, since 2026-11-04; ADR-0017) and the PS7 | yes, with the earlier flow (2026-10-24, yosys and nextpnr around chipsmith's placement and routing: zybo-jtag-counter read back over JTAG at 125.00 MHz, loaded over JTAG and with `flash --ssh` through Linux on the board; a bitstream loaded at every boot through nixos-fpga). chipsmith's own flow is checked at the bitstream (decoded and co-simulated against the RTL, below) and, since 2026-11-06, on the board: zybo-jtag-counter (125.01 MHz), zybo-mult-selftest, zybo-riscv-bram and zybo-bram-selftest (every RAMB18E1 mode it has) read back over JTAG with 0 errors (riscv-bram's register file is 11 RAM32Ms, so their write-data pins as xc7-timing fixed them run on silicon), and through a webcam zybo-blinky's LD0 at 0.93 Hz (125 MHz / 2^27) and zybo-pll's four LEDs, two bits of a counter on each PLL output (125 and 62.5 MHz), at 0.96/1.83 Hz each (expected 0.93/1.86) |

`examples/picorv32` is a RISC-V core (PicoRV32, RV32I) counting on the
LEDs, 852 LEs and two M4Ks (its register file); a build takes 20 to 24 s
on the benchmark machine at load 1.6 to 8 (Compile speed, below).

chipsmith builds much faster than Quartus II 13.0sp1 Web Edition, at
every design size we've tried. Altera's license forbids publishing
benchmarks of Quartus, so there are no numbers here; `bench/vs-quartus.sh`
runs the comparison against your own Quartus install. Since branch
`qor5` (2026-11-07) chipsmith maps each design's gates as built, with
functionally equivalent nodes merged, and also refactored, and keeps the
mapping with the fewest LUTs (femtorv32 518 -> 470 LEs, mult-selftest
956 -> 858, picorv32 876 -> 852, picorv32x24 21,241 -> 20,832).
chipsmith's Fmax estimate is −0.7% to +4.7% off quartus_sta's on its own
placements (femtorv32, picorv32, sha256, mult-selftest, sdram-selftest).
Before the cell delays were measured on silicon (docs/timing-silicon.md)
it was +1.0% to +10.8% above.

`examples/de2-jtag-counter` answers over the JTAG cable, so a load can be
checked without watching the board: the Cyclone II's JTAG block makes it a
64-bit USER1 register holding `"DE2!"` and a Gray-coded 50 MHz counter.
`cargo run --release -p chipsmith-cyclone2 --example de2_user1` reads it
twice, a second apart. Read on 2026-10-26, clocked by TCK:

```
USER1 444532210c0ec0a4: tag "DE2!", counts 134971192
USER1 444532210e8c6078: tag "DE2!", counts 185090128
counter 0: 50118936 counts in 1.002 s: 50.00 MHz
```

Any design's pins can be checked the same way, without a USER1 register:
`cargo run --release -p chipsmith-cyclone2 --example de2_pins -- examples/sha256`
reads them with the EP2C35's boundary-scan SAMPLE instruction (Altera's
BSDL: 1449 cells, input, control and output per ball), which captures every
pin without taking it from the design, and prints each signal of the
project's `[pins]`. On the DE2 2026-11-06 (chipsmith main 1400504, loaded
over JTAG): sha256 folding with LEDG[8] (its core's error flag) 0 in 300
samples; spi-master's SCLK and MOSI toggling, CS_n high between transfers
and mirrored on LEDG[2], MISO high (no loopback jumper) so its received
byte reads 0xff; de2-ioregs' LEDR[3:0] stepping about 3 times a second on
`tick` through the I/O cells' clock enable; vga-pattern's sync pins over
3000 samples low 11.4 % (HSYNC, 640x480 expects 96/800 = 12.0 %), 0.37 %
(VSYNC, 2/525 = 0.38 %) and BLANK 26.3 % (26.9 %), VGA_CLK at 51 %.

Since 2026-10-26 the register is clocked by TCK itself: the JTAG block's
TCK output goes through the core to a clock control and onto a global
clock (cyclone2-re README, "Global clocks from the core"), so the reader
shifts in byte mode. Quartus assembles chipsmith's placement and routing
of it into the same RBF (`bench/quartus-validate.sh`), and on the board
(2026-10-26) it reads back `"DE2!"` counting at 50.00 MHz. The cable
samples TDO after the shifting edge in byte mode too, so the reader still
gets the register from its bit 1. Before, the DE2 back end clocked registers from pins only, so the
register ran on CLOCK_50 and sampled TCK through two flip-flops, and the
reader had to bit-bang its shift slowly enough for that sampling, its
first bit being the register's bit 1.

PLLs: a design instantiates `CY2_PLL` (`crates/chipsmith-cyclone2/src/synth/cells.v`)
with altpll's parameters, the input period and each output's
multiply/divide ratio, phase shift and duty cycle, and chipsmith finds the
counters (`crates/chipsmith-cyclone2/src/pll.rs`, ADR-0008). Its INCLK must
come from a dedicated clock pin, which picks the PLL: CLOCK_50 (PIN_N2)
feeds PLL_1, CLOCK_27 PLL_3, EXT_CLOCK PLL_2. Outputs CLK0-CLK2 reach the
registers on global clocks; LOCKED, and the ARESET and PFDENA inputs, are
logic. The timing report times each output at its frequency.

`examples/de2-pll-counter` measures its PLL over JTAG: PLL_1 makes 100 MHz
and 40 MHz from CLOCK_50, each clock runs a Gray-coded counter, and a
128-bit USER1 register clocked by TCK holds `"PLL!"` and the three counts.
`cargo run --release -p chipsmith-cyclone2 --example de2_user1 -- --words 4`
prints counter 1 at 2.000000 and counter 2 at 0.800000 times counter 0
(CLOCK_50), observed on the DE2 on 2026-10-26: 50.00, 100.0000 and
40.0000 MHz; LEDG[0] is the lock. Quartus assembles chipsmith's placement
and routing of it byte-identically (setup slack 6.21 ns at 100 MHz in
quartus_sta, 6.20 ns estimated).

A PLL's dedicated way off the chip is its clock output pin: CLK2
(counter 2) through that pin's own clock control, PLL_1's being PIN_AA7,
the DE2's DRAM_CLK (cyclone2-re README, "PLLs"; PLL_2: F21, PLL_3: E5,
PLL_4: V21, each where Quartus puts it). chipsmith builds a `CY2_PLL` CLK2
alone on that pin that way. PLL_4 takes the bottom edge's clock pins
(AE14, AF14, AD13, AC13: SW[3]-SW[6] on the DE2); a PLL fed from AE14
with CLK2 on V21 is Quartus byte-identical (`tests/gclk_data.rs`).
Since 2026-10-31 a PLL clock is also data, as in Terasic's `assign VGA_CLK
= pll_clk`: on any other pin, into a LUT or a register's data, it travels
on its global and leaves it through a row clock buffer onto a LAB line,
as Quartus routes it (cyclone2-re README, "Global clocks as data";
docs/de2-periph-status.md). Quartus assembles both byte-identically.

Phase shifts are picked the way Quartus picks them: the shift rounds to
the nearest eighth of a VCO period, and the VCO is the one that hits all
shifts best (then nearest 800 MHz). A negative shift isn't folded into
the period: the feedback counter waits for it and every output with it,
so the outputs lead the input clock. A shift needing more than a preset's
16 VCO cycles is an error (Quartus clamps it, warning 15559). For 34
altpll designs Quartus fit from CLOCK_50 (cyclone2-re's `clk_jobs.txt`,
-20 to +20 ns, 10 to 100 MHz), chipsmith picks the same counters
(`pll.rs` tests), and the 46 Quartus PLL designs added 2026-10-31
assemble byte-identically in cyclone2-re's `cram roundtrip`. The lock
window goes by the phase detector's period (input period times n), not
m.

`examples/sdram-selftest` runs the DE2's 8 MB SDRAM (IS42S16400): a
100 MHz controller on PLL_1's CLK0, the SDRAM clocked from CLK2 on
DRAM_CLK at -1.5 ns (the same clock as +8.5 ns, one period later; the
board runs below were at +8.5 ns), every SDRAM pin through its I/O-cell
registers, 8-word bursts with auto precharge at CAS latency 3. Each pass
writes all 4M words with a 32-bit LFSR seeded from the pass number and
reads them back. USER1 holds `"SDR!"`, the passes, the words that read
back wrong and a CLOCK_50 counter (`de2_user1 -- --words 4`: counter 1 is
the errors, counter 2 the passes). The controller's registers drive the
pins straight; their enables go into the I/O cells' clock enables and
their synchronous resets into logic on the I/O registers' D (Manifest,
below): 777 LEs and Quartus byte-identical since branch de2-ioece (Fmax
estimate 147.3 MHz since gpu-pnr's placer, 163.3 at de2-ioece's merge),
on the board 2026-11-06 (below). On the DE2 2026-10-30 (loaded
over JTAG, 810 LEs with the enables as logic too, 65 registers in I/O
cells, chipsmith de2-ioreg): 5,145
passes at +8.5 ns, 0 errors, 5.88 passes a second. Earlier the same day,
with an extra plain register stage in front of the pins (776 LEs, read
latency 4): 6,989 passes at +8.5 ns,
0 errors (58.6 GB written and checked), and 1,823 passes, 0 errors, at
+7 ns; 5.88 passes a second, 98.7 MB/s of SDRAM traffic, half the bus's
200 MB/s (16 cycles per 8-word burst). Swept over DRAM_CLK's shift, 25 s
each: 0 errors from +6 to +10.5 ns, nearly every word wrong at +5.5 and
+11.5 ns, so +8.5 ns is the window's middle (docs/de2-periph-status.md). Quartus
assembles chipsmith's placement and routing byte-identically, also at
-1.5 ns (2026-10-31; on the board 2026-11-06, below): m 20, the feedback counter
1.5 VCO cycles late, the counters Quartus' own altpll fit picks. The read
latency (`RD_LAT`, FPGA clocks from the controller's READ to the input
registers holding the first word) was found on the board: a build with
the extra stage checking latencies 4-7 side by side read 0 errors at 4
and every word wrong at 5-7. Without the stage the READ reaches the pins
a clock earlier, so it is 3.
Rebuilt with chipsmith main 1400504 (GPU placer, qor rounds 1-3, DRAM_CLK
at -1.5 ns) and run on the DE2 2026-11-06, loaded over JTAG: `"SDR!"`, 0
errors, the passes rising.

`examples/sram-selftest` runs the DE2's 512 KB asynchronous SRAM
(IS61LV25616AL-10, 256K x 16): a 100 MHz controller on PLL_1's CLK0,
every SRAM pin but the constant chip and byte enables through its
I/O-cell registers, a write in 3 clocks (WE# low for one), a read in 2
(the input registers sample on the edge that puts out the next address).
Each pass writes all 256K words from a 32-bit LFSR seeded from the pass
number and reads them back. USER1 holds `"SRM!"`, the passes, the words
that read back wrong and a CLOCK_50 counter (`de2_user1 -- --words 4`:
counter 1 is the errors, counter 2 the passes). 539 LEs since branch
de2-ioece (the enables the cells' own; 606 with them as logic, the build
the board ran), 68 registers in I/O cells; Quartus byte-identical. On the
DE2 2026-10-30 (loaded over
JTAG): 46,680 passes over 611 s, 0 errors (23 GiB written and checked),
76.4 passes a second, 80.1 MB/s (512 KB written
and 512 KB read per pass; the bus would carry 200 MB/s, the controller
spends 5 clocks per word written and read). The checker does catch a
wrong read: a build reading a clock late (`RD_LAT` 3) counted
226,208,709 errors in 862 passes, nearly every word. Not to be loaded
with examples/bidir's SW[4] up: that one drives SRAM_DQ too.
Rebuilt with chipsmith main 1400504 (GPU placer, qor rounds 1-3) and run
on the DE2 2026-11-06, loaded over JTAG: `"SRM!"`, 0 errors in 2,200
passes over 30 s.

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

`examples/de2-io` sets its pins up from the manifest (Manifest, below):
the high switches through input registers onto five red LEDs through
output registers, green LEDs at 4 mA through output registers (one
powering up lit, behind an inverter), weak pull-ups and bus hold on
expansion-header inputs shown on the LEDs, HEX0 at 3.3-V LVCMOS, the keys
read as 2.5-V inputs, and GPIO_0[4] driven through its output and OE
registers while KEY[0] is held, read back through its input register.
Safe to load with nothing plugged into the GPIO_0 header: everything else
it drives is an LED or the 7-segment digit. Loaded on the DE2 2026-10-30
over JTAG. With nothing to watch the LEDs (no camera on the board), a
copy with a USER1 readback of its inputs read every switch down through
the input registers, both keys released (2.5-V inputs), the pulled-up
and bus-hold GPIO_0 inputs at 1 and GPIO_0[4] released and pulled up,
as the board was. The outputs are unobserved.

`examples/de2-ioregs` (2026-10-31) puts registers with the I/O cell's
own controls on the DE2's pins: LEDR from a counter and a shift register
whose enables and asynchronous clear and preset are the cells',
two LEDs sampled on the falling edge, four switches through input
registers with an active-low enable (KEY[1]) and clear (KEY[3]), folded
into the cells, and GPIO_0[4] driven while KEY[2] is held, its input,
output and OE registers on one reset. Since 2026-11-01 that reset is
KEY[0], active low, written as Terasic writes it (`always @(posedge
clk or negedge rst_n)`), and it clears the core registers too, the
24-bit divider's carry chain across two LABs included (it was SW[17],
active high). Safe to load with nothing on the GPIO_0 header. 40 LEs;
Quartus byte-identical; on the board 2026-11-06, LEDR[3:0] stepping on
`tick`, read over boundary scan (`de2_pins`, above).

`examples/vga-pattern` puts 640x480 at 60 Hz on the VGA DAC: colour
bars, four ramps, a white border, the DAC's inputs through I/O-cell
registers (with the reset synthesis finds in `visible ? r : 0` as logic
on their D), 25 MHz pixels from PLL_1, 516 LEs. VGA_CLK (PIN_B8) is
the pixel clock inverted, `assign VGA_CLK = ~pclk` as Terasic writes it,
rising halfway through each pixel (since 2026-10-31; before, a register
on a second PLL clock at 50 MHz made it). USER1 holds `"VGA!"` and counts of the pixel clock
and the frames: on the DE2 2026-10-30, 25.0000 MHz and 59 frames a
second (59.52 expected); the build without the extra register stage
read 25.0000 MHz and 60 (both builds with the register-made VGA_CLK).
The direct VGA_CLK ran on the board 2026-11-06 (sync timing over
boundary scan, above), and a monitor has shown the picture. Quartus
byte-identical.

Pin settings are checked against Quartus the same way
(`bench/quartus-validate.sh`, `bench/io-fuzz.py`), 2026-10-26: examples/de2-io
and 12 random projects (`io-fuzz.py` seeds 1-12: 1,377 pins on all four
sides, 777 I/O registers, 445 non-default LVTTL currents, 204 LVCMOS and 70
2.5-V pins, 280 pull-ups, 233 bus holds, all three unused-pin modes), none
of which any rule was fit on, assemble to the same RBF as Quartus: 13 of
13. The DE2 examples that existed before (blinky-verilog, bidir, ram,
ram_widths, ram-leds, de2-jtag-counter, picorv32) build to the same RBFs
as before. The I/O registers' timing (`chipsmith-route`'s `timing.rs`) is
quartus_sta's on de2-io, where the critical path now ends at GPIO_0[4]'s
OE register: 16.29 ns of slack estimated, 15.67 ns in Quartus.

M4K RAM blocks (4,608 bits each, 105 on the EP2C35) come from chipsmith's
synthesis (`crates/chipsmith-cyclone2/src/synth/m4k.rs`, yosys'
`memory_libmap` as the reference for what a block takes;
`crates/chipsmith-cyclone2/src/m4k.rs` is the block's configuration as one
type the synthesis, packer, bitstream and Quartus export share). Every
memory with synchronous reads that a block can hold goes into blocks, as
Quartus does, unless its declaration says `ramstyle`/`ram_style` "logic":

- single port x1 to x18 (a read of the word being written returns the new
  data), simple dual port x1 to x36 (old data), true dual port up to x18
  (each port sees its own write, the other's old data; written as Quartus'
  template, `q <= we ? d : mem[a]`), ROMs, initial contents;
- mixed widths: a port reading or writing several words at once (Verilog's
  `{mem[{a, 2'd1}], mem[{a, 2'd0}]}`), any pair of widths;
- byte enables (`mem[a][15:8] <= d` under its own condition), packed eight
  bits per nine columns as Quartus packs them;
- clock enables: a read enable the block's clock enable can carry (the
  write under it too, or the read on clock 1 of the same clock); one it
  can't (a write outside it) holds the read data in LE registers;
- a second register on the read data, with an asynchronous clear and an
  enable of its own: the block's output register (clock 0 or 1, clear 0
  or 1);
- two clocks: written on one, read on another, as Quartus builds it (a
  true dual port reading on port A, writing on port B; Quartus 13.0 has no
  simple dual port on two clocks, so up to x18 a block, wider ones split);
- memories deeper than a block (blocks in rows, the write enable decoded
  and the read data multiplexed by the registered top address bits) and
  wider (blocks side by side), and shallower: their top address bit is
  repeated on the block's unused address pins, so every word has an
  address of its own;
- a synchronous reset on the read data (LE logic after the block);
- both ports writing one word on one edge (a true dual port's write
  collision): the block writes neither reliably, so the netlist has one
  port write each bit, the data yosys gives priority (as `memory_map`
  has it). Where the losing port's word fits in the winner's, the loser's
  write enable is masked; where the winner's fits in the loser's wider
  word, the winner's is masked and the loser writes the winner's data
  there. A port that reads its own write's new data then reads something
  else from the block on that edge, so LE registers put its write data
  back on those bits. A collision neither way covers (byte enables on the
  port that would be masked by one signal) leaves the memory in logic.
  Where one port's write enable already requires the two addresses to
  differ (`web & (ab != aa)`, as Quartus' template has it), there are no
  collisions and no such logic: Quartus would find it dead and sweep it.

Not taken: asynchronous reads (logic), a read register that powers up to
anything but 0, a write enable tied high (an unconnected write enable reads
as no write), Quartus' packed single port x32/x36 (two x18 blocks
instead), a same-port read of the old data in a true dual port, more
than two ports per block (no duplication of a memory per read port).
The packer moves a `CY2_M4K` onto the block's physical pins (some address
bits enter on data pins), the placer swaps blocks between M4K sites, and
the router reaches their inputs through the block's LAB lines, the clocks
through row clocks, clock enables and clears through the block's control
inputs, and the read data from the M4K buffers. A data input written as a
constant gets an LE driving it (the block doesn't read an unconnected one
as 0); a yosys-flow `CY2_M4K` doesn't say which columns are memory bits
(the flow pads with 0), so there it is the columns whose read data is
used.

- `examples/ram`: one memory of each shape (7 blocks), written from a
  counter and the switches, read data folded onto the LEDs.
- `examples/ram_widths`: the widths `ram` doesn't use (10 blocks).
- `examples/ram-leds`: a hardware demo. A 256 x 18 ROM steps a bouncing
  light and a filling bar across LEDR about 12 times a second. A 512 x 9
  RAM, initialised to a Gray code, is read at the same step onto LEDG.
  KEY[0] writes SW[8:0] at the address shown, KEY[1] holds the step so the
  value can be seen coming back. Runs on the DE2, from JTAG and from the
  EPCS.

- `examples/m4k-bytes`, `m4k-enables`, `m4k-outreg`, `m4k-tdp`,
  `m4k-mixed`, `m4k-dualclock`, `m4k-deep`, `m4k-shallow`: one mode each
  (the file heads say which memory is which block shape), written from a
  counter and the switches, read data folded onto the LEDs. Not run on the
  board yet.
- `examples/m4k-selftest`: a hardware test read back over JTAG like
  de2-jtag-counter. Eight memories (simple and true dual port, four blocks
  deep, byte enables, mixed widths, an output register, a ROM under a read
  enable, a shallow one, 11 blocks) are written with a pattern of the pass
  number and read back, over and over; USER1 holds `"M4K!"`, the passes and
  the reads that differed, so `de2_user1` prints them as its count (passes
  in the top 16 bits, errors in the bottom 16: 0 on a good board);
  LEDR[3:1] show the pass count's top bits, LEDR[0] lights on an error.
  Run on the DE2 2026-10-28 (chipsmith bfdfb93, 358 LEs, 11 M4Ks, loaded
  over JTAG): `"M4K!"`, the passes rising about 6,100 a second, 0 errors
  over tens of thousands of passes (`de2_user1` can't see bit 0 of the
  low word, so it reads 0 or 1; a real mismatch recurs every pass), and
  LEDR[3:1] counting with LEDR[0] off. Since then a ninth memory, a true
  dual port whose ports write one word on every write edge (the
  collision above), checks each port reads back its own write and the
  word holds port B's. Run with it on the DE2 2026-10-29 (chipsmith
  816ddd5, 12 M4Ks, loaded over JTAG): `"M4K!"`, the passes rising,
  0 errors.

How the M4K mapping is checked (2026-10-28, after merging `open` 5b22b89;
`crates/chipsmith-cyclone2/tests/synth.rs`, times on luna with two checks
at a time):

- **Proven, one block per configuration** (`m4k_shape_*`,
  `tests/support/m4k_shapes.rs`): a memory alone in a design, its ports on
  the design's, for each class: single port x1, x2, x18, simple dual port
  x4, x9, x36, ROM x36, ROM under a read enable, byte enables x16 and x32,
  mixed widths (write x32 read x8, write x8 read x32), true dual port x9
  and x16/x4 (collisions masked in the RTL), the same two with both ports
  writing one word on one edge, two write ports beside one read (the
  idiom `if (a) m[x] <= d1; if (b) m[y] <= d2; q <= a ? d1 : m[x];`), two
  clocks, a clock enable over the whole port, a read enable
  on its own clock, one held in LEs, an output register with a clear and
  an enable, a synchronous reset, a shallow memory. yosys proves the
  netlist equivalent to the RTL with the memory as words on both sides: the
  RTL's through `memory_map`, the block through a model of it
  (`tests/support/mod.rs`, `model_blocks`: contents as registers named after
  the memory words they hold, the block's read and write behaviour, its
  INIT checked against the memory's contents). 3.5 to 7 minutes each
  (ROMs and the shallow memory 8 to 10 s), the true dual ports and
  collisions 20 to 40 minutes; all 24 pass.
- **Proven around the blocks, every design** (`check_staged`): the RTL
  against synthesis' last stage before the M4K mapping, memories compared
  at their ports (read registers outside on both sides, `memory_nordff`),
  and the stage right after the mapping against the final netlist, the
  blocks compared at their pins and parameters. What this leaves out is
  the mapping itself: the blocks and the soft logic between them (row
  decoding, read multiplexers, held read data), which the per-configuration
  proofs cover for one block and co-simulation for several.
- **Co-simulated** (`support::cosim`, iverilog): the RTL against the final
  netlist with the blocks as the same model, 10,000 cycles of random inputs
  (a fixed seed per design, printed on a difference), every output bit the
  RTL defines compared every cycle. Run on the eight examples, the 16
  random designs as built for the DE2, the same 16 with their memory's
  ports on the design's (every address reached every cycle), and the
  examples' large memories alone (2048 x 9 over four blocks, 8192 x 2 over
  four, 256 x 40 over three, the three mixed-width ones). It catches a
  wrong mapping: two address pins of a block swapped, or the write enables
  of two block rows swapped, fail within 200 cycles
  (`m4k_cosim_catches_wrong_blocks`). The three collision shapes are
  co-simulated too (`m4k_cosim_collisions`); before the collision fix,
  one port's read of its own write failed on them. The "ports direct"
  runs and the large memories are separate syntheses of separate designs:
  they check the same mapping code, not the netlist an example ships.
- **What the checks can't see.** yosys' induction has no base case here:
  it proves the step from any state alike on both sides, so power-up
  values (the blocks' 0 output registers, INIT against `initial`) are
  checked only by co-simulation from cycle 0 and by Quartus building the
  same bits. It also steps every register on one edge, so a second
  clock (a write on one clock, a read on another) is co-simulated, not
  proven; and write priority between two ports is proven only in the
  three collision shapes, elsewhere co-simulated.
- **Quartus byte-identical**: `cram quartus-export` of chipsmith's placement
  and routing, assembled by Quartus 13.0sp1, compared with chipsmith's RBF
  (cyclone2-re `tools/validate.sh`).

The earlier check proved whole designs with every memory as words; it took
15 to 70 minutes a design, didn't finish on m4k-deep in 22 hours, and one
run grew to 7 GB.

Results:

All 88 tests of the cyclone2 synthesis suite passed on one binary, 85
minutes under a 20 GB memory cap, two at a time. The collision fix then
changed the true dual ports only; re-run on its final binary: the five
true dual port and collision shape proofs, the collision co-simulation,
m4k-tdp, m4k-random-1 and -5 (both now writing colliding words) with
their direct co-simulations, and the yosys-flow cells. Every other DE2
design built to the same RBF before and after it; m4k-tdp, m4k-random-1,
-5 and m4k-selftest changed and are Quartus byte-identical. Per design, below: the
check around the blocks (time of the proof), the co-simulation (output
bits compared, time with the build), and for the random designs the
co-simulation with the memory's ports on the design's. Registers and
memories without an initial value start at 0 in the RTL's simulation, as
the device powers them up (branch `qor2`; the co-simulation columns and
proof times are from its rerun). Before, they started x, and a memory read
before it was written left the outputs x and uncompared: `ram` compared
108 bits, m4k-mixed 60, m4k-random-4 1,080, m4k-dualclock 1,512, m4k-bytes
2,136, and femtorv32 and picorv32 none. Now every design below compares
every output bit nearly every cycle. serv, m4k-selftest and every design
below are Quartus byte-identical.

| design | memories | M4Ks | Quartus | proven around the blocks | co-simulated | co-simulated, ports direct |
|---|---|---|---|---|---|---|
| m4k-bytes | byte enables: 256 x 16 (2), 128 x 32 (4) | 2 | byte-identical | 1356 $equiv, 7s | 270,000 bits, 28s |  |
| m4k-deep | 2048 x 9 over 4 blocks, 8192 x 2 ROM over 4, 256 x 40 over 3 | 11 | byte-identical | 1804 $equiv, 65s | 270,000 bits, 57s |  |
| m4k-dualclock | written on CLOCK_50, read on CLOCK_27 (x9; x36 as two x18) | 3 | byte-identical | 1104 $equiv, 15s | 270,000 bits, 38s |  |
| m4k-enables | clock enable, read enables on ENA0/ENA1, one held in LEs | 4 | byte-identical | 990 $equiv, 10s | 270,000 bits, 15s |  |
| m4k-mixed | write x32 read x8, write x8 read x32, write x2 read x1 | 3 | byte-identical | 1080 $equiv, 8s | 270,000 bits, 16s |  |
| m4k-outreg | output registers: plain, cleared, own enable; a synchronous reset | 4 | byte-identical | 1102 $equiv, 6s | 270,000 bits, 26s |  |
| m4k-shallow | 64 x 8, 32 x 36, 16 x 4 ROM | 3 | byte-identical | 912 $equiv, 7s | 270,000 bits, 28s |  |
| m4k-tdp | true dual port x9/x9 and x4/x18 | 2 | byte-identical | 994 $equiv, 10s | 270,000 bits, 20s |  |
| m4k-random-1 | TrueDual 600 x 5, colliding writes | 2 | byte-identical | 720 $equiv, 5s | 260,580 bits, 11s | 79,635 bits |
| m4k-random-2 | SimpleDual 16 x 24, byte enables, two clocks | 2 | byte-identical | 957 $equiv, 7s | 270,000 bits, 25s | 240,000 bits |
| m4k-random-3 | SimpleDual 16 x 5, write x1 read x4 words | 1 | byte-identical | 497 $equiv, 5s | 270,000 bits, 18s | 200,000 bits |
| m4k-random-4 | SimpleDual 1024 x 32, byte enables, output register (plain) | 8 | byte-identical | 1915 $equiv, 10s | 270,000 bits, 47s | 320,000 bits |
| m4k-random-5 | TrueDual 2048 x 2, colliding writes | 1 | byte-identical | 564 $equiv, 9s | 270,000 bits, 16s | 40,000 bits |
| m4k-random-6 | Rom 1024 x 32, contents, output register (cleared) | 8 | byte-identical | 1072 $equiv, 13s | 270,000 bits, 61s | 320,000 bits |
| m4k-random-7 | SimpleDual 1024 x 36, read enable, write under it, sync reset | 8 | byte-identical | 1481 $equiv, 17s | 270,000 bits, 48s | 360,000 bits |
| m4k-random-8 | SingleNew 1024 x 36, contents | 8 | byte-identical | 1344 $equiv, 12s | 270,000 bits, 63s | 360,000 bits |
| m4k-random-9 | SingleNew 64 x 32 | 1 | byte-identical | 603 $equiv, 7s | 270,000 bits, 20s | 320,000 bits |
| m4k-random-10 | SimpleDual 512 x 4, write x2 read x1 words, output register (cleared) | 1 | byte-identical | 534 $equiv, 6s | 270,000 bits, 13s | 40,000 bits |
| m4k-random-11 | SimpleDual 4096 x 9, contents | 8 | byte-identical | 1238 $equiv, 34s | 270,000 bits, 74s | 90,000 bits |
| m4k-random-12 | SingleNew 600 x 36, contents | 5 | byte-identical | 1051 $equiv, 16s | 203,067 bits, 82s | 208,728 bits |
| m4k-random-13 | SingleNew 4096 x 8 | 8 | byte-identical | 1117 $equiv, 7s | 270,000 bits, 37s | 80,000 bits |
| m4k-random-14 | SingleNew 16 x 4 | 1 | byte-identical | 349 $equiv, 5s | 270,000 bits, 11s | 40,000 bits |
| m4k-random-15 | SingleOld 2048 x 18, read enable, write under it, output register (plain) | 8 | byte-identical | 1317 $equiv, 13s | 270,000 bits, 45s | 180,000 bits |
| m4k-random-16 | SimpleDual 512 x 5, write x2 read x1 words, output register (plain) | 1 | byte-identical | 548 $equiv, 6s | 270,000 bits, 11s | 50,000 bits |

A memory bit written as a constant gets an LE driving the block's data
input: the block doesn't read an unconnected data input as 0, and Quartus
routes its GND there (found on m4k-selftest, which writes a 32-bit pattern
whose top bit was always 0). Quartus replaces that LE by its own GND
elsewhere, so a design that writes a constant bit builds correctly but not
byte-identical; the examples don't. An inverter alone in front of a write
enable is the other such case (below).

The random designs are `crates/chipsmith-cyclone2/tests/support/m4k_random.rs`,
seeds 1-16 (`cargo run --release -p chipsmith-cyclone2 --example
m4k_random -- <dir> 1 16` writes them as projects): one memory each, its
kind, widths, depth, byte enables, read enable, output register, reset,
clocks and contents drawn from the seed. Every existing DE2 example builds
to the same RBF as on `open` but serv, whose register file is now an M4K
(Quartus byte-identical; it was LE registers). de2-io and de2-pll-counter
didn't build here or on `open` then; they do since the multipliers branch
(2026-10-29, both Quartus byte-identical).

Embedded multipliers (35 on the EP2C35, each one 18 x 18 or two 9 x 9)
come from chipsmith's synthesis (`crates/chipsmith-cyclone2/src/synth/mult.rs`,
ADR-0010; `crates/chipsmith-cyclone2/src/mult.rs` is the block's
configuration the synthesis, packer, bitstream and Quartus export share):

- every `$mul` of two non-constant operands at least two bits wide goes
  to blocks while blocks are left; constant and one-bit factors stay logic
  (shifts and AND gates);
- nine-bit mode when both operands fit nine bits, else 18-bit; a wider
  operand is cut into 18-bit limbs (the top limb signed when the operand
  is), the partial products summed on carry chains; product bits past the
  result are never built;
- signed, unsigned and mixed operands: an unsigned operand's sign input
  reads one shared constant-0 LE, Quartus' `~GND`;
- a register read only by the product moves into the block as its
  operand register, a register taking a one-block product as its product
  register, when it has the block's kind of controls (rising edge,
  active-high enable, asynchronous clear to 0, powering up 0) and the
  block's other registers' clock, enable and clear; an inverter in front
  of a data input folds into the block's data inversion.

Not done: two products in one block (the second 9 x 9 row), the sign
registers, a register shared with other logic duplicated into the block.

How it is checked (`crates/chipsmith-cyclone2/tests/synth.rs`, `mult_*`):

- **Per shape, proven**: one product alone in a design, the block
  modelled by a `$mul` of its operands' widths and signs, which yosys
  matches structurally. 16 mapping shapes (nine-bit and 18-bit; signed,
  unsigned and mixed; a product cut short; two products; five limb
  splits, on sparse operands so SAT finishes) in 1-58 s each, with the
  operand registers kept out of the block: through a block's own register
  a proof is SAT on the whole product. Moving registers in is proven on
  9 x 9 shapes: the operands, the product, both, an operand register
  also read elsewhere (stays in LEs), a product register with another
  enable (stays): 1-312 s each; the asynchronous clear with an enable is
  co-simulated only.
- **Co-simulated** (iverilog, random operands, the extremes first): every
  shape, and dense products over several blocks (20 x 20, signed 24 x 24,
  32 x 32 to 32 bits, signed 32 x 32, 36 x 18) and one 18 x 18 block with
  all its registers in, 5,000 cycles each (160,000-320,000 output bits,
  5-27 s). A swapped operand bit is caught at cycle 17, a wrong sign at
  cycle 3.
- **The examples**: `examples/fir` (an 8-tap filter, eight signed
  16 x 16 products) proven around the blocks, the RTL against the stage
  before the mapping and the stage after it against the netlist with the
  blocks cut at their pins (2,075 `$equiv`, 47 s; 2,148 in 75 s on a
  loaded machine since the products are narrowed, below), and co-simulated
  (180,000 output bits). `examples/mult-selftest` proven around its 15
  blocks the same way (3,460 `$equiv`, 289 s; since 2026-11-01, below)
  and co-simulated for 120,000 cycles (480,000 bits); with one block's
  operand made unsigned the co-simulation catches it at cycle 3,654.
  Its proof hadn't finished before: chipsmith's front end widens a
  product's operands to the result (Verilog's context-determined width),
  yosys' front end doesn't, so the two `$mul`s of each product differed
  in their parameters, and yosys' `equiv_struct` skips `$mul` cells
  anyway: induction SAT-solved each multiplier against its twin, and the
  RTL against its own refined stage ran 30 minutes without finishing.
  The check now narrows both sides' products (`wreduce`), and where the
  RTL has a product of two signals wider than 18 x 18 it lets
  `equiv_struct` merge internal cells (`-icells`) between the two front
  ends, so two products of proven-equal operands are one; where that
  pairs a name-matched wire that legitimately differs, it proves again
  without `-icells`. Narrower products SAT proves directly (`-icells`
  cost fir's RTL step 299 s).
- **Quartus byte-identical**: fir and mult-selftest.

| design | products | blocks | LEs | critical path, estimate / Quartus |
|---|---|---|---|---|
| fir | 8 signed 16 x 16 | 8 | 534 | 13.69 / 13.79 ns |
| mult-selftest | 9, one per mode below | 15 | 1,008 | 14.76 / 15.61 ns |

`examples/mult-selftest` answers over JTAG like m4k-selftest: nine
products of LFSR operands (9 x 9 unsigned and signed, 18 x 18 unsigned and
signed, 12 signed x 7 unsigned, 9 x 9 with its operands registered in the
block, 18 x 18 with its product registered, 32 x 32 to 64 bits and
signed 24 x 24 to 48 bits over four blocks each), each against a bit-serial reference in LEs,
in turn, 128 cycles each. USER1 holds `"MUL!"`, a 16-bit pass count and
the errors (bit 0 of the low word can't be read). On the DE2
(2026-10-29, final build, loaded over JTAG): passes rising at about
43,390 a second (50 MHz / 1,152 = 43,403), 0 errors after about
9 minutes (about 23 million passes):

```
USER1 4d554c2151ae0000: tag "MUL!", counts 1630797824
USER1 4d554c21fbab0000: tag "MUL!", counts 2905735168
```

(`de2_user1` reads the words as Gray-coded counters; here the raw word is
the one to read: `51ae` to `fbab` is 43,517 passes in 1.003 s, low word 0.)

Quartus assembles chipsmith's placement and routing into the same RBF bit
for bit (`bench/quartus-timing.sh`): checked 2026-10-24 on blinky-verilog
and blinky-vhdl (16.65 ns of slack at 50 MHz), on picorv32 (10.60 ns
since cyclone2-re 9cfe9be, see below; 11.08 ns before) and on bidir
(16.64 ns), all built through chipsmith's LUT mapper, and on ram
(16.21 ns), ram_widths (14.80 ns) and ram-leds (14.80 ns). Those were
yosys-synthesized. With chipsmith's own synthesis, `bench/de2-timing.sh`
builds every DE2 example with a clock and has Quartus time the same
placement and routing (re-run 2026-11-01, branch de2-polish: gpu-pnr's
placer, the active-low clear folding and cyclone2-re's fixed export;
50 MHz constraint; error is the estimate's critical path against
Quartus'):

| design | estimate | Quartus | error | RBF byte-identical |
|---|---|---|---|---|
| blinky-verilog, blinky-vhdl | 3.37 ns | 3.35 ns | +0.5% | yes |
| bidir | 3.37 ns | 3.36 ns | +0.3% | yes |
| ram | 3.80 ns | 3.78 ns | +0.5% | yes |
| ram_widths | 3.80 ns | 3.78 ns | +0.6% | yes |
| ram-leds | 3.54 ns | 3.82 ns | -7.2% | yes |
| de2-jtag-counter | 3.80 ns | 3.78 ns | +0.5% | yes |
| uart | 5.53 ns | 6.09 ns | -9.1% | yes |
| spi-master | 3.41 ns | 3.72 ns | -8.3% | yes |
| i2c-master | 6.92 ns | 7.59 ns | -8.8% | yes |
| femtorv32 | 13.92 ns | 14.58 ns | -4.5% | yes |
| serv | 10.38 ns | 10.92 ns | -5.0% | yes |
| sha256 | 20.33 ns | 21.28 ns | -4.5% | yes |
| fir | 13.69 ns | 13.79 ns | -0.8% | yes |
| mult-selftest | 14.76 ns | 15.61 ns | -5.5% | yes |
| crc32 | 3.35 ns | 3.32 ns | +0.9% | yes |
| negedge-selftest | 6.07 ns, half a period | 6.83 ns, half a period | -11.1% | yes |

Eight designs are within 1% (blinky-verilog and -vhdl, bidir, ram,
ram_widths, de2-jtag-counter, crc32, fir); the others run 4.5 to 9.1%
slower in Quartus than estimated, negedge-selftest's falling-to-rising
path 11.1%.
(de2-timing.sh's own row for negedge-selftest compares the full-period
path; the half-period figures are the build's report and quartus_sta's
slack against 10 ns.) Over the 355 connections of the 300 worst
Quartus paths of picorv32, femtorv32, serv and ram-leds that route the
same in both (`bench/timing-calib.py`), Quartus' routing delay is 1.08
times the model's (C4 the most: 312 ps median against the model's 285).
Scaling every routing delay by that brought the four larger designs'
mean error from 6.6% to 5.3%, but Quartus timed three of them slower on
the placements it led to (picorv32 8.57 to 9.07 ns, i2c-master 8.83 to
9.89 ns), so the model stays as it is; the gap is where per-wire delays
(fanout, wire length) would have to come in. Since cyclone2-re's
export names an inverting feeder from a register or a pin Quartus' way
(`<src>~_wirecell`, placed in the feeder's LE; cyclone2-re f0406b4,
2026-10-29), spi-master, sha256, de2-jtag-counter and de2-io are
byte-identical too; uart and i2c-master were down to 2 and 6 bits in the
I/O rows, and all 43 DE2 designs (the examples, the 16 random M4K
designs) re-validated then: 41 byte-identical. Those bits were the input
delay chains of pins read only through a `~_wirecell`: the export pinned
each pad-to-core delay with an assignment to the feeder LUT, a name
Quartus' netlist doesn't have, so the fitter ignored it and left the
chain off. With the assignment on the wirecell (cyclone2-re 3cd8278,
2026-11-01), 33 of the 34 DE2 examples re-validated byte-identical, uart
and i2c-master included; picorv32x24 (28,449 LEs) isn't validated:
Quartus' fit of it didn't finish.
picorv32 has no M4K; its slack moved because cyclone2-re's synthesis
script gained `memory_libmap`, which renames yosys' cells and nets. Same
logic, same 1,175 LEs, but other net ids and so another placement. The
same chipsmith with the previous script places it at 11.08 ns again.
An inverter from a pin straight into an M4K's write enable doesn't
validate: quartus_map removes the one-input LUT and folds it into the
block (cram doesn't model that bit yet). The bitstream still works; `ram-leds` registers its key. Since chipsmith places
designs itself, the RBFs no longer match the
[cyclone2-re](https://github.com/cramt/cyclone2-re) examples built with
nextpnr's placement.

The Cyclone II limits come from cyclone2-re's open flow: M4K RAM as
above, multipliers as above, PLLs without clock
switchover or reconfiguration, I/O registers without a synchronous clear
of their own (an output or OE register's synchronous reset is logic on
its D; an input register with one can't go into an I/O cell, nor can one
with an asynchronous load or both an asynchronous set and clear), 3.3-V
LVCMOS at its default current only.
The timing model's cell delays are chipsmith's own (docs/timing-silicon.md):
LUT, carry-chain and multiplier delays measured on the DE2 with ring
oscillators, scaled to the slow corner; register, M4K and I/O-cell
constants fitted to quartus_sta's path totals on chipsmith's placements.
Routing is per-element-type averages from quartus_sta; the M4K buffer's
routing delay is from one quartus_sta path.

The Zybo examples:

- `examples/zybo-blinky`: a counter on LD0-LD3. Builds in about 20 s.
- `examples/zybo-jtag-counter`: the Zybo's USB-UART is wired to the
  Zynq's processor (PS MIO 48/49, Digilent's Zybo reference manual,
  section 6), not to the FPGA fabric. So without software on the ARM
  cores, no PL design can print to it. This design answers over the JTAG
  cable instead. A BSCANE2 makes it a 64-bit USER1 register holding
  `"ZYBO"` and a Gray-coded 125 MHz counter.
  `cargo run --release --example xc7_user1` reads it twice, a second apart,
  with chipsmith's own cable code. Builds in about 20 s. Read back
  2026-10-24 with the earlier flow (yosys and nextpnr around chipsmith's
  placement and routing), loaded with `flash --ssh`:

  ```
  USER1 0x5a59424fd795d8f0: tag "ZYBO", count 2598801568
  USER1 0x5a59424ff37745fb: tag "ZYBO", count 2723838290
  125036722 counts in 1.000 s: 125.00 MHz
  ```

  With zybo-blinky loaded instead, the same read returns all zeros.
- `examples/zybo-pll`: a PLL's 125 MHz and 62.5 MHz, the second 90
  degrees later, each with a counter on two of the LEDs (`XC7_PLL`, "The
  Zybo flow", below). Builds in about 20 s.
- `examples/zybo-picorv32`: `examples/picorv32`'s sources with its ports
  on arbitrary Zybo balls, a placement and routing bench, not for
  hardware. Builds in about 2 minutes.
- `examples/zybo-sha256`: `examples/sha256`'s sources the same way, a
  synthesis, placement and routing bench. Builds in about 18 minutes on a
  machine at a load of 30, 14 of them placing.
- `examples/zybo-bram-selftest`: `examples/m4k-selftest` in block RAM
  (nine RAMB18E1s): memories in most block modes written and read back
  over and over, `"BRAM"`, the passes and the errors in USER1, read as
  for zybo-jtag-counter. 153.7 MHz estimated at 125 MHz. On the board
  (2026-11-06) its errors first saturated: two block RAM bugs the
  bitstream checks couldn't see (7-series block RAM, below); fixed, 0
  errors and the passes rising.
- `examples/zybo-mult-selftest`: `examples/mult-selftest` on DSP48E1s
  (fourteen): nine product shapes against a shift-and-add reference,
  `"MUL!"`, the passes and the errors in USER1. Its test logic runs at
  75 MHz from a PLL (3/5 of the 125 MHz clock, since branch xc7-mmcm; a
  register toggling made 62.5 MHz before): the 32 x 32 product and its
  sum take about 13 ns, and chipsmith has no multicycle constraints.
  Vivado on chipsmith's routing: 75.6 to 78.8 MHz over five seeds.
- `examples/zybo-riscv-bram`: PicoRV32 running a self-checking program
  (`prog/selftest.S`, `prog/build.sh` assembles it) out of one 1,024 x 32
  block RAM that also holds its data: each pass writes 64 words and some
  of their bytes and reads them back. `"RV32"`, the passes and the errors
  in USER1. The CPU runs at 112.5 MHz from a PLL (9/10 of the 125 MHz
  clock, since branch xc7-mmcm; 62.5 MHz from a toggling register
  before), its reset held until the PLL locks: Vivado on chipsmith's
  routing makes 114.2 to 128.1 MHz over five seeds, so 125 MHz would fail
  on four.

#### 7-series synthesis

Since 2026-11-01 the Zybo build runs chipsmith's own synthesis
(`crates/chipsmith-xc7/src/synth`), not yosys' `synth_xilinx`: the
device-independent passes are the Cyclone II's (chipsmith-synth), LUT
mapping is the same priority-cut mapper with K = 6
(`chipsmith_synth::lutmap::LutSize`), and what is the 7-series' own is
small: `$alu` onto CARRY4 chains (`arith_map.v`'s mapping: propagate
`A ^ B` in the LUT in front, generate `A` on DI), registers onto
FDRE/FDSE/FDCE/FDPE (CE, synchronous or asynchronous reset or set, either
clock edge, INIT; `$sdffce`'s reset gated with the enable), memories of up
to 32 words with one write port and asynchronous reads onto RAM32Ms,
memories with synchronous reads onto block RAM (below), products onto
DSP48E1s (below), every port bit through IBUF/OBUF/OBUFT/IOBUF and every
clock through a BUFG. Latches and asynchronous loads aren't mapped yet.
Since branch xc7-qor sums of three or more terms are built by arrival,
as the Cyclone II's (`SumOrder::Arrival`), and a register read by more
than 32 cell inputs is copied, each copy taking a run of the readers in
netlist order (`synth/replicate.rs`), as Vivado's synthesis copies
sha256's round counter; a register that clocks others stays one.

The packer puts two LUTs in one LUT6 (LUT6_2, one out of O6 and one out of
O5) where their inputs together are five nets or fewer and they share at
least two (`crates/chipsmith-xc7/src/pack.rs`, `pair_luts`). Measured on
2026-11-02 (LUT6 sites, slices, estimated MHz), unpaired against two
shared inputs: zybo-jtag-counter 97/57 against 96/61, zybo-picorv32
700/242/204.1 against 664/233/192.1, zybo-sha256 2,572/1,076/61.1 against
2,289/993/59.8. One shared input saves a few more sites (picorv32 651,
sha256 2,285) and costs picorv32 a sixth of its Fmax (169.2 MHz); three
save less.

There are no MUXF7/F8. Measured on chipsmith's mapped netlists of
zybo-picorv32 and zybo-sha256: no LUT is a multiplexer of other LUTs'
outputs (LUT3 2:1, LUT6 4:1), and no cone of three or more single-fanout
LUTs has eight inputs or fewer, so a MUXF7 or MUXF8 would save no LUT;
seven two-LUT cones of seven inputs on picorv32 (none on sha256) could
trade a LUT level for an F7. Vivado's MUXF7s (28 on picorv32) come from
its own synthesis, which keeps wide multiplexers as such; chipsmith's
mapper has merged them into its LUTs by then. Vivado's lead in LUTs is
elsewhere.

Cell counts against `synth_xilinx -flatten -abc9 -arch xc7` on the same
Verilog (`crates/chipsmith-xc7/tests/synth.rs`, which prints both):

| design | LUTs chipsmith / synth_xilinx | registers | CARRY4 | RAM32M |
|---|---|---|---|---|
| zybo-blinky | 1 / 1 | 27 / 27 | 7 / 7 | |
| zybo-jtag-counter | 66 / 65 | 128 / 128 | 8 / 8 | |
| zybo-picorv32 | 611 / 694 | 335 / 375 | 58 / 57 | 11 / 12 |
| zybo-sha256 | 2,539 / 2,516 | 1,907 / 1,910 | 176 / 110 | |

(Measured on `open` df16e49. Since branch xc7-qor register copies add
27 to picorv32's registers and 128 to sha256's.)

synth_xilinx's LUT counts include its INV cells. picorv32's 40 fewer
registers are ones chipsmith's sequential optimisation finds constant or
equal (proven, below). picorv32's extra CARRY4 is a chain lengthened by a bit whose sum and
carry out are both read (one slice pin carries either). sha256's extra
CARRY4s are its multi-operand sums:
chipsmith's `alumacc` chains them adder by adder where yosys' makes
some of them LUT compressors.

Every design is proven equivalent to its RTL by yosys
(`support::check`, the Cyclone II synthesis tests' check, the netlist's
primitives read through yosys' own `xilinx/cells_sim.v`; since branch
xc7-qor without register copies, `Options::register_copies`, which
induction can't tie to their originals: the copies are co-simulated
through the bitstream): blinky,
jtag-counter (around its BSCANE2), a test of every register shape and
the carry arithmetic, tri-state and bidirectional pins, and sha256 (whole,
15 minutes). Designs with RAM32Ms are co-simulated against the RTL
(`support::cosim`, iverilog): RAM32Ms of one to three read ports, odd
widths, initial contents and a falling-edge write, three seeds of 3,000
cycles; picorv32 running its loop for 20,000 cycles; and proven with
distributed RAM off.

#### 7-series block RAM

Since 2026-11-02 every memory with synchronous reads that a block can take
goes into RAMB18E1s (`crates/chipsmith-xc7/src/synth/bram.rs`, ADR-0014),
to the DE2's M4K standard: the M4K mapper's analysis of the ports
(mixed widths, single, simple and true dual port, two writes on one
word resolved in logic, memories deeper and wider than a block), with
what the 7-series block does on top:

- each port's write mode follows from the RTL: READ_FIRST where the
  other port reads the word's old data on the same edge (UG473: the other
  port's read is defined only then), WRITE_FIRST where its own read sees
  its new data; where both hold, READ_FIRST and the port's own read of
  its write a register of the written data in logic. A read held while
  its port writes (`if (we) m[a] <= d; else q <= m[a];`) is NO_CHANGE;
- byte enables, two clocks (either edge, as the block's clock inversion),
  the read data's power-up value (INIT) and a synchronous reset of it
  gated by the read enable (RSTRAM, to SRVAL), a register after the
  read data with its own enable and synchronous reset (the output
  register, REGCE and RSTREG);
- SDP mode where a side is x36 (read on port A, written on port B), and a
  single port wider than x18 split over both ports at adjacent words
  (512 x 36 in one RAMB18E1, both ports x18 WRITE_FIRST, as Vivado
  builds it).

Not mapped: an asynchronous reset of the read data (logic), memories of 64
words or fewer (registers; Vivado puts them in LUT RAM), RAMB36E1s
(two RAMB18E1s take what one would, in the same BRAM tile; Vivado reports
a RAMB36E1 there), a READ_FIRST write of some of its bytes read on the same
edge by a narrower port (logic: UG473 says old data, Vivado's own model
new). A RAMB36E1 instantiated in a design is built.

Checked (ADR-0014):

- **chipsmith's block model** (`crates/chipsmith-xc7/src/sim.v`, yosys'
  `cells_sim.v` having RAMB18E1 only as a timing blackbox) against
  Vivado's unisim models (`crates/chipsmith-xc7/tests/models.rs`, run
  with `CHIPSMITH_UNISIMS` pointing at Vivado's `data/verilog/src`): 40
  runs of six random RAMB18E1/RAMB36E1 configurations, 600 cycles each,
  2,843,576 output bits compared, none different; 2,277 x in chipsmith's
  model where Vivado's has a value (collisions it calls undefined);
- **every shape synthesized** (`crates/chipsmith-xc7/tests/designs/bram`,
  `block_ram_shapes` in `tests/synth.rs`), co-simulated against its RTL,
  three seeds of 3,000 cycles;
- **every shape through the bitstream** (`block_ram` in
  `crates/chipsmith/tests/xc7_bitstream.rs`): built, prjxray's
  `fasm2frames`/`xc7frames2bit` making the same `.bit` of the FASM,
  decoded (`chipsmith_xc7::decode`, a RAMB18E1 per half of a BRAM tile)
  and co-simulated;
- **on the board** (2026-11-06): zybo-bram-selftest, 0 errors. Its
  first run found two bugs that encoder and decoder shared, so the
  co-simulation passed: (1) a route through a BRAM tile's address cascade
  (`BRAM_CASCOUT_ADDR*`, into the next tile's `CASCIN{TOP,BOT}`) needs the
  driving tile's `CASCOUT_ARD_ACTIVE`/`CASCOUT_BWR_ACTIVE` bit, which
  chipsmith never wrote; (2) port B's write enables in true dual port
  went one per pin (nextpnr's `pack_bram`), where the pins are byte
  lanes: Vivado puts WE[i] on lanes 2i and 2i+1 (x18) or WE[0] on all
  four (x9 and narrower), so writes to port B's odd lanes were lost.
  Found by having Vivado write the same netlist on the same sites and
  diffing bit2fasm's reading of both. The decoder now drives a cascade
  only under its bit and refuses lanes that aren't one enable per lane
  pair (x18) or per port, so `zybo_bram_selftest` fails on either bug;
- **the decoder on Vivado's bitstreams** of the same RTL: ten of twelve
  co-simulate against the RTL. The other two: Vivado's true dual port,
  both ports WRITE_FIRST, reads x where the other port writes the word on
  the same edge (undefined in the block; the RTL reads the old data,
  which chipsmith's build gives), and Vivado's LUT RAM for a 64-word
  memory, which the decoder doesn't read. The block's INIT and SRVAL bits
  come bit-reversed in an x18 layout, found with one-hot INIT values in a
  Vivado bitstream.

| shape | memory | chipsmith | Vivado | co-simulated (synthesis) | through the bitstream | Vivado's bitstream decoded |
|---|---|---|---|---|---|---|
| sp_read_first | 1024 x 18, old data | 1 RAMB18 | 1 RAMB18 | 77,652 bits | 25,974 bits | 25,974 bits |
| sp_write_first | 512 x 36, new data, port enable | 1 RAMB18 (both ports x18), 1 LUT | 1 RAMB18 (both ports x18) | 237,780 | 79,416 | 79,416 |
| sp_no_change | 2048 x 9, held while writing | 1 RAMB18 | 1 RAMB18 | 24,129 | 7,965 | 7,965 |
| sdp | 512 x 32, simple dual port | 1 RAMB18 (SDP) | 1 RAMB18 (SDP) | 197,280 | 66,912 | 66,912 |
| sdp_x72 | 512 x 72 | 2 RAMB18 (SDP) | (too many pins for the board) | 441,072 | | |
| dualclock | 1024 x 16, two clocks, read enable | 1 RAMB18 | 1 RAMB18 | 71,888 | 22,560 | 22,560 |
| tdp | 1024 x 16, true dual port | 1 RAMB18, 34 FFs, 39 LUTs | 1 RAMB18 | 241,664 | 80,704 | x on same-edge cross reads |
| rom | 1024 x 16, contents, read enable | 1 RAMB18 | 1 RAMB18 | 144,000 | 48,000 | 48,000 |
| bytes | 1024 x 32, byte enables | 2 RAMB18 | 1 RAMB36 | 137,864 | 46,520 | (no bitstream) |
| outreg | 2048 x 8, output register (enable, reset) | 1 RAMB18 | 1 RAMB18 | 72,000 | 24,000 | 24,000 |
| mixed | written x32, read x8 | 1 RAMB18 (SDP) | 1 RAMB18 (SDP) | 59,704 | 19,736 | 19,736 |
| deep | 16384 x 4 | 4 RAMB18 | 2 RAMB36 | 1,720 | 600 | 600 |
| srst | 1024 x 18, reset and enable on the read, power-up value | 1 RAMB18 | 1 RAMB18 | 119,016 | 39,726 | 39,726 |
| tiny | 64 x 8 | 520 FFs, 232 LUTs | 12 LUTs as memory, 8 FFs | 68,696 | | (LUT RAM) |

The bit counts are output bits the RTL defines, compared every cycle;
`deep` reads mostly unwritten words. On the board: owed.

#### 7-series multipliers

Since 2026-11-02 every product of two signals at least two bits wide goes
into DSP48E1s while the device has them (80 on the xc7z010;
`crates/chipsmith-xc7/src/synth/dsp.rs`, after the Cyclone II's
multiplier mapping, ADR-0015): one DSP up to 25 x 18 signed or 24 x 17
unsigned, wider operands cut into limbs whose partial products are summed
with carry chains. The operands go whichever way round takes fewer DSPs.
Rising-edge registers on the operands, on the product and after it become
AREG/BREG, PREG and MREG, with their enables and synchronous resets. The
packer, placer (DSP tile sites, swapped by the anneal), site routing, FASM
writer, decoder and timing (DSP_L.sdf) know the block; one Config type
(`crates/chipsmith-xc7/src/dsp.rs`) carries its configuration.

Checked:

- **yosys' DSP48E1 model**, which the co-simulations use, against Vivado's
  unisim (`crates/chipsmith-xc7/tests/models.rs`, with
  `CHIPSMITH_UNISIMS`): random configurations of what synthesis and the
  decoder make (A and B of 0 to 2 stages, M, P and the control registers,
  either clock edge, P = A * B and Vivado's P = A * B + PCIN >>> 17),
  random data, enables and resets: 40 runs of six blocks, 600 cycles each,
  6.57 million output bits compared, none different;
- **every shape** co-simulated against its RTL after synthesis (three
  seeds of 3,000 cycles), through the bitstream (decoded, a DSP48E1 per
  site, and co-simulated), and Vivado's bitstream of the same RTL decoded
  and co-simulated the same way.

| shape | product | chipsmith | Vivado | co-simulated (synthesis) | through the bitstream | Vivado's bitstream decoded |
|---|---|---|---|---|---|---|
| mul_comb | 18 x 18 signed, registered | 1 DSP (PREG) | 1 DSP | 324,000 bits | 108,000 bits | 108,000 bits |
| mul_mixed | 12 signed x 9 unsigned, and 7 x 5 | 2 DSPs | 1 DSP, 31 LUTs, 12 FFs | 306,000 | 102,000 | 102,000 |
| mul_pipe | 25 x 18 signed, three stages, reset | 1 DSP (AREG/BREG, MREG, PREG) | 1 DSP | 386,871 | 128,957 | 128,957 |
| mul_regs | 24 x 17 unsigned, enables | 1 DSP (AREG/BREG, PREG) | 1 DSP | 368,754 | 122,918 | 122,918 |
| mul_split | 26 x 22 signed | 2 DSPs (cascaded, since branch xc7-qor; 31 LUTs and 12 CARRY4s before), 48 FFs | 2 DSPs (cascaded), 17 FFs | 431,856 | 143,952 | 143,952 |
| mul_wide | 32 x 32 unsigned | 4 DSPs (two cascaded pairs since branch xc7-qor; before, 134 LUTs, 48 CARRY4s), 64 FFs | (too many pins for the board) | 575,808 | | |

Since branch xc7-qor chipsmith sums a split product's partial products
in the DSPs where Vivado does: A's limb times two of B's limbs 17 bits
apart is a cascaded pair, the upper adding the lower's P shifted down 17
bits (PCIN, OPMODE 1010101), placed together in one DSP tile; what is
left sums in carry chains. A pair cascades where both DSPs have the same
operand registers and neither M nor P, and a cascade is two DSPs: Vivado
also takes mul_split's product register into the upper DSP's P, 31 FFs
fewer. Vivado also builds a 7 x 5 product in LUTs;
chipsmith takes every product to DSPs. On the board: owed.

Zybo limits:

- Every pin is LVCMOS33, outputs at 16 mA and slow slew (what nextpnr
  wrote, `LVCMOS33_LVTTL.DRIVE.I12_I16`), inputs with no pull. The
  manifest has no I/O standards yet, and the Zybo's PL banks (34, 35) are
  both 3.3 V.
- Pins are the package's 100 PL user I/O balls (prjxray-db's
  `package_pins.csv`). PS pins (MIO, DDR) are rejected in the manifest.
- The PS isn't configured from the fabric side beyond one thing: every
  design gets a PS7 with every input tied to 0 (1,770 of them; the ones
  the fabric can reach, as nextpnr tied them). IRQF2P[19:16] are the ARM
  cores' FIQ and IRQ, active high, and an interconnect input nothing
  drives reads as 1. So a design without a PS7, Vivado's PL-only blinky
  included, holds FIQ on both cores, and the moment the PL->PS level
  shifters come on under Linux (Linux loading it, or a JTAG load while
  U-Boot runs) both cores spin in the FIQ handler. Observed on venus,
  halted over JTAG: both cores in `__fiq_svc`/`ct_nmi_enter`; with
  IRQF2P tied to 0 Linux stays up. A design that connects the PS7's pins
  is rejected (`crates/chipsmith-xc7/src/pack.rs`).
- Cells: what synthesis makes ("7-series synthesis", above). No SRLs,
  MUXF7/F8, latches or I/O-tile registers; no DSP48E1 ALU modes or
  pre-adder (a product, or a cascaded pair's sum).
- Timing is chipsmith's estimate (`crates/chipsmith-xc7/src/delay.rs`,
  ADR-0016): routing per node as fitted to Vivado's timer on chipsmith's
  own routing, cell delays from prjxray-db's SDF with the register's as
  Vivado has them. Against Vivado on the same placement and routing it
  reads 6.5% median and 17.0% p95 off per endpoint, where the
  earlier model read 32.1% and 44.1% short ("Timing against Vivado", below). A
  clock's `[clocks]` frequency is checked against it; a clock with no pin
  (a BSCANE2's TCK) is reported unconstrained.
- The flow is deterministic: two builds, and builds on 1 and 8 threads,
  give the same `.bit` byte for byte (`crates/chipsmith/tests/determinism.rs`:
  zybo-jtag-counter, zybo-picorv32 with and without the electrostatic
  start); the header carries no date.
- The Zybo Z7 has its clock on K17. It needs only another pin map; the
  Z7-20 (xc7z020) needs another `Device`.

#### The Zybo flow

Since 2026-11-01 the whole Zybo build is chipsmith's own from the Verilog
to the FASM, and since 2026-11-02 to the `.bit` (ADR-0013):

- **Packing** (`crates/chipsmith-xc7/src/pack.rs`): synthesis' cells into
  the placer's design directly, in shapes legal by construction. Every
  CARRY4 S is a LUT's O6 in that bit's 6LUT (a feed-through LUT1 where S
  comes from anything else, which is most of a counter's), DI comes in on
  the bit's X bypass pin, a RAM32M is eight RAMD32 LUTs owning a SLICEM,
  and the constants are nets every sink taps from its nearest GND_WIRE or
  VCC_WIRE. A register's CE tied 1 or SR tied 0 is no connection at all.
- **Placement** (`place.rs`): the 7-series annealer as before; from 200
  BLEs, chains and RAMs on (since branch xc7-timing; 1,000 before), it
  starts cold from chipsmith-place's electrostatic global placement
  (ADR-0012) instead of hot from a greedy start, the global placement's
  nets weighted by their criticality every 25 iterations as the anneal
  weights them. Five seeds each by the calibrated estimate, against the
  greedy start or an untimed global placement: sha256 +5%, picorv32 +4%,
  riscv-bram +2%, bram-selftest +4%, mult-selftest's 62.5 MHz logic -6%;
  by Vivado on picorv32's five seeds, even (145.4 against 145.1 MHz,
  137.7-155.7 against 131.0-158.0); placement 4 to 5 times faster on
  picorv32 and riscv-bram.
- **Clocks** (`cmt.rs`, since 2026-11-04, ADR-0017): `XC7_PLL` and
  `XC7_MMCM` ask for a PLLE2_ADV or MMCME2_ADV by ratio, CY2_PLL's terms
  (ADR-0008): `INCLK0_INPUT_FREQUENCY` in ps, per output
  `CLK<k>_MULTIPLY_BY`, `_DIVIDE_BY`, `_PHASE_SHIFT` (ps) and
  `_DUTY_CYCLE` (%), `COMPENSATION` `"ZHOLD"` (feedback through a BUFG,
  the default) or `"INTERNAL"`:

  ```verilog
  XC7_PLL #(.INCLK0_INPUT_FREQUENCY(8000), .CLK0_MULTIPLY_BY(3), .CLK0_DIVIDE_BY(5))
    pll (.INCLK(clk), .ARESET(1'b0), .CLK0(clk75), .LOCKED(locked));
  ```

  Synthesis finds the counters Vivado's clocking wizard would (110 of 117
  of its exact answers, every PLL one; bench/cmt/README.md) and the build
  log names them (`PLLE2_ADV pll: CLKFBOUT_MULT 33, DIVCLK_DIVIDE 5, VCO
  825.000 MHz; CLKOUT0 /11 75.000 MHz`). A request no counters make
  exactly is an error. The block goes in the tile nearest its input,
  which a clock pin reaches by its CCIO; the outputs' frequencies come
  from the counters, so their clocks are checked with no `[clocks]` entry,
  and the pin's `[clocks]` frequency must be the period the block was
  solved for. The bits are Vivado's for the same counters: 113 harvested
  blocks match bit for bit, lock and filter tables included (`tests/cmt.rs`).
  Not built: phase on a fractional divider, BANDWIDTH LOW/HIGH, DRP,
  dynamic phase shift, CLKIN2.
- **Site routing** (`phys.rs`): each cell port is one site pin, by rule:
  a logic LUT's inputs on the pins their drivers reach soonest by the
  routing graph's delays, the most critical by the placement's timing
  first (since branch xc7-qor; before, the fastest pins: a site pin costs
  0.56 ns into the slice at `<L>1`, 0.03 ns at `<L>6`, but
  zybo-jtag-counter's register reached the `D6` beside it by a long line
  out and back; `<L>5` down beside a 5LUT, whose `<L>6` is tied high), a register's D from the 6LUT beside it,
  the CARRY4's sum or the `<L>X` bypass, a RAM32M's write data (bit 0,
  the 5LUT's half, on `<L>I`, bit 1 on `<L>X`: Vivado's mapping, which
  chipsmith had the other way round until branch xc7-timing) with `<L>6`
  tied high, a pin's buffer on its IOB's `I`, `O`, `T` through the I/O
  tile's ILOGIC and OLOGIC.
- **Routing** (`chipsmith_route`): every net, clocks and constants
  included, in the prjxray graph, which now keeps the pseudo pips a
  design passes through with the bits nextpnr sets for them (an I/O
  tile's ILOGIC and OLOGIC, a clock row's BUFHCE). A BUFG's net is
  confined to the clock tree (block RAM and DSP clocks too, since branch
  xc7-timing: they were routed beside the BUFG). Routing is
  timing-driven with each node's delay as fitted to Vivado's timer, on
  every connection the timing report times (since branch xc7-qor2: before,
  the router's timing graph held only the connections it routes, so every
  path broke inside a slice, at a LUT into its CARRY4 or a sum into its
  register, and the LUTs in front of an adder were routed as uncritical).
- **FASM** (`fasm.rs`): what nextpnr writes, feature for feature where
  they configure the same thing (its `fasm.cc`), clock tree buffer enables
  included.
- **Frames and `.bit`** (`frames.rs`, since 2026-11-02): each FASM feature's
  bits from prjxray-db's segbits into the frames of its tile's tilegrid
  entry (through the tile's alias where it has no segbits of its own),
  every frame of the part written in part.yaml's column order, two empty
  frames after each row, each frame's ECC in word 50, then
  xc7frames2bit's start-up sequence. On blinky, jtag-counter, picorv32 and
  sha256 the frames are byte-identical to prjxray's `fasm2frames` and the
  `.bit` payload to its `xc7frames2bit` (the header aside, theirs carrying
  a date); the bitstream test checks that on every build. 0.8 s.

Each build is checked at the bitstream (`crates/chipsmith/tests/xc7_bitstream.rs`):
prjxray's `bit2fasm` reads the `.bit` back, `chipsmith_xc7::decode`
turns its bits into a netlist by the hardware's rules (every site input
traced back through the enabled pips; each used slice half its LUTs,
registers and carry chain, whatever of them is used), and iverilog runs
that netlist beside the RTL. The decoder is checked first on nextpnr's
bitstreams of blinky and jtag-counter, which ran on the board, and of
regs_arith. On chipsmith's own, 2026-11-01:

| design | checked | output bits compared |
|---|---|---|
| zybo-blinky | 3,000 cycles | 12,000 |
| zybo-jtag-counter (around its BSCANE2) | 3,000 cycles | 15,000 |
| zybo-picorv32, its loop running | 20,000 cycles | 540,000 |
| zybo-sha256 | 3,000 cycles | 81,000 |
| zybo-bram-selftest, two passes | 17,000 cycles | 85,000 |
| zybo-mult-selftest, three passes (its PLL's 75 MHz since branch xc7-mmcm) | 5,800 cycles | 29,000 |
| zybo-riscv-bram, its program's passes (its CPU on its PLL's 112.5 MHz) | 50,000 cycles | 250,000 |
| zybo-pll, two PLL clocks | 3,000 cycles | 12,000 |
| a PLL's five clocks (a phase, x2, a 25% duty cycle sampled on a shifted clock) and an MMCM's fractional /6.625 (`tests/designs/cmt.v`) | 3,000 cycles | 63,000 |
| every register shape and the carry arithmetic | 3,000 cycles | 161,999 |
| RAM32Ms of one to three read ports, contents, a falling-edge write | 3,000 cycles | 94,274 |

bit2fasm reads no bits of the single-IOB (`_SING`) I/O tiles, so the
check takes their pin paths as given (`decode.rs`). Two things the check
found in nextpnr's own flow: its RAM32M drops initial contents (it reads
`INITA` where yosys writes `INIT_A`), and its picorv32 bitstream (never
run on the board) doesn't match its RTL; neither was chased.

Per design, against Vivado 2026.1's own placement and routing of the same
Verilog (xc7-re `tools/oracle.sh`, 8 ns clock; Vivado's LUTs include its
LUTs as memory, and it uses MUXF7s, which chipsmith doesn't). chipsmith's
LUTs are LUT6 sites in use, feed-throughs, RAM32Ms and LUT6_2 pairs
included. Vivado's "Slice LUTs" leave out the LUTs a route passes
through, which chipsmith's feed-through LUT1s in front of a carry chain
are: 43 of picorv32's 666, so its logic is about 577 sites to Vivado's 545
(and Vivado's 28 MUXF7s). Fmax: chipsmith's estimate,
Vivado's timer on chipsmith's own placement and routing ("Timing against
Vivado", below; branch xc7-qor2, 2026-11-08, the default seed), the
earlier flow's nextpnr STA of chipsmith's routing, and Vivado's own
placement and routing. Placement and routing seconds on a shared machine
at a load of 10 to 40 (most of it this branch's own benches):

| design | LUTs chipsmith / Vivado | registers | CARRY4 | slices | place + route | Fmax chipsmith (estimate) / Vivado on chipsmith's / earlier flow / Vivado's own |
|---|---|---|---|---|---|---|
| zybo-blinky | 28 / 1 | 27 / 27 | 7 / 7 | 7 | 1.3 + 0.4 s | 383.4 / 397.1 / 391.5 / 390.3 MHz |
| zybo-jtag-counter (clk) | 96 / 51 | 128 / 128 | 8 / 8 | 54 | 1.5 + 1.7 s | 367.4 / 378.1 / 352.5 / 377.8 MHz |
| zybo-picorv32 | 666 / 589 | 362 / 375 | 58 / 51 | 251 | 8.8 + 29.7 s | 168.2 / 170.2 / 167.9 / 156.9 MHz |
| zybo-sha256 | 2,326 / 2,012 | 2,035 / 1,934 | 160 / 112 | 1,078 | 106 + 251 s | 103.0 / 96.4 / 61.0 / 82.8 MHz |

On a quiet machine (load 2 to 3, 2026-11-10) the same placements route
faster: the router batches its first four iterations, 8 nets side by
side, and skips the critical-path repair, which moved neither design's
critical path by more than 0.01 ns. zybo-picorv32 places in 4 s and
routes in 12 to 15 s (24 s before), zybo-sha256 in 52 s and 65 to 82 s
(113 to 120 s before), at the same Fmax estimate over three placer seeds
(docs/fast-pnr-status.md, "7-series routing").

The self-tests against Vivado the same way (2026-11-08, branch xc7-qor2; Vivado's own
2026-11-02, bram-selftest's on its earlier RTL):

| design | LUTs chipsmith / Vivado | registers | block RAM | DSPs | Fmax chipsmith (estimate) / Vivado on chipsmith's / Vivado's own |
|---|---|---|---|---|---|
| zybo-bram-selftest | 259 / 10,646 | 193 / 8,346 | 9 RAMB18 / 3 tiles | | 159.0 / 156.9 / 118.1 MHz |
| zybo-mult-selftest (its PLL's 75 MHz test clock) | 639 / 671 | 456 / 446 | | 14 / 10 | 87.0 / 83.1 MHz / not timed |
| zybo-riscv-bram (its CPU's 112.5 MHz PLL clock) | 941 / 779 | 509 / 528 | 2 RAMB18 / 1 tile | | 138.6 / 134.8 / 143.5 MHz (Vivado's at 125 MHz) |

bram-selftest registers each memory's compare and counts an error a
cycle later, since branch xc7-timing, and riscv-bram's CPU runs from a
PLL's 112.5 MHz (62.5 MHz from a toggling register until branch
xc7-mmcm): by honest timing both missed 125 MHz (Vivado's own build of
bram-selftest's earlier RTL made 118.1 MHz; chipsmith's riscv-bram made
115 to 129 MHz over five seeds), and a clock short of its `[clocks]`
fails the build.

Vivado builds most of bram-selftest's memories in registers and 1,768
MUXF7s: it won't put a memory in a block where a register with an initial
value drives its address. mult-selftest's four fewer DSPs aren't its
cascades (chipsmith cascades the same pairs since branch xc7-qor): it
builds the three 9 x 9 products in LUTs and the 18 x 18 unsigned one on
one DSP, B's top bit added through C.

#### Timing against Vivado

Since branch xc7-timing Vivado times chipsmith's own placement and
routing, as Quartus times the DE2's (ADR-0016): the `xc7_vivado` example
writes a build's bitstream, decoded, as a netlist of unisim primitives
with every cell's site and bel and every net's route as `FIXED_ROUTE`,
and xc7-re's `tools/import.sh` has Vivado place and route by them and
report (`bench/zybo-timing.sh <example>`; a net Vivado's site routing
can't take, such as a RAM32M's WE it wants on CE, goes to Vivado's
router: 0 to 7 of 850-3,500 per design).

chipsmith's estimate against Vivado's timer on the same routing, worst
path per endpoint, data path delay (register to register):

| | endpoints | median | 95th percentile | signed median |
|---|---|---|---|---|
| before (open at 0d0af2b: prjxray-db's delays by wire class) | 7,715 | 32.1% | 44.1% | -31.4% |
| after (seven designs, the default seed) | 8,632 | 6.5% | 17.0% | -4.2% |

The model (`crates/chipsmith-xc7/src/delay.rs`): each routing node's delay
by its class (the pip into it), fitted to Vivado's net delays on 38,573
connections (`data/xc7z010-1-routing.tsv`, `bench/zybo-timing-fit.py`),
1.1% median and 7.5% p95 per connection where prjxray-db's intrinsic pip
delays read 19% short; a site input's own delay (a slice's A1 0.56 ns,
its A6 0.03 ns); the register's clock to Q and setups as Vivado has them
(`bench/zybo-cell-delays.py`; prjxray-db's SDF leaves the site's routing
out of them); 140 ps on every path end for the clock tree. Per design
(8 ns clock; Vivado's Fmax 1 / (8 ns - WNS); seed 0, the default):

| design | before (0d0af2b): chipsmith's estimate / Vivado on chipsmith's routing | after | Vivado's own |
|---|---|---|---|
| zybo-blinky | 444.8 / 314.8 MHz | 375.9 / 392.5 MHz | 390.3 MHz |
| zybo-jtag-counter | 384.6 / 298.7 MHz | 330.4 / 344.2 MHz (five seeds: 340.6 to 364.2) | 377.8 MHz |
| zybo-picorv32 | 192.1 / 112.7 MHz | 150.1 / 137.7 MHz (five seeds: 137.7 to 155.7) | 156.9 MHz |
| zybo-sha256 | 59.8 / 42.0 MHz | 53.7 / 49.6 MHz | 82.8 MHz |
| zybo-riscv-bram | 139.6 / 108.5 MHz | its CPU now at 62.5 MHz: 120.1 / 121.6 MHz | 143.5 MHz |
| zybo-bram-selftest | 153.7 / 84.4 MHz | on its new RTL: 155.4 / 158.9 MHz | 118.1 MHz (its earlier RTL) |
| zybo-mult-selftest (the 62.5 MHz clock) | 76.1 MHz / not timed | 71.0 / 69.4 MHz | not timed |

Before, the block RAMs' and DSPs' clocks came through general
interconnect (below), which is most of riscv-bram's, bram-selftest's
and mult-selftest's difference.

Branch xc7-qor (2026-11-04): sums by arrival, register copies, LUT pins
by route delay, cascaded DSPs. Vivado on chipsmith's routing, five seeds
each (`ZYBO_SEEDS="0 1 2 3 4"`), the slowest logic clock:

| design | open df16e49 | xc7-qor | Vivado's own |
|---|---|---|---|
| zybo-blinky | 392.5 MHz (every seed) | 392.5 MHz (every seed) | 390.3 MHz |
| zybo-jtag-counter | 340.6-364.2 MHz | 343.2-378.1 (mean 364.9) | 377.8 MHz |
| zybo-picorv32 | 137.7-155.7 (mean 145.4) | 131.9-158.2 (mean 147.0) | 156.9 MHz |
| zybo-sha256 | 49.0-51.6 (mean 50.0) | 71.8-80.8 (mean 75.8) | 82.8 MHz |
| zybo-bram-selftest | 137.0-158.9 (mean 149.4) | 143.6-152.9 (mean 150.3) | 118.1 MHz (earlier RTL) |
| zybo-mult-selftest (62.5 MHz clock) | 66.9-69.7 (mean 68.6) | 70.7-76.1 (mean 74.6) | not timed |
| zybo-riscv-bram (CPU clock) | 114.6-129.0 (mean 123.1) | 117.4-127.0 (mean 122.2) | 143.5 MHz (at 125) |

Branch xc7-mmcm (2026-11-05): the two self-tests on PLL clocks (ADR-0017),
the toggling registers gone. Vivado on chipsmith's routing, five seeds,
the PLL's clock:

| design | clock | chipsmith's estimate | Vivado on chipsmith's routing | endpoints, \|error\| median / p95 |
|---|---|---|---|---|
| zybo-mult-selftest | 75 MHz (PLL 3/5: VCO 825 MHz, /11) | 77.1-81.5 MHz | 75.6-78.8 (mean 77.1) | 2.7% / 9.7% (4,355) |
| zybo-riscv-bram (CPU) | 112.5 MHz (PLL 9/10: VCO 900 MHz, /8) | 117.8-134.4 MHz | 114.2-128.1 (mean 122.3) | 4.5% / 12.6% (5,180) |
| zybo-riscv-bram (CPU), asked for 125 MHz | 125 MHz (PLL 1/1: VCO 875 MHz, /7) | the same placements | 114.8-128.9 (mean 123.0): four of five fail | 4.5% / 12.6% |

The placement and routing don't depend on the frequency asked for (the
same seed gives the same estimate at 112.5 and 125 MHz); Vivado's own
figures move by up to 0.8 MHz with the PLL's VCO. mult-selftest at 75 MHz
has 0.6 MHz to spare on its slowest seed.

sha256's is mostly the sums: by arrival alone 63.8-71.9 (mean 68.7); its
round counter's 201-load register, copied, is the rest. Its critical path
is now control (a LUT to 166 loads, then nets of 33 and 32 loads at 3.1
and 3.7 ns) where Vivado's is the adders. Tried and rejected, by Vivado
unless said:

- carry-save sums (the three terms ready first as XOR and majority, one
  chain per three terms, Vivado's ternary adder): sha256's CARRY4s 160 to
  112, Vivado's count, but 55.9 MHz on seed 0 against 68.5 by arrival (the
  majority LUTs add a level per three terms);
- LUT copies as register copies: sha256 78.0 against 85.5 MHz on seed 0,
  by the estimate, with 258 copies;
- register copies from 16 or 24 loads: by the estimate within seed noise
  of 32, with 259 and 170 copies to 128;
- carry chains in either slice of a tile: sha256 the same (72.6 MHz mean
  over four seeds), zybo-jtag-counter 310.2-378.1 (mean 354.3), its
  counter's registers reaching their own LUTs through detours in the odd
  slice, picorv32's estimate 129.1 against 145.2 MHz;
- LUT pins by the least criticality-weighted sum of delays, not the most
  critical input first: bram-selftest 137.4-154.3 (mean 143.5), picorv32
  mean 142.0, jtag-counter mean 361.5;
- more priority cuts (16) and area passes (4 and 4) in the LUT mapper:
  picorv32 613 to 611 LUTs.

What timing Vivado's way found and fixed: a LUT's inputs went on A1 up,
the slowest pins (now the fastest, the most critical first); block RAM
and DSP clocks were routed beside the BUFG through general interconnect;
a RAM32M's two write data pins were the other way round from Vivado's
reading of the same bits (co-simulation agreed with itself; the board
would swap each pair of bits); a loop of two LUTs left 298 of
picorv32's timing nodes, its worst path among them, untimed. Tried and
not taken, five seeds each: the placer's hop delays as fitted (picorv32
-5%), a linear fit of routed delay to distance as the placer's estimate
(no better), timing weight 10 with exponent 2 (riscv-bram -3%, sha256
-2%), LUT pins re-ranked from the routed timing and routed again
(riscv-bram -3%).

Branch xc7-qor2 (2026-11-08): the router times every connection. Its
timing graph held only the connections it routes, driver to each sink
pin's first port, so every path broke inside a slice (a LUT into its
CARRY4's S, a sum into its register, the second LUT of a pair on a shared
pin), and the LUTs in front of an adder looked uncritical: on sha256's
seed 0 the router timed 8.04 ns of what the report (and Vivado) time at
13.76 ns, and routed a 32-load select's critical sink over 25 wires, west
and back east, 4.2 ns. Those connections now go to the router at 0 ps, as
the report takes them (`pnr::router_graph`). Found by having Vivado route
chipsmith's own seed 0 placement (`tools/import.sh` with an empty
`route.tcl`): 97.0 MHz against 71.8 on chipsmith's routing of it, so the
placement wasn't the gap. sha256 by Vivado on chipsmith's routing, five
seeds, the placements unchanged (chipsmith's estimate in brackets):

| seed | main 6c2d740 | xc7-qor2 |
|---|---|---|
| 0 | 71.8 (72.7) MHz | 96.4 (103.0) MHz |
| 1 | 76.3 (82.1) | 96.4 (103.2) |
| 2 | 80.8 (83.2) | 95.5 (102.1) |
| 3 | 75.9 (78.2) | 98.7 (104.7) |
| 4 | 74.0 (79.4) | 98.5 (105.1) |
| range, mean | 71.8-80.8, 75.8 | 95.5-98.7, 97.1 |

Vivado's own build of sha256 makes 82.8 MHz. The estimate reads 7%
optimistic on these routings (endpoints: 4.8% and 5.2% median |error| on
seeds 0 and 4, against 6.5% before). Vivado routing the same seed 0
placement with the LUT pins free to swap made 98.4 MHz, 1.5% over the
pins locked: swapping LUT pins as it routes is worth little here. Tried
and dropped, by chipsmith's estimate after the fix, three seeds each: a
per-connection timing cost in the anneal beside the weighted wirelength
(VPR's; weight 5: 102.7, 105.5 and 101.3 MHz against 103.0, 103.2 and
102.1; weight 20: 100.6 on seed 0), and critical-path repair of nets
wider than 32 sinks (the same estimate on seeds 0 and 1).

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
| Verilog front end | chipsmith: preprocessor, parser, elaboration to yosys' `proc` level, `.sv` files as SystemVerilog (Spade, below), with the Cyclone II cell library (`crates/chipsmith-verilog`, `crates/chipsmith-cyclone2/src/synth/cells.v`) | ours |
| synthesis | chipsmith: what cyclone2-re's `cyclone2.ys` does, on chipsmith's netlist IR (`crates/chipsmith-synth`, `crates/chipsmith-cyclone2/src/synth`) | ours |
| LUT mapping | chipsmith: AIG plus priority cuts, in place of `abc -lut 4` (`crates/chipsmith-synth/src/lutmap.rs`) | ours |
| packing | chipsmith: cyclone2-re's nextpnr packer, ported (`crates/chipsmith-cyclone2/src/pack`) | ours |
| placement | chipsmith: electrostatic global placement of every LE from 700 LEs (`crates/chipsmith-place`, on CPU threads or, built with `--features gpu`, a GPU; ADR-0012), legalised LE by LE onto LAB slots and refined by a short annealer, nextpnr's LAB rules (`crates/chipsmith-cyclone2/src/place.rs`, `place/le.rs`); smaller designs, and any the LE legaliser can't fit, by the LAB-cluster flow | ours |
| routing | chipsmith: PathFinder over the chip database, rerouting only congested and critical branches (`crates/chipsmith-route`, `crates/chipsmith-cyclone2/src/route.rs`); optionally (`CHIPSMITH_ROUTE_ACCEL=cpu`, `cuda`) the first 10 iterations in batches of 64 nets on CPU threads or a GPU, the same routes either way, 1.7 (sha256) to 2.1 (picorv32x24) times faster than A* on a GTX 1660 (docs/gpu-pnr.md) | ours |
| bitstream | chipsmith: cyclone2-re's assembler (`cram pack`), moved here and still checked against Quartus by cyclone2-re's `cram` (`crates/chipsmith-cyclone2/src/bitstream`, ADR-0003) | ours |
| JTAG stream | chipsmith: the SVF `cram svf` wrote, CONF_DONE checked (`crates/chipsmith-cyclone2/src/bitstream/jtag.rs`) | ours |
| cable | chipsmith: USB-Blaster and FT2232 (MPSSE) drivers, SVF player, 7-series JTAG loader (`crates/chipsmith-jtag`) | ours |
| configuration flash | chipsmith: serial flash loader (built by chipsmith) and EPCS16 driver (`crates/chipsmith-cyclone2/src/persist`) | ours |

The plan is `[ghdl,] synthesis, pack, place and route, bitstream`. yosys
and cyclone2-re's techmap are test oracles now (ADR-0004): the devshell has
them, the build doesn't run them.

Zynq-7010 PL (xc7z010clg400-1): chipsmith's own from the Verilog to the
`.bit`, prjxray-db as data.

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | as for Cyclone II |
| Verilog front end | chipsmith, with the 7-series cell library (`crates/chipsmith-xc7/src/synth/cells.v`) | ours |
| synthesis | chipsmith: chipsmith-synth's passes, LUT6 mapping, CARRY4, FD*, RAM32M, RAMB18E1, pin and clock buffers (`crates/chipsmith-xc7/src/synth`) | ours |
| packing | chipsmith: slices, carry chains with their feed-through LUTs, RAM32M SLICEMs, pins, constants (`crates/chipsmith-xc7/src/pack.rs`) | ours |
| placement | chipsmith: simulated annealing over slices, from 1,000 objects on starting from chipsmith-place's electrostatic global placement, timing-driven with prjxray-db's delays (`crates/chipsmith-xc7/src/place.rs`, `crates/chipsmith-xc7/src/delay.rs`) | ours |
| site routing | chipsmith: each cell port's site pin by rule (`crates/chipsmith-xc7/src/phys.rs`) | ours |
| routing | chipsmith: PathFinder over the prjxray-db graph, clocks and constants included (`crates/chipsmith-xc7/src/xray.rs`, `crates/chipsmith-xc7/src/pnr.rs`) | ours |
| timing | chipsmith: prjxray-db's cell and pip delays (`crates/chipsmith-xc7/src/pnr.rs`) | ours |
| FASM | chipsmith (`crates/chipsmith-xc7/src/fasm.rs`) | ours |
| FASM → frames → `.bit` | chipsmith, from prjxray-db's segbits, tilegrid and part.yaml, byte-identical to prjxray's `fasm2frames` and `xc7frames2bit` (`crates/chipsmith-xc7/src/frames.rs`) | ours |
| cable | chipsmith: FT2232 (MPSSE) driver and 7-series JTAG loader (`crates/chipsmith-jtag`), loads in 1.3 s with DONE checked | ours |

prjxray, fasm and prjxray-db come from one pinned
[openXC7/toolchain-nix](https://github.com/openXC7/toolchain-nix)
revision (`flake.nix`), with openXC7's nextpnr, which the devshell keeps to
compare against. Replacing a tool means replacing one step. The nix
package pins every tool. The devshell pins them too.

Verilog is the input language. Anything else, VHDL today and later Chisel,
Amaranth and the like, gets translated into it. Internally, the stages will
share chipsmith's own netlist IR, not Verilog.

### Spade

[Spade](https://spade-lang.org)'s compiler writes SystemVerilog, and
chipsmith reads a `.sv` file as SystemVerilog: IEEE 1800-2017's keywords
reserved, `logic`, `always_comb`/`always_ff`/`always_latch` and
`unique`/`unique0`/`priority` on `if` and `case`, as much as synthesis
needs (`crates/chipsmith-verilog/src/lex.rs`, `Dialect`). A `.v` file stays
Verilog-2005, where `logic` is still a name. So a swim project builds with a
`chipsmith.toml` beside its `swim.toml` that takes swim's output as its
source:

```
swim build                        # build/spade.sv
chipsmith flash                   # builds first, then loads
```

`examples/spade-jtag-counter` is
examples/de2-jtag-counter in Spade, its JTAG block an `extern entity` of
chipsmith's `CY2_JTAG` cell. On the DE2 2026-11-09 (loaded over JTAG)
`de2_user1` read `"SPD!"` and CLOCK_50 at 50.00 MHz, 8 reads of 8. The
compiler is pinned in its `swim.lock` (Spade 2a6beb42 from codeberg).
yosys proves chipsmith's reading of Spade's output equivalent to its own
`read_verilog -sv` for that example and for a design with an enum state
machine, a pipeline, the stdlib's FIFO and a multiplier
(`crates/chipsmith-verilog/tests/yosys.rs`, `spade`).

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
     were Quartus' DMF values (cyclone2-re docs/timing-model.md) until
     2026-11-10; since then they are chipsmith's own, from silicon and an
     aggregate fit (docs/timing-silicon.md). Routing
     delays are quartus_sta's median per element type; register-to-register
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
   - Compile speed (2026-10-30, docs/fast-pnr-status.md). Placement
     (since gpu-pnr only below 700 LEs, see below) clusters LEs into
     LAB-sized groups, places them analytically
     (`crates/chipsmith-place`, device-generic: quadratic wirelength with
     the bound-to-bound net model, CG, recursive-bisection spreading),
     legalises them onto LAB slots and refines with a short annealer: 8
     anneals from different seeds in parallel, and the one with the
     shortest estimated critical path wins, so the result doesn't depend
     on thread timing. The router reroutes only a congested net's
     branches through overused nodes and its critical sinks, and keeps
     each search within 3 tiles of its net's bounding box. A rebuild
     skips every step whose inputs' contents haven't changed (ADR-0011).
     `open` (33fb8a4) against this, on the same machine, back to back
     (other services running, load 4 to 11); critical paths are the
     median of three placement seeds (`CHIPSMITH_PLACE_SEED`), with the
     range:

     Since gpu-pnr (docs/gpu-pnr.md, ADR-0012) placement works on every
     LE of designs from 700 LEs, with a short anneal after it:
     picorv32x24 places in 39.7 s instead of 636.6 and builds 4.2 times
     faster (835 → 201 s), sha256 2.0 times (129 → 64 s), back to back,
     load 4 to 7; routed critical paths within `open`'s range over five
     seeds. The whole DE2 flow is deterministic on the CPU, and a test
     holds it to that (2026-11-01, `crates/chipsmith/tests/determinism.rs`):
     ten examples, blinky to sha256, built three times each, twice on 8
     threads and once on 1, give one RBF each; all 34 DE2 examples gave
     the same RBFs on 4 and 8 threads. The table below is fast-pnr's,
     before that.

     | design | LEs | place s | route s | build s | critical path ns, median of 3 seeds [range] |
     |---|---|---|---|---|---|
     | bidir | 29 | 0.1 → 0.3 | 0.3 → 0.3 | 5.1 → 4.4 | 3.37 [3.37-3.37] → 3.37 [3.37-3.37] |
     | blinky-verilog | 26 | 0.4 → 0.4 | 0.2 → 0.3 | 4.8 → 4.2 | 3.37 [3.37-3.37] → 3.37 [3.37-3.37] |
     | blinky-vhdl | 26 | 0.4 → 0.4 | 0.2 → 0.3 | 5.0 → 4.2 | 3.37 [3.37-3.37] → 3.37 [3.37-3.37] |
     | crc32 | 118 | 0.8 → 0.6 | 1.3 → 0.6 | 7.3 → 4.4 | 3.87 [3.35-3.87] → 3.36 [3.24-3.47] |
     | de2-io | 33 | 0.1 → 0.1 | 0.3 → 0.3 | 4.7 → 3.9 | 3.71 [3.71-3.71] → 3.71 [3.71-3.71] |
     | de2-jtag-counter | 129 | 0.5 → 0.4 | 0.7 → 0.5 | 5.5 → 4.1 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
     | de2-pll-counter | 321 | 2.0 → 1.0 | 1.4 → 0.8 | 7.8 → 4.3 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
     | femtorv32 | 788 | 10.9 → 6.1 | 10.4 → 4.1 | 26.5 → 11.8 | 13.18 [12.97-13.74] → 13.15 [12.74-13.97] |
     | fir | 534 | 3.6 → 2.2 | 5.8 → 4.2 | 13.8 → 7.6 | 13.44 [12.93-13.45] → 13.55 [13.24-13.57] |
     | i2c-master | 214 | 1.6 → 1.0 | 1.7 → 0.9 | 7.9 → 4.5 | 7.87 [6.81-7.89] → 7.16 [7.03-7.77] |
     | m4k-bytes | 98 | 0.3 → 0.3 | 0.8 → 0.9 | 5.3 → 4.2 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
     | m4k-deep | 128 | 0.7 → 0.5 | 1.3 → 1.0 | 6.5 → 4.3 | 3.80 [3.80-3.98] → 3.80 [3.80-3.80] |
     | m4k-dualclock | 112 | 0.3 → 0.3 | 0.8 → 0.8 | 5.4 → 4.1 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
     | m4k-enables | 73 | 0.2 → 0.2 | 0.8 → 0.7 | 5.5 → 4.0 | 3.59 [3.59-3.59] → 3.59 [3.59-3.59] |
     | m4k-mixed | 90 | 0.3 → 0.2 | 0.8 → 0.7 | 5.5 → 4.2 | 3.73 [3.73-3.73] → 3.73 [3.73-3.73] |
     | m4k-outreg | 83 | 0.3 → 0.2 | 0.9 → 0.7 | 5.6 → 4.0 | 3.59 [3.59-3.79] → 3.59 [3.59-3.59] |
     | m4k-selftest | 405 | 4.4 → 2.6 | 63.7 → 2.5 | 72.7 → 6.2 | 12.54 [12.26-12.73] → 11.60 [11.45-11.83] |
     | m4k-shallow | 95 | 0.3 → 0.2 | 0.8 → 0.7 | 5.3 → 4.0 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
     | m4k-tdp | 97 | 0.3 → 0.2 | 1.0 → 0.8 | 5.5 → 3.9 | 3.86 [3.80-3.91] → 3.93 [3.80-4.14] |
     | mult-selftest | 1008 | 17.2 → 8.0 | 16.4 → 7.9 | 39.0 → 17.9 | 15.58 [15.03-15.70] → 15.80 [15.41-15.86] |
     | negedge-selftest | 481 | 3.2 → 2.0 | 4.3 → 1.8 | 11.9 → 5.1 | 10.12 [9.51-10.85] → 9.72 [9.44-10.18] |
     | picorv32 | 1171 | 20.2 → 8.9 | 19.0 → 6.5 | 45.6 → 17.5 | 8.17 [8.12-8.72] → 7.90 [7.89-8.12] |
     | ram | 151 | 0.8 → 0.6 | 2.1 → 1.4 | 7.9 → 4.3 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
     | ram-leds | 42 | 0.2 → 0.2 | 0.8 → 0.4 | 5.9 → 4.2 | 5.04 [4.77-5.15] → 4.03 [3.78-4.70] |
     | ram_widths | 78 | 0.5 → 0.3 | 1.1 → 0.8 | 6.1 → 4.1 | 3.80 [3.80-3.80] → 3.80 [3.80-3.80] |
     | serv | 345 | 3.8 → 1.5 | 2.8 → 1.2 | 13.2 → 4.4 | 10.37 [10.23-10.47] → 10.19 [10.19-10.61] |
     | spi-master | 173 | 1.1 → 0.5 | 1.9 → 0.8 | 9.4 → 4.2 | 4.50 [4.36-4.69] → 4.00 [3.89-4.22] |
     | uart | 164 | 0.9 → 0.5 | 1.1 → 0.6 | 6.4 → 4.1 | 5.85 [5.76-6.49] → 5.70 [5.53-5.83] |

     Bigger designs, one seed:

     | design | LEs | place s | route s | build s | critical path ns |
     |---|---|---|---|---|---|
     | sha256 | 6,046 | 608.5 → 63.1 | 149.3 → 45.0 | 769.3 → 115.3 | 24.18 → 20.99 |
     | picorv32x24 | 28,449 | about 720 → 593.6 | did not finish in 7 h → 196.3 | 827.8 | → 10.65 |

     (picorv32x24's `open` row is from 385aa15 on a loaded machine.
     Before, its clusters didn't fit: 1,733 clusters and 361 carry-chain
     LABs against 2,076 LABs. They now fit because the legaliser packs
     small clusters into the free slots of chains' last LABs.) No
     example's critical path is worse than `open`'s beyond the spread of
     its seeds. The refining anneal is now most of the placement time on
     the large designs. GPUs don't pay at these sizes:
     `experiments/gpu-bench` measures one CG iteration on 2,000 rows at
     413 µs on a GTX 1660 against 232 µs on one CPU thread, and the CG
     solves are 0.4% of sha256's placement.
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
     and don't-care resynthesis (`mfs2`, `lutpack`, 788). More cuts per
     node (16, 32), more recovery passes, or relaxing depth by a level
     didn't help (844 to 847). An earlier comparison (842 vs 847) had
     counted abc's 75 one-input buffer LUTs, which `opt -fast` removes.
     Since 2026-11-07 (branch `qor5`) the
     Cyclone II mapper also maps the gates swept (functionally equivalent
     nodes merged, each merge checked exhaustively over a common cut of at
     most 12 nodes) and swept and refactored (each node's function over a
     cut of at most 10 leaves as a factored irredundant sum of products),
     and keeps the mapping with the fewest LUTs at no more depth:
     femtorv32 518 -> 470 LEs, picorv32 876 -> 852, mult-selftest 956 ->
     858, picorv32x24 21,241 -> 20,832. abc's choices and `mfs2`/`lutpack`
     still find more on picorv32 and mult-selftest (807 and 851 LEs
     through chipsmith's back end).
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
     (`synth/m4k.rs`, above), the other memories to
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

     The M4K cells of ram, ram_widths and ram-leds (mode, width, contents,
     which pins are tied) are the yosys flow's, cell for cell. picorv32 synthesizes in 1 s (yosys 14 s).
     `crates/chipsmith-cyclone2/tests/synth.rs` proves every example, the
     loader and five specimens sequentially equivalent to yosys' RTL of the
     same Verilog, every stage on three of them, and that a flipped LUT
     bit fails the proof. The check is chipsmith-netlist's `equiv_*`
     script in two steps (ADR-0007): the RTL against the netlist with x
     resolved, then that against the result, with the registers synthesis
     made constant (989 on picorv32) and its state registers' value sets
     first proven invariant by `sat -tempinduct`; memories in M4Ks are
     compared at their ports in the first step and word by word in the
     second (the blocks through a model of the block). All 174 plain-Verilog cyclone2-re specimens pass it and
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
   - Gaps closed (2026-10-27). What made synthesis stop is now built:
     latches (a LUT feeding its own output back, as Quartus 13.0sp1 does on
     this family, and with Quartus' warning that their power-up value is
     undefined) and registers with an asynchronous load or set and reset
     (Quartus' emulation: a latch, an LE register holding `D ^ latch`, a
     LUT choosing; ADR-0009); `/` and `%`, signed and unsigned, a power of
     two as wiring, constant divisors with only the stages they need. A
     sum of three or more terms is a balanced tree of carry chains: four
     18-bit terms run at 204 MHz (137 MHz as a chain; the yosys flow's
     `$macc` 186 MHz with 123 LUTs against 90). The front end takes
     multi-dimensional arrays and an asynchronous reset tested inside a
     named block, with memories written under such a reset made registers.
     Re-evaluating the exclusive `$pmux` trade (ADR-0007): masking a
     select only by the later selects it can overlap maps picorv32 to the
     same 1,058 LUTs, so the trade stays as it was. Every DE2 example builds
     to the RBF it built before except bidir: an output reading an inout
     (`assign LEDR = GPIO_0`) became a tri-state copy of its driver and
     floated while the pin was released; it now reads the pin's input
     buffer. The checker hadn't caught that (tri-states were opaque cells);
     it now cuts every pin into an outside input and `EN ? A : outside` on
     both sides.

   - Gaps closed (2026-10-30):
     - Falling-edge registers take their LAB clock's inversion
       (`LCELL_FF`'s `CLK_INV`) instead of a LUT inverting the clock
       into a clock from logic. A LAB holds one clock net on one edge:
       which of its two LAB clocks Quartus gives each edge isn't
       predictable (examples/negedge-selftest's ten mixed LABs fit no
       rule), so falling- and rising-edge registers don't share a LAB
       until that is modelled. The timing report times a path from one
       edge to the other against half a period. Proven (`negedge`, 40
       `$equiv`) and co-simulated (199,996 output bits; with the inversion
       dropped the co-simulation fails at cycle 0, which the proof,
       stepping every register on one edge, can't see).
     - Arrays declared inside functions, written and read a word and a
       bit at a time with constant indices, one- and two-dimensional
       (`chipsmith-verilog`'s `arrays_in_functions`, proven against
       yosys). That brings Forencich's `lfsr.v` back: examples/crc32,
       Ethernet's CRC-32 through its mask function. Its elaboration is
       proven against a textbook bitwise CRC-32 with the same top
       (yosys' own front end takes over 20 minutes on `lfsr.v`, so the
       reference is the oracle's RTL; a wrong polynomial in the reference
       fails it), and the netlist is co-simulated against it (270,000
       output bits). Quartus byte-identical. Synthesis on it is proven
       too since 2026-11-01, the reference's RTL to the netlist (174
       `$equiv`, 50 s): the CRC register powers up all ones, so synthesis
       stores it inverted, and the display register loads from that
       register directly. Nothing tied the stored register to the RTL's,
       and the induction left those 27 bits open. Synthesis now names
       each register it stores inverted after the bit it inverts
       (`crc[3]~inv`, purged with every internal name, so no RBF
       changes), and the check pairs it with `~crc[3]` on the RTL's
       side. A flipped LUT bit in front of one of them fails the proof
       (`crc32_a_wrong_crc_bit_is_caught`).
     - A register whose data is its own enable loads a 1 (quartus_map
       folds it so and drops the sync-load route the data would take,
       found on negedge-selftest's `if (p[0]) n2 <= p`); no other DE2
       example changes RBF.

     `examples/negedge-selftest` checks falling-edge registers on the
     board over JTAG: a rising-edge counter, a falling-edge register
     taking it half a cycle later and a rising-edge one taking that, and
     a falling-edge register with an enable. Every rising edge compares
     them with what the edges imply and counts mismatches. USER1 holds
     `"NEG!"`, the error count and the counter, both Gray-coded (`de2_user1
     -- --words 3`). On the DE2 (2026-10-30, loaded over JTAG): the counter
     at 50.00 MHz, 0 errors after 17 minutes (about 51 billion checked
     cycles). With the first falling-edge
     register made rising-edge, the error count climbs at 50.00 MHz, one
     per cycle. Quartus byte-identical; quartus_sta's worst path is a
     falling-to-rising one, 6.04 ns of data against half a period (the
     estimate 5.53 ns; on 2026-11-01's placement 6.83 and 6.07 ns).

     A corpus of open-source cores, unmodified behind DE2 wrappers
     (`examples/<name>`, upstream and commit in each manifest), each proven
     equivalent to yosys' RTL. LEs and Fmax, chipsmith's synthesis against
     the yosys flow (`de2_synth --yosys`; both through chipsmith's back
     end, Fmax its estimate, which runs up to 9% optimistic, see the
     timing table above). chipsmith and the yosys flow
     re-measured 2026-11-01, on gpu-pnr's placer (`bench/speed.sh`,
     `de2_synth --yosys`):

     | design | chipsmith | yosys flow |
     |---|---|---|
     | uart (Forencich) | 164 LEs, 180.8 MHz | 225 LEs, 144.8 MHz |
     | spi-master (nandland) | 130 LEs, 293.3 MHz | can't build: an asynchronous reset to 1 that powers up 0 |
     | i2c-master (Forencich) | 214 LEs, 144.5 MHz | 245 LEs, 141.8 MHz |
     | femtorv32 quark | 788 LEs, 71.8 MHz | 832 LEs, 91.1 MHz |
     | serv | 345 LEs + 1 M4K, 96.3 MHz | 3,111 LEs, 79.2 MHz |
     | sha256 (secworks) | 4,242 LEs, 49.2 MHz | can't build (as spi-master) |
     | picorv32 | 1,171 LEs, 119.2 MHz | 1,175 LEs, 126.5 MHz |
     | crc32 (Forencich's lfsr.v) | 118 LEs, 298.5 MHz | not run: yosys' front end takes over 20 minutes on it |

     chipsmith's uart, spi-master, i2c-master and sha256 are as of
     2026-10-29: a carry chain's low cells whose carry out is forced now
     get no LE (quartus_map sweeps them out of a VQM), which took 26, 6,
     11 and 7 LEs off them (190, 179, 225 and 6,053 before). spi-master
     and sha256 are as of 2026-11-01: both reset with `negedge`, and the
     LAB now inverts the clear (below), which took 43 and 1,804 inverter
     LEs off them (173 LEs at 232.2 MHz and 6,046 at 47.9 MHz on branch
     open the same day, Fmax the build's estimate).

     Still short: femtorv32's register file (chipsmith's M4K mapping
     doesn't take its shape yet) and wide multiplexers: sha256's 16-word
     schedule read maps to about 20 LUTs a bit (the mapper, not the
     netlist: the same read as a `$bmux` tree maps no better; on 2026-11-07
     it was 400 LUTs for 32 bits, 12.5 a bit). Still rejected: `$pow` with a non-constant base, the yosys flow's own
     limits above.
   - Gaps closed (2026-11-01):
     - An active-low asynchronous reset, `always @(posedge clk or negedge
       rst_n)`, the reset of Terasic's and most university DE2 code. Synthesis
       puts an inverter in front of each register's clear, and nothing
       folded it: a counter's every bit had a clear net of its own, and a
       carry chain's LEs couldn't share a LAB (examples/de2-ioregs with
       KEY[0] as its reset didn't fit). Quartus keeps one clear net and
       the LAB inverts it, one bit per clear line (cyclone2-re README,
       "Asynchronous resets"; 18 Quartus designs). The packer now folds
       the inverter into the register (`LCELL_FF`'s `ACLR_INV`), the placer
       treats the clear and its polarity as the LAB's control, and the
       bitstream sets the LAB's `LAB_CLR_INVERT_PAIR` bit. Checked by
       co-simulating the packed netlist against the RTL
       (`active_low_clears_fold_into_the_lab`: 159,987 output bits; with
       `ACLR_INV` dropped it fails at cycle 0), a flow test down to the
       RBF's bit (`tests/lab_clears.rs`: a 24-bit counter on one inverted
       clear over two LABs, the same pin as an active-high clear in
       another), and Quartus: examples/de2-ioregs, sha256 and spi-master
       byte-identical. Those three are the DE2 examples whose RBF changed:
       sha256 4,242 LEs (6,046 before), spi-master 130 (173), de2-ioregs
       with its source; the other 30 (picorv32x24 not rebuilt) build to
       the same RBF as on branch open. The chip database is unchanged.
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
   - Zybo (done, 2026-11-08, without Linux; 2026-10-24 through Linux).
     `chipsmith flash --persist` on a `.bit` writes a QSPI boot image
     (BootROM header, chipsmith's 56-instruction loader, the bitstream)
     through the ARM's debug port, with no code on the ARM, and reads it
     back. It reads the flash first and leaves it alone if it already
     holds the image. With the boot-mode jumper (JP5) on QSPI it then
     resets the PS so the BootROM boots the image as at power-on;
     otherwise it configures the PL from the flash directly and says to
     move the jumper. On the board 2026-11-08: zybo-bram-selftest written
     and verified in 157 s (2,086,236 bytes), booted by the BootROM from
     QSPI, USER1 "BRAM" with passes rising and 0 errors; the same command
     again found the flash current in 18 s and rebooted (21 s in all).
     The flash's clock is set by chipsmith (IO PLL, SCK about 20 MHz):
     after a QSPI boot the BootROM leaves it at the ARM PLL / 40, at which
     the batched writes programmed zeros.
     Through Linux: with NixOS on the board,
     nixos-fpga's `hardware.fpga.bitstream` loads the build's `.bin` at
     every boot ("Loading a Zybo from its own Linux", above), so
     `--persist` with `--ssh` points there instead of writing flash.
     Without Linux, persistence means writing a boot image to QSPI: done,
     all three pieces below, and `flash --persist` (above) runs them:

     The QSPI flash hangs off the Zynq's processor (PS
     MIO 1-6); the PL can't reach it, so a PL bridge like the DE2's can't
     work. openFPGALoader refuses to write Zynq flash for that reason;
     Vivado's `program_flash` runs an FSBL plus a U-Boot helper on the ARM
     core over the debug port (traced for the ZynqMP in openFPGALoader
     issue 654). The PS BootROM loads only a first-stage
     loader from QSPI, never a bitstream, so a PL-only design still needs
     a boot image: a BootROM header, a first-stage loader in OCM, the
     bitstream. It doesn't need Xilinx's FSBL. Three pieces, two of them
     done:
     1. Done 2026-11-07 (`chipsmith_jtag::adiv5`, example `zynq_dap`):
        ARM debug access over JTAG: the ADIv5 DAP on the Zybo's chain
        (IR 4, beside the xc7z010), DPACC/APACC scans, AHB-AP memory reads
        and writes. With that the host can drive the PS's QSPI controller
        (0xE000D000; MIO and LQSPI clock set up in the SLCR) in I/O mode
        and program the 128 Mbit QSPI flash itself, with no code on the ARM.
        On the board, with Linux running on the ARM: AP 0's IDR reads
        0x44770001 (AHB-AP), PSS_IDCODE 0x13722093 (matches the chain's
        IDCODE) and BOOT_MODE 5 (SD card). `chipsmith_jtag::zynq_qspi`
        drives the QSPI controller in I/O mode from the host: the flash's
        JEDEC ID reads 01 20 18 (Spansion S25FL128S), 3 of 3 runs. Two
        things found on the board: the controller's reserved config bit 17
        must stay 1, and its RX-not-empty flag never sets, so an empty RX
        FIFO is detected by the bus error a read of it returns.
        Example `zynq_flash` (2026-11-07, on the Zybo, Linux running on the
        ARM): `dump` reads all 16 MiB through the controller's linear
        window in batched AHB-AP reads, 74-84 KiB/s (3.5 minutes; two dumps
        identical). `write` erases (4 KiB parameter sectors over the first
        128 KiB, 64 KiB after: this part reports sector architecture 01),
        programs 256-byte pages in batched I/O-mode frames and reads back:
        300,000 random bytes at 0xF00000 in 25 s (11.6 KiB/s), identical.
        A WAIT inside a write batch fails it, since a FIFO write can't be
        repeated.
     2. Done 2026-11-07: a first-stage loader of our own, built with an open ARM toolchain
        from nixpkgs: about 40 instructions that start a PCAP DMA from the
        QSPI linear window (0xFC000000 + offset) into the PL and wait for
        DONE. No DDR or PLL setup for a PL-only design.
        `crates/chipsmith-xc7/src/zynq_loader.S`, 56 instructions (256
        bytes with its literals), assembled by the devshell's
        arm-none-eabi binutils and committed (a test checks it's current);
        `zynq::qspi_boot_image` lays out header, loader and `.bin`. Its
        register sequence ran first from the host over the DAP
        (`zynq_pcap`): on the Zybo, examples/zybo-jtag-counter configured
        from flash 0xFCE00000 (DONE in 784 ms including the host's polls),
        and again from offset 0x9C0 inside the boot image written at flash
        offset 0 (2,086,236 bytes in 233 s, read back identical); USER1
        read "ZYBO" at 125.01 MHz both times. Then with JP5 on QSPI and a
        power cycle the BootROM ran it: BOOT_MODE 1 (QSPI), devcfg
        INT_STS 0x50023004 (DMA and PCFG done, no errors), LVL_SHFTR_EN
        0xF (the loader's last store), USER1 "ZYBO" at 125.00 MHz with
        the count at 1.15e9 (about 9 s since configuration, so from this
        boot). The factory image's 16 MiB are kept outside the
        repo (two dumps identical, sha256 fb8ea154...).
     3. Done 2026-11-07 (`chipsmith_xc7::zynq::boot_image`, byte-identical
        to U-Boot's `mkimage -T zynqimage`): the BootROM header (UG585 section 6.3: width word, `XLNX`, loader
        offset, length, checksum), written by chipsmith; bootgen isn't
        needed for an unencrypted image.
     The board then also has to boot from QSPI: the boot-mode jumper JP5
     must be on QSPI, which only someone at the board can set; chipsmith
     reads it over the DAP (SLCR BOOT_MODE, 0xF800025C; on SD card as of
     2026-11-07). A PS soft reset over the DAP (SLCR PSS_RST_CTRL) would
     then rerun the BootROM without a power cycle. On the Zybo JPROGRAM
     only clears the PL; the PL is the PS's slave, so it doesn't reload
     from flash the way a standalone 7-series in master mode does.
     The SD card is the other path (BOOT.bin on FAT32, same boot image
     and jumper).

The DE2 is done: every step above is chipsmith's, and every fabric
feature it builds (logic, carry chains, M4Ks, multipliers, PLLs, I/O-cell
registers, falling-edge clocks, both external RAMs, the EPCS) has run on
the board. The polish left after it (2026-11-07, branch `qor5`): fewer LEs, with
abc's choices and don't-care resynthesis as the next gain (LUT mapping,
above). The board's other peripherals (flash, audio,
Ethernet, USB, LCD, PS/2, SD) would test controller RTL, not the
toolchain, so they aren't goals.

Next devices:

- **Zybo** (Zynq-7000, 7-series fabric): it builds and loads (above).
  Project X-Ray already documents the fabric, so owning the stages means
  importing prjxray-db, not reverse engineering. xc7-re holds the
  groundwork. Same order as for Cyclone II: router, placer, packer.
  Since 2026-11-02 every stage from the Verilog to the `.bit` is
  chipsmith's ("The Zybo flow", above). The router and placer items below
  record how they were built, against nextpnr, while the flow was: yosys;
  nextpnr packs (`--pack-only`); chipsmith places; nextpnr routes that
  placement (`-o preplaced`, nothing left to place) for the site routing;
  chipsmith reroutes the interconnect; nextpnr writes the FASM and times
  it (`--report`).
  - Synthesis, packer, site routing and FASM (done, 2026-11-01; ADR-0013,
    "7-series synthesis" and "The Zybo flow", above; frames and `.bit` in
    process since 2026-11-02; block RAM 2026-11-02, ADR-0014; DSP48E1
    2026-11-02, ADR-0015; LUT6_2 pairs; timing fitted to Vivado's,
    ADR-0016; on the board 2026-11-06). No MUXF7/F8, by measurement
    ("7-series synthesis", above). The electrostatic placer is
    timing-driven since branch xc7-timing (nets reweighted by criticality
    every 25 iterations). Open: the annealer after it shorter (sha256's
    cold anneal is still about 13 million moves).
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

    Since done: the site pins and LUT pin permutations are chipsmith's
    (`phys.rs`) and so is the FASM writer (`fasm.rs`), both since
    2026-11-01; routing is timing-driven on each node's delay as fitted to
    Vivado's timer (ADR-0016), and since branch xc7-qor2 on every
    connection the placer times ("Timing against Vivado", above). Open:
    LUT pins swapped while routing, as Vivado's router does (on sha256's
    seed 0 placement, Vivado's routing with the pins free made 98.4 MHz
    against 97.0 with chipsmith's pins locked).
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
    weight pulls together. Since done: chipsmith's own packer (`pack.rs`,
    2026-11-01), the 5LUTs and 5FFs in LUT6_2 pairs. Open: a delay model
    with the wires' RC.
- **DE25-Nano** (Agilex 5): no public reverse engineering existed, so
  agilex5-re does it. The Secure Device Manager accepts a fabric image we
  write: on an unprovisioned device it checks neither the design hash nor
  an owner signature, only each section's keyed hash, whose key
  agilex5-re commits (docs/legal.md). Since 2026-11-09, cores from the
  open flow (a5pnr, `a5db rbf hps-first=`) load from the board's own
  Linux through the fpga-manager, after an HPS-first shell build from
  Quartus Pro once per board. A wrong HPS IO hash or keyed hash wedges
  the SDM until phase 1 is reloaded over JTAG, so a loader must check the
  hash before it writes. agilex5-re's database is a flake (`db`, since
  2026-11-12), and synthesis, place and route and assembly are
  chipsmith's (`crates/chipsmith-agilex5`, 2026-11-12). Synthesis writes
  a5synth's netlist for seven designs, blinky to sha_st (`tests/synth.rs`). From a placed and routed
  design file it writes the `.sof` and the core of each of the three
  open-flow designs that loaded on the board, byte for byte, and from
  Quartus' `.sof` Quartus' own core (`tests/image.rs`). Place and route
  writes a5pnr's design file and flop map for eight designs, from blinky
  to sha_st (1,375 ALMs) and the board-tested hpsf_open, with the IOPLL
  settings in Rust instead of a5pnr's Python (`tests/pnr.rs`). Both tests
  are ignored by default: they need agilex5-re's specimens.
  On 2026-11-12 the first core with every step chipsmith's (hpsf_open:
  Verilog through synthesis, place and route and assembly, with the HPS
  shell lines from Quartus' HPS-first build) loaded on the board from
  mercury's Linux: GIC bit 17 (irq0[0] = cnt[23]) at 3.1 Hz and 48 %
  high, bit 20 (cnt[24] & cnt[23]) 26 % high, where the design gives
  2.98 Hz at 50 % and 25 %. Since the same day `chipsmith build` and
  `chipsmith flash --ssh` do it (`examples/de25-irq`, its core the same
  bytes; build 71 s, most of it loading the routing graph twice). Also
  2026-11-12: the board's QSPI holds that HPS-first phase 1 (Quartus'
  `.jic`, factory image backed up first), so after a power cycle the HPS
  boots Linux with no JTAG and no fabric, and `flash --ssh` loads
  examples/de25-irq onto it: bit 17 at 3.0 Hz and 52 % high, bit 20 26 %.
  chipsmith-agilex5 puts any U-Boot SPL into a Quartus phase 1
  (`rbf::phase1::with_spl`): the HPS section is one deflated stream of
  SPL and HPS handoff data, and the rest stays Quartus' bytes. Given
  either of agilex5-re's two Quartus builds of designs/hpsf (GCC 16 and
  Arm GNU 11.2 SPLs), it writes the other byte for byte
  (`tests/phase1.rs`). `jic::jic` packs a phase 1 into the QSPI `.jic`
  Quartus writes (the CMF four times, the rest at 2 MiB, bits reversed,
  CRC-16/X-25): the same bytes as Quartus' `.jic` for both builds, among
  them the one in the board's QSPI. Open: phase 1 built by nixos-fpga
  (SPL from source),
  `[clocks]` and the I/O registers in a DE25 manifest, more HPS inputs
  than f2h_irq0[0] and [3], and the HPS-to-FPGA bridges after a load from
  Linux.

## Manifest

```toml
[project]
name = "blinky"            # build/blinky.rbf
top = "blinky"             # module or entity whose ports are pins
sources = ["src/*.vhd"]    # .v and .sv, or .vhd/.vhdl: one language per design

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

The DE25-Nano's Agilex 5 (`device = "A5EB013BB23BE4SCS"`) boots
HPS-first: Quartus Prime Pro's build of the HPS configures it (phase 1),
and chipsmith builds phase 2, the core, as `build/<name>.core.rbf`. The
SDM takes a core only with phase 1's HPS IO hash, so the manifest names
that phase 1 (`examples/de25-irq`):

```toml
[target]
device = "A5EB013BB23BE4SCS"
hps_shell = "hps-shell.txt"   # phase 1's HPS and I/O lines (agilex5-re tools/hps_shell.sh)
hps_io_hash = "D921CDF7…"     # phase 1's HPS IO hash (quartus_pfg -i), 64 hex digits

[pins]
CLOCK1_50 = { pin = "V16", vccio = "3.3 V" }   # a ball and its bank's voltage
irq0 = "f2h_irq0[0]"                           # an HPS input, f2h_irq0[0] or [3]
```

A ball takes its bank's voltage (`3.3 V`, `1.8 V` or `1.1 V`) and nothing
else; `[clocks]` isn't taken yet (place and route times against its delay
model alone). The database, keys and routing graph come with chipsmith
(agilex5-re's flake), and so do the 15 files of Quartus Prime Pro an image
needs, the SDM firmware among them (docs/legal.md): a DE25-Nano build needs
no Quartus. `chipsmith flash --ssh HOST` loads
the core through the HPS's own Linux (nixos-fpga's `/dev/fpga-load`) and
refuses a core whose HPS IO hash isn't the manifest's: the SDM would wedge
on it. It can't tell which phase 1 the board actually booted.

Phase 1 itself is agilex5-re's `phase1/de25-nano` (the one in the board's
QSPI). `chipsmith agilex5-phase1 BASE SPL --rbf OUT --jic OUT` puts another
U-Boot SPL (`u-boot-spl-dtb.bin`) into it and writes the `.hps.rbf` for a
JTAG load and the QSPI `.jic` quartus_pgm programs; the shell lines and HPS
IO hash stay the same, so cores built for the board still load.

A `[clocks]` table gives the frequency of a clock entering at a pin:

```toml
[clocks]
CLOCK_50 = "50 MHz"        # a port bit with a pin in [pins]
```

On the Zynq each clock becomes an XDC `create_clock` for nextpnr; the
build prints nextpnr's Fmax and slack per clock and fails if one misses
its constraint. On Cyclone II `[clocks]` gives the clocks entering at
pins; a PLL's outputs take their frequency from it, and a clock made by
logic (a divided clock, the JTAG block's TCK) has none. The build prints,
per global clock, the critical path from its registers to its registers,
the estimated slack against its frequency where it has one, and the
longest path between each pair of clocks (not checked: how two clocks
relate is the design's business). It doesn't fail on the slack: the estimate is not sign-off (yosys-synthesized picorv32: 11.26 ns estimated, 10.60 ns in Quartus). The
placer and router stay timing-driven against the critical path itself,
whatever the clock. Without `[clocks]`, nextpnr times against its default
12 MHz and Cyclone II reports no slack.

A DE2 pin, or a whole bus, can be a table with its I/O settings
(`crates/chipsmith-cyclone2/src/pin.rs`):

```toml
[target]
device = "EP2C35F672C6"
unused_pins = "pull-up"    # "tri-stated" (default), "pull-up" or "bus-hold"

[pins]
LEDG = { pins = ["PIN_AE22", "PIN_AF22"], drive = "4 mA", registers = ["output"] }
GPIO_IO = { pin = "PIN_F24", pull = "up", registers = ["input", "output", "output-enable"] }
HEX0 = { pins = ["PIN_AF10", "PIN_AB12"], standard = "3.3-V LVCMOS" }
KEY = { pins = ["PIN_G26", "PIN_N23"], standard = "2.5 V" }
```

- `standard`: `"3.3-V LVTTL"` (the default), `"3.3-V LVCMOS"`, or `"2.5 V"`
  for inputs. The DE2 powers all eight banks at 3.3 V, and Quartus takes
  a 2.5-V input in a 3.3-V bank but nothing at 1.8 or 1.5 V (error
  169029), so `"1.8 V"` and `"1.5 V"` are errors that name the VCCIO they
  would need, and a 2.5-V port that drives is an error once synthesis
  knows its direction.
- `drive`: 3.3-V LVTTL's current, `"4 mA"` to `"24 mA"` (default) in 4 mA
  steps. 3.3-V LVCMOS has its default current only (12 mA on row IOEs,
  24 on column IOEs): cyclone2-re hasn't decoded its other drive codes.
- `pull`: `"up"` (the weak pull-up) or `"bus-hold"`; one key, so a pin
  can't ask for both. Dedicated clock inputs have neither.
- `registers`: the I/O-cell registers synthesis moves the design's
  registers into, any of `"input"`, `"output"`, `"output-enable"`.
  Quartus' `FAST_INPUT_REGISTER` and friends.
- `unused_pins`: what pins the design doesn't use do. Quartus' "as output
  driving ground" isn't offered: on the DE2 it would short every chip
  output wired to an unused FPGA pin.

I/O registers are opt-in, not automatic. Quartus moves registers into
I/O cells on its own only when that helps the timing it is given for the
pins (`OPTIMIZE_IOC_REGISTER_PLACEMENT_FOR_TIMING`); chipsmith has no pin
timing constraints, so it can't tell. A register in the I/O cell fixes the
pin's clock-to-output or setup time but moves the register to the chip's
edge, which lengthens the path from the logic that feeds it; only the
designer knows which side matters. An input register is the one sampling
the pin, moved. An output or OE register is a copy of the one driving the
pin's data or enable, so a register several pins share (a bus's enable)
gets one copy per pin, and the original stays if logic reads it. Its
power-up value goes with it.

What goes into the cell is what Quartus puts there (cyclone2-re README,
"I/O-cell register controls"; 35 Quartus designs): a clock enable of
either polarity becomes the cell's clock enable, an asynchronous clear or
preset of either polarity the cell's reset (which restores the power-up
value, so a preset powers up 1), a falling edge the cell's clock
inversion. An inverter in front of one folds into the cell, as quartus_map
folds it. The cell shares some of these: one asynchronous reset for its
three registers, one clock (and edge) for the output and OE registers, one
enable line for those two, which each uses or not. Two different enables
there become logic on D (Quartus leaves the OE register in the core
instead), different resets or edges an error.

A synchronous reset, which synthesis finds in ordinary RTL (`q <= c ? x :
0`, a state machine that holds a value), stays logic, as Quartus keeps
it: the I/O cell's register samples the original's next state, a mux on
its D that reads the original's Q, so the two hold the same value on every
edge (`io_registers_with_enables_and_resets` and
`io_registers_with_cell_controls` prove them against the RTL and
co-simulate them; `tests/ioe_controls.rs` takes them through the whole
flow). The original stays, and the mux costs a LUT input or a LUT per pin.
An input register can't have one (its D is the pad; Quartus leaves it in
the core too), nor can any register have an asynchronous load or both an
asynchronous set and clear, or an initial value its asynchronous reset
contradicts. A request synthesis can't satisfy is an error that says why.
The timing report then times the paths into and out of those registers; a
pin without them stays untimed.

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
| `crates/chipsmith-xc7` | 7-series: synthesis, packing, prjxray-db import, placement, delays, site routing, FASM, bitstream decoding, Zynq `.bin` |
| `crates/chipsmith-jtag` | cables (USB-Blaster, FT2232), SVF player, 7-series JTAG loader |

Cargo examples sit with their crate and run from the root by name, e.g.
`cargo run --release --example xc7_user1` (`chipsmith-jtag`).

A bare `cargo build` or `cargo test` at the root builds the open crates
only (`default-members`); `--workspace` adds the `crates/vendor-*` ones
below.

## chipsmith-vendor

The Quartus toolchain manager that used to be chipsmith is still here,
renamed `chipsmith-vendor`, in `crates/vendor-*`. It installs, NixOS-patches
and runs the vendor tools (Quartus II 13.0sp1, Quartus Prime, Quartus Prime
Pro) that the open flow is tested and reverse-engineered against. The open
flow never runs it.

```
nix build .#chipsmith-vendor
chipsmith-vendor install --backend quartus-ii-13 13.0sp1
chipsmith-vendor run --backend quartus-pro --version 26.1.1 quartus_sh -- --version
```

`CHIPSMITH_INSTALL_ROOT` picks where the installs live. Its own manifest
format, examples, NixOS module (`nixosModules.chipsmith-vendor`) and
`vendorLib.buildProject` are documented in
[crates/vendor-cli/README.md](crates/vendor-cli/README.md), with its
glossary in [CONTEXT.md](crates/vendor-cli/CONTEXT.md) and notes for
working on it in [CLAUDE.md](crates/vendor-cli/CLAUDE.md).

## License

MIT. Where chipsmith touches vendor software and data, and why we think
that's allowed: docs/legal.md.
