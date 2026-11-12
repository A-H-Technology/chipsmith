//! chipsmith's Agilex 5 (DE25-Nano, die sm4revb) back end, moved from
//! agilex5-re's `a5synth`, `a5pnr` and `a5db` (ADR-0003): synthesis of
//! Verilog into a netlist (`synth`), place and route of that netlist into a
//! design file (`pnr`), that design over the idle image into a
//! configuration image and its `.sof` (`rebuild`, `sofgen`), and the `.rbf`
//! the SDM loads from it (`rbf`). It reads agilex5-re's database, the
//! routing graph dumped from Quartus, and the firmware and codebooks of the
//! user's own Quartus Prime Pro install (docs/legal.md).
mod blank;
pub mod db;
mod ddb;
pub mod flow;
pub mod graph;
pub mod jic;
mod pins;
pub mod pnr;
pub mod rbf;
pub mod rebuild;
pub mod route;
pub mod sof;
pub mod sofgen;
pub mod synth;
