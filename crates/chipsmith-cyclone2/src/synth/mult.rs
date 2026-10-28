//! `$mul` onto the embedded multipliers (`CY2_MULT`, crate `mult`).
//!
//! The rule: a product of two non-constant operands, each at least two bits
//! wide after width reduction, goes to blocks while the device has them
//! left; the rest stays logic (techmap's shift-and-add). An operand up to
//! 18 bits takes one block. Wider operands are split into 18-bit limbs, low
//! limbs unsigned and the top limb carrying the operand's sign; each pair of
//! limbs is one block and the partial products are summed with adders,
//! which then map like any other. Product bits above the result width are
//! never built: a partial product shifted past it is dropped, and operands
//! are cut to the result width first (the low bits of a product don't
//! depend on higher operand bits).

use std::collections::BTreeMap;

use chipsmith_netlist::{
    Binary, BinaryOp, Cell, CellKind, Dir, Generic, GenericPort, Module, Operand, ParamValue,
    SigBit, SigSpec,
};

use crate::mult::{BLOCKS, MAX_WIDTH};

/// Maps the products the blocks take, in netlist order; returns how many
/// blocks the design uses, its own `CY2_MULT` instances included.
pub fn map(m: &mut Module) -> usize {
    let instantiated = m
        .cells
        .iter()
        .filter(|c| matches!(&c.kind, CellKind::Other(g) if g.ty == super::cells::MULT))
        .count();
    let mut left = BLOCKS.saturating_sub(instantiated);
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    let cells = std::mem::take(&mut m.cells);
    let mut out = Vec::with_capacity(cells.len());
    let mut used = instantiated;
    for c in cells {
        let CellKind::Binary(b) = &c.kind else {
            out.push(c);
            continue;
        };
        let Some(plan) = Plan::of(b).filter(|p| b.op == BinaryOp::Mul && p.blocks() <= left) else {
            out.push(c);
            continue;
        };
        left -= plan.blocks();
        used += plan.blocks();
        let product = plan.build(m, &c.name, &mut out);
        for (&y, p) in b.y.iter().zip(product) {
            if let SigBit::Net(n) = y {
                subst.set(n, p);
            }
        }
    }
    m.cells = out;
    subst.apply(m);
    used
}

/// One operand cut to what the product needs: its significant bits and
/// whether they're two's complement.
#[derive(Debug, Clone)]
struct Factor {
    bits: SigSpec,
    signed: bool,
}

impl Factor {
    /// `op` as `$mul` reads it for a `width`-bit result: extended (signed
    /// only if both operands are) or cut to `width`, then with redundant top
    /// bits dropped: constant zeros of an unsigned operand, copies of the
    /// sign bit of a signed one.
    fn new(op: &Operand, signed: bool, width: usize) -> Factor {
        let mut bits: SigSpec = op.sig.iter().copied().take(width).collect();
        // Cut to the result width, the value's sign no longer matters: it
        // only changes the product by a multiple of 2^width.
        let signed = signed && op.sig.len() < width;
        if signed {
            while bits.len() > 1 && bits[bits.len() - 1] == bits[bits.len() - 2] {
                bits.pop();
            }
        } else {
            while bits.last() == Some(&SigBit::ZERO) {
                bits.pop();
            }
        }
        Factor { bits, signed }
    }

    fn is_constant(&self) -> bool {
        self.bits.iter().all(|b| matches!(b, SigBit::Const(_)))
    }

    /// 18-bit limbs, least significant first: every limb but the top one
    /// unsigned, the top one signed if the operand is.
    fn limbs(&self) -> Vec<Factor> {
        let chunks: Vec<&[SigBit]> = self.bits.chunks(MAX_WIDTH).collect();
        let n = chunks.len();
        chunks
            .into_iter()
            .enumerate()
            .map(|(i, c)| Factor {
                bits: c.to_vec(),
                signed: self.signed && i == n - 1,
            })
            .collect()
    }
}

/// How one `$mul` maps: its factors and result width.
#[derive(Debug)]
struct Plan {
    a: Vec<Factor>,
    b: Vec<Factor>,
    width: usize,
}

impl Plan {
    /// `None` when the product stays logic: a constant or one-bit factor
    /// (shifts and AND gates are cheaper than a block).
    fn of(b: &Binary) -> Option<Plan> {
        let width = b.y.len();
        let signed = b.a.signed && b.b.signed;
        let fa = Factor::new(&b.a, signed, width);
        let fb = Factor::new(&b.b, signed, width);
        if fa.bits.len() < 2 || fb.bits.len() < 2 || fa.is_constant() || fb.is_constant() {
            return None;
        }
        let plan = Plan {
            a: fa.limbs(),
            b: fb.limbs(),
            width,
        };
        Some(plan).filter(|p| p.blocks() > 0)
    }

    /// Limb pairs whose product lands at least partly inside the result.
    fn pairs(&self) -> Vec<(usize, usize)> {
        let mut v = Vec::new();
        for i in 0..self.a.len() {
            for j in 0..self.b.len() {
                if (i + j) * MAX_WIDTH < self.width {
                    v.push((i, j));
                }
            }
        }
        v
    }

    fn blocks(&self) -> usize {
        self.pairs().len()
    }

    /// The blocks and adders; returns the `width` product bits.
    fn build(&self, m: &mut Module, name: &str, out: &mut Vec<Cell>) -> SigSpec {
        let mut sum: Option<SigSpec> = None;
        for (i, j) in self.pairs() {
            let (a, b) = (&self.a[i], &self.b[j]);
            let dout = m.new_sig(a.bits.len() + b.bits.len());
            out.push(Cell {
                name: format!("{name}.mult{i}_{j}"),
                kind: cell(a, b, dout.clone()),
                attributes: Default::default(),
            });
            // The partial product, extended (it's signed if either limb is)
            // and shifted into place.
            let shift = (i + j) * MAX_WIDTH;
            let fill = if a.signed || b.signed {
                *dout.last().expect("non-empty product")
            } else {
                SigBit::ZERO
            };
            let part: SigSpec = (0..self.width)
                .map(|k| match k.checked_sub(shift) {
                    None => SigBit::ZERO,
                    Some(k) => dout.get(k).copied().unwrap_or(fill),
                })
                .collect();
            sum = Some(match sum {
                None => part,
                Some(acc) => {
                    let y = m.new_sig(self.width);
                    out.push(Cell {
                        name: format!("{name}.add{i}_{j}"),
                        kind: CellKind::Binary(Binary {
                            op: BinaryOp::Add,
                            a: Operand {
                                sig: acc,
                                signed: false,
                            },
                            b: Operand {
                                sig: part,
                                signed: false,
                            },
                            y: y.clone(),
                        }),
                        attributes: Default::default(),
                    });
                    y
                }
            });
        }
        sum.expect("a planned product has a block")
    }
}

/// A combinational `CY2_MULT` computing `P = A * B`.
fn cell(a: &Factor, b: &Factor, dout: SigSpec) -> CellKind {
    let port = |name: &str, dir, sig: SigSpec| GenericPort {
        name: name.into(),
        dir: Some(dir),
        sig,
    };
    let flag = |v: bool| ParamValue::int(i64::from(v));
    CellKind::Other(Generic {
        ty: super::cells::MULT.into(),
        params: BTreeMap::from([
            ("A_WIDTH".to_string(), ParamValue::int(a.bits.len() as i64)),
            ("B_WIDTH".to_string(), ParamValue::int(b.bits.len() as i64)),
            ("A_SIGNED".to_string(), flag(a.signed)),
            ("B_SIGNED".to_string(), flag(b.signed)),
        ]),
        ports: vec![
            port("A", Dir::In, a.bits.clone()),
            port("B", Dir::In, b.bits.clone()),
            port("P", Dir::Out, dout),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_netlist::{Net, State};

    fn nets(from: u32, n: usize) -> SigSpec {
        (0..n as u32).map(|i| SigBit::Net(Net(from + i))).collect()
    }

    fn mul(a: SigSpec, b: SigSpec, signed: bool, width: usize) -> Binary {
        Binary {
            op: BinaryOp::Mul,
            a: Operand { sig: a, signed },
            b: Operand { sig: b, signed },
            y: nets(1000, width),
        }
    }

    #[test]
    fn operands_up_to_18_bits_take_one_block() {
        let p = Plan::of(&mul(nets(0, 18), nets(100, 12), false, 30)).unwrap();
        assert_eq!(p.blocks(), 1);
        let p = Plan::of(&mul(nets(0, 18), nets(100, 18), true, 36)).unwrap();
        assert_eq!((p.a[0].signed, p.b[0].signed), (true, true));
    }

    #[test]
    fn wide_operands_split_into_limbs_inside_the_result() {
        // 32 x 32 -> 64: 2 x 2 limbs, all four products reach the result.
        let p = Plan::of(&mul(nets(0, 32), nets(100, 32), false, 64)).unwrap();
        assert_eq!(p.blocks(), 4);
        // 32 x 32 -> 32 (C's `*`): the high x high product starts at bit 36.
        let p = Plan::of(&mul(nets(0, 32), nets(100, 32), false, 32)).unwrap();
        assert_eq!(p.blocks(), 3);
        // Signed: the top limb carries the sign, the low one doesn't.
        let p = Plan::of(&mul(nets(0, 24), nets(100, 24), true, 48)).unwrap();
        assert_eq!(
            p.a.iter().map(|f| (f.bits.len(), f.signed)).collect::<Vec<_>>(),
            [(18, false), (6, true)]
        );
    }

    #[test]
    fn constants_and_single_bits_stay_logic() {
        let k: SigSpec = [State::One, State::Zero, State::One]
            .map(SigBit::Const)
            .to_vec();
        assert!(Plan::of(&mul(nets(0, 8), k, false, 11)).is_none());
        assert!(Plan::of(&mul(nets(0, 8), nets(100, 1), false, 9)).is_none());
    }

    #[test]
    fn redundant_top_bits_are_dropped() {
        // Unsigned: constant zeros on top; signed: copies of the sign bit.
        let mut a = nets(0, 6);
        a.extend([SigBit::ZERO; 4]);
        let p = Plan::of(&mul(a, nets(100, 6), false, 16)).unwrap();
        assert_eq!(p.a[0].bits.len(), 6);
        let mut a = nets(0, 6);
        a.extend([SigBit::Net(Net(5)); 3]);
        let p = Plan::of(&mul(a, nets(100, 6), true, 18)).unwrap();
        assert_eq!(p.a[0].bits.len(), 6);
    }
}
