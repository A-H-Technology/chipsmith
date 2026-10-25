# Synthesis resolves undefined behaviour first, so every later step is an equivalence

RTL leaves some values open: a `'bx` assignment, an out-of-range `$shiftx` part-select, a register without a power-up value, a `$pmux` with two selects set (RTLIL makes that x). yosys resolves them wherever it's convenient: `opt_expr -mux_undef` picks the other input of a multiplexer, `opt_dff` assumes `$pmux` selects are one-hot, `setundef -zero -init` runs halfway through `cyclone2.ys`. chipsmith's Cyclone II synthesis resolves all of them in one pass before any other (`chipsmith_synth::undef::refine`): an x multiplexer input takes the other input's value, every other x becomes 0, a register powers up as its asynchronous reset sets it or else 0, and a `$pmux` whose selects aren't exclusive by construction (comparisons of one signal with different constants, which is what `proc` makes of a `case`) gets them made exclusive, the last one winning as in yosys' SAT model.

The reason is the proof. Resolving x is a refinement: the result does one of the things the RTL allowed. yosys' `equiv_*` passes check that with `-undef` (x in the reference matches anything), which is fine for logic. It isn't for registers synthesis removes: picorv32 running its ROM has about a thousand registers that never leave their power-up value, and showing that needs an inductive invariant. Under x-modelling, induction lets every register be x and proves nothing; in two values, the claims can be false for the RTL itself, because the RTL's x and multi-select `$pmux` resolve differently in yosys' model than in chipsmith's netlist. Resolved once, up front, the proof splits: the RTL against the resolved netlist is a refinement with every register still there (`-undef`, no invariants needed), and from the resolved netlist on every step is an exact equivalence, so the stuck registers are proven constant on it by two-valued `sat -tempinduct` and the rest by `equiv_induct`. yosys' own flow can't be proven against its RTL by the netlist tests' script: on picorv32's `synth -run :fine` netlist 608 `$equiv` stay unproven.

## Considered options

- Resolve where convenient, as yosys does, and check by bounded model checking from power-up: no proof beyond the bound, and on picorv32 40 cycles took 10 minutes.
- Give `$pmux` one exact meaning everywhere (the OR of the selected inputs, as `simplemap` builds it) instead of exclusive selects: no extra logic, but the register enables `opt_dff` finds through a `$pmux`, and `opt_muxtree`'s pruning of its inputs, would then be wrong where two selects are set, so both lose what they do on a CPU's decoder.

## Consequences

- Exclusive selects cost logic where yosys gets them for free: on picorv32, 1,085 LUTs against 1,079 without them, same registers.
- Each x is decided once, before optimisation; a choice that would have been cheaper in a later context is lost.
- The power-up rule is one function (`chipsmith_synth::rewrite::define_inits`) the checker applies to the RTL too. A different rule changes which registers are provably stuck, so it changes in one place.
