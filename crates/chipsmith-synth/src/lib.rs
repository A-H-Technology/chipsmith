//! chipsmith's own synthesis, device-independent half: RTL optimization,
//! arithmetic and memory extraction, lowering to gates and LUT mapping, on
//! chipsmith-netlist's IR. A device crate drives these passes and adds its
//! own mapping (carry chains, registers, RAM blocks, pin buffers).

pub mod alumacc;
pub mod clean;
pub mod lutmap;
pub mod memory;
pub mod opt;
pub mod rewrite;
pub mod techmap;
