# Finishing the Zybo and the DE25-Nano

The open items the README lists for each board, as one checklist, worked
top to bottom: the Zybo first, then the DE25-Nano. An item is done when its
check has run (on the board, or against Vivado/Quartus where the board can't
tell), the README says so, and this file ticks it.

Out of scope: boards we don't have (Zybo Z7-10/-20 pin maps, the xc7z020),
and the DE2's peripherals (README, Roadmap).

## Zybo (xc7z010)

- [x] **Z1 I/O standards** (2026-11-17). Per pin `standard` (LVCMOS33,
  LVTTL, LVCMOS25/18/15/12), `drive`, `slew` and `pull` (up, down,
  keeper) in `[pins]`, one VCCO per bank. Every I/O feature matches
  Vivado's bitstream of the same settings at all five bank voltages
  (`bench/zybo-io/sweep.py`, but PUDC_B), and examples/zybo-io's pull-up,
  pull-down, keepers and LVTTL 24 mA loopback hold on the board.
- [x] **Z2 I/O-tile registers** (2026-11-17). `registers` in `[pins]`
  moves a pin's input, output and output-enable registers into its
  ILOGICE3/OLOGICE3, every slice register shape. Site features match
  Vivado's on 34 of 34 pins (`bench/zybo-ioreg/sweep.py`), both
  bitstreams co-simulate (`io_registers`), and examples/zybo-ioreg's two
  pins read back 697,668 checks with 0 wrong on the board.
- [x] **Z3 Wide muxes and shift registers** (2026-11-17). Shift
  registers: SRLC32Es from synthesis through pack, place, FASM and the
  decoder; Vivado's and chipsmith's bitstreams co-simulate, and
  examples/zybo-srl runs 55 million checks with 0 wrong on the board.
  Wide muxes: a design's own MUXF7/MUXF8 build (lowered to LUTs); slice
  F7/F8 mapping measured at a fifth of a 16:1-64:1 multiplexer's LUTs and
  nothing on the examples, left out until a design needs it (README).
- [x] **Z4 Latches** (2026-11-17). LDCE/LDPE from RTL latches and a
  design's own, resets and gate enables found in the front end's enable;
  Vivado's and chipsmith's bitstreams co-simulate, and examples/zybo-latch
  runs 60 million checks with 0 wrong on the board.
Z5-Z9 and slice F7/F8 mapping moved to "Cool stuff to do eventually"
(2026-11-17): the Zybo does what its designs need without them.

## DE25-Nano (A5EB013BB23BE4SCS)

- [x] **D1 `[clocks]`** (2026-11-17). A DE25 manifest takes clock
  frequencies; the build prints each clock's Fmax and slack by the delay
  model (named by its input port) and fails on a miss: de25-lw at 50 MHz
  has 17.9 ns of slack, at 600 MHz the build fails. Cores are unchanged
  byte for byte.
- [x] **D2 Bursts on the lightweight bridge** (2026-11-17).
  examples/de25-burst's memory, written and read from Linux with 32-, 64-
  and 128-bit accesses and memcpy (bursts of up to 4 beats written, 8
  read), every word right; a core with the burst address held fails.
- [ ] **D3 I/O registers** in a DE25 manifest.
- [ ] **D4 The other bridges.** HPS-to-FPGA (the full 128-bit bridge),
  FPGA-to-HPS and FPGA-to-SDRAM, each named by signal like the lightweight
  bridge, each with a board test.
- [ ] **D5 Router speed.** facet-hdl's hello under 31 s, with the board as
  the check since faster routes leave a5pnr's tests.
- [ ] **D6 Which phase 1 booted.** `flash --ssh` reads the board's HPS IO
  hash (or an equivalent) before loading, instead of trusting the manifest.

## Cool stuff to do eventually

Not needed to call a board finished; worth doing when a design wants it.

- **Z5 DSP48E1 pre-adder and ALU modes.** (A+D)*B and multiply-
  accumulate in the DSP, as Vivado infers them. Done when the shape bench
  co-simulates through the bitstream and the board runs it.
- **Z6 The PS7's other ports.** Timing arcs for every PS7 fabric port
  Vivado times (GP1, the S_AXI_GP/HP/ACP ports, the fabric clocks and
  resets), and an HP-port design on the board: the fabric writes DDR
  through S_AXI_HP0 and Linux reads it back.
- **Z7 Clock skew.** The STA takes each clock's insertion delay to
  every sink (BUFG, the clock rows, the PS7's clock pin), against Vivado's
  report on the same routing.
- **Z8 LUT pin swapping while routing**, as Vivado's router does.
  Done when sha256 gains by Vivado's timer over five seeds.
- **Z9 The owed board runs.** The block RAM and multiplier shape
  benches (README tables) run on the board through a JTAG selftest.
- **Slice MUXF7/MUXF8 mapping**: a fifth of a 16:1-64:1 multiplexer's
  LUTs and a LUT level (README, "7-series synthesis"), nothing on the
  examples; needs a placement object of four LUTs and the muxes in one
  slice.
