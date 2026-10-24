//! chipsmith's netlist IR, shared by the Verilog front end and synthesis,
//! in yosys' RTLIL cell vocabulary and read and written as yosys' JSON
//! netlist (ADR-0004).
//!
//! # Shape
//!
//! A [`Design`] is [`Module`]s. A module's connectivity is bit-level
//! [`Net`]s: a [`SigBit`] is a net or a constant (`0 1 x z`), a
//! [`SigSpec`] a `Vec` of them. Nets are canonical, as in yosys' JSON
//! rather than RTLIL's wires-plus-`connect()`, so "same net" is `==` and
//! a driver/user index ([`Connectivity`]) needs no alias map. [`Wire`]s
//! name nets; the ones with a direction are the ports.
//!
//! # Typed cells
//!
//! A [`Cell`]'s [`CellKind`] is typed for the RTLIL families synthesis
//! pattern-matches on: operators ([`Unary`], [`Binary`] by op enum, so
//! "every `$add`" is one `match`), multiplexers, `$alu`, `$lut`,
//! `$tribuf`, registers and latches ([`Ff`], one type for all 16
//! word-level and ~50 gate-level storage cells), gates ([`Gate`]) and
//! `$mem_v2` ([`Mem`]). Ports are fields, so a `$mux` without `S` can't be
//! built and passes don't look ports up by string. Parameters that are
//! only widths are not stored; they're derived from the ports, so they
//! can't disagree. The relations between port widths that the types don't
//! carry (`$mux`'s A, B, Y equal) are checked when reading and by
//! [`Module::check`].
//!
//! Everything else is [`CellKind::Other`]: a type name, parameters and
//! named ports, carried through untouched. That covers instances of the
//! design's own modules, blackboxes and device cells, and RTLIL cells not
//! modelled yet (`$memrd_v2`, `$macc`, `$scopeinfo`, `$print`, ...).
//! Adding a typed variant later doesn't change the JSON: both forms go
//! through [`CellKind::from_generic`] and [`CellKind::to_generic`].
//!
//! The alternative, every cell generic plus validation, is what yosys
//! does in C++; it makes every pass re-check that the ports it reads
//! exist. A typed enum per cell type (`$dff`, `$dffe`, ... separately)
//! would make register legalization a match over 60 variants.

mod cell;
mod comb;
pub mod ff;
mod gate;
mod index;
pub mod json;
mod mem;
mod module;
mod param;
mod ports;
mod sig;
#[cfg(test)]
mod tests;

pub use cell::{Cell, CellKind, PortSig};
pub use comb::{
    Alu, Binary, BinaryOp, Bmux, Demux, Lut, Mux, Operand, Pmux, Tribuf, Unary, UnaryOp,
};
pub use ff::{Bit, DffControl, Ff, FfKind, LatchReset, SetClr, SyncEnable, Word};
pub use gate::{Gate, GateOp};
pub use index::{Connectivity, Endpoint};
pub use json::{read_json, write_json, ReadError, WriteError};
pub use mem::{Mem, MemRd, MemRdSync, MemWr};
pub use module::{CellId, Design, Memory, Module, Wire, WireId};
pub use param::{attr, is_set, Attributes, ParamValue};
pub use ports::{CellError, Dir, Generic, GenericPort};
pub use sig::{Const, Ctrl, Net, Polarity, SigBit, SigSpec, State};
