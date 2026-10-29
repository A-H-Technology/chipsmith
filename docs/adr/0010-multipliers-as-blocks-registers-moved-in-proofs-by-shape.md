# Multipliers go to the embedded blocks, registers move in, and the proof is per shape

The EP2C35 has 35 embedded multipliers (one 18 x 18 or two 9 x 9 each, with optional operand and product registers). `$mul` of two non-constant operands at least two bits wide maps to them while blocks are left; constant and one-bit factors stay logic (shifts and AND gates are cheaper). An operand up to 18 bits takes one block, in nine-bit mode when both fit nine bits; wider operands are cut into 18-bit limbs, the top limb signed when the operand is, and the partial products are summed with adders, as Quartus does. Product bits past the result width are never built.

A register whose output only this product reads becomes the block's operand register; a register that takes the product bits of a one-block product becomes its product register. The block's registers share one clock, enable and clear, so only registers with the same controls move in, and only the kinds the block has (rising edge, active-high enable, asynchronous clear to 0, powering up 0). A register that also feeds other logic stays an LE register rather than being duplicated.

The chip's unsigned sign input reads 1 undriven, so an unsigned operand needs a 0 on it: one LUT with no inputs and an all-zero mask serves the whole design, as Quartus' single `~GND` cell does, and cyclone2-re's export names it `~GND` so Quartus keeps it where it is placed.

## Verification

SAT can't prove a dense product over several blocks against a flat multiplier (a 20 x 20 limb sum doesn't finish in an hour), so the checks are split:

- One proof per shape (`tests/support/mult.rs`): one product alone in a design, the block modelled by a Verilog `$mul` of the operands' own widths and signs, which yosys' `equiv_struct` matches structurally; seconds each. Limb splitting is proven on sparse operands (a few variable bits at the top and bottom of each, zeros between): the limbs are still cut at bit 18 and every partial product is built and summed by the real code, and the proof finishes in seconds.
- Co-simulation (iverilog) of the RTL against the netlist on thousands of random operand pairs, the extremes first, for dense products over several blocks and for every shape; a mutation test shows it catches a swapped operand bit and a wrong sign.
- Quartus assembles chipsmith's placement and routing byte-identically for the multiplier designs.

## Considered options

- Leave multiplication in logic, as before: an 18 x 18 product is about 400 LEs and 25 ns of carry chains; the blocks do it in 2.9 ns.
- Duplicate a register into the block when it also feeds logic: more registers, and not what the equivalence check compares register for register.
- Both 9 x 9 rows of a block for two narrow products: halves the blocks used for small products; not done, one product per block.

## Consequences

- A design's `$mul`s are blocks unless they are constant, one-bit, or the chip is out of blocks; a wide product is several blocks and an adder tree, and the logic is what's left.
- The timing model has one cell delay for the block (2.9 ns in to out, from quartus_sta) and the LE register's setup and clock-to-output for its registers until measured.
- Only one multiplier per block; the second 9 x 9 row of a block in nine-bit mode is unused.
