//! Additions, subtractions and comparisons into `$alu` cells (yosys'
//! `alumacc`), which a device maps to its carry chains.
//!
//! A comparison is a subtraction whose flags give the answer: unsigned
//! `A < B` is no carry out of `A - B`, signed is overflow xor sign, and
//! `A == B` is every bit of `A ^ ~B` set. Comparisons of the same operands
//! share one subtraction. Unlike yosys, sums of more than two terms aren't
//! merged into one multi-operand adder: each `+` is its own `$alu`.

use std::collections::HashMap;

use chipsmith_netlist::{Alu, BinaryOp, CellKind, Module, Operand, SigBit, SigSpec, UnaryOp};

use crate::rewrite::{Gates, Subst};
use crate::techmap::extend;

/// One subtraction several comparisons read.
struct Cmp {
    alu: Alu,
}

impl Cmp {
    fn lt(&self, g: &mut Gates, signed: bool) -> SigBit {
        let w = self.alu.y.len();
        if signed {
            let carry_in = if w >= 2 { self.alu.co[w - 2] } else { self.alu.ci };
            let of = g.xor(self.alu.co[w - 1], carry_in);
            g.xor(of, self.alu.y[w - 1])
        } else {
            g.not(self.alu.co[w - 1])
        }
    }

    fn eq(&self, g: &mut Gates) -> SigBit {
        g.reduce_and(&self.alu.x)
    }
}

pub fn alumacc(m: &mut Module) {
    let mut g = Gates::new(m, "alumacc");
    let cells = std::mem::take(&mut g.m.cells);
    let mut subst = Subst::new(g.m);
    let mut kept = Vec::new();
    let mut alus: Vec<Alu> = Vec::new();
    // Comparisons by (A, B, signed), sharing the subtraction; in a `Vec`
    // so the cells come out in a fixed order.
    let mut cmps: Vec<Cmp> = Vec::new();
    let mut cmp_of: HashMap<(SigSpec, SigSpec, bool), usize> = HashMap::new();
    let mut pending: Vec<(BinaryOp, usize, bool, bool, SigSpec)> = Vec::new();
    for c in cells {
        match &c.kind {
            CellKind::Binary(b) if matches!(b.op, BinaryOp::Add | BinaryOp::Sub) => {
                let sub = b.op == BinaryOp::Sub;
                let signed = b.a.signed && b.b.signed;
                let w = b.y.len();
                let k = SigBit::from(chipsmith_netlist::State::from(sub));
                alus.push(Alu {
                    a: Operand {
                        sig: b.a.sig.clone(),
                        signed,
                    },
                    b: Operand {
                        sig: b.b.sig.clone(),
                        signed,
                    },
                    ci: k,
                    bi: k,
                    x: g.m.new_sig(w),
                    y: b.y.clone(),
                    co: g.m.new_sig(w),
                });
            }
            CellKind::Unary(u) if u.op == UnaryOp::Neg => {
                let w = u.y.len();
                alus.push(Alu {
                    a: Operand {
                        sig: vec![SigBit::ZERO],
                        signed: u.a.signed,
                    },
                    b: u.a.clone(),
                    ci: SigBit::ONE,
                    bi: SigBit::ONE,
                    x: g.m.new_sig(w),
                    y: u.y.clone(),
                    co: g.m.new_sig(w),
                });
            }
            CellKind::Binary(b)
                if matches!(
                    b.op,
                    BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
                ) =>
            {
                let signed = b.a.signed && b.b.signed;
                let w = b.a.sig.len().max(b.b.sig.len());
                // Both operands at the subtraction's width, so `A < B` and
                // `B > A` are one key.
                let (a, bb) = (extend(&b.a.sig, w, signed), extend(&b.b.sig, w, signed));
                let (key, swapped) = if cmp_of.contains_key(&(bb.clone(), a.clone(), signed)) {
                    ((bb, a, signed), true)
                } else {
                    ((a, bb, signed), false)
                };
                let i = *cmp_of.entry(key.clone()).or_insert_with(|| {
                    cmps.push(Cmp {
                    alu: Alu {
                        a: Operand {
                            sig: key.0.clone(),
                            signed,
                        },
                        b: Operand {
                            sig: key.1.clone(),
                            signed,
                        },
                        ci: SigBit::ONE,
                        bi: SigBit::ONE,
                        x: g.m.new_sig(w),
                        y: g.m.new_sig(w),
                        co: g.m.new_sig(w),
                    },
                    });
                    cmps.len() - 1
                });
                pending.push((b.op, i, key.2, swapped, b.y.clone()));
            }
            _ => kept.push(c),
        }
    }
    for (op, i, signed, swapped, y) in pending {
        let cmp = &cmps[i];
        // `B op A` read off `A - B`: less and greater trade places.
        let less = matches!(op, BinaryOp::Lt | BinaryOp::Le) != swapped;
        let or_equal = matches!(op, BinaryOp::Le | BinaryOp::Ge);
        let lt = cmp.lt(&mut g, signed);
        let eq = cmp.eq(&mut g);
        let strict = if less {
            lt
        } else {
            let le = g.or(lt, eq);
            g.not(le)
        };
        let bit = if or_equal { g.or(strict, eq) } else { strict };
        for (i, yb) in y.iter().enumerate() {
            if let SigBit::Net(n) = yb {
                subst.set(*n, if i == 0 { bit } else { SigBit::ZERO });
            }
        }
    }
    for alu in alus.into_iter().chain(cmps.into_iter().map(|c| c.alu)) {
        g.add(CellKind::Alu(alu));
    }
    g.m.cells.extend(kept);
    subst.apply(m);
}
