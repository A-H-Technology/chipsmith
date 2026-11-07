//! chipsmith's 7-series back end: synthesis to the 7-series primitives,
//! the prjxray-db routing graph, placement, routing and the Zynq `.bin`.

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
