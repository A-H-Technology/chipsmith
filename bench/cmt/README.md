# PLL and MMCM benches

What `chipsmith_xc7::cmt` was checked against: Vivado 2026.1's clocking
wizard for the counters a request gets, and Vivado's own bitstreams for the
bits those counters take. Vivado runs through xc7-re's `tools/vivado.sh`
(local only).

## The clocking wizard's counters

`wizard.tcl` puts each request of `wizard-requests.txt` (input MHz, outputs
MHz with an optional `@phase` in degrees and `%duty`) through clk_wiz 6.0
for a PLLE2_ADV and an MMCME2_ADV and prints the counters it chose. The
results are `crates/chipsmith-xc7/tests/data/clk_wiz.txt`, and
`tests/cmt.rs`'s `the_clocking_wizards_counters` holds `Request::solve` to
them: of the 117 requests with exact answers, 110 get the wizard's
counters, every PLL one among them; the 7 others are MMCM requests, listed
in the test with their reasons (2026-11-04).

The wizard's choice is a jitter model's, which isn't public (its
`GetClosestSolution` is compiled into Vivado). What it does, as far as the
data show:

- **The shifts and duty cycles first.** 125 MHz at 10 degrees takes a
  1125 MHz VCO (`/9`, 2 eighths exactly) over 875 MHz (`/7`, 1.56); a 25%
  duty at 125 MHz takes `/8` (2 periods high) over `/7` (1.75).
- **PLL: the lowest VCO, but not 800 MHz with an even divider.** 125 MHz
  from 125 is 875 MHz `/7`; 62.5 MHz 812.5 MHz `/13`; 50 MHz 850 MHz `/17`.
  800 MHz is the lowest the PLL has, and the wizard takes it for 160 and
  32 MHz (`/5`, `/25`) but passes over it for 100, 80, 50, 40, 25, 20, 16,
  12.5 and 8 MHz, all even there (`/8`, `/10`, ...), even for a VCO up to
  1200 MHz (16 and 80 MHz). For 100 and 160 MHz together, where only 800
  and 1600 MHz make both, it takes 800. chipsmith ranks such an 800 as
  1200.5 MHz, which matches every PLL request.
- **MMCM: the VCO nearest 1000 MHz, fractional counters allowed.** 150 MHz
  from 125 is 993.75 MHz with CLKOUT0 `/6.625`, 75 MHz 1003.125 `/13.375`.
  At some low frequencies the model takes other counters for the same
  frequency: 25 MHz at 912.5 MHz (`/36.5`) where 1000 `/40` is exact; 12.5,
  8 and 31.25 MHz likewise. chipsmith keeps the rule: the frequencies are
  the same.
- **Approximations.** Where no counters make a request exactly, the wizard
  takes the nearest (148.5 MHz as 148.44); for 64 and 48 MHz together it
  approximates even though a 1200 MHz VCO makes both. chipsmith refuses a
  request it can't make exactly, as the DE2's CY2_PLL does (ADR-0008).

## Vivado's bits

`harvest.py` writes designs with the device's four blocks (two PLLs, two
MMCMs) each, every block's multiplier, pre-divider, outputs, phases, duty
cycles and bandwidth pseudo-random from the design's number, the
multipliers swept 2 to 64 across 32 designs; `harvest.py <dir> fill` adds
the multipliers the sweep left without a BANDWIDTH OPTIMIZED block, and
MMCMs with ZHOLD feedback. Each design's `run.tcl` (`fill.tcl`) builds them
in one Vivado session; no clock is constrained on the pin, so each block's
CLKIN1_PERIOD stands as given and sets a VCO in range whatever the board's
clock is. prjxray's `bit2fasm` reads each `top.bit`, and `extract.py`
writes:

- `crates/chipsmith-xc7/tests/data/cmt_vivado.txt`: per block its
  attributes and the bits Vivado set in its site;
- `crates/chipsmith-xc7/src/cmt_tables.txt`: what doesn't come from the
  counters, per block kind: the bits every block has (`FILTREG1_RESERVED`,
  `LOCKREG3_RESERVED`, the MMCM's `POWER_REG`) and the lock and loop-filter
  tables (`LKTABLE`, `TABLE`) per whole CLKFBOUT_MULT, which depend on
  nothing else (two blocks of one multiplier and different VCOs,
  pre-dividers or outputs never differ; the script checks).

`tests/cmt.rs` configures every OPTIMIZED block from its attributes and
compares the bits, and decodes Vivado's bits back to the attributes. What
the harvest showed, besides XAPP888's counter fields:

- Inputs tied to a constant are left unrouted, with their `ZINV_` bits set
  (an unrouted pin reads 1; the bit inverts it): RST, PWRDWN and the MMCM's
  PSEN, PSINCDEC. CLKINSEL (CLKIN1) is unrouted and not inverted.
- A PLL's ZHOLD is `COMPENSATION.ZHOLD_NO_CLKIN_BUF`, INTERNAL
  `Z_ZHOLD_OR_CLKIN_BUF`; an MMCM has `COMP.Z_ZHOLD` for both, so the
  decoder reads the MMCM's compensation off the feedback's routing.
- An MMCM's fractional counter's EDGE is its whole part's odd bit, and a
  whole CLKOUT0 or CLKFBOUT copies its PHASE_MUX into the fractional
  register's PHASE_MUX_F.
- A high or low time over 64 VCO periods doesn't fit the 6-bit fields:
  Vivado writes it cut to 6 bits (CLKOUT3 `/112` at 79% reads back as
  `/48`). chipsmith refuses such a counter.
