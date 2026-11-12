//! chipsmith's Agilex 5 (DE25-Nano, die sm4revb) back end. So far assembly:
//! a placed and routed design file (a5pnr's) into a configuration image and
//! its `.sof` (`rebuild`, `sofgen`), and the `.rbf` the SDM loads from it
//! (`rbf`). It reads agilex5-re's database, the routing graph dumped from
//! Quartus, and the firmware and codebooks of the user's own Quartus Prime
//! Pro install (docs/legal.md). Moved from agilex5-re's `a5db` (ADR-0003).
mod blank;
pub mod db;
mod ddb;
pub mod graph;
mod pins;
pub mod rbf;
pub mod rebuild;
pub mod route;
pub mod sof;
pub mod sofgen;
