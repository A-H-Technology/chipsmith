//! Additions, subtractions and comparisons into `$alu` cells (yosys'
//! `alumacc`), which a device maps to its carry chains.
//!
//! A comparison is a subtraction whose flags give the answer: unsigned
//! `A < B` is no carry out of `A - B`, signed is overflow xor sign. A
//! comparison shares the `$alu` of a subtraction of the same operands
//! (`a - b` next to `a < b` and `$signed(a) < $signed(b)`, as a CPU's ALU
//! has them), or of an earlier comparison. Unlike yosys, sums of more
//! than two terms aren't merged into one multi-operand adder.

use std::collections::HashMap;

use chipsmith_netlist::{Alu, BinaryOp, CellKind, Module, Operand, SigBit, SigSpec, State, UnaryOp};

use crate::rewrite::{Gates, Subst};

/// An `$alu` being built.
struct Node {
    alu: Alu,
    /// `A - B` (so comparisons can read it).
    subtract: bool,
    signed: bool,
}

impl Node {
    fn width(&self) -> usize {
        self.alu.y.len()
    }

    /// Whether a comparison of `a` and `b` can read this node: yosys'
    /// `cmp_mergeable`, keeping signedness mixes to equal widths with no
    /// extension at all.
    fn fits(&self, a: &SigSpec, b: &SigSpec, signed: bool) -> bool {
        let w = a.len().max(b.len());
        self.subtract
            && w <= self.width()
            && (signed == self.signed || (a.len() == b.len() && w == self.width()))
    }

    fn lt(&self, g: &mut Gates, signed: bool, cmp_width: usize) -> SigBit {
        let w = self.width();
        let co = &self.alu.co;
        if signed {
            if self.signed && w > cmp_width {
                // Sign-extended past the operands: no overflow, the sign is
                // the answer.
                return self.alu.y[w - 1];
            }
            let carry_in = if w >= 2 { co[w - 2] } else { self.alu.ci };
            let of = g.xor(co[w - 1], carry_in);
            g.xor(of, self.alu.y[w - 1])
        } else {
            g.not(co[w - 1])
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
    let mut nodes: Vec<Node> = Vec::new();
    // Subtractions and comparisons by their operands.
    let mut by_operands: HashMap<(SigSpec, SigSpec), Vec<usize>> = HashMap::new();
    let mut compares = Vec::new();
    for c in cells {
        match &c.kind {
            CellKind::Binary(b) if matches!(b.op, BinaryOp::Add | BinaryOp::Sub) => {
                let sub = b.op == BinaryOp::Sub;
                let signed = b.a.signed && b.b.signed;
                let w = b.y.len();
                let k = SigBit::from(State::from(sub));
                if sub {
                    by_operands
                        .entry((b.a.sig.clone(), b.b.sig.clone()))
                        .or_default()
                        .push(nodes.len());
                }
                nodes.push(Node {
                    alu: Alu {
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
                    },
                    subtract: sub,
                    signed,
                });
            }
            CellKind::Unary(u) if u.op == UnaryOp::Neg => {
                let w = u.y.len();
                nodes.push(Node {
                    alu: Alu {
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
                    },
                    subtract: false,
                    signed: u.a.signed,
                });
            }
            CellKind::Binary(b)
                if matches!(
                    b.op,
                    BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
                ) =>
            {
                compares.push(b.clone())
            }
            _ => kept.push(c),
        }
    }
    for b in compares {
        let signed = b.a.signed && b.b.signed;
        let (sa, sb) = (b.a.sig.clone(), b.b.sig.clone());
        let mut less = matches!(b.op, BinaryOp::Lt | BinaryOp::Le);
        let or_equal = matches!(b.op, BinaryOp::Le | BinaryOp::Ge);
        let find = |key: &(SigSpec, SigSpec), nodes: &[Node]| {
            by_operands.get(key).and_then(|v| {
                v.iter()
                    .copied()
                    .find(|&i| nodes[i].fits(&key.0, &key.1, signed))
            })
        };
        let node = match find(&(sa.clone(), sb.clone()), &nodes) {
            Some(i) => i,
            None => match find(&(sb.clone(), sa.clone()), &nodes) {
                Some(i) => {
                    // `B op A` read off `A - B`: less and greater trade places.
                    less = !less;
                    i
                }
                None => {
                    let w = sa.len().max(sb.len());
                    by_operands
                        .entry((sa.clone(), sb.clone()))
                        .or_default()
                        .push(nodes.len());
                    nodes.push(Node {
                        alu: Alu {
                            a: Operand {
                                sig: sa.clone(),
                                signed,
                            },
                            b: Operand {
                                sig: sb.clone(),
                                signed,
                            },
                            ci: SigBit::ONE,
                            bi: SigBit::ONE,
                            x: g.m.new_sig(w),
                            y: g.m.new_sig(w),
                            co: g.m.new_sig(w),
                        },
                        subtract: true,
                        signed,
                    });
                    nodes.len() - 1
                }
            },
        };
        let n = &nodes[node];
        let lt = n.lt(&mut g, signed, sa.len().max(sb.len()));
        let eq = n.eq(&mut g);
        let strict = if less {
            lt
        } else {
            let le = g.or(lt, eq);
            g.not(le)
        };
        let bit = if or_equal { g.or(strict, eq) } else { strict };
        for (i, yb) in b.y.iter().enumerate() {
            if let SigBit::Net(net) = yb {
                subst.set(*net, if i == 0 { bit } else { SigBit::ZERO });
            }
        }
    }
    for n in nodes {
        g.add(CellKind::Alu(n.alu));
    }
    g.m.cells.extend(kept);
    subst.apply(m);
}
