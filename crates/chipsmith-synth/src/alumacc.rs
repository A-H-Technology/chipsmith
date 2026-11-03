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

/// A comparison with a constant on this many bits or fewer stays logic:
/// a function of that many bits is a LUT or a few (`step >= 3 && step <=
/// 10` on 4 bits: one), where a carry chain takes an LE a bit and more for
/// the flags. sha256: 3,706 -> 3,674 LEs; sdram-selftest 761 -> 752 and
/// 147 -> 160 MHz (quartus_sta, two placement seeds each), the chain it
/// had on its critical path gone.
const SMALL_COMPARE: usize = 8;

fn small_constant_compare(b: &Binary) -> bool {
    let constant = |s: &SigSpec| s.iter().all(|x| matches!(x, SigBit::Const(_)));
    let w = b.a.sig.len().max(b.b.sig.len());
    w <= SMALL_COMPARE && (constant(&b.a.sig) || constant(&b.b.sig))
}

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

/// How a sum of three or more terms becomes a tree of two-operand adders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SumOrder {
    /// Balanced, pairing the terms as written.
    AsWritten,
    /// Balanced by arrival, Huffman-style: the two terms ready first are
    /// added first, so a deep term (another sum's result) goes in last.
    /// sha256's `t1 + sum0 + maj` as written added `t1`, three adders
    /// deep, first, and `w_data` went through five adders to `a_reg`;
    /// Quartus' path through it has four.
    Arrival,
}

/// Logic levels a sum counts for in [`arrivals`]: its carry chain and the
/// hop into it are about as slow as a LUT level and a half, and a LUT
/// takes two or three levels of two-input gates.
const SUM_LEVELS: u32 = 4;

/// Each net's depth from a register, memory or input in logic levels
/// (a gate, multiplexer or comparison 1, a LUT 2, a sum `SUM_LEVELS`).
fn arrivals(m: &Module) -> HashMap<Net, u32> {
    let combinational = |k: &CellKind| {
        !matches!(
            k,
            CellKind::Ff(_) | CellKind::FfGate(_) | CellKind::Mem(_) | CellKind::Other(_)
        )
    };
    let cost = |k: &CellKind| match k {
        CellKind::Binary(b) if matches!(b.op, BinaryOp::Add | BinaryOp::Sub) => SUM_LEVELS,
        CellKind::Alu(_) => SUM_LEVELS,
        CellKind::Unary(u) if u.op == UnaryOp::Neg => SUM_LEVELS,
        CellKind::Lut(_) => 2,
        _ => 1,
    };
    let mut driver: HashMap<Net, usize> = HashMap::new();
    for (i, c) in m.cells.iter().enumerate().filter(|(_, c)| combinational(&c.kind)) {
        c.kind.for_each_port(|p| {
            if p.dir == Some(chipsmith_netlist::Dir::Out) {
                for n in p.bits.iter().filter_map(|b| b.net()) {
                    driver.insert(n, i);
                }
            }
        });
    }
    let inputs = |i: usize| {
        let mut v = Vec::new();
        m.cells[i].kind.for_each_port(|p| {
            if p.dir != Some(chipsmith_netlist::Dir::Out) {
                v.extend(p.bits.iter().filter_map(|b| b.net()).filter_map(|n| driver.get(&n).copied()));
            }
        });
        v
    };
    // Depth-first with an explicit stack; a cell met again while open (a
    // combinational loop) counts as ready.
    let mut level: Vec<Option<u32>> = vec![None; m.cells.len()];
    let mut open = vec![false; m.cells.len()];
    for root in 0..m.cells.len() {
        if level[root].is_some() || !combinational(&m.cells[root].kind) {
            continue;
        }
        let mut stack = vec![root];
        while let Some(&i) = stack.last() {
            if level[i].is_some() {
                stack.pop();
                continue;
            }
            let ins = inputs(i);
            if !open[i] {
                open[i] = true;
                stack.extend(ins.iter().filter(|&&j| level[j].is_none() && !open[j]));
                continue;
            }
            let ready = ins.iter().map(|&j| level[j].unwrap_or(0)).max().unwrap_or(0);
            level[i] = Some(ready + cost(&m.cells[i].kind));
            stack.pop();
        }
    }
    driver
        .into_iter()
        .map(|(n, i)| (n, level[i].unwrap_or(0)))
        .collect()
}

/// Rewrites every sum of three or more terms as a balanced adder tree.
fn balance_sums(m: &mut Module, order: SumOrder) {
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
    let arrival = match order {
        SumOrder::AsWritten => HashMap::new(),
        SumOrder::Arrival => arrivals(m),
    };
    let ready = |t: &Term| {
        t.sig
            .iter()
            .filter_map(|b| b.net())
            .map(|n| arrival.get(&n).copied().unwrap_or(0))
            .max()
            .unwrap_or(0)
    };
    let mut namer = crate::rewrite::Namer::new(m, "alumacc");
    let mut dead = vec![false; m.cells.len()];
    let mut new = Vec::new();
    for (y, cells, terms) in trees {
        let w = y.len();
        for c in cells {
            dead[c] = true;
        }
        let mut level = terms;
        if order == SumOrder::Arrival {
            // Earliest first; a sum joins after the terms as ready as it.
            let mut pending: Vec<(u32, Term)> = level.into_iter().map(|t| (ready(&t), t)).collect();
            while pending.len() > 1 {
                let first = (0..pending.len()).min_by_key(|&i| (pending[i].0, i)).expect("terms");
                let (ra, a) = pending.remove(first);
                let second = (0..pending.len()).min_by_key(|&i| (pending[i].0, i)).expect("terms");
                let (rb, b) = pending.remove(second);
                let out = if pending.is_empty() && !(a.negative && b.negative) {
                    y.clone()
                } else {
                    m.new_sig(w)
                };
                let (op, x, z, negative) = match (a.negative, b.negative) {
                    (false, true) => (BinaryOp::Sub, &a, &b, false),
                    (true, false) => (BinaryOp::Sub, &b, &a, false),
                    (n, _) => (BinaryOp::Add, &a, &b, n),
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
                pending.push((ra.max(rb) + SUM_LEVELS, Term { sig: out, negative }));
            }
            level = pending.into_iter().map(|(_, t)| t).collect();
        }
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
    alumacc_with(m, SumOrder::AsWritten)
}

pub fn alumacc_with(m: &mut Module, order: SumOrder) {
    balance_sums(m, order);
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
                ) && !small_constant_compare(b) =>
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

#[cfg(test)]
mod tests {
    use super::*;

    fn compare(m: &mut Module, op: BinaryOp, a: SigSpec, b: SigSpec) {
        let y = m.new_sig(1);
        let operand = |sig| Operand { sig, signed: false };
        m.add_cell(
            "cmp",
            CellKind::Binary(Binary {
                op,
                a: operand(a),
                b: operand(b),
                y,
            }),
        );
    }

    fn alus(m: &Module) -> usize {
        m.cells
            .iter()
            .filter(|c| matches!(c.kind, CellKind::Alu(_)))
            .count()
    }

    fn binary(m: &mut Module, op: BinaryOp, a: &SigSpec, b: &SigSpec) -> SigSpec {
        let y = m.new_sig(a.len());
        let operand = |sig: &SigSpec| Operand {
            sig: sig.clone(),
            signed: false,
        };
        let name = format!("c{}", m.cells.len());
        m.add_cell(
            name,
            CellKind::Binary(Binary {
                op,
                a: operand(a),
                b: operand(b),
                y: y.clone(),
            }),
        );
        y
    }

    /// sha256's `a_new = t1 + sum0 + maj`, `t1` itself a sum (read twice,
    /// so a term here): by arrival, `sum0 + maj` comes first and `t1` goes
    /// into the last adder; as written, `t1 + sum0` came first.
    #[test]
    fn a_late_term_goes_into_the_last_adder() {
        let build = |order: SumOrder| {
            let mut m = Module::new("top");
            let (h, w, sum0, maj, d) = (m.new_sig(8), m.new_sig(8), m.new_sig(8), m.new_sig(8), m.new_sig(8));
            let t1 = binary(&mut m, BinaryOp::Add, &h, &w);
            let e = binary(&mut m, BinaryOp::Add, &d, &t1);
            let t2 = binary(&mut m, BinaryOp::Add, &sum0, &maj);
            let a = binary(&mut m, BinaryOp::Add, &t1, &t2);
            for (name, y) in [("e", e), ("a", a.clone())] {
                let mut wire = chipsmith_netlist::Wire::new(name, y);
                wire.port = Some(chipsmith_netlist::Dir::Out);
                m.wires.push(wire);
            }
            alumacc_with(&mut m, order);
            // The adder whose result is `a`: does it read `t1` itself?
            m.cells.iter().any(|c| match &c.kind {
                CellKind::Alu(alu) => alu.y == a && (alu.a.sig == t1 || alu.b.sig == t1),
                _ => false,
            })
        };
        assert!(build(SumOrder::Arrival), "t1 into the last adder");
        assert!(!build(SumOrder::AsWritten), "as written, t1 first");
    }

    /// A comparison with a constant on a few bits stays logic for the LUT
    /// mapper; a wider one, or one of two signals, takes a carry chain.
    #[test]
    fn small_constant_compares_stay_logic() {
        let constant = |v: usize, w: usize| -> SigSpec {
            (0..w).map(|i| SigBit::Const(State::from(v >> i & 1 == 1))).collect()
        };
        let mut m = Module::new("top");
        let step = m.new_sig(4);
        compare(&mut m, BinaryOp::Ge, step.clone(), constant(3, 4));
        alumacc(&mut m);
        assert_eq!(alus(&m), 0, "4 bits against a constant");

        let mut m = Module::new("top");
        let wide = m.new_sig(10);
        compare(&mut m, BinaryOp::Ge, wide, constant(700, 10));
        alumacc(&mut m);
        assert_eq!(alus(&m), 1, "10 bits against a constant");

        let mut m = Module::new("top");
        let (a, b) = (m.new_sig(4), m.new_sig(4));
        compare(&mut m, BinaryOp::Lt, a, b);
        alumacc(&mut m);
        assert_eq!(alus(&m), 1, "two signals");
    }
}
