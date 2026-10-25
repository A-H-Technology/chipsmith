//! yosys' `opt` family on the IR: constants, merging, multiplexer trees,
//! registers.
//!
//! Bitwise and logic operators are lowered to gates up front
//! ([`lower_bitwise`]): they map one to one onto gates anyway, and the
//! gate-level folding and merging then covers what `opt_expr` and
//! `opt_merge` do for them. What stays word-level is what later passes
//! need whole: multiplexers (registers' enables and resets are found in
//! them), arithmetic, comparisons, shifts, registers and memories.

mod dff;
mod gates;
mod muxtree;
pub mod seqconst;
mod word;

use anyhow::Result;
use chipsmith_netlist::{BinaryOp, CellKind, Module, UnaryOp};

pub use dff::opt_dff;
pub use gates::opt_gates;
pub use muxtree::opt_muxtree;
pub use word::{opt_merge, opt_word};

use crate::{clean, techmap};

/// Lowers the cells that are bitwise anyway to gates.
pub fn lower_bitwise(m: &mut Module) -> Result<()> {
    techmap::lower(m, |k| match k {
        CellKind::Unary(u) => u.op != UnaryOp::Neg,
        CellKind::Binary(b) => matches!(
            b.op,
            BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor
                | BinaryOp::Xnor
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::Eqx
                | BinaryOp::Nex
        ),
        CellKind::Lut(_) => true,
        _ => false,
    })
}

/// The `opt` loop: until nothing changes. `removable` names the device
/// cells [`clean::clean`] may remove.
pub fn opt(m: &mut Module, removable: &[&str]) {
    for _ in 0..100 {
        let mut changed = opt_gates(m);
        changed |= opt_word(m);
        changed |= opt_merge(m);
        changed |= opt_muxtree(m);
        changed |= opt_dff(m);
        let before = m.cells.len();
        clean::clean(m, removable);
        changed |= m.cells.len() != before;
        if !changed {
            break;
        }
    }
}

/// [`opt`], then registers stuck at their power-up value become constants,
/// until neither finds anything more.
pub fn opt_seq(m: &mut Module, removable: &[&str]) -> Result<()> {
    loop {
        opt(m, removable);
        let consts = seqconst::find(m)?;
        if seqconst::apply(m, &consts) == 0 {
            return Ok(());
        }
    }
}
