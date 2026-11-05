//! Proofs that one signal is never 1 on the clock edge after one where
//! another is: by evaluating both over every value of what they depend on.
//!
//! Two frames: the edge before (`t-1`), where every register and input is
//! free, and the edge itself (`t`), where a register on the clock is its
//! next-state function of frame `t-1` and the rest is free. Freeing frame
//! `t-1`'s registers drops what the design can reach, so a pair proven
//! never to follow is never followed in any run; one that fails may only
//! be unreachable. A third frame, power-up, checks the very first edge,
//! which has no edge before it: the registers at their power-up values.
//!
//! Cheap where the two signals are control logic (a one-hot state and a few
//! decoded instruction bits, as a CPU's register write and read enables):
//! anything depending on more than [`MAX_VARS`] bits is not proven.

use std::collections::HashMap;

use chipsmith_netlist::{
    CellKind, Ctrl, DffControl, Ff, FfKind, GateOp, Module, Net, Polarity, SigBit, State,
    SyncEnable, Word,
};
use rustc_hash::FxHashMap;

use crate::techmap;

/// 2^24 assignments, 64 at a time: well under a second.
const MAX_VARS: usize = 24;
/// Deeper than any control cone; keeps the recursion off a datapath.
const MAX_DEPTH: usize = 2_000;

/// Whether `late` is 0 on every edge of `clk` after one where any of
/// `early` is 1, and on the first edge after power-up (`inits`, a register's
/// power-up value by its Q net; one without is free).
pub fn never_after(
    m: &Module,
    clk: Ctrl,
    early: &[SigBit],
    late: SigBit,
    inits: &FxHashMap<Net, State>,
) -> bool {
    // Gates only, so the evaluator is small. The probe is a register
    // `lower` leaves in place, whose inputs it renames with the rest.
    let mut c = m.clone();
    let probe_q: Vec<SigBit> = (0..early.len() + 1)
        .map(|_| SigBit::Net(c.new_net()))
        .collect();
    let mut probe_d = early.to_vec();
    probe_d.push(late);
    const PROBE: &str = "$hazard_probe";
    c.add_cell(
        PROBE,
        CellKind::Ff(Ff {
            q: probe_q,
            kind: FfKind::Dff {
                clk,
                d: probe_d,
                control: DffControl::Plain { en: None },
            },
        }),
    );
    if techmap::lower(&mut c, techmap::is_lowerable).is_err() {
        return false;
    }
    let Some(CellKind::Ff(Ff {
        kind: FfKind::Dff { d, .. },
        ..
    })) = c.cells.iter().find(|x| x.name == PROBE).map(|x| &x.kind)
    else {
        return false;
    };
    let (early, late) = (d[..early.len()].to_vec(), d[early.len()]);
    let mut e = Eval::new(&c, clk, inits);
    let Some(step) = (|| {
        let mut any = e.node(SigBit::ZERO, Frame::Before, 0)?;
        for &b in &early {
            let n = e.node(b, Frame::Before, 0)?;
            any = e.push(Op::Or(any, n));
        }
        let l = e.node(late, Frame::Edge, 0)?;
        Some(e.push(Op::And(any, l)))
    })() else {
        return false;
    };
    let Some(first) = e.node(late, Frame::PowerUp, 0) else {
        return false;
    };
    e.always_zero(step) && e.always_zero(first)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Frame {
    Before,
    Edge,
    PowerUp,
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Var(usize),
    Const(bool),
    Not(usize),
    And(usize, usize),
    Or(usize, usize),
    Xor(usize, usize),
    /// `s ? b : a`
    Mux(usize, usize, usize),
}

struct Eval<'a> {
    m: &'a Module,
    clk: Ctrl,
    inits: &'a FxHashMap<Net, State>,
    gate_of: FxHashMap<Net, usize>,
    ff_of: FxHashMap<Net, (usize, usize)>,
    memo: HashMap<(Net, Frame), usize>,
    ops: Vec<Op>,
    vars: usize,
}

impl<'a> Eval<'a> {
    fn new(m: &'a Module, clk: Ctrl, inits: &'a FxHashMap<Net, State>) -> Eval<'a> {
        let mut gate_of = FxHashMap::default();
        let mut ff_of = FxHashMap::default();
        for (i, c) in m.cells.iter().enumerate() {
            match &c.kind {
                CellKind::Gate(g) => {
                    if let SigBit::Net(n) = g.y {
                        gate_of.insert(n, i);
                    }
                }
                CellKind::Ff(ff) => {
                    for (k, b) in ff.q.iter().enumerate() {
                        if let SigBit::Net(n) = b {
                            ff_of.insert(*n, (i, k));
                        }
                    }
                }
                _ => {}
            }
        }
        Eval {
            m,
            clk,
            inits,
            gate_of,
            ff_of,
            memo: HashMap::new(),
            ops: Vec::new(),
            vars: 0,
        }
    }

    fn push(&mut self, op: Op) -> usize {
        self.ops.push(op);
        self.ops.len() - 1
    }

    fn var(&mut self) -> Option<usize> {
        if self.vars == MAX_VARS {
            return None;
        }
        self.vars += 1;
        Some(self.push(Op::Var(self.vars - 1)))
    }

    fn ctrl(&mut self, c: Ctrl, f: Frame, depth: usize) -> Option<usize> {
        let s = self.node(c.sig, f, depth)?;
        Some(match c.polarity {
            Polarity::Pos => s,
            Polarity::Neg => self.push(Op::Not(s)),
        })
    }

    /// The node computing `b` in frame `f`; `None` past the limits.
    fn node(&mut self, b: SigBit, f: Frame, depth: usize) -> Option<usize> {
        if depth > MAX_DEPTH {
            return None;
        }
        let n = match b {
            SigBit::Const(State::Zero) => return Some(self.push(Op::Const(false))),
            SigBit::Const(State::One) => return Some(self.push(Op::Const(true))),
            // x or z: any value.
            SigBit::Const(_) => return self.var(),
            SigBit::Net(n) => n,
        };
        if let Some(&i) = self.memo.get(&(n, f)) {
            return Some(i);
        }
        let i = if let Some(&g) = self.gate_of.get(&n) {
            self.gate(g, f, depth)?
        } else if let Some(&(cell, bit)) = self.ff_of.get(&n) {
            self.register(cell, bit, f, depth)?
        } else {
            // An input, a memory's read data, a cell left as it is.
            self.var()?
        };
        self.memo.insert((n, f), i);
        Some(i)
    }

    fn gate(&mut self, g: usize, f: Frame, depth: usize) -> Option<usize> {
        let CellKind::Gate(gate) = &self.m.cells[g].kind else {
            unreachable!()
        };
        let op = gate.op();
        if op == GateOp::Tbuf {
            return self.var();
        }
        let ins: Vec<SigBit> = gate.inputs().to_vec();
        let mut x = Vec::with_capacity(ins.len());
        for b in ins {
            x.push(self.node(b, f, depth + 1)?);
        }
        Some(match op {
            GateOp::Buf => x[0],
            GateOp::Not => self.push(Op::Not(x[0])),
            GateOp::And => self.push(Op::And(x[0], x[1])),
            GateOp::Nand => {
                let y = self.push(Op::And(x[0], x[1]));
                self.push(Op::Not(y))
            }
            GateOp::Or => self.push(Op::Or(x[0], x[1])),
            GateOp::Nor => {
                let y = self.push(Op::Or(x[0], x[1]));
                self.push(Op::Not(y))
            }
            GateOp::Xor => self.push(Op::Xor(x[0], x[1])),
            GateOp::Xnor => {
                let y = self.push(Op::Xor(x[0], x[1]));
                self.push(Op::Not(y))
            }
            GateOp::AndNot => {
                let nb = self.push(Op::Not(x[1]));
                self.push(Op::And(x[0], nb))
            }
            GateOp::OrNot => {
                let nb = self.push(Op::Not(x[1]));
                self.push(Op::Or(x[0], nb))
            }
            GateOp::Mux => self.push(Op::Mux(x[2], x[1], x[0])),
            GateOp::Nmux => {
                let y = self.push(Op::Mux(x[2], x[1], x[0]));
                self.push(Op::Not(y))
            }
            GateOp::Aoi3 | GateOp::Oai3 | GateOp::Aoi4 | GateOp::Oai4 => {
                let (inner_and, n) = match op {
                    GateOp::Aoi3 => (true, 3),
                    GateOp::Oai3 => (false, 3),
                    GateOp::Aoi4 => (true, 4),
                    _ => (false, 4),
                };
                let pair = |e: &mut Self, a: usize, b: usize| {
                    e.push(if inner_and {
                        Op::And(a, b)
                    } else {
                        Op::Or(a, b)
                    })
                };
                let p = pair(self, x[0], x[1]);
                let q = if n == 3 { x[2] } else { pair(self, x[2], x[3]) };
                let y = self.push(if inner_and {
                    Op::Or(p, q)
                } else {
                    Op::And(p, q)
                });
                self.push(Op::Not(y))
            }
            GateOp::Tbuf => unreachable!(),
        })
    }

    /// A register's output: free before, its next value at the edge (on
    /// the clock, and of a kind modelled here), its power-up value first.
    fn register(&mut self, cell: usize, bit: usize, f: Frame, depth: usize) -> Option<usize> {
        let CellKind::Ff(ff) = &self.m.cells[cell].kind else {
            unreachable!()
        };
        let q = ff.q[bit];
        match f {
            Frame::Before => self.var(),
            Frame::PowerUp => match q.net().and_then(|n| self.inits.get(&n)) {
                Some(State::Zero) => Some(self.push(Op::Const(false))),
                Some(State::One) => Some(self.push(Op::Const(true))),
                _ => self.var(),
            },
            Frame::Edge => {
                let Some(next) = next_state(ff, bit, self.clk) else {
                    return self.var();
                };
                let d = depth + 1;
                let data = self.node(next.d, Frame::Before, d)?;
                let held = |e: &mut Self, v: usize, en: Option<Ctrl>| -> Option<usize> {
                    Some(match en {
                        None => v,
                        Some(en) => {
                            let s = e.ctrl(en, Frame::Before, d)?;
                            let old = e.node(q, Frame::Before, d)?;
                            e.push(Op::Mux(s, v, old))
                        }
                    })
                };
                match next.srst {
                    None => held(self, data, next.en),
                    Some((r, v, over_en)) => {
                        let r = self.ctrl(r, Frame::Before, d)?;
                        let v = self.push(Op::Const(v));
                        if over_en {
                            let inner = held(self, data, next.en)?;
                            Some(self.push(Op::Mux(r, v, inner)))
                        } else {
                            let inner = self.push(Op::Mux(r, v, data));
                            held(self, inner, next.en)
                        }
                    }
                }
            }
        }
    }

    /// Whether `node` is 0 under every assignment of the variables.
    fn always_zero(&self, node: usize) -> bool {
        const LANES: [u64; 6] = [
            0xAAAA_AAAA_AAAA_AAAA,
            0xCCCC_CCCC_CCCC_CCCC,
            0xF0F0_F0F0_F0F0_F0F0,
            0xFF00_FF00_FF00_FF00,
            0xFFFF_0000_FFFF_0000,
            0xFFFF_FFFF_0000_0000,
        ];
        let chunks: u64 = 1 << self.vars.saturating_sub(6);
        let mut v = vec![0u64; node + 1];
        for chunk in 0..chunks {
            for (i, op) in self.ops[..=node].iter().enumerate() {
                v[i] = match *op {
                    Op::Var(k) if k < 6 => LANES[k],
                    Op::Var(k) => {
                        if chunk >> (k - 6) & 1 == 1 {
                            !0
                        } else {
                            0
                        }
                    }
                    Op::Const(c) => {
                        if c {
                            !0
                        } else {
                            0
                        }
                    }
                    Op::Not(a) => !v[a],
                    Op::And(a, b) => v[a] & v[b],
                    Op::Or(a, b) => v[a] | v[b],
                    Op::Xor(a, b) => v[a] ^ v[b],
                    Op::Mux(s, b, a) => (v[s] & v[b]) | (!v[s] & v[a]),
                };
            }
            if v[node] != 0 {
                return false;
            }
        }
        true
    }
}

/// One register bit's next value, as data, enable and synchronous reset
/// (signal, value, whether it acts on disabled edges too).
struct Next {
    d: SigBit,
    en: Option<Ctrl>,
    srst: Option<(Ctrl, bool, bool)>,
}

fn next_state(ff: &Ff<Word>, bit: usize, clk: Ctrl) -> Option<Next> {
    let FfKind::Dff { clk: c, d, control } = &ff.kind else {
        return None;
    };
    if *c != clk {
        return None;
    }
    let d = d[bit];
    let value = |s: State| match s {
        State::Zero => Some(false),
        State::One => Some(true),
        _ => None,
    };
    Some(match control {
        DffControl::Plain { en } => Next {
            d,
            en: *en,
            srst: None,
        },
        DffControl::Sync { srst, value: v, en } => {
            let v = value(v.0[bit])?;
            match en {
                SyncEnable::Always => Next {
                    d,
                    en: None,
                    srst: Some((*srst, v, true)),
                },
                SyncEnable::ResetOverEnable(e) => Next {
                    d,
                    en: Some(*e),
                    srst: Some((*srst, v, true)),
                },
                SyncEnable::EnableOverReset(e) => Next {
                    d,
                    en: Some(*e),
                    srst: Some((*srst, v, false)),
                },
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_netlist::{Gate, Module};

    /// A two-state machine: `a` and `b` alternate. A write in `a` is never
    /// followed by a read in `a`, but is by one in `b`.
    #[test]
    fn alternating_states() {
        let mut m = Module::new("t");
        let clk = Ctrl::pos(SigBit::Net(m.new_net()));
        let a = SigBit::Net(m.new_net());
        let b = SigBit::Net(m.new_net());
        let ff = |q: SigBit, d: SigBit| {
            CellKind::Ff(Ff {
                q: vec![q],
                kind: FfKind::Dff {
                    clk,
                    d: vec![d],
                    control: DffControl::Plain { en: None },
                },
            })
        };
        // b' = a, a' = !a (b is a one edge late, not a's complement: the
        // proof can't know a and b differ, only what follows what).
        m.add_cell("b", ff(b, a));
        let na = SigBit::Net(m.new_net());
        m.add_cell(
            "na",
            CellKind::Gate(Gate::new(GateOp::Not, &[a], na).unwrap()),
        );
        m.add_cell("a", ff(a, na));
        let inits = FxHashMap::from_iter([(a.net().unwrap(), State::Zero)]);
        // a then a: never (a' = !a).
        assert!(never_after(&m, clk, &[a], a, &inits));
        // a then b: always (b' = a).
        assert!(!never_after(&m, clk, &[a], b, &inits));
        // b has no power-up value: the first edge may read in b.
        assert!(!never_after(&m, clk, &[SigBit::ZERO], b, &inits));
        // a powers up 0.
        assert!(never_after(&m, clk, &[SigBit::ZERO], a, &inits));
    }
}
