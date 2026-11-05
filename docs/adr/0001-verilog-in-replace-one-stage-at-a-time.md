# Verilog in, one stage replaced at a time

chipsmith started as a driver around existing open tools (GHDL, yosys, nextpnr, cram, prjxray) and replaces them one stage at a time, each replacement checked against the tool it replaces (same bitstream, or an equivalent netlist) before the tool goes. Every stage is one `Step` in a `Plan`, so a replacement swaps one step.

The input language is Verilog, the synthesizable subset that GHDL's `--synth --out=verilog` and HDL generators emit. Other languages are translated to it in front of the flow; GHDL stays as the VHDL translator and is the one external tool the finished flow keeps. Verilog is a poor IR, but it is the one every open front end can already write, so chipsmith parses it once and works on its own netlist from there.

## Consequences

- "No shelling out" means no external program between the Verilog and the programmed chip. GHDL runs before that boundary.
- Vendor tools (Quartus, Vivado) are oracles in the reverse-engineering repos only; chipsmith never runs them.
