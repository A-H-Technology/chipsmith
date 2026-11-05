// async-trait's expansion puts a bare #[must_use] on a function returning a
// boxed future, which clippy (1.98) flags as double_must_use. Remove once
// async-trait stops emitting it (still does in 0.1.89).
#![allow(clippy::double_must_use)]

pub mod build;
pub mod download;
pub mod install;
pub mod nixos;
pub mod product;
pub mod qsf;
pub mod runner;
pub mod sdc;
pub mod simulator;
pub mod timing;
pub mod toolchain;

pub use product::QuartusProduct;
pub use simulator::QuartusSimulator;
pub use toolchain::QuartusToolchain;
