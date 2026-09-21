pub mod build;
pub mod download;
pub mod install;
pub mod nixos;
pub mod product;
pub mod qsf;
pub mod runner;
pub mod sdc;
pub mod timing;
pub mod toolchain;

pub use product::QuartusProduct;
pub use toolchain::QuartusToolchain;
