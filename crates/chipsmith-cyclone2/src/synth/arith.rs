//! `$alu` onto the LE's arithmetic mode, as cyclone2-re's `arith_map.v`:
//! each LE adds one bit (DATAA = A, DATAB = B, CIN = carry in); the upper
//! half of its mask (DATAD tied 1) is the sum, the lower half (what COUT
//! always uses, DATAD = 0) the carry. A constant carry-in folds into bit 0,
//! whose mask then ignores C, so the chain head's hardware carry-in doesn't
//! matter; a signal carry-in gets one extra LE in front. The carry out of
//! the top bit leaves the chain through one extra LE (COMBOUT = CIN); the
//! other carry outs are recovered from the sums.

use chipsmith_netlist::{Alu, CellKind, Module, SigBit, SigSpec, State};
use chipsmith_synth::rewrite::{Gates, Subst};
use chipsmith_synth::techmap::extend;

use super::cells::Comb;

/// Adders this short are cheaper as plain LUTs.
const MIN_WIDTH: usize = 3;

/// Where an LE's C input comes from.
#[derive(Clone, Copy)]
enum CarryIn {
    /// The chain: C is CIN.
    Chain,
    /// The constant carry-in of the chain's head.
    Const(bool),
}

/// The mask of one adder LE: sum on the upper half, carry on the lower.
fn mask(carry: CarryIn, invert_b: bool) -> u16 {
    (0..16).fold(0u16, |m, idx| {
        let a = idx & 1 == 1;
        let b = (idx >> 1 & 1 == 1) ^ invert_b;
        let c = match carry {
            CarryIn::Chain => idx >> 2 & 1 == 1,
            CarryIn::Const(v) => v,
        };
        let d = idx >> 3 & 1 == 1;
        let bit = if d {
            a ^ b ^ c
        } else {
            (a & b) | (a & c) | (b & c)
        };
        m | (u16::from(bit) << idx)
    })
}

fn const_bit(b: SigBit) -> Option<bool> {
    match b {
        SigBit::Const(State::Zero) => Some(false),
        SigBit::Const(State::One) => Some(true),
        _ => None,
    }
}

/// Maps every `$alu` at least [`MIN_WIDTH`] wide; returns how many chains.
pub fn map(m: &mut Module) -> usize {
    let mut g = Gates::new(m, "arith");
    let cells = std::mem::take(&mut g.m.cells);
    let mut subst = Subst::new(g.m);
    let mut kept = Vec::new();
    let mut chains = 0;
    for c in cells {
        match c.kind {
            CellKind::Alu(alu) if alu.y.len() >= MIN_WIDTH => {
                chain(&mut g, &mut subst, &alu);
                chains += 1;
            }
            kind => kept.push(chipsmith_netlist::Cell { kind, ..c }),
        }
    }
    g.m.cells.extend(kept);
    subst.apply(m);
    chains
}

fn chain(g: &mut Gates, subst: &mut Subst, alu: &Alu) {
    let w = alu.y.len();
    let mut a = extend(&alu.a.sig, w, alu.a.signed);
    let mut b = extend(&alu.b.sig, w, alu.b.signed);
    // A constant BI folds into the masks; a signal one inverts B in front.
    let bi = const_bit(alu.bi);
    // In a sum the operands can trade places: the one with more constant
    // bits goes on DATAA, where the yosys flow ends up with it, leaving the
    // signal on DATAB. Measured on blinky's counter (same LEs, same
    // registers): 3.75 ns critical path with the counter on DATAA, 3.37 ns
    // on DATAB.
    let consts = |s: &[SigBit]| s.iter().filter(|b| const_bit(**b).is_some()).count();
    if bi == Some(false) && consts(&b) > consts(&a) {
        std::mem::swap(&mut a, &mut b);
    }
    let invert_b = bi == Some(true);
    let bb: SigSpec = match bi {
        Some(_) => b.clone(),
        None => b
            .iter()
            .map(|&x| {
                let nx = g.not(x);
                g.mux(x, nx, alu.bi)
            })
            .collect(),
    };
    let ci = const_bit(alu.ci);

    // The carry into each bit, and out of the top one.
    let mut carry: SigSpec = Vec::with_capacity(w + 1);
    if ci.is_none() {
        let c0 = SigBit::Net(g.m.new_net());
        g.add(
            Comb {
                mask: 0x00AA,
                data: [alu.ci, SigBit::ONE, SigBit::ONE, SigBit::ONE],
                cin: SigBit::ONE,
                combout: None,
                cout: Some(c0),
            }
            .cell(),
        );
        carry.push(c0);
    } else {
        carry.push(SigBit::ONE);
    }
    let y: SigSpec = (0..w).map(|_| SigBit::Net(g.m.new_net())).collect();
    for i in 0..w {
        let next = SigBit::Net(g.m.new_net());
        let (carry_in, cin) = match ci {
            Some(v) if i == 0 => (CarryIn::Const(v), SigBit::ONE),
            _ => (CarryIn::Chain, carry[i]),
        };
        g.add(
            Comb {
                mask: mask(carry_in, invert_b),
                data: [a[i], bb[i], SigBit::ONE, SigBit::ONE],
                cin,
                combout: Some(y[i]),
                cout: Some(next),
            }
            .cell(),
        );
        carry.push(next);
    }
    let top = SigBit::Net(g.m.new_net());
    g.add(
        Comb {
            mask: 0xF000,
            data: [SigBit::ONE; 4],
            cin: carry[w],
            combout: Some(top),
            cout: None,
        }
        .cell(),
    );

    // X = A ^ B as the adder sees it; CO from the sums.
    let x: SigSpec = (0..w)
        .map(|i| {
            let bx = if invert_b { g.not(b[i]) } else { bb[i] };
            g.xor(a[i], bx)
        })
        .collect();
    let mut co: SigSpec = (1..w).map(|i| g.xor(y[i], x[i])).collect();
    co.push(top);
    for (outs, vals) in [(&alu.y, &y), (&alu.x, &x), (&alu.co, &co)] {
        for (o, v) in outs.iter().zip(vals.iter()) {
            if let SigBit::Net(n) = o {
                subst.set(*n, *v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_match_arith_map() {
        // arith_map.v's lcmask(0, 0): sum a^b^c on top, majority below.
        assert_eq!(mask(CarryIn::Chain, false), 0x96E8);
        // Head with carry-in 0: a + b, carry a & b.
        assert_eq!(mask(CarryIn::Const(false), false), 0x6688);
        // Subtraction head (B inverted, carry-in 1): a - b.
        assert_eq!(mask(CarryIn::Const(true), true), 0x66BB);
    }
}
