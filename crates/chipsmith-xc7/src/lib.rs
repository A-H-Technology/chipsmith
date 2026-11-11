//! chipsmith's 7-series back end: synthesis to the 7-series primitives,
//! the prjxray-db routing graph, placement, routing and the Zynq `.bin`.

// Both flag this crate's idiom rather than a defect: short-lived tuple maps
// keyed by (tile, index), and loops over the slice letters A-D that index
// several parallel tables by the same k.
#![allow(clippy::type_complexity, clippy::needless_range_loop)]

pub mod bram;
pub mod cmt;
pub mod decode;
pub mod delay;
pub mod dsp;
mod fasm;
pub mod frames;
mod pack;
pub mod part;
mod phys;
mod place;
pub mod pnr;
pub mod synth;
pub mod vivado;
pub mod xray;
pub mod zynq;
