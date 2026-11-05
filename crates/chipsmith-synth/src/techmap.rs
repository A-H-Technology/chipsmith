//! Word-level cells to gates: yosys' `simplemap` and `techmap.v`, with the
//! same decompositions (so the AIG the LUT mapper sees has the same shape)
//! and constants folded on the way.
//!
//! Lowering one cell whose inputs are all constant folds to constants, so
//! this is also how word-level passes evaluate a cell: the semantics of
//! each RTLIL operator are written once, here.

use anyhow::{bail, Result};
use chipsmith_netlist::{
    Alu, Binary, BinaryOp, Bit, Bmux, CellKind, Const, Demux, DffControl, Ff, FfKind, GateOp,
    LatchReset, Lut, Module, Mux, Net, Operand, Pmux, SetClr, SigBit, SigSpec, State, Unary,
    UnaryOp, Word,
};

use crate::rewrite::{Gates, Subst};

/// `sig` extended (sign or zero) or truncated to `width`, RTLIL's
/// `extend_u0`.
pub fn extend(sig: &[SigBit], width: usize, signed: bool) -> SigSpec {
    let fill = if signed {
        sig.last().copied().unwrap_or(SigBit::ZERO)
    } else {
        SigBit::ZERO
    };
    (0..width)
        .map(|i| sig.get(i).copied().unwrap_or(fill))
        .collect()
}

fn ext(op: &Operand, width: usize) -> SigSpec {
    extend(&op.sig, width, op.signed)
}

/// A cell's output bits and what each is now.
pub type Outputs = Vec<(SigBit, SigBit)>;

fn outputs(y: &[SigBit], values: SigSpec) -> Outputs {
    y.iter().copied().zip(values).collect()
}

/// Whether [`lower_comb`] lowers this kind (rather than leaving it).
pub fn is_lowerable(kind: &CellKind) -> bool {
    match kind {
        CellKind::Unary(_)
        | CellKind::Mux(_)
        | CellKind::Pmux(_)
        | CellKind::Bmux(_)
        | CellKind::Demux(_)
        | CellKind::Alu(_)
        | CellKind::Lut(_) => true,
        CellKind::Binary(b) => !matches!(
            b.op,
            BinaryOp::Div | BinaryOp::Mod | BinaryOp::DivFloor | BinaryOp::ModFloor | BinaryOp::Pow
        ),
        _ => false,
    }
}

/// Gates computing a combinational cell. `None` for cells this doesn't
/// lower (registers, memories, tri-states, device cells).
pub fn lower_comb(g: &mut Gates, kind: &CellKind) -> Result<Option<Outputs>> {
    Ok(Some(match kind {
        CellKind::Unary(c) => outputs(&c.y, unary(g, c)),
        CellKind::Binary(c) => outputs(&c.y, binary(g, c)?),
        CellKind::Mux(c) => outputs(&c.y, mux(g, c)),
        CellKind::Pmux(c) => outputs(&c.y, pmux(g, c)),
        CellKind::Bmux(c) => outputs(&c.y, bmux(g, c)),
        CellKind::Demux(c) => outputs(&c.y, demux(g, c)),
        CellKind::Lut(c) => vec![(c.y, lut(g, c))],
        CellKind::Alu(c) => {
            let (x, y, co) = alu(g, c);
            let mut o = outputs(&c.x, x);
            o.extend(outputs(&c.y, y));
            o.extend(outputs(&c.co, co));
            o
        }
        _ => return Ok(None),
    }))
}

/// Lowers every cell `select` picks: combinational cells to gates,
/// registers to gate-level registers. Others stay.
pub fn lower(m: &mut Module, select: impl Fn(&CellKind) -> bool) -> Result<()> {
    let mut g = Gates::new(m, "techmap");
    let cells = std::mem::take(&mut g.m.cells);
    let mut subst = Subst::new(g.m);
    let mut kept = Vec::new();
    for c in cells {
        if !select(&c.kind) {
            kept.push(c);
            continue;
        }
        if let CellKind::Ff(ff) = &c.kind {
            for bit in split_ff(ff) {
                g.add(CellKind::FfGate(bit));
            }
            continue;
        }
        match lower_comb(&mut g, &c.kind)? {
            Some(outs) => {
                for (y, v) in outs {
                    if let SigBit::Net(n) = y {
                        subst.set(n, v);
                    }
                }
            }
            None => kept.push(c),
        }
    }
    g.m.cells.extend(kept);
    subst.apply(m);
    Ok(())
}

/// The constant a cell computes, if all its inputs are constant and it is
/// one [`lower_comb`] handles: `None` otherwise.
pub fn eval(kind: &CellKind) -> Option<Vec<(Net, State)>> {
    if !is_lowerable(kind) {
        return None;
    }
    let mut ok = true;
    kind.for_each_port(|p| {
        if p.dir == Some(chipsmith_netlist::Dir::In) {
            ok &= p
                .bits
                .iter()
                .all(|b| matches!(b, SigBit::Const(State::Zero | State::One)));
        }
    });
    if !ok {
        return None;
    }
    let mut scratch = Module::new("eval");
    let mut g = Gates::new(&mut scratch, "eval");
    let outs = lower_comb(&mut g, kind).ok()??;
    outs.into_iter()
        .filter_map(|(y, v)| match (y, v) {
            (SigBit::Net(n), SigBit::Const(s)) => Some(Some((n, s))),
            (SigBit::Net(_), _) => Some(None),
            _ => None,
        })
        .collect()
}

fn unary(g: &mut Gates, c: &Unary) -> SigSpec {
    let w = c.y.len();
    let one = |b: SigBit| extend(&[b], w, false);
    match c.op {
        UnaryOp::Not => ext(&c.a, w).into_iter().map(|b| g.not(b)).collect(),
        UnaryOp::Pos => ext(&c.a, w),
        UnaryOp::Neg => {
            let zero = Operand {
                sig: vec![SigBit::ZERO],
                signed: c.a.signed,
            };
            add(g, &zero, &c.a, true, w)
        }
        UnaryOp::ReduceAnd => one(g.reduce_and(&c.a.sig)),
        UnaryOp::ReduceOr | UnaryOp::ReduceBool => one(g.reduce_or(&c.a.sig)),
        UnaryOp::ReduceXor => one(g.reduce(GateOp::Xor, &c.a.sig, SigBit::ZERO)),
        UnaryOp::ReduceXnor => {
            let x = g.reduce(GateOp::Xor, &c.a.sig, SigBit::ZERO);
            one(g.not(x))
        }
        UnaryOp::LogicNot => {
            let any = g.reduce_or(&c.a.sig);
            one(g.not(any))
        }
    }
}

fn binary(g: &mut Gates, c: &Binary) -> Result<SigSpec> {
    let w = c.y.len();
    let one = |b: SigBit| extend(&[b], w, false);
    let (a, b) = (ext(&c.a, w), ext(&c.b, w));
    let pairs = a.into_iter().zip(b);
    Ok(match c.op {
        BinaryOp::And => pairs.map(|(a, b)| g.and(a, b)).collect(),
        BinaryOp::Or => pairs.map(|(a, b)| g.or(a, b)).collect(),
        BinaryOp::Xor => pairs.map(|(a, b)| g.xor(a, b)).collect(),
        BinaryOp::Xnor => pairs.map(|(a, b)| g.xnor(a, b)).collect(),
        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
            let a = g.reduce_or(&c.a.sig);
            let b = g.reduce_or(&c.b.sig);
            one(if c.op == BinaryOp::LogicAnd {
                g.and(a, b)
            } else {
                g.or(a, b)
            })
        }
        BinaryOp::Eq | BinaryOp::Eqx | BinaryOp::Ne | BinaryOp::Nex => {
            let ne = not_equal(g, &c.a, &c.b);
            one(if matches!(c.op, BinaryOp::Eq | BinaryOp::Eqx) {
                g.not(ne)
            } else {
                ne
            })
        }
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let (lt, eq) = compare(g, &c.a, &c.b);
            one(match c.op {
                BinaryOp::Lt => lt,
                BinaryOp::Le => g.or(lt, eq),
                BinaryOp::Gt => {
                    let le = g.or(lt, eq);
                    g.not(le)
                }
                _ => g.not(lt),
            })
        }
        BinaryOp::Add => add(g, &c.a, &c.b, false, w),
        BinaryOp::Sub => add(g, &c.a, &c.b, true, w),
        BinaryOp::Mul => mul(g, &c.a, &c.b, w),
        BinaryOp::Shl | BinaryOp::Sshl | BinaryOp::Shr | BinaryOp::Sshr => shift(g, c),
        BinaryOp::Shift | BinaryOp::Shiftx => shiftx(g, c),
        BinaryOp::Div | BinaryOp::Mod | BinaryOp::DivFloor | BinaryOp::ModFloor | BinaryOp::Pow => {
            bail!("{} is not supported", c.op.name())
        }
    })
}

/// `A != B`, both extended to the wider of the two (`simplemap_eqne`).
fn not_equal(g: &mut Gates, a: &Operand, b: &Operand) -> SigBit {
    let w = a.sig.len().max(b.sig.len());
    let signed = a.signed && b.signed;
    let x: SigSpec = extend(&a.sig, w, signed)
        .into_iter()
        .zip(extend(&b.sig, w, signed))
        .map(|(a, b)| g.xor(a, b))
        .collect();
    g.reduce_or(&x)
}

/// `A < B` and `A == B` from `A - B`, the way `alumacc` reads them off an
/// `$alu`: unsigned less is no carry out, signed less is overflow xor sign.
fn compare(g: &mut Gates, a: &Operand, b: &Operand) -> (SigBit, SigBit) {
    let signed = a.signed && b.signed;
    let w = a.sig.len().max(b.sig.len());
    let a = extend(&a.sig, w, signed);
    let nb: SigSpec = extend(&b.sig, w, signed)
        .into_iter()
        .map(|b| g.not(b))
        .collect();
    let (x, y, co) = ripple(g, &a, &nb, SigBit::ONE);
    let eq = g.reduce_and(&x);
    let lt = if signed {
        let ci = if w >= 2 { co[w - 2] } else { SigBit::ONE };
        let of = g.xor(co[w - 1], ci);
        g.xor(of, y[w - 1])
    } else {
        g.not(co[w - 1])
    };
    (lt, eq)
}

/// A ripple-carry adder: `(A ^ B, A + B + ci, carries out)`.
pub fn ripple(
    g: &mut Gates,
    a: &[SigBit],
    b: &[SigBit],
    ci: SigBit,
) -> (SigSpec, SigSpec, SigSpec) {
    let mut c = ci;
    let (mut x, mut y, mut co) = (Vec::new(), Vec::new(), Vec::new());
    for (&a, &b) in a.iter().zip(b) {
        let p = g.xor(a, b);
        let gen = g.and(a, b);
        let t = g.and(p, c);
        y.push(g.xor(p, c));
        c = g.or(gen, t);
        x.push(p);
        co.push(c);
    }
    (x, y, co)
}

fn add(g: &mut Gates, a: &Operand, b: &Operand, sub: bool, w: usize) -> SigSpec {
    let a = ext(a, w);
    let b: SigSpec = ext(b, w)
        .into_iter()
        .map(|b| if sub { g.not(b) } else { b })
        .collect();
    ripple(g, &a, &b, SigBit::from(State::from(sub))).1
}

fn mul(g: &mut Gates, a: &Operand, b: &Operand, w: usize) -> SigSpec {
    let a = ext(a, w);
    let b = ext(b, w);
    let mut acc: SigSpec = vec![SigBit::ZERO; w];
    for (i, &bi) in b.iter().enumerate() {
        let row: SigSpec = (0..w)
            .map(|k| {
                if k < i {
                    SigBit::ZERO
                } else {
                    g.and(a[k - i], bi)
                }
            })
            .collect();
        acc = ripple(g, &acc, &row, SigBit::ZERO).1;
    }
    acc
}

/// `$shl`, `$shr`, `$sshl`, `$sshr` as techmap.v's barrel shifter.
fn shift(g: &mut Gates, c: &Binary) -> SigSpec {
    let (aw, yw) = (c.a.sig.len(), c.y.len());
    let left = matches!(c.op, BinaryOp::Shl | BinaryOp::Sshl);
    let sign_extend = c.a.signed && c.op == BinaryOp::Sshr;
    let width = aw.max(yw);
    let reach = if left {
        yw
    } else if c.a.signed {
        width
    } else {
        aw
    };
    let bb = (clog2(reach) + 1).min(c.b.sig.len());
    let b = &c.b.sig;
    let overflow = g.reduce_or(&b[bb..]);
    let fill_bit = |buf: &[SigBit]| {
        if sign_extend {
            buf[width - 1]
        } else {
            SigBit::ZERO
        }
    };
    let a = ext(&c.a, width);
    let fill = if sign_extend { a[aw - 1] } else { SigBit::ZERO };
    let mut buf: SigSpec = a.iter().map(|&x| g.mux(x, fill, overflow)).collect();
    for (i, &bit) in b[..bb].iter().enumerate() {
        let amount = 1usize << i.min(usize::BITS as usize - 2);
        let f = fill_bit(&buf);
        let shifted: SigSpec = (0..width)
            .map(|k| {
                if left {
                    if k >= amount {
                        buf[k - amount]
                    } else {
                        SigBit::ZERO
                    }
                } else if k + amount < width {
                    buf[k + amount]
                } else {
                    f
                }
            })
            .collect();
        buf = buf
            .iter()
            .zip(&shifted)
            .map(|(&keep, &sh)| g.mux(keep, sh, bit))
            .collect();
    }
    buf.truncate(yw);
    buf
}

/// `$shift` and `$shiftx`: B may be signed (a negative B shifts left), and
/// `$shiftx` shifts in x.
fn shiftx(g: &mut Gates, c: &Binary) -> SigSpec {
    let (aw, yw, bw) = (c.a.sig.len(), c.y.len(), c.b.sig.len());
    let extbit = if c.op == BinaryOp::Shift {
        SigBit::ZERO
    } else {
        SigBit::X
    };
    let a_padding = if c.op == BinaryOp::Shiftx {
        extbit
    } else if c.a.signed {
        c.a.sig.last().copied().unwrap_or(SigBit::ZERO)
    } else {
        SigBit::ZERO
    };
    let b_signed = c.b.signed;
    let bb = (clog2(aw.max(yw)) + if b_signed { 2 } else { 1 }).min(bw);
    if bb == 0 {
        return extend(&c.a.sig, yw, false)
            .into_iter()
            .enumerate()
            .map(|(i, b)| if i < aw { b } else { a_padding })
            .collect();
    }
    let width = aw.max(yw) + if b_signed { 1 << (bb - 1) } else { 0 };
    let mut buf = vec![extbit; width];
    buf[..yw].fill(a_padding);
    buf[..aw].copy_from_slice(&c.a.sig);
    let b = &c.b.sig;
    if bw > bb {
        let overflow = if b_signed {
            let top = b[bb - 1];
            let diffs: SigSpec = b[bb..].iter().map(|&x| g.xor(x, top)).collect();
            g.reduce_or(&diffs)
        } else {
            g.reduce_or(&b[bb..])
        };
        buf = buf.iter().map(|&x| g.mux(x, extbit, overflow)).collect();
    }
    if b_signed {
        let amount = 1usize << (bb - 1);
        let s = b[bb - 1];
        buf = (0..width)
            .map(|k| {
                let sh = if k >= amount { buf[k - amount] } else { extbit };
                g.mux(buf[k], sh, s)
            })
            .collect();
    }
    let steps = if b_signed { bb - 1 } else { bb };
    for (i, &bit) in b[..steps].iter().enumerate() {
        let amount = 1usize << i.min(usize::BITS as usize - 2);
        buf = (0..width)
            .map(|k| {
                let sh = if k + amount < width {
                    buf[k + amount]
                } else {
                    extbit
                };
                g.mux(buf[k], sh, bit)
            })
            .collect();
    }
    buf.truncate(yw);
    buf
}

fn clog2(n: usize) -> usize {
    if n <= 1 {
        0
    } else {
        (usize::BITS - (n - 1).leading_zeros()) as usize
    }
}

fn mux(g: &mut Gates, c: &Mux) -> SigSpec {
    c.a.iter()
        .zip(&c.b)
        .map(|(&a, &b)| g.mux(a, b, c.s))
        .collect()
}

fn pmux(g: &mut Gates, c: &Pmux) -> SigSpec {
    let w = c.a.len();
    let any = g.reduce_or(&c.s);
    (0..w)
        .map(|i| {
            let terms: SigSpec =
                c.s.iter()
                    .enumerate()
                    .map(|(j, &s)| g.and(c.b[j * w + i], s))
                    .collect();
            let b = g.reduce_or(&terms);
            g.mux(c.a[i], b, any)
        })
        .collect()
}

fn bmux(g: &mut Gates, c: &Bmux) -> SigSpec {
    let w = c.y.len();
    let mut data = c.a.clone();
    for &s in &c.s {
        let half = data.len() / 2;
        let mut next = Vec::with_capacity(half);
        for i in (0..half).step_by(w.max(1)) {
            for k in 0..w {
                next.push(g.mux(data[i * 2 + k], data[i * 2 + w + k], s));
            }
        }
        data = next;
    }
    data
}

fn demux(g: &mut Gates, c: &Demux) -> SigSpec {
    let w = c.a.len();
    let n = 1usize << c.s.len();
    let mut out = Vec::with_capacity(w * n);
    for i in 0..n {
        let terms: SigSpec =
            c.s.iter()
                .enumerate()
                .map(|(k, &s)| if i >> k & 1 == 1 { s } else { g.not(s) })
                .collect();
        let sel = g.reduce_and(&terms);
        out.extend(c.a.iter().map(|&a| g.and(a, sel)));
    }
    out
}

fn lut(g: &mut Gates, c: &Lut) -> SigBit {
    let mut data: SigSpec = c.table.0.iter().map(|&s| SigBit::Const(s)).collect();
    for &s in &c.a {
        data = data.chunks(2).map(|p| g.mux(p[0], p[1], s)).collect();
    }
    data[0]
}

/// `$alu`: `Y = A + (BI ? ~B : B) + CI`, `X = A ^ B'`, `CO` each bit's
/// carry out.
fn alu(g: &mut Gates, c: &Alu) -> (SigSpec, SigSpec, SigSpec) {
    let w = c.y.len();
    let a = ext(&c.a, w);
    let b: SigSpec = ext(&c.b, w)
        .into_iter()
        .map(|b| {
            let nb = g.not(b);
            g.mux(b, nb, c.bi)
        })
        .collect();
    ripple(g, &a, &b, c.ci)
}

/// A word-level register as one gate-level register per bit.
pub fn split_ff(ff: &Ff<Word>) -> Vec<Ff<Bit>> {
    (0..ff.width()).map(|i| slice_ff(ff, i)).collect()
}

fn value_bit(c: &Const, i: usize) -> bool {
    c.0[i] == State::One
}

fn slice_setclr(s: &SetClr<Word>, i: usize) -> SetClr<Bit> {
    SetClr {
        set: s.set[i],
        set_polarity: s.set_polarity,
        clr: s.clr[i],
        clr_polarity: s.clr_polarity,
    }
}

/// Bit `i` of a word-level register. An x reset value becomes 0: a
/// gate-level register resets to 0 or 1.
fn slice_ff(ff: &Ff<Word>, i: usize) -> Ff<Bit> {
    let kind = match &ff.kind {
        FfKind::Global { d } => FfKind::Global { d: d[i] },
        FfKind::Dff { clk, d, control } => FfKind::Dff {
            clk: *clk,
            d: d[i],
            control: match control {
                DffControl::Plain { en } => DffControl::Plain { en: *en },
                DffControl::Async { arst, value, en } => DffControl::Async {
                    arst: *arst,
                    value: value_bit(value, i),
                    en: *en,
                },
                DffControl::Sync { srst, value, en } => DffControl::Sync {
                    srst: *srst,
                    value: value_bit(value, i),
                    en: *en,
                },
                DffControl::Load { aload, ad, en } => DffControl::Load {
                    aload: *aload,
                    ad: ad[i],
                    en: *en,
                },
                DffControl::SetClr { sr, en } => DffControl::SetClr {
                    sr: slice_setclr(sr, i),
                    en: *en,
                },
            },
        },
        FfKind::Dlatch { en, d, reset } => FfKind::Dlatch {
            en: *en,
            d: d[i],
            reset: match reset {
                LatchReset::None => LatchReset::None,
                LatchReset::Async { arst, value } => LatchReset::Async {
                    arst: *arst,
                    value: value_bit(value, i),
                },
                LatchReset::SetClr(s) => LatchReset::SetClr(slice_setclr(s, i)),
            },
        },
        FfKind::Sr(s) => FfKind::Sr(slice_setclr(s, i)),
    };
    Ff { q: ff.q[i], kind }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_netlist::{Binary, BinaryOp, Operand};

    fn k(v: i64, w: usize) -> SigSpec {
        Const::from_int(v, w)
            .0
            .into_iter()
            .map(SigBit::Const)
            .collect()
    }

    fn value(out: Vec<(Net, State)>) -> i64 {
        Const(out.into_iter().map(|(_, s)| s).collect())
            .as_u64()
            .unwrap() as i64
    }

    fn bin(op: BinaryOp, a: i64, b: i64, signed: bool, aw: usize, bw: usize, yw: usize) -> i64 {
        let mut m = Module::new("t");
        let y = m.new_sig(yw);
        let cell = CellKind::Binary(Binary {
            op,
            a: Operand {
                sig: k(a, aw),
                signed,
            },
            b: Operand {
                sig: k(b, bw),
                signed,
            },
            y,
        });
        value(eval(&cell).expect("constant"))
    }

    #[test]
    fn arithmetic_and_comparisons_fold() {
        assert_eq!(bin(BinaryOp::Add, 200, 100, false, 8, 8, 9), 300);
        assert_eq!(bin(BinaryOp::Sub, 3, 5, false, 8, 8, 8), 254);
        assert_eq!(bin(BinaryOp::Mul, 13, 11, false, 8, 8, 8), 143);
        assert_eq!(bin(BinaryOp::Lt, 3, 5, false, 4, 4, 1), 1);
        assert_eq!(bin(BinaryOp::Lt, -3, 5, true, 4, 4, 1), 1);
        assert_eq!(bin(BinaryOp::Lt, -3, 5, false, 4, 4, 1), 0);
        assert_eq!(bin(BinaryOp::Ge, -8, 7, true, 4, 4, 1), 0);
        assert_eq!(bin(BinaryOp::Gt, 7, -8, true, 4, 4, 1), 1);
        assert_eq!(bin(BinaryOp::Le, 5, 5, false, 4, 4, 1), 1);
        assert_eq!(bin(BinaryOp::Eq, 5, 5, false, 4, 3, 1), 1);
    }

    #[test]
    fn shifts_fold() {
        assert_eq!(bin(BinaryOp::Shl, 0b1011, 2, false, 4, 3, 6), 0b101100);
        assert_eq!(bin(BinaryOp::Shr, 0b1011, 1, false, 4, 3, 4), 0b0101);
        assert_eq!(bin(BinaryOp::Sshr, -8, 2, true, 4, 3, 4), 0b1110);
        assert_eq!(bin(BinaryOp::Shl, 1, 9, false, 4, 4, 4), 0);
        // A negative shift amount shifts left.
        let mut m = Module::new("t");
        let y = m.new_sig(4);
        let cell = CellKind::Binary(Binary {
            op: BinaryOp::Shift,
            a: Operand {
                sig: k(0b0011, 4),
                signed: false,
            },
            b: Operand {
                sig: k(-1, 3),
                signed: true,
            },
            y,
        });
        assert_eq!(value(eval(&cell).unwrap()), 0b0110);
    }
}
