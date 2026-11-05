# The Verilog front end builds the netlists yosys' `proc` builds

`chipsmith-verilog` lowers `always` blocks to the same multiplexer structure yosys' `proc` makes, not to the smallest one: a case's arms from last to first, a `$pmux` per run of arms that can't overlap with its later selects winning, a `$mux` level per priority group, arms that leave a signal as it was left out of that signal's `$pmux`, one tree per group of bits the block always assigns together (yosys' snippets), `full_case` read as yosys 0.70 reads it, and initial values kept, dropped or tied off by what drives the variable, as yosys 0.70's `proc` does.

The reason is the check. The front end is proven against yosys' own front end by sequential equivalence (`equiv_make`, `equiv_simple`, `equiv_induct`, the script from ADR-0004's netlist tests). Induction considers states the design never reaches, and yosys' SAT encoding reads `x` as 0. Two netlists that agree on every reachable state but differ where, say, two `parallel_case` selects are both set fail the proof, and picorv32 failed it that way until the lowering matched yosys'. Matching the structure makes the two agree in every state, so the proof goes through on the whole corpus and still fails on a one-bit miscompile.

## Considered options

- The smallest lowering (a priority chain, only the bits that change), checked by bounded model checking from the initial state: a proof only up to the bound, and it needs every register's initial value.
- `equiv_induct -seq N` with a larger N, or `equiv_simple -undef`: still sensitive to what unreachable states do, slower, and no longer the script the netlist tests use.

## Consequences

- The lowering follows yosys 0.70. A yosys whose `proc` builds differently shows up as failing proofs, not as wrong hardware.
- Synthesis downstream sees the netlists it sees from yosys' front end. Through cyclone2-re's `cyclone2.ys` every DE2 example gives the same cells, but picorv32 gives 4 LUTs fewer (989 vs 993): logic that a parameter makes dead is folded while elaborating, where yosys leaves it to `opt`, and abc maps the slightly different input differently.
