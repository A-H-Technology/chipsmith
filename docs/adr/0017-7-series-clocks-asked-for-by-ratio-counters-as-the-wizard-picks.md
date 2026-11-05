# 7-series clocks: a PLL or MMCM asked for by ratio, its counters as Vivado's wizard picks them, its bits as Vivado writes them

The Zybo build had no clock generation: zybo-mult-selftest and zybo-riscv-bram ran at 62.5 MHz from a register toggling on the 125 MHz clock, a clock the build couldn't check against a frequency. The xc7z010 has two clock management tiles, each a PLLE2_ADV and an MMCME2_ADV; prjxray-db has their bits (the DRP registers, XAPP888's fields) and their tiles' routing.

So:

- **Asked for by ratio, as on the DE2.** `XC7_PLL` and `XC7_MMCM` (`chipsmith_xc7::synth::cells`) take ADR-0008's parameters: the input period in picoseconds and per output a multiply/divide ratio, a phase shift in picoseconds and a duty cycle in percent, plus `COMPENSATION` (`ZHOLD`, feedback through a BUFG as the wizard builds it, or `INTERNAL`). Synthesis lowers each to the PLLE2_ADV or MMCME2_ADV it solves for, with Vivado's attributes (`chipsmith_xc7::cmt::Config::params`), so the export to Vivado, the decoder and the packer read one form.
- **The counters as the clocking wizard picks them.** Among the counters that make every output exactly, `Request::solve` takes the shifts and duty cycles nearest the request, then the PLL's lowest VCO or the MMCM's VCO nearest 1000 MHz (fractional CLKFBOUT and CLKOUT0 included), then the smallest pre-divider. That is Vivado 2026.1's clk_wiz on 110 of 117 exact requests, every PLL one (bench/cmt/README.md); the wizard's own rule is a jitter model that isn't public.
- **The bits as Vivado writes them.** A `Config` holds the counters in the hardware's steps (a phase in eighths of a VCO period, a high time in half periods), so only encodable configurations exist. Its DRP fields follow XAPP888; the lock and loop-filter tables and the bits every block carries are Vivado's, read out of 168 blocks of Vivado bitstreams (bench/cmt/harvest.py, `cmt_tables.txt`). Of the 129 with BANDWIDTH OPTIMIZED, 113 are matched bit for bit and decode back to their attributes (`tests/cmt.rs`); the other 16 have a high or low time over 64 VCO periods, which chipsmith refuses (below).
- **Placed by its input, routed by the router.** A block goes in the free site of its kind nearest the cell driving CLKIN1; the router finds the pin's way in (its CCIO into the HCLK_CMT's CLKIN1 mux, as Vivado routes it), the outputs' ways to their BUFGs and the feedback's. Each output's frequency follows from the counters and CLKIN1_PERIOD, so the timing report checks it with no `[clocks]` entry; the pin's `[clocks]` frequency must be the period the block was solved for.

The bitstream is checked as the rest (ADR-0013): the decoder reads the block's bits back to a `Config` (only bits chipsmith would write for those counters, lock and filter tables included), reads its compensation off the feedback's routing, and the co-simulation runs a model of each side: the request's `XC7_PLL` against the decoded `PLLE2_ADV`, both locking on the input's 8th edge and running every output at its ratio, phase and duty cycle.

## Considered options

- **The unisims with explicit counters**, as Vivado designs instantiate them: no solver, but every design repeats the arithmetic, and a configuration Vivado's wizard wouldn't build (a lock table for another multiplier, a count over 64) is easy to write. Synthesis's output is that form, so it is what the rest of the flow reads.
- **Frequencies in MHz, as the wizard takes them**: the wizard then approximates (148.5 MHz from 125 as 148.44). A ratio is exact or refused, as on the DE2.
- **XAPP888's tables** (and nextpnr-xilinx's): the app note's lock table is the MMCM's, which the PLL's departs from above a multiplier of 10 (nextpnr's own notes), and its sources aren't chipsmith's to copy. Vivado's bitstreams are the ground truth either way, and reading them out is one bench run.

## Consequences

- zybo-mult-selftest and zybo-riscv-bram run from PLLs; the toggling registers are gone (README, "The Zybo flow").
- Where the wizard's jitter model departs from the rule (some low MMCM frequencies), chipsmith's counters make the same frequency with others; the bits are still Vivado's for those counters.
- Not built: phase shifts on a fractional divider, BANDWIDTH LOW or HIGH, dynamic reconfiguration (DRP), dynamic phase shift (PSEN), CLKIN2 and spread spectrum. A high or low time over 64 VCO periods is refused: Vivado writes it cut to 6 bits.
- An MMCM's ZHOLD and INTERNAL have the same bits (`COMP.Z_ZHOLD`); only the routing tells them apart.
- On the board: owed. The boards were off; everything above is offline.
