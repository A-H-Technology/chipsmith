# The 7-series back end is chipsmith's own: one K-input LUT mapper, a packer per device, site routing by rule

The Zybo build ran yosys' `synth_xilinx`, then openXC7's nextpnr three times: to pack, to route chipsmith's placement for the site routing chipsmith's router started from, and to write the FASM. chipsmith placed and rerouted the interconnect in between. The packer couldn't be replaced on its own: each later nextpnr run packed the netlist again and found chipsmith's placement by cell name, so a packer of chipsmith's would have had to reproduce nextpnr's cell names. Replacing the packer meant replacing everything nextpnr did after it.

So the 7-series back end now runs chipsmith's own stages from the Verilog to the `.bit`, with prjxray-db as data (ADR-0003):

- **Synthesis per device on shared passes.** The LUT mapper takes K as a parameter (`chipsmith_synth::lutmap::LutSize`): 4 for the Cyclone II's LE, 6 for the 7-series' LUT6. K = 4 runs the code it ran before, truth tables in the low 16 bits of a `u64`, so the DE2 builds are byte-identical. `chipsmith_xc7::synth` runs chipsmith-synth's passes in the Cyclone II's order and adds only what is the 7-series' own: CARRY4 chains, FDRE/FDSE/FDCE/FDPE, RAM32M, pin and clock buffers.
- **A packer per device that hands the placer its own design.** `chipsmith_xc7::pack` turns synthesis' cells straight into the placer's cells, BLEs, carry chains and RAMs, and records what each cell configures. Its shapes are legal by construction: every CARRY4 S has a LUT of its own in that bit's 6LUT (a feed-through LUT1 where S comes from anything else), DI comes in on the bit's X bypass pin, a RAM32M owns a SLICEM, and constants are two nets that every sink taps from its nearest GND_WIRE or VCC_WIRE.
- **Site routing by rule.** The cell set is small enough that each cell port maps to one site pin (`chipsmith_xc7::phys`). chipsmith's router then routes every net, clocks and constants included, in the prjxray graph. That graph now keeps the few pseudo pips a design passes through, with the bits nextpnr sets for them: an I/O tile's ILOGIC and OLOGIC, and a clock row's BUFHCE. A BUFG's net is confined to clock wires.
- **A FASM writer that writes what nextpnr writes**, feature for feature where they configure the same thing, so prjxray's tools treat both alike.
- **Frames and the `.bit` from prjxray-db's segbits, tilegrid and part.yaml** (`chipsmith_xc7::frames`), byte-identical to prjxray's `fasm2frames` and `xc7frames2bit` on the same FASM. Those two stay as oracles in the bitstream test.

The bitstream is checked by reading it back: `bit2fasm` and `chipsmith_xc7::decode` turn the `.bit` into a netlist, which is co-simulated against the RTL (`crates/chipsmith/tests/xc7_bitstream.rs`). The decoder follows the hardware's rules, not the writer's choices, and it is checked first on nextpnr's bitstreams that ran on the board.

## Considered options

- Keep nextpnr's packer and run it as an oracle for names: chipsmith's packer would have had to match nextpnr's naming of every cell it adds. That is fragile, and nextpnr would still be in the flow.
- Model the sites as a graph and search it, as nextpnr does: this is more general (LUT input permutation, route-throughs, fracturable LUTs), but the rules cover every cell synthesis makes, and a search isn't needed until the packer fractures LUTs.
- Check on the board instead of decoding: the boards were off. A decoded bitstream checks every build in CI, and finds the problem in the netlist, not on a scope.
- Two LUT mappers: one would drift from the other. K is the only difference that matters to the algorithm.

## Consequences

- No external program in the build: no nextpnr (nor its 2.5-minute chip database), no prjxray tool. The devshell keeps them as oracles.
- What chipsmith packs is what its synthesis makes. Not yet supported: block RAM, DSPs, SRLs, MUXF7/F8, LUT6_2 (5LUTs are used only by RAM32Ms), latches, I/O-tile registers, and a design that drives the PS7.
- The placer is still the 7-series annealer, not chipsmith-place's electrostatic placer (ADR-0012). Above a few thousand LUTs it is the slowest stage.
- Timing is prjxray-db's delays without wire RC, so the reported Fmax is an estimate above Vivado's. Vivado can't take chipsmith's routing, so the comparison is between the tools. (Superseded by ADR-0016: Vivado takes chipsmith's routing, and the delays are fitted to its timer.)
- bit2fasm reads no bits of single-IOB (`_SING`) I/O tiles, so the check takes their pin paths as given.
