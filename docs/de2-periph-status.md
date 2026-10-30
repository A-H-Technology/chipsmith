# DE2 peripherals: status (2026-10-30)

Before this work every DE2 example drove only LEDs, switches, JTAG, M4K
blocks or multipliers. This note covers the SDRAM, the SRAM, the VGA DAC
and examples/de2-io on the board, and the gaps found along the way.

Board checks read the design's USER1 register over JTAG with
`cargo run --release -p chipsmith-cyclone2 --example de2_user1 -- --words N`.
The one camera (b4-cam-http on eros) showed a blurry surface, not the
board, so nothing here was checked by looking at LEDs or a monitor.

## What works

**PLL clock to a pin.** A PLL clock leaves the chip only on its PLL's
clock output pin: counter 2 goes through a clock control of its own to
`IO_RAW_DATAOUT` and the pin's output. PLL_1's pin is PIN_AA7, the DE2's
DRAM_CLK; PLL_2 uses F21, PLL_3 E5 and PLL_4 V21. Quartus had no other
path, and neither did cyclone2-re's chip database (its fuzz design p12 was
the one PLL design it couldn't assemble). The fix:

- cyclone2-re: four `PLLOUT` bels and the missing `IO_RAW_DATAOUT` → pin
  pip in the chip database, `cram quartus-export` and `cram roundtrip`
  support.
- chipsmith: the packer, the placer and the IOE bit `IO_CORE_PLL_SELECT`.
- How it was found: the residual of a Quartus design (PLL_1 c2 to AA7)
  that cram assembled without that output was 4 bits: that IOE bit, the
  data inverter, and 2 lock-window bits that belong to the negative-phase
  gap below.

chipsmith's build of the same thing is Quartus byte-identical. Other DE2
examples (de2-pll-counter, de2-jtag-counter, m4k-selftest, mult-selftest,
de2-io) build to the same RBFs on the new chip database.

**examples/sdram-selftest.** The 8 MB IS42S16400 SDRAM:

- Clocks: a 100 MHz controller on PLL_1 CLK0, and DRAM_CLK from CLK2 at
  +8.5 ns.
- Pins: every non-constant SDRAM pin goes through its I/O-cell
  registers.
- Bursts: 8-word bursts with auto precharge, CAS latency 3, a refresh
  every 7 µs.
- Test: each pass writes all 4M words with a 32-bit LFSR seeded from the
  pass number, then reads them back and compares.
- Size: 810 LEs since the controller's registers drive the pins
  straight (below); 776 with the extra register stage it had first.
  Quartus byte-identical; chipsmith estimates Fmax at 154.2 MHz (152.6
  before).

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
- Size: 606 LEs, Fmax estimated at 175.2 MHz. Quartus byte-identical.
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

- Clocks: PLL_1 makes the 25 MHz pixel clock and 50 MHz.
- DAC pins: all through their I/O-cell registers, straight from the
  pixel logic since gap 2 below was fixed (519 LEs).
- VGA_CLK: made by a register (see the gaps below).
- Quartus byte-identical.

On the board, USER1 counts the pixel clock at 25.0000 MHz (0.500000 times
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
- Still errors, since every `registers` entry is an explicit request (the
  manifest has no "if possible"): an asynchronous reset or load and a
  falling-edge clock (a mux on D acts only on an edge; the I/O cell's
  clear isn't modelled). The error now says why and what to write.
- Input registers still must be plain. The same trick isn't the same
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

## Gaps, ranked

1. **A global clock as data.** A PLL clock (or any global clock) can feed
   a pin only on its PLL_OUT path. It can't drive an ordinary pin or a
   LUT input; chipsmith rejects it at pack time. Terasic's DE2 designs
   write `assign VGA_CLK = pll_clk` (and likewise the audio codec's
   AUD_XCK), and those don't build. vga-pattern works around it with a
   register clocked at twice the rate. How Quartus routes a global into
   the core or onto a non-PLL pin is not reverse-engineered; that's the
   first RE task for "Terasic designs build unchanged".
2. **Negative PLL phase shifts.** They are an error now; before, they
   wrapped to 4294964296 ps. Quartus makes them by delaying the feedback
   counter (`m_initial` > 1) and every output with it. A design asks for
   the period minus the shift instead (sdram-selftest: +8.5 ns, not
   -1.5 ns). The same Quartus designs pick a lock window the m sweep
   doesn't predict (m 20, `m_initial` 4: 11, not 15); those are the 2
   bits cram's roundtrip still misses on its own PLL_1-to-AA7 design.
3. **Observation.** No camera view of the board or a monitor, so VGA
   output and every LED-only example remain unobserved; JTAG readback is
   the only check. Boundary-scan SAMPLE would read every pin's state
   without a camera, but the EP2C35F672 BSDL isn't in the Quartus
   install, and bsdl.info needs a browser session.
4. **Input registers with an enable or reset.** An I/O cell's input
   register must still be a plain one (see "Fixed" above for why the
   output-side trick isn't used); a design samples the pin first. The fix
   is the IOE register's clock enable and clear in cyclone2-re (phase 5),
   which also lets Quartus-style asynchronous clears into I/O cells.
5. **Untested DE2 peripherals:** the 4 MB flash, the
   WM8731 audio codec (I2C plus I2S; its master clock hits gap 1), the
   ADV7181 TV decoder, the ISP1362 USB, the DM9000A Ethernet, the SD
   card, PS/2, IrDA and the 16x2 LCD.
6. **PLL_4** has never been seen in a Quartus design. E5, F21 and V21
   are inferred from the routing database alone.
