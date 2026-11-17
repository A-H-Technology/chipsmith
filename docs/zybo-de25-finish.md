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
- [ ] **Z4 Latches.** LDCE/LDPE (a slice register in latch mode). Done like
  Z3.
- [ ] **Z5 DSP48E1 pre-adder and ALU modes.** (A+D)*B and multiply-
  accumulate in the DSP, as Vivado infers them. Done when the shape bench
  co-simulates through the bitstream and the board runs it.
- [ ] **Z6 The PS7's other ports.** Timing arcs for every PS7 fabric port
  Vivado times (GP1, the S_AXI_GP/HP/ACP ports, the fabric clocks and
  resets), and an HP-port design on the board: the fabric writes DDR
  through S_AXI_HP0 and Linux reads it back.
- [ ] **Z7 Clock skew.** The STA takes each clock's insertion delay to
  every sink (BUFG, the clock rows, the PS7's clock pin), against Vivado's
  report on the same routing.
- [ ] **Z8 LUT pin swapping while routing**, as Vivado's router does.
  Done when sha256 gains by Vivado's timer over five seeds.
- [ ] **Z9 The owed board runs.** The block RAM and multiplier shape
  benches (README tables) run on the board through a JTAG selftest.

## DE25-Nano (A5EB013BB23BE4SCS)

- [ ] **D1 `[clocks]`.** A DE25 manifest takes clock frequencies; the
  build prints per-clock Fmax and slack from the place and route's timing
  and fails on a miss, as the other devices do.
- [ ] **D2 Bursts on the lightweight bridge.** AXI4 INCR bursts read and
  written from Linux (a memcpy through the uio mapping), checked word for
  word.
- [ ] **D3 I/O registers** in a DE25 manifest.
- [ ] **D4 The other bridges.** HPS-to-FPGA (the full 128-bit bridge),
  FPGA-to-HPS and FPGA-to-SDRAM, each named by signal like the lightweight
  bridge, each with a board test.
- [ ] **D5 Router speed.** facet-hdl's hello under 31 s, with the board as
  the check since faster routes leave a5pnr's tests.
- [ ] **D6 Which phase 1 booted.** `flash --ssh` reads the board's HPS IO
  hash (or an equivalent) before loading, instead of trusting the manifest.
