//! chipsmith's Cyclone II (EP2C35) back end: packing, placement, routing,
//! M4K RAM and hard blocks, the bitstream and its JTAG stream, the DE2's
//! configuration flash, and synthesis to its cells.

pub mod bitstream;
pub mod hard;
pub mod m4k;
pub mod mult;
pub mod pack;
pub mod persist;
pub mod pin;
pub mod place;
pub mod pll;
pub mod route;
pub mod synth;
