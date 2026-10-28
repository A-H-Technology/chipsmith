//! Additions, subtractions and comparisons into `$alu` cells (yosys'
//! `alumacc`), which a device maps to its carry chains.
//!
//! A comparison is a subtraction whose flags give the answer: unsigned
//! `A < B` is no carry out of `A - B`, signed is overflow xor sign. A
//! comparison shares the `$alu` of a subtraction of the same operands
//! (`a - b` next to `a < b` and `$signed(a) < $signed(b)`, as a CPU's ALU
//! has them), or of an earlier comparison.
//!
//! A sum of more than two terms (`a + b + c - d`, adders feeding only
//! adders) becomes a balanced tree of two-operand adders, not yosys'
//! multi-operand `$macc`: the same carry chains as written, but
//! `ceil(log2(n))` of them deep instead of `n - 1`. A compressor tree in
//! front of one adder, as `maccmap` builds, costs a LUT per bit per
//! compressor on a device with carry chains and no ternary adder: on four
//! 18-bit terms 123 LUTs and 186 MHz through the yosys flow, against 90
//! LUTs and 204 MHz balanced (137 MHz as a chain).

use std::collections::HashMap;

use chipsmith_netlist::{
    Alu, Binary, BinaryOp, CellKind, Module, Net, Operand, SigBit, SigSpec, State, Unary, UnaryOp,
};

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

/// A term of a sum, extended to the sum's width.
#[derive(Clone)]
struct Term {
    sig: SigSpec,
    negative: bool,
}

/// Rewrites every sum of three or more terms as a balanced adder tree.
fn balance_sums(m: &mut Module) {
    let is_sum = |k: &CellKind| matches!(k, CellKind::Binary(b) if matches!(b.op, BinaryOp::Add | BinaryOp::Sub));
    let mut uses: HashMap<Net, usize> = HashMap::new();
    let mut driver: HashMap<Net, usize> = HashMap::new();
    for (i, c) in m.cells.iter().enumerate() {
        c.kind.for_each_port(|p| {
            for n in p.bits.iter().filter_map(|b| b.net()) {
                if p.dir == Some(chipsmith_netlist::Dir::Out) {
                    if is_sum(&c.kind) {
                        driver.insert(n, i);
                    }
                } else {
                    *uses.entry(n).or_default() += 1;
                }
            }
        });
    }
    for (_, w, _) in m.ports() {
        for n in w.bits.iter().filter_map(|b| b.net()) {
            *uses.entry(n).or_default() += 1;
        }
    }
    // The sum an operand reads, if it reads only that sum's low bits, wide
    // enough, and nothing else reads them.
    let inner = |op: &[SigBit], width: usize| -> Option<usize> {
        let i = *driver.get(&op.first()?.net()?)?;
        let CellKind::Binary(b) = &m.cells[i].kind else {
            return None;
        };
        (b.y.len() >= width
            && op.len() == width
            && b.y[..width] == *op
            && b.y.iter().all(|y| {
                y.net()
                    .is_some_and(|n| uses.get(&n).copied().unwrap_or(0) <= 1)
            }))
        .then_some(i)
    };
    let sum = |i: usize| match &m.cells[i].kind {
        CellKind::Binary(b) => b,
        _ => unreachable!("a sum"),
    };
    let mut absorbed = vec![false; m.cells.len()];
    let mut child: Vec<[Option<usize>; 2]> = vec![[None, None]; m.cells.len()];
    for (i, slots) in child.iter_mut().enumerate() {
        if !is_sum(&m.cells[i].kind) {
            continue;
        }
        let b = sum(i);
        let w = b.y.len();
        let ext = |o: &Operand| crate::techmap::extend(&o.sig, w, b.a.signed && b.b.signed);
        for (k, op) in [&b.a, &b.b].into_iter().enumerate() {
            if let Some(j) = inner(&ext(op), w).filter(|&j| j != i) {
                slots[k] = Some(j);
                absorbed[j] = true;
            }
        }
    }
    let mut trees = Vec::new();
    for (root, &inside) in absorbed.iter().enumerate() {
        if !is_sum(&m.cells[root].kind) || inside {
            continue;
        }
        let w = sum(root).y.len();
        let mut terms = Vec::new();
        let mut cells = Vec::new();
        let mut stack = vec![(root, false)];
        while let Some((i, neg)) = stack.pop() {
            cells.push(i);
            let b = sum(i);
            let signed = b.a.signed && b.b.signed;
            for (k, op) in [&b.a, &b.b].into_iter().enumerate() {
                let negative = neg ^ (k == 1 && b.op == BinaryOp::Sub);
                match child[i][k] {
                    Some(j) => stack.push((j, negative)),
                    None => terms.push(Term {
                        sig: crate::techmap::extend(&op.sig, w, signed),
                        negative,
                    }),
                }
            }
        }
        if terms.len() >= 3 {
            trees.push((sum(root).y.clone(), cells, terms));
        }
    }
    if trees.is_empty() {
        return;
    }
    let mut namer = crate::rewrite::Namer::new(m, "alumacc");
    let mut dead = vec![false; m.cells.len()];
    let mut new = Vec::new();
    for (y, cells, terms) in trees {
        let w = y.len();
        for c in cells {
            dead[c] = true;
        }
        let mut level = terms;
        while level.len() > 1 {
            let mut next = Vec::new();
            for pair in level.chunks(2) {
                let [a, b] = pair else {
                    next.push(pair[0].clone());
                    continue;
                };
                let out = if level.len() == 2 && !(a.negative && b.negative) {
                    y.clone()
                } else {
                    m.new_sig(w)
                };
                let (op, x, z, negative) = match (a.negative, b.negative) {
                    (false, true) => (BinaryOp::Sub, a, b, false),
                    (true, false) => (BinaryOp::Sub, b, a, false),
                    (n, _) => (BinaryOp::Add, a, b, n),
                };
                let operand = |t: &Term| Operand {
                    sig: t.sig.clone(),
                    signed: false,
                };
                new.push(CellKind::Binary(Binary {
                    op,
                    a: operand(x),
                    b: operand(z),
                    y: out.clone(),
                }));
                next.push(Term { sig: out, negative });
            }
            level = next;
        }
        let last = level.pop().expect("a sum has terms");
        if last.negative {
            new.push(CellKind::Unary(Unary {
                op: UnaryOp::Neg,
                a: Operand {
                    sig: last.sig,
                    signed: false,
                },
                y,
            }));
        }
    }
    let mut i = 0;
    m.cells.retain(|_| {
        i += 1;
        !dead[i - 1]
    });
    for k in new {
        let name = namer.fresh();
        m.add_cell(name, k);
    }
}

pub fn alumacc(m: &mut Module) {
    balance_sums(m);
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
