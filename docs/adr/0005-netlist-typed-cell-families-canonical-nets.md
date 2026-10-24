# Netlist cells are typed by RTLIL family, and nets are canonical bits

`chipsmith-netlist` models the RTLIL cells synthesis matches on as Rust types, one per family, with the ports as fields: `$add` and `$eq` are a `Binary` with an op, all 16 word-level and 129 gate-level registers and latches are one `Ff` that says which clock, enable and reset it has, and `$mem_v2` is a `Mem` with a struct per port. Parameters that only give a port's width aren't stored. Any other cell type (user modules, device cells, RTLIL cells not modelled yet) stays generic: a type name, parameters and named ports. Connectivity is by canonical bit-level nets, as in yosys' JSON: a module has no `connect()` aliases, so two ports are connected when they hold the same `Net`.

## Considered options

- Generic cells plus validation, as RTLIL does in C++: every pass would look ports up by string and re-check that they exist and have the right widths.
- One Rust type per RTLIL cell type: register legalization would match on 60-odd variants that differ only in which controls they have.
- RTLIL's wires plus `connect()`: every driver/user query would go through an alias map (yosys' `SigMap`) that passes must keep up to date. A pass that wants to join two nets rewrites one into the other with `Module::map_bits` instead.
