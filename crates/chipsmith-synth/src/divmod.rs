//! Division and modulo (`$div`, `$mod`, `$divfloor`, `$modfloor`) into
//! subtractions, comparisons and multiplexers, which `alumacc` and the
//! device's carry chains then take: yosys' `techmap.v`
//! (`$__div_mod_u`, `$__div_mod_trunc`, `$__div_mod_floor`), so a division
//! by zero gives what yosys' model of it gives (all ones, the dividend as
//! the remainder).
//!
//! Long division of `W` bits is `W` stages, each a compare-and-subtract of
//! the partial remainder's top bits by the divisor. Unlike `techmap.v`, a
//! stage only spans the bits it can change (`i + 1` of them, with the
//! divisor's higher bits making the quotient bit 0 when any is set), so a
//! 32-bit division is about half the LEs before `wreduce`. A constant
//! divisor drops the stages it can't fit in, and a power of two is wiring.

use chipsmith_netlist::{
    Binary, BinaryOp, CellKind, Module, Mux, Operand, SigBit, SigSpec, State, Unary, UnaryOp,
};

use crate::rewrite::{Namer, Subst};
use crate::techmap::extend;

/// Word-level cells added to a module.
struct Words<'m> {
    m: &'m mut Module,
    namer: Namer,
}

impl Words<'_> {
    fn add(&mut self, kind: CellKind) {
        let name = self.namer.fresh();
        self.m.add_cell(name, kind);
    }

    fn binary(&mut self, op: BinaryOp, a: &[SigBit], b: &[SigBit], width: usize) -> SigSpec {
        let y = self.m.new_sig(width);
        self.add(CellKind::Binary(Binary {
            op,
            a: unsigned(a),
            b: unsigned(b),
            y: y.clone(),
        }));
        y
    }

    fn unary(&mut self, op: UnaryOp, a: &[SigBit], width: usize) -> SigSpec {
        let y = self.m.new_sig(width);
        self.add(CellKind::Unary(Unary {
            op,
            a: unsigned(a),
            y: y.clone(),
        }));
        y
    }

    fn bit(&mut self, op: UnaryOp, a: &[SigBit]) -> SigBit {
        match a {
            [] => SigBit::from(State::from(matches!(op, UnaryOp::LogicNot))),
            _ => self.unary(op, a, 1)[0],
        }
    }

    /// `s ? b : a`.
    fn mux(&mut self, a: SigSpec, b: SigSpec, s: SigBit) -> SigSpec {
        match s {
            SigBit::ZERO => a,
            SigBit::ONE => b,
            _ => {
                let y = self.m.new_sig(a.len());
                self.add(CellKind::Mux(Mux {
                    a,
                    b,
                    s,
                    y: y.clone(),
                }));
                y
            }
        }
    }

    fn neg(&mut self, a: &[SigBit]) -> SigSpec {
        self.unary(UnaryOp::Neg, a, a.len())
    }

    /// `s ? -a : a`.
    fn neg_if(&mut self, a: SigSpec, s: SigBit) -> SigSpec {
        if s == SigBit::ZERO {
            return a;
        }
        let n = self.neg(&a);
        self.mux(a, n, s)
    }

    /// Unsigned quotient and remainder of two `W`-bit words.
    fn divmod_u(&mut self, a: &[SigBit], b: &[SigBit]) -> (SigSpec, SigSpec) {
        let w = a.len();
        if let Some(k) = power_of_two(b) {
            let q = (0..w).map(|i| a.get(i + k).copied().unwrap_or(SigBit::ZERO));
            let r = (0..w).map(|i| if i < k { a[i] } else { SigBit::ZERO });
            return (q.collect(), r.collect());
        }
        let mut q = vec![SigBit::ZERO; w];
        let mut rem: SigSpec = a.to_vec();
        for i in 0..w {
            // The divisor shifted up by `k` against the remainder: only
            // its top `i + 1` bits are compared and changed.
            let k = w - 1 - i;
            let high = self.bit(UnaryOp::ReduceOr, &b[i + 1..]);
            if high == SigBit::ONE {
                continue;
            }
            let top = rem[k..].to_vec();
            let low = &b[..=i];
            let ge = self.binary(BinaryOp::Ge, &top, low, 1)[0];
            let fits = match high {
                SigBit::ZERO => ge,
                _ => {
                    let none = self.bit(UnaryOp::LogicNot, &[high]);
                    self.binary(BinaryOp::And, &[ge], &[none], 1)[0]
                }
            };
            let diff = self.binary(BinaryOp::Sub, &top, low, i + 1);
            let next = self.mux(top, diff, fits);
            rem.splice(k.., next);
            q[k] = fits;
        }
        (q, rem)
    }
}

fn unsigned(sig: &[SigBit]) -> Operand {
    Operand {
        sig: sig.to_vec(),
        signed: false,
    }
}

/// `k` if `b` is the constant `2^k`.
fn power_of_two(b: &[SigBit]) -> Option<usize> {
    let mut one = None;
    for (i, &bit) in b.iter().enumerate() {
        match bit {
            SigBit::ZERO => {}
            SigBit::ONE if one.is_none() => one = Some(i),
            _ => return None,
        }
    }
    one
}

/// Lowers every division and modulo. Returns whether there was one.
pub fn lower(m: &mut Module) -> bool {
    let (div, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut m.cells).into_iter().partition(|c| {
        matches!(&c.kind, CellKind::Binary(b) if matches!(
            b.op,
            BinaryOp::Div | BinaryOp::Mod | BinaryOp::DivFloor | BinaryOp::ModFloor
        ))
    });
    m.cells = keep;
    if div.is_empty() {
        return false;
    }
    let mut subst = Subst::new(m);
    let namer = Namer::new(m, "divmod");
    let mut w = Words { m, namer };
    for c in div {
        let CellKind::Binary(b) = c.kind else {
            unreachable!("partitioned on Binary")
        };
        let y = lower_one(&mut w, &b);
        for (o, v) in b.y.iter().zip(y) {
            if let SigBit::Net(n) = o {
                subst.set(*n, v);
            }
        }
    }
    subst.apply(m);
    true
}

/// `techmap.v`'s `$__div_mod_trunc` and `$__div_mod_floor`: magnitudes
/// divided unsigned, signs put back.
fn lower_one(w: &mut Words, c: &Binary) -> SigSpec {
    let width = c.a.sig.len().max(c.b.sig.len()).max(c.y.len());
    let a = extend(&c.a.sig, width, c.a.signed);
    let b = extend(&c.b.sig, width, c.b.signed);
    let sign = |s: &SigSpec, signed: bool| if signed { s[width - 1] } else { SigBit::ZERO };
    let (sa, sb) = (sign(&a, c.a.signed), sign(&b, c.b.signed));
    let au = w.neg_if(a, sa);
    let bu = w.neg_if(b.clone(), sb);
    let (qu, ru) = w.divmod_u(&au, &bu);
    let both = c.a.signed && c.b.signed;
    let differ = if both {
        w.binary(BinaryOp::Xor, &[sa], &[sb], 1)[0]
    } else {
        SigBit::ZERO
    };
    let r_trunc = if both {
        w.neg_if(ru.clone(), sa)
    } else {
        ru.clone()
    };
    let mut y = match c.op {
        BinaryOp::Div => w.neg_if(qu, differ),
        BinaryOp::Mod => r_trunc,
        BinaryOp::DivFloor => {
            // Rounding toward -inf: one less where a remainder is dropped,
            // and `-q - 1` is `~q`.
            let n = w.neg(&qu);
            let inv = w.unary(UnaryOp::Not, &qu, width);
            let exact = w.bit(UnaryOp::LogicNot, &ru);
            let neg = w.mux(inv, n, exact);
            w.mux(qu, neg, differ)
        }
        BinaryOp::ModFloor => {
            let nonzero = w.bit(UnaryOp::ReduceBool, &r_trunc);
            let adjust = w.binary(BinaryOp::And, &[nonzero], &[differ], 1)[0];
            let sum = w.binary(BinaryOp::Add, &b, &r_trunc, width);
            w.mux(r_trunc, sum, adjust)
        }
        _ => unreachable!("only division and modulo are lowered"),
    };
    y.truncate(c.y.len());
    y
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::techmap;

    /// `op(a, b)` through the lowering, constants folded by `techmap`'s
    /// gate-level lowering.
    fn eval(op: BinaryOp, a: i64, b: i64, signed: bool, w: usize) -> i64 {
        let k = |v: i64| -> SigSpec {
            (0..w)
                .map(|i| SigBit::from(State::from(v >> i & 1 == 1)))
                .collect()
        };
        let mut m = Module::new("t");
        let y = m.new_sig(w);
        m.add_cell(
            "c",
            CellKind::Binary(Binary {
                op,
                a: Operand { sig: k(a), signed },
                b: Operand { sig: k(b), signed },
                y: y.clone(),
            }),
        );
        let mut wire = chipsmith_netlist::Wire::new("y", y);
        wire.port = Some(chipsmith_netlist::Dir::Out);
        m.wires.push(wire);
        assert!(lower(&mut m));
        // The inputs are constant: gates and constant folding leave only
        // constants.
        techmap::lower(&mut m, techmap::is_lowerable).unwrap();
        while crate::opt::opt_gates(&mut m) {}
        let bits = &m.wires.iter().find(|w| w.name == "y").unwrap().bits;
        let mut v = 0i64;
        for (i, b) in bits.iter().enumerate() {
            match *b {
                SigBit::ONE => v |= 1 << i,
                SigBit::ZERO => {}
                other => panic!("bit {i} not constant: {other:?}"),
            }
        }
        if signed && v >> (w - 1) & 1 == 1 {
            v -= 1 << w;
        }
        v
    }

    #[test]
    fn exhaustive_four_bits() {
        let w = 4;
        for signed in [false, true] {
            let range: Vec<i64> = if signed {
                (-8..8).collect()
            } else {
                (0..16).collect()
            };
            let wrap = |v: i64| {
                let m = v.rem_euclid(16);
                if signed && m >= 8 {
                    m - 16
                } else {
                    m
                }
            };
            for &a in &range {
                for &b in &range {
                    let (q, r, fq, fr) = if b == 0 {
                        // yosys' model: all ones, and for signed
                        // division of a negative number 1; remainder A.
                        let q = if signed && a < 0 { 1 } else { wrap(-1) };
                        (q, a, q, a)
                    } else {
                        let floor = (a as f64 / b as f64).floor() as i64;
                        (wrap(a / b), wrap(a % b), wrap(floor), wrap(a - b * floor))
                    };
                    assert_eq!(eval(BinaryOp::Div, a, b, signed, w), q, "{a} / {b}");
                    assert_eq!(eval(BinaryOp::Mod, a, b, signed, w), r, "{a} % {b}");
                    if b != 0 {
                        assert_eq!(eval(BinaryOp::DivFloor, a, b, signed, w), fq, "{a} div {b}");
                        assert_eq!(eval(BinaryOp::ModFloor, a, b, signed, w), fr, "{a} mod {b}");
                    }
                }
            }
        }
    }
}
