//! RTLIL's cells evaluated on constants, as yosys' `const_*` functions
//! (kernel/calc.cc) do: the elaborator folds every cell whose inputs are
//! all constant, so constant expressions (parameters, generate conditions,
//! constant functions) and logic share one implementation of the
//! operators and their x rules.
//!
//! Values are `State` slices, LSB first; `z` counts as `x`.

use chipsmith_netlist::{BinaryOp, State, UnaryOp};

fn defined(bits: &[State]) -> bool {
    bits.iter().all(|&b| matches!(b, State::Zero | State::One))
}

/// Extends or truncates to `width`, sign-extending if `signed`.
pub fn extend(bits: &[State], width: usize, signed: bool) -> Vec<State> {
    let mut v: Vec<State> = bits.iter().take(width).copied().collect();
    let fill = if signed {
        bits.last().copied().unwrap_or(State::Zero)
    } else {
        State::Zero
    };
    v.resize(width, fill);
    v
}

/// Unsigned arithmetic on `width`-bit words, modulo `2^width`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    limbs: Vec<u64>,
    width: usize,
}

impl Word {
    fn from_bits(bits: &[State], width: usize, signed: bool) -> Word {
        let ext = extend(bits, width, signed);
        let mut limbs = vec![0u64; width.div_ceil(64).max(1)];
        for (i, b) in ext.iter().enumerate() {
            if *b == State::One {
                limbs[i / 64] |= 1 << (i % 64);
            }
        }
        Word { limbs, width }
    }

    fn zero(width: usize) -> Word {
        Word {
            limbs: vec![0; width.div_ceil(64).max(1)],
            width,
        }
    }

    fn bit(&self, i: usize) -> bool {
        i < self.width && self.limbs[i / 64] >> (i % 64) & 1 == 1
    }

    fn set_bit(&mut self, i: usize) {
        self.limbs[i / 64] |= 1 << (i % 64);
    }

    fn bits(&self) -> Vec<State> {
        (0..self.width).map(|i| State::from(self.bit(i))).collect()
    }

    fn mask(&mut self) {
        let rem = self.width % 64;
        let n = self.width.div_ceil(64).max(1);
        self.limbs.truncate(n);
        if self.width == 0 {
            self.limbs[0] = 0;
        } else if rem != 0 {
            self.limbs[n - 1] &= (1u64 << rem) - 1;
        }
    }

    fn add(&self, o: &Word) -> Word {
        let mut r = Word::zero(self.width);
        let mut carry = 0u128;
        for i in 0..r.limbs.len() {
            let s = self.limbs[i] as u128 + o.limbs[i] as u128 + carry;
            r.limbs[i] = s as u64;
            carry = s >> 64;
        }
        r.mask();
        r
    }

    fn not(&self) -> Word {
        let mut r = Word {
            limbs: self.limbs.iter().map(|l| !l).collect(),
            width: self.width,
        };
        r.mask();
        r
    }

    fn neg(&self) -> Word {
        let mut one = Word::zero(self.width);
        if self.width > 0 {
            one.set_bit(0);
        }
        self.not().add(&one)
    }

    fn sub(&self, o: &Word) -> Word {
        self.add(&o.neg())
    }

    fn mul(&self, o: &Word) -> Word {
        let n = self.limbs.len();
        let mut acc = vec![0u64; n];
        for i in 0..n {
            let mut carry = 0u128;
            for j in 0..n - i {
                let cur = acc[i + j] as u128 + self.limbs[i] as u128 * o.limbs[j] as u128 + carry;
                acc[i + j] = cur as u64;
                carry = cur >> 64;
            }
        }
        let mut r = Word {
            limbs: acc,
            width: self.width,
        };
        r.mask();
        r
    }

    fn is_zero(&self) -> bool {
        self.limbs.iter().all(|&l| l == 0)
    }

    fn cmp_u(&self, o: &Word) -> std::cmp::Ordering {
        for i in (0..self.limbs.len()).rev() {
            match self.limbs[i].cmp(&o.limbs[i]) {
                std::cmp::Ordering::Equal => {}
                c => return c,
            }
        }
        std::cmp::Ordering::Equal
    }

    /// Unsigned division: (quotient, remainder). `o` nonzero.
    fn divmod_u(&self, o: &Word) -> (Word, Word) {
        let mut q = Word::zero(self.width);
        let mut r = Word::zero(self.width);
        for i in (0..self.width).rev() {
            // r = r << 1 | bit i
            let mut carry = self.bit(i) as u64;
            for l in r.limbs.iter_mut() {
                let next = *l >> 63;
                *l = *l << 1 | carry;
                carry = next;
            }
            r.mask();
            if r.cmp_u(o) != std::cmp::Ordering::Less {
                r = r.sub(o);
                q.set_bit(i);
            }
        }
        (q, r)
    }

    fn to_u64_saturating(&self) -> u64 {
        if self.limbs[1..].iter().any(|&l| l != 0) {
            u64::MAX
        } else {
            self.limbs[0]
        }
    }
}

fn all_x(width: usize) -> Vec<State> {
    vec![State::X; width]
}

fn and(a: State, b: State) -> State {
    match (a, b) {
        (State::Zero, _) | (_, State::Zero) => State::Zero,
        (State::One, State::One) => State::One,
        _ => State::X,
    }
}

fn or(a: State, b: State) -> State {
    match (a, b) {
        (State::One, _) | (_, State::One) => State::One,
        (State::Zero, State::Zero) => State::Zero,
        _ => State::X,
    }
}

fn xor(a: State, b: State) -> State {
    match (a, b) {
        (State::Zero | State::One, State::Zero | State::One) => State::from(a != b),
        _ => State::X,
    }
}

fn not(a: State) -> State {
    match a {
        State::Zero => State::One,
        State::One => State::Zero,
        _ => State::X,
    }
}

/// RTLIL's `as_bool` with x: 1 if any bit is 1, x if none is but one is
/// undefined, else 0.
fn truth(bits: &[State]) -> State {
    if bits.contains(&State::One) {
        State::One
    } else if defined(bits) {
        State::Zero
    } else {
        State::X
    }
}

fn result_bit(s: State, width: usize) -> Vec<State> {
    extend(&[s], width, false)
}

pub fn unary(op: UnaryOp, a: &[State], signed: bool, width: usize) -> Vec<State> {
    match op {
        UnaryOp::Not => extend(a, width, signed).into_iter().map(not).collect(),
        UnaryOp::Pos => extend(a, width, signed),
        UnaryOp::Neg => {
            if !defined(a) {
                return all_x(width);
            }
            Word::from_bits(a, width, signed).neg().bits()
        }
        UnaryOp::ReduceAnd => result_bit(a.iter().copied().fold(State::One, and), width),
        UnaryOp::ReduceOr | UnaryOp::ReduceBool => result_bit(truth(a), width),
        UnaryOp::ReduceXor => result_bit(a.iter().copied().fold(State::Zero, xor), width),
        UnaryOp::ReduceXnor => result_bit(not(a.iter().copied().fold(State::Zero, xor)), width),
        UnaryOp::LogicNot => result_bit(not(truth(a)), width),
    }
}

/// `Y = A op B` with RTLIL's semantics for the operand flags and widths.
pub fn binary(
    op: BinaryOp,
    a: &[State],
    a_signed: bool,
    b: &[State],
    b_signed: bool,
    width: usize,
) -> Vec<State> {
    use BinaryOp as B;
    let both_signed = a_signed && b_signed;
    match op {
        B::And | B::Or | B::Xor | B::Xnor => {
            let ae = extend(a, width, a_signed);
            let be = extend(b, width, b_signed);
            ae.iter()
                .zip(&be)
                .map(|(&x, &y)| match op {
                    B::And => and(x, y),
                    B::Or => or(x, y),
                    B::Xor => xor(x, y),
                    _ => not(xor(x, y)),
                })
                .collect()
        }
        B::LogicAnd => result_bit(and(truth(a), truth(b)), width),
        B::LogicOr => result_bit(or(truth(a), truth(b)), width),
        B::Eq | B::Ne | B::Eqx | B::Nex => {
            let w = a.len().max(b.len());
            let ae = extend(a, w, both_signed);
            let be = extend(b, w, both_signed);
            let r = match op {
                B::Eqx | B::Nex => State::from(ae == be),
                _ => {
                    let mut r = State::One;
                    for (&x, &y) in ae.iter().zip(&be) {
                        match (x, y) {
                            (State::Zero | State::One, State::Zero | State::One) if x != y => {
                                r = State::Zero;
                                break;
                            }
                            (State::Zero | State::One, State::Zero | State::One) => {}
                            _ => r = State::X,
                        }
                    }
                    r
                }
            };
            let r = if matches!(op, B::Ne | B::Nex) { not(r) } else { r };
            result_bit(r, width)
        }
        B::Lt | B::Le | B::Gt | B::Ge => {
            if !defined(a) || !defined(b) {
                return result_bit(State::X, width);
            }
            let w = a.len().max(b.len()) + 1;
            let x = Word::from_bits(a, w, both_signed);
            let y = Word::from_bits(b, w, both_signed);
            // Offset by the sign bit to compare signed values unsigned.
            let (x, y) = if both_signed {
                let mut s = Word::zero(w);
                s.set_bit(w - 1);
                (x.add(&s), y.add(&s))
            } else {
                (x, y)
            };
            let c = x.cmp_u(&y);
            use std::cmp::Ordering::*;
            let r = match op {
                B::Lt => c == Less,
                B::Le => c != Greater,
                B::Gt => c == Greater,
                _ => c != Less,
            };
            result_bit(State::from(r), width)
        }
        B::Add | B::Sub | B::Mul => {
            if !defined(a) || !defined(b) {
                return all_x(width);
            }
            let x = Word::from_bits(a, width, a_signed);
            let y = Word::from_bits(b, width, b_signed);
            match op {
                B::Add => x.add(&y),
                B::Sub => x.sub(&y),
                _ => x.mul(&y),
            }
            .bits()
        }
        B::Div | B::Mod | B::DivFloor | B::ModFloor => {
            if !defined(a) || !defined(b) {
                return all_x(width);
            }
            let w = a.len().max(b.len()).max(width) + 1;
            let x = Word::from_bits(a, w, a_signed);
            let y = Word::from_bits(b, w, b_signed);
            if y.is_zero() {
                return all_x(width);
            }
            let xn = a_signed && x.bit(w - 1);
            let yn = b_signed && y.bit(w - 1);
            let xm = if xn { x.neg() } else { x };
            let ym = if yn { y.neg() } else { y };
            let (q, r) = xm.divmod_u(&ym);
            let q = if xn != yn { q.neg() } else { q };
            let r = if xn { r.neg() } else { r };
            let res = match op {
                B::Div => q,
                B::Mod => r,
                B::DivFloor => {
                    if xn != yn && !r.is_zero() {
                        let mut one = Word::zero(w);
                        one.set_bit(0);
                        q.sub(&one)
                    } else {
                        q
                    }
                }
                _ => {
                    if xn != yn && !r.is_zero() {
                        r.add(&Word::from_bits(b, w, b_signed))
                    } else {
                        r
                    }
                }
            };
            extend(&res.bits(), width, true)
        }
        B::Pow => pow(a, a_signed, b, b_signed, width),
        B::Shl | B::Sshl | B::Shr | B::Sshr => {
            if !defined(b) {
                return all_x(width);
            }
            let amount = Word::from_bits(b, b.len().max(1), false).to_u64_saturating();
            let left = matches!(op, B::Shl | B::Sshl);
            let arith = a_signed && matches!(op, B::Sshl | B::Sshr);
            let src = if arith {
                a.to_vec()
            } else if left {
                extend(a, width, a_signed)
            } else {
                extend(a, width.max(a.len()), a_signed)
            };
            let sign = if arith {
                src.last().copied().unwrap_or(State::Zero)
            } else {
                State::Zero
            };
            (0..width)
                .map(|i| {
                    let pos = if left {
                        (i as u64).checked_sub(amount).map(|p| p as i128).unwrap_or(-1)
                    } else {
                        i as i128 + amount as i128
                    };
                    if pos < 0 {
                        State::Zero
                    } else if pos as usize >= src.len() {
                        sign
                    } else {
                        src[pos as usize]
                    }
                })
                .collect()
        }
        B::Shift | B::Shiftx => {
            if !defined(b) {
                return all_x(width);
            }
            let off = signed_value(b, b_signed);
            let other = if op == B::Shift { State::Zero } else { State::X };
            (0..width)
                .map(|i| {
                    let pos = i as i128 + off;
                    if pos < 0 || pos >= a.len() as i128 {
                        other
                    } else {
                        a[pos as usize]
                    }
                })
                .collect()
        }
    }
}

/// A defined value as an integer, saturating far outside `i128`.
fn signed_value(b: &[State], signed: bool) -> i128 {
    const MAX: usize = 120;
    let neg = signed && b.last() == Some(&State::One);
    if b.len() > MAX && b[MAX..].iter().any(|&s| (s == State::One) != neg) {
        return if neg { -(1 << MAX) } else { 1 << MAX };
    }
    let w = b.len().min(MAX);
    let v = (0..w)
        .filter(|&i| b[i] == State::One)
        .fold(0i128, |v, i| v | 1 << i);
    if neg {
        v - (1i128 << w)
    } else {
        v
    }
}

/// `**`, IEEE 1364-2005 Table 5-6 for negative exponents.
fn pow(a: &[State], a_signed: bool, b: &[State], b_signed: bool, width: usize) -> Vec<State> {
    if !defined(a) || !defined(b) {
        return all_x(width);
    }
    let base = Word::from_bits(a, width.max(a.len()) + 1, a_signed);
    let wide = base.width;
    let base_neg = a_signed && base.bit(wide - 1);
    let base_mag = if base_neg { base.neg() } else { base.clone() };
    let one_w = {
        let mut o = Word::zero(wide);
        o.set_bit(0);
        o
    };
    let exp_neg = b_signed && b.last() == Some(&State::One);
    if exp_neg {
        let r = if base_mag.is_zero() {
            return all_x(width);
        } else if base_mag == one_w {
            if base_neg {
                // (-1) ** b: 1 if b is even.
                if b[0] == State::Zero {
                    one_w
                } else {
                    one_w.neg()
                }
            } else {
                one_w
            }
        } else {
            Word::zero(wide)
        };
        return extend(&r.bits(), width, true);
    }
    let mut result = Word::from_bits(&[State::One], width.max(1), false);
    let mut sq = Word::from_bits(a, width.max(1), a_signed);
    for &bit in b {
        if bit == State::One {
            result = result.mul(&sq);
        }
        sq = sq.mul(&sq);
    }
    extend(&result.bits(), width, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_netlist::Const;

    fn c(v: i64, w: usize) -> Vec<State> {
        Const::from_int(v, w).0
    }

    fn val(bits: &[State], signed: bool) -> i64 {
        let c = Const(bits.to_vec());
        if signed {
            c.as_i64().unwrap()
        } else {
            c.as_u64().unwrap() as i64
        }
    }

    #[test]
    fn signed_division_truncates_toward_zero() {
        let r = binary(BinaryOp::Div, &c(-7, 8), true, &c(2, 8), true, 8);
        assert_eq!(val(&r, true), -3);
        let r = binary(BinaryOp::Mod, &c(-7, 8), true, &c(2, 8), true, 8);
        assert_eq!(val(&r, true), -1);
        let r = binary(BinaryOp::Div, &c(5, 8), false, &c(0, 8), false, 8);
        assert!(r.iter().all(|&s| s == State::X));
    }

    #[test]
    fn comparisons_follow_operand_signedness() {
        // -1 < 1 signed, 255 > 1 unsigned.
        let lt = |s| val(&binary(BinaryOp::Lt, &c(-1, 8), s, &c(1, 8), s, 1), false);
        assert_eq!(lt(true), 1);
        assert_eq!(lt(false), 0);
    }

    #[test]
    fn shifts() {
        // 8'sb1000_0000 >>> 3 = 1111_0000
        let r = binary(BinaryOp::Sshr, &c(-128, 8), true, &c(3, 4), false, 8);
        assert_eq!(val(&r, false), 0xf0);
        let r = binary(BinaryOp::Shr, &c(-128, 8), false, &c(3, 4), false, 8);
        assert_eq!(val(&r, false), 0x10);
        let r = binary(BinaryOp::Shiftx, &c(0b1011, 4), false, &c(-1, 4), true, 2);
        assert_eq!(r, vec![State::X, State::One]);
    }

    #[test]
    fn x_rules() {
        let a = [State::X, State::Zero];
        assert_eq!(binary(BinaryOp::And, &a, false, &c(0, 2), false, 2), c(0, 2));
        assert_eq!(binary(BinaryOp::Add, &a, false, &c(1, 2), false, 2), all_x(2));
        // 2'b0x == 2'b10 is false: a defined bit differs.
        assert_eq!(binary(BinaryOp::Eq, &a, false, &c(2, 2), false, 1), c(0, 1));
        assert_eq!(binary(BinaryOp::Eq, &a, false, &c(0, 2), false, 1), vec![State::X]);
    }

    #[test]
    fn power() {
        assert_eq!(val(&binary(BinaryOp::Pow, &c(3, 32), true, &c(4, 32), true, 32), true), 81);
        assert_eq!(val(&binary(BinaryOp::Pow, &c(2, 32), true, &c(-1, 32), true, 32), true), 0);
        assert_eq!(val(&binary(BinaryOp::Pow, &c(-1, 32), true, &c(-3, 32), true, 32), true), -1);
    }
}
