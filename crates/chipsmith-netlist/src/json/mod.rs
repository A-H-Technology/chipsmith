//! yosys' JSON netlist format, both ways.
//!
//! Covered: modules with attributes and parameter defaults, ports, wires
//! (`netnames`, with `offset`/`upto`/`signed`), cells (typed where the IR
//! models the type, generic otherwise) and RTLIL memory objects. Not
//! covered: the `models` section `write_json -aig` adds, and the signed and
//! real flags of parameters, which yosys' JSON doesn't carry either.

mod read;
mod write;

pub use read::{read_json, ReadError};
pub use write::{write_json, WriteError};
