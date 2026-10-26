# A PLL is asked for by frequency ratio, and chipsmith picks the counters

A Cyclone II PLL is configured by counters: a pre-divider n, a feedback divider m, a post-scale counter per output with its high and low counts, an initial count and a VCO tap for phase, and loop settings (charge pump, loop filter, lock window) that depend on m. A design could set those itself, or say what it wants and let the tool find them.

`CY2_PLL` takes altpll's parameters: the input period (`INCLK0_INPUT_FREQUENCY`, ps) and per output `CLKk_MULTIPLY_BY`, `CLKk_DIVIDE_BY`, `CLKk_PHASE_SHIFT` (ps) and `CLKk_DUTY_CYCLE` (%). `crate::pll` finds n, m and the counters that make every output exactly `f_in * multiply / divide` with the VCO at 500-1000 MHz and the phase detector at 10-402.5 MHz, the limits Quartus' own atoms carry, preferring the VCO nearest 800 MHz as Quartus does. The loop settings follow from m, as the fitter sets them (cyclone2-re README, "PLLs"). A request no counters can meet is an error naming the frequencies.

The reason is that the counters are the device's business and their constraints aren't obvious: m and n trade against the VCO and phase-detector ranges, an odd count needs odd mode for a 50% duty cycle, a phase shift splits into whole VCO periods and eighths of one, and the loop settings must match m or the PLL may not lock. A ratio is what a designer knows, it reads the same in Quartus' altpll, and it gives the timing report each output's frequency for nothing.

## Considered options

- Explicit counters (`M`, `N`, `C0_HIGH`, ...), as the `cycloneii_pll` atom has them: no solver, and any configuration the hardware has, but every design repeats the arithmetic and can set a combination that doesn't lock.
- altpll itself, by name and with all its parameters: Quartus designs would build unchanged, but altpll has dozens of ports and modes (switchover, reconfiguration, external feedback) chipsmith doesn't have, and accepting the name would promise them.

## Consequences

- chipsmith's counters can differ from Quartus' for the same request where several meet it equally well; both make the same frequencies. The Quartus export carries chipsmith's counters, so the two still assemble the same bits.
- Loop settings are only known for m (times the VCO post-scale) of 4-32 and VCOs of 600-800 MHz; the solver doesn't go below m = 4.
- Exact ratios only: a request off by a fraction of a ppm is refused rather than approximated.
