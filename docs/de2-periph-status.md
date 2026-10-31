# DE2 peripherals: status (2026-10-31)

Before this work every DE2 example drove only LEDs, switches, JTAG, M4K
blocks or multipliers. This note covers the SDRAM, the SRAM, the VGA DAC
and examples/de2-io on the board, and the gaps found along the way.

Board checks read the design's USER1 register over JTAG with
`cargo run --release -p chipsmith-cyclone2 --example de2_user1 -- --words N`.
The one camera (b4-cam-http on eros) showed a blurry surface, not the
board, so nothing here was checked by looking at LEDs or a monitor.

## What works

**PLL clock to its clock output pin.** A PLL's dedicated way off the
chip is its clock output pin: counter 2 goes through a clock control of
its own to `IO_RAW_DATAOUT` and the pin's output. PLL_1's pin is PIN_AA7,
the DE2's DRAM_CLK; PLL_2 uses F21, PLL_3 E5 and PLL_4 V21. cyclone2-re's
chip database didn't have that path (its fuzz design p12 was the one PLL
design it couldn't assemble). Any other pin takes the clock as data on
its global ("Fixed: a global clock as data" below). Since branch
de2-ioece Quartus has placed each of F21, E5 and V21 itself ("Fixed:
PLL_4" below). The fix:

- cyclone2-re: four `PLLOUT` bels and the missing `IO_RAW_DATAOUT` → pin
  pip in the chip database, `cram quartus-export` and `cram roundtrip`
  support.
- chipsmith: the packer, the placer and the IOE bit `IO_CORE_PLL_SELECT`.
- How it was found: the residual of a Quartus design (PLL_1 c2 to AA7)
  that cram assembled without that output was 4 bits: that IOE bit, the
  data inverter, and 2 lock-window bits (the lock window goes by the
  phase detector's period; "Fixed: negative PLL phase shifts" below).

chipsmith's build of the same thing is Quartus byte-identical. Other DE2
examples (de2-pll-counter, de2-jtag-counter, m4k-selftest, mult-selftest,
de2-io) build to the same RBFs on the new chip database.

**examples/sdram-selftest.** The 8 MB IS42S16400 SDRAM:

- Clocks: a 100 MHz controller on PLL_1 CLK0, and DRAM_CLK from CLK2 at
  -1.5 ns since 2026-10-31, the same clock as the +8.5 ns all the board
  runs below used (not yet on the board).
- Pins: every non-constant SDRAM pin goes through its I/O-cell
  registers.
- Bursts: 8-word bursts with auto precharge, CAS latency 3, a refresh
  every 7 µs.
- Test: each pass writes all 4M words with a 32-bit LFSR seeded from the
  pass number, then reads them back and compares.
- Size: 777 LEs since branch de2-ioece, the controller's enables the I/O
  cells' own (Fmax estimate 163.3 MHz; not yet on the board); 810 with
  them as logic on D, the build the board ran (154.2 MHz); 776 with the
  extra register stage it had first. Quartus byte-identical.

On the board (loaded over JTAG):

| DRAM_CLK shift | passes | errors | notes |
|---|---|---|---|
| +7.0 ns (first build) | 1,823 | 0 | 304 s, 5.88 passes/s |
| +8.5 ns (the example) | 6,989 | 0 | 1,189 s, 5.88 passes/s |
| +8.5 ns, no extra stage, `RD_LAT` 3 | 5,145 | 0 | 875 s, 5.88 passes/s |
| +2.0, +4.0, +5.5 ns | 153 each | about every word wrong | 25 s each |
| +6.0, +6.5, +8.5, +9.5, +10.5 ns | 153 each | 0 | 25 s each |
| +11.5 ns | 153 | 334 M | |
| +12.5 ns | 153 | about every word wrong | |

So with a read latency of 4 FPGA clocks (the first build's, with the
extra stage), the 0-error window runs from +6
to +10.5 ns, and +8.5 ns sits in its middle. Throughput is 5.88 passes a
second: 98.7 MB/s of SDRAM traffic (8 MiB written and 8 MiB read per
pass). That is half of the 200 MB/s a 16-bit bus carries at 100 MHz,
because the controller spends 16 clocks on each 8-word burst. That's a
choice in this controller, not a toolchain limit.

The read latency (`RD_LAT`) was measured, not computed. A build with four
checkers side by side read 0 errors at latency 4 and every word wrong at
5, 6 and 7. A snapshot of the input registers showed burst 0's words 0-7
at the expected edges. Without the extra stage the READ reaches the pins
a clock earlier and the latency is 3: 0 errors over 5,145 passes.

**examples/sram-selftest.** The 512 KB IS61LV25616AL-10 asynchronous
SRAM:

- Clock: a 100 MHz controller on PLL_1 CLK0.
- Pins: address, WE#, OE# and the data bus (input, output and OE) through
  their I/O-cell registers, the registers' enables and resets as logic on
  D (the fix below); CE#, UB# and LB# tied low. Pins from Terasic's
  DE2_pin_assignments.csv.
- Writes: 3 clocks, WE#-controlled with OE# high: address and data out,
  WE# low for one clock (10 ns; the part wants 8), WE# high with both
  held; they change a clock later, not on WE#'s rising edge.
- Reads: 2 clocks. The input registers sample on the edge that puts out
  the next address (10 ns access, 2.5 ns output hold), so `RD_LAT` is 2.
- Test: as sdram-selftest's, all 256K words a pass.
- Size: 539 LEs since branch de2-ioece (the enables the I/O cells' own;
  Fmax estimate 167.9 MHz; not yet on the board), 606 and 175.2 MHz with
  them as logic, the build the board ran. Quartus byte-identical.
- Simulated first (iverilog, a behavioural SRAM): 0 errors over 2
  passes, and one corrupted word counted once.

On the board (loaded over JTAG):

| build | passes | errors | notes |
|---|---|---|---|
| `RD_LAT` 2, 16-bit pass counter | 46,680 | 0 | 611 s, 76.4 passes/s |
| `RD_LAT` 2 (the example, 32-bit pass counter) | not read | | loaded; by the next read 16 min later the board had gone dark (below) |
| `RD_LAT` 3 | 862 | 226,208,709 | 10 s; nearly every word |

The first row's count is exact: its 16-bit pass counter wraps at 65,536.
The committed example counts passes in 32 bits; its RBF is the one
loaded in the second row. That run was never read: 16 minutes after the
load, USER1 read all zeros, and a fresh load failed with every TDO bit 0
(the USB-Blaster still enumerated). That is a board without power or
with its JTAG cable off. Nobody was at the board, and why it went dark
is unknown. The SRAM design drives the bus only while OE# is high (no
fight in simulation), and its first build ran 611 s without trouble.

76.4 passes a second is 80.1 MB/s (1 MiB of traffic a pass). The bus
would carry 200 MB/s at 100 MHz; this controller spends 3 clocks a write
and 2 a read.

**examples/vga-pattern.** A 640x480 60 Hz pattern: colour bars, four
ramps and a white border.

- Clocks: PLL_1 makes the 25 MHz pixel clock (and made 50 MHz for
  VGA_CLK's register until branch de2-gclkdata).
- DAC pins: all through their I/O-cell registers, straight from the
  pixel logic since the I/O-register fix below.
- VGA_CLK: `assign VGA_CLK = ~pclk`, the PLL clock itself on its global
  ("Fixed: a global clock as data" below), 516 LEs; before that a
  register on the 50 MHz clock made it (519 LEs).
- Quartus byte-identical.

On the board (the register-made VGA_CLK build; the direct one has not
been on the board), USER1 counts the pixel clock at 25.0000 MHz (0.500000 times
CLOCK_50) and 59 frames a second (59.52 expected; the reading counts whole
frames per second); the build without the extra stage read 25.0000 MHz
and 60. **Nobody has seen the picture.** No monitor was in
view of the camera, so the timing generator is shown to run, but the sync
polarity, the DAC timing and the picture are not.

**examples/de2-io.** It loads and configures over JTAG. A copy of it
with a USER1 readback of its inputs (not committed) read the following,
which is what the board should give:

- SW[3:0] and SW[17:13] through their input registers: 0 (all switches
  down).
- Both KEYs (2.5-V inputs): 1 (released).
- The weak-pull-up GPIO_0 inputs: 1.
- The bus-hold inputs: 1.
- GPIO_0[4] through its input register: 1 (released and pulled up).

Pressing keys and flipping switches needs someone at the board. Its
outputs (LED drive strengths, the LED that powers up lit, HEX0) can't be
seen without a camera on the board.

## Fixed: the I/O cell's own register controls (was gap 2)

An input register had to be plain, and an output or OE register's enable
and synchronous reset were logic on its D ("Fixed: I/O-cell registers with
an enable or reset" below); an asynchronous reset or a falling edge was an
error. Since branch de2-ioece chipsmith puts them into the I/O cell as
Quartus does. cyclone2-re found how (its README, "I/O-cell register
controls"; 35 Quartus designs in `tools/ioce_jobs.txt`):

- What the cell has: per register a clock enable (the input register its
  own, the output and OE registers one line between them, each using it
  or tied off), one asynchronous reset for all three that returns a
  register to its power-up value (a preset is a clear plus power-up 1), a
  synchronous clear, and clock inversion per side. Each control is a LAB
  line into a source mux (`IO_PIA_TO_INREG_CE`, `IO_PIA_TO_OEREG_CE`,
  `IO_PIA_TO_CLR`, `IO_PIA_TO_SCLR`) with an inverter; the clear is
  active low in the cell. 34 of the 35 designs assemble byte-identically
  in `cram roundtrip`; the miss is a core LAB with two clears.
- What Quartus packs: enables, clears, presets and falling edges on all
  three registers. A synchronous reset it makes logic on an output or OE
  register's D and leaves an input register with one in the core. It also
  leaves an input register in the core beside a different reset, and an OE
  register beside an output register with another edge or enable.
- chipsmith (`synth/io.rs`, `pack/mod.rs`, `bitstream/io.rs`): the same.
  An enable or reset goes in natively, an inverter in front folds into
  the cell. A synchronous reset stays logic on D (two different output and
  OE enables become logic too, where Quartus leaves the OE register out).
  The chip database gives each user IOE bel `INCLKENA`, `OUTCLKENA` and
  `ARESET` pins; cyclone2-re's Quartus export writes them (and switches
  off Quartus' automatic globals, which moved a pin driving clears onto a
  global and left the routing dangling).
- Still errors, as Quartus won't pack them: an input register with a
  synchronous reset, an asynchronous load, both an asynchronous set and
  clear, different resets in one cell, output and OE registers on
  different edges, and an initial value its asynchronous reset
  contradicts (Quartus drops the initial value).
- Proof: `io_registers_with_cell_controls` (`tests/synth.rs`) proves input
  and output registers with every control against the RTL (26 `$equiv`)
  and co-simulates them; `io_registers_tri_state_cell_controls`
  co-simulates tri-state pins (their held values aren't visible to the
  proof); `tests/ioe_controls.rs` builds a design through the whole flow
  and checks what the bitstream writes, a bidir's three registers on one
  reset included; `io_registers_that_cant_be_are_errors` the errors. An
  inverted reset in the cell builder fails the proof; dropping the
  inverter folding fails the flow test. examples/de2-ioregs (enables,
  clear, preset, active-low enable and clear, falling edge, the bidir) is
  Quartus byte-identical.
- RBFs: of the 33 other DE2 examples (picorv32x24 not rebuilt), two change
  through the packing: sdram-selftest (810 to 777 LEs) and sram-selftest
  (606 to 539), whose controllers' enables now are the cells'. 12 more
  change through the chip database alone (open's chipsmith on the new
  database builds them as this branch does): crc32, de2-io,
  de2-jtag-counter, de2-pll-counter, femtorv32, m4k-bytes, m4k-tdp, ram,
  ram-leds, serv, sha256, vga-pattern. The new pins' pips sit on every
  IOE tile's LAB lines, and the router breaks ties between equal-cost
  routes differently with them in the graph. All 14 changed builds are
  Quartus byte-identical. The other 19 (blinky-vhdl included) build to
  the same RBF.

## Fixed: PLL_4 (was gap 4)

PLL_4 had never been in a Quartus design; its pins came from the routing
database alone. Since branch de2-ioece, nine Quartus designs (cyclone2-re
`tools/clk_jobs.txt` q4a-q4f, q1x-q3x) confirm them:

- PLL_4 takes the bottom edge's clock pins: AE14, AF14, AD13 and AC13
  (the DE2's SW[3]-SW[6]), each tried; with no location given, Quartus
  puts a PLL on AE14 onto PLL_4 itself.
- Left to place `PLL_OUT` itself, Quartus puts each PLL's counter 2 on
  AA7 (PLL_1), F21 (PLL_2), E5 (PLL_3) and V21 (PLL_4).
- All nine assemble byte-identically in `cram roundtrip` with no change to
  the database. chipsmith builds a PLL from AE14 with CLK2 on V21 on
  PLL_4 through its own clock output, Quartus byte-identical
  (`pll_4_from_a_bottom_clock_pin_to_its_own_pin` in `tests/gclk_data.rs`).

## Fixed: a global clock as data (was gap 1)

Terasic's DE2 designs write `assign VGA_CLK = pll_clk` (and likewise the
audio codec's AUD_XCK). A PLL clock reached a pin only on its PLL<n>_OUT
path, and anything else on it was an error at pack time. Since branch
de2-gclkdata it builds, the way Quartus builds it. cyclone2-re found how
(its README, "Global clocks as data"; 15 Quartus designs, gd1-gd15 in
`tools/clk_jobs.txt`):

- What Quartus does: a global leaves the clock network through the row
  clock buffers onto LAB lines (`BLK_CLK_BUF` → `LOCAL_INTERCONNECT`),
  and from there to an LE input or an I/O cell's data like any signal. A
  top or bottom pin is reached through the I/O row's clock spine
  (`TB_IO_CLK`), a side pin through a row clock. No dedicated mux, no
  bits beyond the routing edges'. The chip database had these edges
  already: 14 of the 15 designs assembled byte-identically in `cram
  roundtrip` with no change (gd4's 2 bits are a LAB clock from a LAB
  line, an older not-covered case).
- A PLL output used only as data still gets a clock control and a global
  (gd11). A dedicated clock pin used as data doesn't go through its
  global: its clock buffer drives R24 and C4 lines itself (gd1, gd6,
  gd10, gd14).
- chipsmith (`pack/mod.rs`, `place.rs`): every load of a PLL output goes
  on its global, except counter 2 on its PLL's external clock output;
  a PLL output gets a clock control if anything uses it. Counter 2 alone
  on one plain pin is still packed onto the external clock output, as
  before; whether that pin is the PLL's PLL<n>_OUT pin only placement
  knows, so the placer moves an ordinary pin onto the clock's global
  (the external output cell becomes the clock control, or the pin joins
  a global the clock has already). A clock pin's other loads keep their
  own routing, as Quartus routes them. The router needed nothing: the
  chip database has every graph edge as a pip.
- Proof: vga-pattern with `assign VGA_CLK = ~pclk` (PIN_B8) is Quartus
  byte-identical, and chipsmith routed it as Quartus routes gd7 (the
  same `TB_IO_CLK` → `BLK_CLK_BUF` X17 Y36 → line → `IO_DATAOUT`). So
  are three test designs (`tests/gclk_data.rs`): c0 onto B8 and into a
  LUT with counter 2 alone on A5; counter 2 on A5 beside a register on
  it; counter 2 on AA7, which keeps its external output. Those tests
  build each through the whole flow and check the route leaves the
  global through a row clock buffer; they fail on the old packer (the
  error at pack time) and placer. Pack tests check the netlist rules.
- Not the same as Quartus: a clock from the core (a divided clock, TCK)
  used as data keeps its own routing for the data; Quartus puts those
  loads on the global too (gd12). Either is legal; changing it would
  change every such example's RBF. And counter 2 on its PLL<n>_OUT pin
  and on another pin goes all on the global, where Quartus also uses the
  external output for the PLL<n>_OUT pin (gd15).
- RBFs: vga-pattern's changed with its source. The other 31
  DE2 examples build to the same RBF as on branch open (built on both;
  picorv32x24, 26k LUTs and no PLL, not rebuilt): none puts a PLL clock
  anywhere but on registers or its PLL<n>_OUT pin.

## Fixed: I/O-cell registers with an enable or reset (was gap 2)

Synthesis finds synchronous resets and clock enables in ordinary RTL
(`q <= c ? x : 0`, a state machine that holds a value), and the
manifest's `registers` used to fail on them ("can't go into the I/O cell:
it has a reset"), so sdram-selftest and vga-pattern had an extra plain
register stage in front of the pins. Since branch de2-ioreg:

- An output or OE register is a copy of the LE register driving the pin.
  The copy is now a plain register sampling the original's next state:
  `srst ? value : en ? d : q`, in whichever order the register has them
  (`$sdffe` or `$sdffce`), either polarity, with q the original's Q. The
  original stays, so the two hold the same value on every edge given the
  same power-up value, which the copy takes from the original. No RE: the
  logic is LUTs in the core (`synth/io.rs`, `next_state`).
- Proof: `io_registers_with_enables_and_resets` (chipsmith-cyclone2's
  `tests/synth.rs`) synthesizes every enable/reset order, both polarities,
  power-up 0 and 1, an OE register shared by two pins and a state
  machine's outputs, and proves the result against the RTL with the I/O
  registers in the pin buffers' models (28 `$equiv`), then co-simulates
  10,000 cycles. Swapping the enable's polarity in `next_state` fails the
  proof.
- Still errors then (until branch de2-ioece, above), since every
  `registers` entry is an explicit request (the
  manifest has no "if possible"): an asynchronous reset or load and a
  falling-edge clock (a mux on D acts only on an edge; the I/O cell's
  clear isn't modelled). The error now says why and what to write.
- Input registers still had to be plain (until branch de2-ioece, above).
  The same trick isn't the same
  there: the I/O register would sample the pin every edge, and the enable
  and reset would have to be retimed over it (the controls registered
  too, the held value in an LE register, a LUT after the pin's register).
  That is sound (the same recurrence, with the extra registers powering
  up inactive) but costs three LEs a pin, puts a LUT between the pin's
  register and the logic, and the equivalence proof by register matching
  doesn't cover it. Not done; the error says to sample the pin in a plain
  register and put the enable or reset on the next one. The real fix is
  the IOE's clock enable and clear, which cyclone2-re hasn't decoded
  (phase 5: "register clear/enable").
- Cost: sdram-selftest is 810 LEs without the stage, 776 with it: the
  controller's registers stay and each pin gets a next-state LUT.
- RBFs: only the three examples with `registers` could change. de2-io's
  is the same as before (its registers are plain); sdram-selftest's and
  vga-pattern's changed with their sources, and both are Quartus
  byte-identical.

## Fixed: negative PLL phase shifts (was gap 2)

A negative shift was an error (and before that wrapped to 4294964296
ps); a design asked for the period minus the shift. Since branch
de2-pllneg, chipsmith builds it as Quartus does. Found by compiling 46
small PLL designs with Quartus and reading their post-fit atoms
(cyclone2-re `tools/clk_jobs.txt`, "Phase shifts"; its README, "PLLs"):

- Encoding: a shift is a whole number of eighths of the counters' clock
  period, whole cycles into the counter's preset and the rest onto a VCO
  tap. Quartus never folds a negative shift into the period. It delays
  the feedback counter (`m_initial`, `m_ph`) by the most negative
  output's shift and every output by the same, so the outputs lead the
  input. -3 ns at 100 MHz is m 20 with `m_initial` 4; sdram-selftest's
  -1.5 ns next to c0 at 0 is `m_initial` 2, `m_ph` 4 and c0 at preset 2,
  tap 4; -12 ns is 12 cycles of feedback delay.
- Picking the counters: post-scale 1 over 2, then the smallest summed
  phase error, then the VCO nearest 800 MHz, then the smallest n. So a
  shift moves the VCO: +8.5 ns at 100 MHz is now m 20 (exact) where the
  old solver took m 16 and made 8437.5 ps; +700 ps is m 18 and 694 ps.
  With post-scale 2 the VCO is 600-1000 MHz, not 500-1000.
- Limits: a preset holds 16 cycles plus a tap. Past that at every
  post-scale-1 VCO, Quartus clamps the shift (-40 ns at 100 MHz made
  -30 ns, warning 15559). chipsmith refuses it with an error instead.
- Bits: the lock window goes by the phase detector's period (the input's
  times n): 3 to 5 ns, 4 to 7.5, 7 to 10, 8 to 12.5, 11 to 20, 15 above.
  The old m sweep ran the VCO at 800 MHz, where m and that period move
  together; designs at the same period with m 10, 18 and 20 split them.
  With post-scale 2, a counter on tap t also sets VCO tap 2t mod 8, and
  2t - 7 when 2t >= 8.
- Proof: the solver picks Quartus' counters for all 34 altpll designs
  (`phase_shifts_as_quartus_fits_them` in `pll.rs`, -20 to +20 ns, 10 to
  100 MHz, one to three outputs); all 46 designs assemble byte-identically
  in `cram roundtrip`. sdram-selftest at -1.5 ns, vga-pattern and
  de2-pll-counter built by chipsmith are Quartus byte-identical.
- Not checked: a VCO that clamps could beat one that doesn't in Quartus'
  scoring for a request near the preset limit; chipsmith never picks
  one that clamps (never seen). That post-scale 1 wins over a smaller
  phase error at post-scale 2 is seen once (that -40 ns design).
- RBFs: sdram-selftest's changed (-1.5 ns, m 20). vga-pattern's is the
  same as de2-ioreg's build. de2-pll-counter has no shift, so its
  counters and lock window (20 ns: 11, as m 16 gave) are the same by
  construction; no earlier build on this chip database was at hand to
  compare.

## Gaps, ranked

1. **Observation.** No camera view of the board or a monitor, so VGA
   output and every LED-only example remain unobserved; JTAG readback is
   the only check. Boundary-scan SAMPLE would read every pin's state
   without a camera, but the EP2C35F672 BSDL isn't in the Quartus
   install, and bsdl.info needs a browser session.
2. **Untested DE2 peripherals:** the 4 MB flash, the
   WM8731 audio codec (I2C plus I2S; its master clock, AUD_XCK, builds
   now), the
   ADV7181 TV decoder, the ISP1362 USB, the DM9000A Ethernet, the SD
   card, PS/2, IrDA and the 16x2 LCD.
3. **An active-low asynchronous reset on LE registers.** `always @(posedge
   clk or negedge rst_n)` on a counter doesn't fit: each register gets an
   inverter of its own for its clear (15 clear nets for 15 registers in a
   draft of examples/de2-ioregs, on branch open too), and a carry chain's
   LEs can't share a LAB then. cyclone2-re hasn't decoded the LAB's clear
   inversion. Write the reset active high (`wire rst = ~rst_n`) meanwhile.
   An I/O cell's register takes an active-low reset fine.
4. **Not yet on the board:** examples/de2-ioregs, and sdram-selftest and
   sram-selftest as rebuilt with the cells' enables (the DE2 is off).
