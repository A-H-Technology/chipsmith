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
