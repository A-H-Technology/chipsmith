# 7-series multipliers: products on DSP48E1 limbs summed in carry chains, yosys' model checked against Vivado's

The Zybo build made every product of logic: LUTs and carry chains. The DE2's has mapped products onto its embedded multipliers since ADR-0010, with registers moved into the block where the RTL has them. The xc7z010's 80 DSP48E1s are that block and more: a 25 x 18 signed multiplier, an ALU and a 48-bit accumulator behind it, a pre-adder, a pattern detector and cascades to the DSP above, each register stage with its own enable and reset on one shared clock.

So:

- **A product only.** Synthesis (`chipsmith_xc7::synth::dsp`, after chipsmith-cyclone2's `synth/mult.rs`) builds P = A * B (OPMODE 0000101, ALUMODE 0000) and nothing else of the ALU: no accumulators, pre-adder or pattern detect. A product wider than one DSP is cut into limbs (24 x 17 unsigned, 25 x 18 where a limb carries the operand's sign), and their partial products summed with adders that map onto CARRY4s like any other. The operands go whichever way round takes fewer DSPs: 26 x 22 signed takes two with the 22 bits on A, four the other way.
- **Registers move in where only the product reads them.** A rising-edge register holding an operand becomes AREG/BREG, one holding a one-DSP product PREG, a second one after it MREG; their enables and synchronous resets to 0 become CE/RST pins. A register that powers up 1, has an enable or reset of the other polarity, or a falling-edge clock stays in the fabric.
- **One configuration type** (`chipsmith_xc7::dsp::Config`): register stages, cascade inputs, the D port, pin inversions, MASK and PATTERN, what a bitstream configures. Synthesis writes it as the primitive's parameters; the packer, FASM writer and decoder read it back. OPMODE, ALUMODE, INMODE and CARRYINSEL are pins, so a decoded DSP is whatever the routing ties them to, cascades included.
- **yosys' model is the co-simulations', checked against Vivado's.** `xilinx/cells_sim.v` models the DSP48E1 in full but refuses IS_CLK_INVERTED, so the decoder folds every pin inversion into the pin (a constant flipped, a signal through a LUT1), and synthesis doesn't build falling-edge DSP registers. `tests/models.rs` runs yosys' model beside Vivado's unisim on random configurations of what synthesis and the decoder make, Vivado's PCIN cascade included: 40 runs of six blocks, 6.57 million output bits, none different.

## Considered options

- Sum the partial products in the DSPs (PCIN shifted 17 bits, as Vivado does): no LUTs or carry chains for the sum, and cascaded DSPs stacked in one column. It needs the placer to keep cascaded DSPs adjacent, a constraint it doesn't have. The decoder already reads Vivado's cascades.
- Keep small products in LUTs, as Vivado does for 7 x 5: depends on a cost model of LUTs against DSPs that chipsmith doesn't have. The DE2's mapper takes every product to a block too.
- A behavioural model of chipsmith's own, as for block RAM: yosys' is complete for what's built, and Vivado's agrees with it.

## Consequences

- The DE2 standard applies: every shape co-simulates against its RTL after synthesis (`tests/synth.rs`, `dsp_shapes`) and through the bitstream (`crates/chipsmith/tests/xc7_bitstream.rs`, `dsp`), and Vivado's bitstreams of the same RTL decode to netlists that co-simulate too. The board check is owed.
- A design with more products than DSPs maps the rest to logic. Products of a constant stay in logic.
- Falling-edge registers around a product cost fabric registers until the co-simulation can fold IS_CLK_INVERTED the way the decoder does.
