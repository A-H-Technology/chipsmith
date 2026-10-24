//! yosys' `opt` family on the IR: constants, merging, multiplexer trees,
//! registers.

mod dff;
mod gates;

pub use dff::opt_dff;
pub use gates::opt_gates;
