# Latches and asynchronous loads are built as Quartus builds them

The Cyclone II LE register has an asynchronous clear and nothing else asynchronous: no latch mode, no preset, no asynchronous load. RTL has all of them (`$dlatch`, `$adlatch`, `$dlatchsr`, `$sr`, `$aldff`, `$dffsr`, and an `$adff` that resets to the other value than it powers up to). chipsmith builds them the way Quartus 13.0sp1 does on this family, read off its post-map netlist (`quartus_cdb --vqm`) of a design with one of each:

- A latch is one LUT whose COMBOUT comes back on its own DATAC: `COMBOUT = load ? value : COMBOUT`, the controls on the other three inputs (Quartus warning 10240).
- A register with an asynchronous load is Quartus' `~reg0_emulated` circuit (warnings 13004, 13310): a latch L that holds the value the load sets, an LE register holding `D ^ L` and cleared while loading, and `Q = load ? value : reg ^ L`. Set and clear are a load of `!clr` while either acts.

yosys' `dfflegalize` can't do either for the six register types `cyclone2.ys` allows; it stops with an error. Matching Quartus keeps one reference for what the hardware does with these circuits, and lets `bench/quartus-timing.sh` assemble and time them like any other design.

## Considered options

- Reject them, as before: real designs (cores with asynchronous set and reset, `always @*` blocks that miss a branch) didn't build.
- A register clocked by the latch enable: a clock from logic, which the DE2 back end doesn't route yet, and it changes when the latch is transparent.
- yosys' `dfflegalize` emulation of `$dffsr` with two registers and a multiplexer: it needs a latch too, and it isn't what Quartus does.

## Consequences

- Netlists can have combinational loops, of exactly one shape (a LUT feeding itself). The LUT mapper sees the loop LUT as a device cell, the timing graph cuts the feedback arc (a latch is timed from its controls, as Quartus "analyzes combinational loops as latches"), and the equivalence check cuts it through a `$ff`, which is `async2sync`'s model of the RTL's latch.
- What a loop holds at power-up isn't defined by the hardware (Quartus says so too). The proof assumes 0, as for a register; the build warns for every latch and emulated register.
- Glitches on a latch's controls reach its output, as with Quartus. The build doesn't check for them.
