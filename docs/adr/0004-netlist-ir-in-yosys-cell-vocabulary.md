# The netlist IR uses yosys' cell vocabulary, and yosys stays as a test oracle

chipsmith's own synthesis replaces yosys, but its netlist IR uses yosys' RTLIL cell library (`$add`, `$mux`, `$dff`, `$mem_v2`, ... and the `$_AND_`-style gate cells) and reads and writes yosys' JSON netlist format. That lets each piece be built and checked on its own: the synthesis passes can start from yosys' elaborated output before chipsmith's Verilog front end exists, the front end's output can be handed to yosys' synthesis, and every stage's result can be checked for equivalence against yosys' with yosys' own `equiv_*`/SAT passes.

yosys is a dev-dependency and test oracle after the switch, like Quartus is in cyclone2-re: the flow never runs it.

## Consequences

- The IR inherits RTLIL's semantics (cell parameters, signedness rules, `x`/`z` bits). Those are well documented and battle-tested; inventing our own would mean also writing the translation layer to compare against yosys.
- Where chipsmith wants something RTLIL can't say, it adds `$chipsmith_*` cells rather than changing the meaning of a yosys one.
