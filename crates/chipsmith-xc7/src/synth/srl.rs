//! Shift registers into SLICEM LUTs (SRLC32E), as Vivado's default
//! `srl_style` (`reg_srl_reg`) maps them: a chain of slice registers with
//! no reset, one clock, edge and clock enable, each stage read only by the
//! next, keeps its first and last register and puts the stages between
//! into SRLs of up to 32 stages each. The first register keeps a pin's
//! input register in its I/O tile and the input's timing; the last keeps
//! the output's (an SRL's clock to out is about three times a register's).
//!
//! A chain needs four registers: two kept and at least two shifting in a
//! LUT, where one would save a register for a LUT.

use std::collections::HashMap;

use chipsmith_netlist::{CellKind, Dir, Generic, Module, Net, ParamValue, SigBit, State};

use super::cells::{Srl, FFS};

/// The registers kept at each end of a chain.
const KEPT: usize = 2;
/// Fewest registers between them worth a LUT.
const MIN_SHIFT: usize = 2;
/// Stages in one SRLC32E.
const STAGES: usize = 32;

/// A register that may shift: an FDRE with its reset tied off.
struct Stage {
    cell: usize,
    d: SigBit,
    q: Net,
    clk: SigBit,
    ce: SigBit,
    clk_inv: bool,
    init: bool,
}

fn bit(g: &Generic, port: &str) -> Option<SigBit> {
    g.port(port)?.sig.first().copied()
}

fn param(g: &Generic, name: &str) -> bool {
    matches!(g.params.get(name), Some(ParamValue::Bits(c)) if c.0.first() == Some(&State::One))
}

fn stage(cell: usize, g: &Generic) -> Option<Stage> {
    if g.ty != FFS[0] || bit(g, "R")? != SigBit::ZERO {
        return None;
    }
    Some(Stage {
        cell,
        d: bit(g, "D")?,
        q: bit(g, "Q")?.net()?,
        clk: bit(g, "C")?,
        ce: bit(g, "CE")?,
        clk_inv: param(g, "IS_C_INVERTED"),
        init: param(g, "INIT"),
    })
}

/// Maps the module's shift registers; returns how many SRLC32Es it made
/// and how many registers they took.
pub fn map(m: &mut Module) -> (usize, usize) {
    let mut readers = vec![0u32; m.net_count()];
    for c in &m.cells {
        c.kind.for_each_port(|p| {
            if p.dir != Some(Dir::Out) {
                for n in p.bits.iter().filter_map(|b| b.net()) {
                    readers[n.index()] += 1;
                }
            }
        });
    }
    for (_, w, d) in m.ports() {
        if d != Dir::In {
            for n in w.bits.iter().filter_map(|b| b.net()) {
                readers[n.index()] += 1;
            }
        }
    }

    let stages: Vec<Stage> = m
        .cells
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match &c.kind {
            CellKind::Other(g) => stage(i, g),
            _ => None,
        })
        .collect();
    // The stage reading each stage's Q, where nothing else reads it.
    let by_d: HashMap<SigBit, usize> = stages.iter().enumerate().map(|(i, s)| (s.d, i)).collect();
    let next: Vec<Option<usize>> = stages
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let n = *by_d.get(&SigBit::Net(s.q))?;
            let t = &stages[n];
            let same = t.clk == s.clk && t.ce == s.ce && t.clk_inv == s.clk_inv;
            (same && readers[s.q.index()] == 1 && n != i).then_some(n)
        })
        .collect();
    let mut has_prev = vec![false; stages.len()];
    for n in next.iter().flatten() {
        has_prev[*n] = true;
    }

    let mut gone = vec![false; m.cells.len()];
    let mut srls = Vec::new();
    for head in (0..stages.len()).filter(|&i| !has_prev[i]) {
        let mut chain = vec![head];
        while let Some(n) = next[*chain.last().expect("non-empty")] {
            if chain.contains(&n) {
                break;
            }
            chain.push(n);
        }
        if chain.len() < KEPT + MIN_SHIFT {
            continue;
        }
        for piece in chain[1..chain.len() - 1].chunks(STAGES) {
            let (first, last) = (&stages[piece[0]], &stages[*piece.last().expect("a chunk")]);
            let init = piece
                .iter()
                .enumerate()
                .fold(0u32, |v, (i, &s)| v | u32::from(stages[s].init) << i);
            srls.push(Srl {
                d: first.d,
                clk: first.clk,
                clk_inv: first.clk_inv,
                ce: first.ce,
                len: piece.len(),
                init,
                q: SigBit::Net(last.q),
            });
            for &s in piece {
                gone[stages[s].cell] = true;
            }
        }
    }
    let mut k = 0;
    m.cells.retain(|_| {
        k += 1;
        !gone[k - 1]
    });
    let count = srls.len();
    let shifted = gone.iter().filter(|&&g| g).count();
    for (i, s) in srls.into_iter().enumerate() {
        m.add_cell(format!("$srl{i}"), s.cell());
    }
    (count, shifted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::cells::{Fd, Reset};

    /// A chain of `n` registers from `input`, its last Q on an output.
    fn chain(n: usize, reset_at: Option<usize>) -> (Module, Net, Net) {
        let mut m = Module::new("top");
        let clk = m.new_net();
        let input = m.new_net();
        let mut d = SigBit::Net(input);
        let mut last = input;
        for i in 0..n {
            let q = m.new_net();
            let reset = match reset_at {
                Some(r) if r == i => Reset::SyncClear(SigBit::Net(clk)),
                _ => Reset::None,
            };
            m.add_cell(
                format!("r{i}"),
                Fd {
                    d,
                    clk: SigBit::Net(clk),
                    clk_inv: false,
                    ce: SigBit::ONE,
                    reset,
                    init: i % 3 == 0,
                    q: SigBit::Net(q),
                }
                .cell(),
            );
            d = SigBit::Net(q);
            last = q;
        }
        let mut w = chipsmith_netlist::Wire::new("out", vec![SigBit::Net(last)]);
        w.port = Some(Dir::Out);
        m.wires.push(w);
        (m, input, last)
    }

    fn count(m: &Module, ty: &str) -> usize {
        m.cells
            .iter()
            .filter(|c| matches!(&c.kind, CellKind::Other(g) if g.ty == ty))
            .count()
    }

    #[test]
    fn a_chain_keeps_its_ends_and_shifts_the_rest_in_srls() {
        let (mut m, ..) = chain(70, None);
        assert_eq!(map(&mut m), (3, 68)); // 32, 32, 4
        assert_eq!(count(&m, FFS[0]), 2);
        let lens: Vec<u64> = m
            .cells
            .iter()
            .filter_map(|c| match &c.kind {
                CellKind::Other(g) if g.ty == super::super::cells::SRLC32E => Some(
                    g.port("A")
                        .unwrap()
                        .sig
                        .iter()
                        .enumerate()
                        .fold(0, |v, (i, b)| v | u64::from(*b == SigBit::ONE) << i)
                        + 1,
                ),
                _ => None,
            })
            .collect();
        assert_eq!(lens, [32, 32, 4]);
    }

    #[test]
    fn short_chains_and_resets_stay_registers() {
        let (mut m, ..) = chain(3, None);
        assert_eq!(map(&mut m), (0, 0));
        // A reset in the middle splits it into chains too short to shift.
        let (mut m, ..) = chain(6, Some(3));
        assert_eq!(map(&mut m), (0, 0));
        assert_eq!(count(&m, FFS[0]), 6);
    }
}
