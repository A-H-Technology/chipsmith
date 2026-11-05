// async-trait's expansion puts a bare #[must_use] on a function returning a
// boxed future, which clippy (1.98) flags as double_must_use. Remove once
// async-trait stops emitting it (still does in 0.1.89).
#![allow(clippy::double_must_use)]

pub mod error;
pub mod manifest;
pub mod process;
pub mod scaffold;
pub mod timing;
pub mod toolchain;
