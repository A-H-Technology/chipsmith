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
//!
//! Registers move into the block where the block has one: a register
//! whose output is read by this product alone becomes the operand's input
//! register, and a register that takes a product bit (of a product that is
//! one block, so there is no adder in between) becomes the product
//! register. The block's registers share one clock, enable and clear, so
//! only registers with the same controls move in, and only the kinds the
//! block has: rising edge, active-high enable, asynchronous clear to 0,
//! powering up 0.

use std::collections::{BTreeMap, HashMap, HashSet};

use chipsmith_netlist::{
    Binary, BinaryOp, Cell, CellId, CellKind, Connectivity, DffControl, Dir, Endpoint, Ff, FfKind,
    Generic, GenericPort, Module, Net, Operand, ParamValue, Polarity, SigBit, SigSpec, State, Word,
};
use chipsmith_synth::rewrite::{inits, Subst};

use crate::mult::{Regs, BLOCKS, MAX_WIDTH};

/// Maps the products the blocks take, in netlist order; returns how many
/// blocks the design uses, its own `CY2_MULT` instances included.
pub fn map(m: &mut Module) -> usize {
    let instantiated = m
        .cells
        .iter()
        .filter(|c| matches!(&c.kind, CellKind::Other(g) if g.ty == super::cells::MULT))
        .count();
    let mut left = BLOCKS.saturating_sub(instantiated);
    let conn = Connectivity::new(m);
    let regs = Registers::index(m);
    let mut subst = Subst::new(m);
    let mut used = instantiated;
    let mut mapped = HashSet::new();
    let mut taken = Taken::default();
    let mut new_cells = Vec::new();
    for i in 0..m.cells.len() {
        let CellKind::Binary(b) = &m.cells[i].kind else {
            continue;
        };
        if b.op != BinaryOp::Mul {
            continue;
        }
        let Some(plan) = Plan::of(b).filter(|p| p.blocks() <= left) else {
            continue;
        };
        let (b, name) = (b.clone(), m.cells[i].name.clone());
        left -= plan.blocks();
        used += plan.blocks();
        let product = plan.build(
            m,
            &name,
            Context {
                cell: i,
                conn: &conn,
                regs: &regs,
                taken: &mut taken,
                subst: &mut subst,
            },
            &mut new_cells,
        );
        for (&y, p) in b.y.iter().zip(product) {
            if let SigBit::Net(n) = y {
                subst.set(n, p);
            }
        }
        mapped.insert(i);
    }
    // The products mapped go, and the register bits that moved into blocks.
    let mut i = 0;
    m.cells.retain_mut(|c| {
        let keep = !mapped.contains(&i);
        if let Some(bits) = taken.bits.get(&i) {
            let CellKind::Ff(ff) = &mut c.kind else {
                unreachable!("only registers lose bits")
            };
            drop_bits(ff, bits);
            if ff.width() == 0 {
                i += 1;
                return false;
            }
        }
        i += 1;
        keep
    });
    m.cells.extend(new_cells);
    subst.apply(m);
    used
}

/// What a block's registers need to agree on: the clock and, if any, the
/// enable and the clear (all rising edge or active high).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Controls {
    clk: SigBit,
    en: Option<SigBit>,
    clr: Option<SigBit>,
}

/// One bit of an eligible register.
#[derive(Debug, Clone, Copy)]
struct RegBit {
    cell: CellId,
    bit: usize,
    controls: Controls,
    d: SigBit,
}

/// The registers a block could take, by their output bits.
struct Registers {
    by_q: HashMap<Net, RegBit>,
}

impl Registers {
    fn index(m: &Module) -> Registers {
        let init = inits(m);
        let mut by_q = HashMap::new();
        for (cell, c) in m.cells.iter().enumerate() {
            let CellKind::Ff(ff) = &c.kind else {
                continue;
            };
            let FfKind::Dff { clk, d, control } = &ff.kind else {
                continue;
            };
            if clk.polarity != Polarity::Pos {
                continue;
            }
            let positive =
                |c: &chipsmith_netlist::Ctrl| (c.polarity == Polarity::Pos).then_some(c.sig);
            let (en, clr) = match control {
                DffControl::Plain { en } => (en.map(|e| positive(&e)), None),
                DffControl::Async { arst, value, en }
                    if value.0.iter().all(|&s| s == State::Zero) =>
                {
                    (en.map(|e| positive(&e)), Some(positive(arst)))
                }
                _ => continue,
            };
            // An enable or clear of the wrong polarity: the block can't
            // invert it.
            let (en, clr) = match (en, clr) {
                (Some(None), _) | (_, Some(None)) => continue,
                (en, clr) => (en.flatten(), clr.flatten()),
            };
            let controls = Controls {
                clk: clk.sig,
                en,
                clr,
            };
            for (bit, (&q, &d)) in ff.q.iter().zip(d).enumerate() {
                let SigBit::Net(q) = q else { continue };
                if init.get(&q) == Some(&State::One) {
                    continue;
                }
                by_q.insert(
                    q,
                    RegBit {
                        cell,
                        bit,
                        controls,
                        d,
                    },
                );
            }
        }
        Registers { by_q }
    }
}

/// Register bits that moved into blocks: by register cell, which bits.
#[derive(Default)]
struct Taken {
    bits: BTreeMap<CellId, Vec<usize>>,
}

impl Taken {
    fn take(&mut self, r: RegBit) {
        let v = self.bits.entry(r.cell).or_default();
        if !v.contains(&r.bit) {
            v.push(r.bit);
        }
    }
}

/// Removes `bits` (any order) from a word register.
fn drop_bits(ff: &mut Ff<Word>, bits: &[usize]) {
    let keep = |sig: &mut SigSpec| {
        let mut i = 0;
        sig.retain(|_| {
            let k = !bits.contains(&i);
            i += 1;
            k
        });
    };
    keep(&mut ff.q);
    match &mut ff.kind {
        FfKind::Dff { d, control, .. } => {
            keep(d);
            if let DffControl::Async { value, .. } = control {
                let mut i = 0;
                value.0.retain(|_| {
                    let k = !bits.contains(&i);
                    i += 1;
                    k
                });
            }
        }
        _ => unreachable!("only edge-triggered registers move into blocks"),
    }
}

/// What `build` needs to move registers: the product's own cell id (the
/// one user an operand register may have), the module's connectivity and
/// registers as they were, and what has moved so far.
struct Context<'a> {
    cell: CellId,
    conn: &'a Connectivity,
    regs: &'a Registers,
    taken: &'a mut Taken,
    subst: &'a mut Subst,
}

impl Context<'_> {
    /// The register bits behind an operand's bits, if every net bit comes
    /// from a register this product alone reads and they share controls.
    /// Constant bits need no register.
    fn operand_regs(&self, bits: &[SigBit]) -> Option<(Controls, Vec<RegBit>)> {
        let mut controls = None;
        let mut regs = Vec::new();
        for b in bits {
            let SigBit::Net(n) = b else { continue };
            let r = *self.regs.by_q.get(n)?;
            if controls.is_some_and(|c| c != r.controls) {
                return None;
            }
            controls = Some(r.controls);
            let only_this = self
                .conn
                .users(*n)
                .iter()
                .all(|u| matches!(u, Endpoint::Cell { cell, .. } if *cell == self.cell));
            if !only_this {
                return None;
            }
            regs.push(r);
        }
        Some((controls?, regs))
    }

    /// The register bits every used product bit (of the `$mul`'s own
    /// result, `y`) feeds, each with the bit's index, if they all are
    /// registers with one set of controls and nothing else reads the bit.
    fn product_regs(&self, y: &[SigBit]) -> Option<(Controls, Vec<(RegBit, usize)>)> {
        let mut controls = None;
        let mut regs = Vec::new();
        for (k, &b) in y.iter().enumerate() {
            let SigBit::Net(n) = b else { continue };
            for u in self.conn.users(n) {
                let &Endpoint::Cell { cell, bit, .. } = u else {
                    return None;
                };
                let r = self
                    .regs
                    .by_q
                    .values()
                    .find(|r| r.cell == cell && r.bit == bit as usize && r.d == b)?;
                if controls.is_some_and(|c| c != r.controls) {
                    return None;
                }
                controls = Some(r.controls);
                regs.push((*r, k));
            }
        }
        Some((controls?, regs))
    }
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
    /// The `$mul`'s result bits.
    y: SigSpec,
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
            y: b.y.clone(),
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
    fn build(&self, m: &mut Module, name: &str, cx: Context, out: &mut Vec<Cell>) -> SigSpec {
        let pairs = self.pairs();
        let single = pairs.len() == 1;
        let mut sum: Option<SigSpec> = None;
        for (i, j) in pairs {
            // The wider limb on operand A: quartus_map swaps a mac_mult's
            // operands to have it so (u32, s24: 14 x 18 and 6 x 18 came back
            // as 18 x 14 and 18 x 6), and the design assembles the same only
            // when the block is built that way.
            let (a, b) = if self.a[i].bits.len() < self.b[j].bits.len() {
                (&self.b[j], &self.a[i])
            } else {
                (&self.a[i], &self.b[j])
            };
            let dout = m.new_sig(a.bits.len() + b.bits.len());
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

            // Registers: the operands' first (they fix the controls), then
            // the product's when this block is the whole product.
            let mut controls: Option<Controls> = None;
            let mut regs = Regs::default();
            let mut operands = [a.bits.clone(), b.bits.clone()];
            for (k, f) in [a, b].into_iter().enumerate() {
                let Some((c, bits)) = cx.operand_regs(&f.bits) else {
                    continue;
                };
                if controls.is_some_and(|have| have != c) {
                    continue;
                }
                controls = Some(c);
                if k == 0 {
                    regs.a = true;
                } else {
                    regs.b = true;
                }
                let mut d = bits.iter().map(|r| r.d);
                for bit in operands[k].iter_mut() {
                    if matches!(bit, SigBit::Net(_)) {
                        *bit = d.next().expect("one register bit per net bit");
                    }
                }
                for r in bits {
                    cx.taken.take(r);
                }
            }
            if single {
                if let Some((c, bits)) = cx.product_regs(&self.y) {
                    if controls.is_none_or(|have| have == c) {
                        controls = Some(c);
                        regs.p = true;
                        for (r, k) in bits {
                            let SigBit::Net(q) = m.cells[r.cell].q_bit(r.bit) else {
                                unreachable!("an indexed register bit is a net")
                            };
                            cx.subst.set(q, part[k]);
                            cx.taken.take(r);
                        }
                    }
                }
            }
            out.push(Cell {
                name: format!("{name}.mult{i}_{j}"),
                kind: cell(
                    (&operands[0], a.signed),
                    (&operands[1], b.signed),
                    dout.clone(),
                    regs,
                    controls,
                ),
                attributes: Default::default(),
            });
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

trait QBit {
    fn q_bit(&self, bit: usize) -> SigBit;
}

impl QBit for Cell {
    fn q_bit(&self, bit: usize) -> SigBit {
        match &self.kind {
            CellKind::Ff(ff) => ff.q[bit],
            _ => unreachable!("a register"),
        }
    }
}

/// A `CY2_MULT` computing `P = A * B`, its registers (if any) on
/// `controls`.
fn cell(
    (a, a_signed): (&SigSpec, bool),
    (b, b_signed): (&SigSpec, bool),
    dout: SigSpec,
    regs: Regs,
    controls: Option<Controls>,
) -> CellKind {
    let port = |name: &str, dir, sig: SigSpec| GenericPort {
        name: name.into(),
        dir: Some(dir),
        sig,
    };
    let flag = |v: bool| ParamValue::int(i64::from(v));
    let mut params = BTreeMap::from([
        ("A_WIDTH".to_string(), ParamValue::int(a.len() as i64)),
        ("B_WIDTH".to_string(), ParamValue::int(b.len() as i64)),
        ("A_SIGNED".to_string(), flag(a_signed)),
        ("B_SIGNED".to_string(), flag(b_signed)),
    ]);
    for (k, v) in Regs::PARAMS.iter().zip(regs.flags()) {
        params.insert(k.to_string(), flag(v));
    }
    let mut ports = vec![port("A", Dir::In, a.clone()), port("B", Dir::In, b.clone())];
    if let Some(c) = controls {
        debug_assert!(regs.any());
        ports.push(port("CLK", Dir::In, vec![c.clk]));
        ports.push(port("ENA", Dir::In, vec![c.en.unwrap_or(SigBit::ONE)]));
        ports.push(port("ACLR", Dir::In, vec![c.clr.unwrap_or(SigBit::ZERO)]));
    }
    ports.push(port("P", Dir::Out, dout));
    CellKind::Other(Generic {
        ty: super::cells::MULT.into(),
        params,
        ports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mul(m: &mut Module, a: usize, b: usize, signed: bool, width: usize) -> Binary {
        let (a, b) = (m.new_sig(a), m.new_sig(b));
        mul_of(m, a, b, signed, width)
    }

    fn mul_of(m: &mut Module, a: SigSpec, b: SigSpec, signed: bool, width: usize) -> Binary {
        Binary {
            op: BinaryOp::Mul,
            a: Operand { sig: a, signed },
            b: Operand { sig: b, signed },
            y: m.new_sig(width),
        }
    }

    #[test]
    fn operands_up_to_18_bits_take_one_block() {
        let m = &mut Module::new("t");
        let p = Plan::of(&mul(m, 18, 12, false, 30)).unwrap();
        assert_eq!(p.blocks(), 1);
        let p = Plan::of(&mul(m, 18, 18, true, 36)).unwrap();
        assert_eq!((p.a[0].signed, p.b[0].signed), (true, true));
    }

    #[test]
    fn wide_operands_split_into_limbs_inside_the_result() {
        let m = &mut Module::new("t");
        // 32 x 32 -> 64: 2 x 2 limbs, all four products reach the result.
        let p = Plan::of(&mul(m, 32, 32, false, 64)).unwrap();
        assert_eq!(p.blocks(), 4);
        // 32 x 32 -> 32 (C's `*`): the high x high product starts at bit 36.
        let p = Plan::of(&mul(m, 32, 32, false, 32)).unwrap();
        assert_eq!(p.blocks(), 3);
        // Signed: the top limb carries the sign, the low one doesn't.
        let p = Plan::of(&mul(m, 24, 24, true, 48)).unwrap();
        assert_eq!(
            p.a.iter()
                .map(|f| (f.bits.len(), f.signed))
                .collect::<Vec<_>>(),
            [(18, false), (6, true)]
        );
    }

    #[test]
    fn constants_and_single_bits_stay_logic() {
        let m = &mut Module::new("t");
        let k: SigSpec = [State::One, State::Zero, State::One]
            .map(SigBit::Const)
            .to_vec();
        let a = m.new_sig(8);
        assert!(Plan::of(&mul_of(m, a, k, false, 11)).is_none());
        assert!(Plan::of(&mul(m, 8, 1, false, 9)).is_none());
    }

    #[test]
    fn redundant_top_bits_are_dropped() {
        let m = &mut Module::new("t");
        // Unsigned: constant zeros on top; signed: copies of the sign bit.
        let mut a = m.new_sig(6);
        a.extend([SigBit::ZERO; 4]);
        let b = m.new_sig(6);
        let p = Plan::of(&mul_of(m, a, b, false, 16)).unwrap();
        assert_eq!(p.a[0].bits.len(), 6);
        let mut a = m.new_sig(6);
        a.extend([a[5]; 3]);
        let b = m.new_sig(6);
        let p = Plan::of(&mul_of(m, a, b, true, 18)).unwrap();
        assert_eq!(p.a[0].bits.len(), 6);
    }

    /// A register read by the product alone moves in as its operand's
    /// register; one that also feeds something else stays.
    #[test]
    fn operand_registers_move_in_when_the_product_alone_reads_them() {
        let m = &mut Module::new("t");
        let clk = SigBit::Net(m.new_net());
        let (da, db, qa, qb) = (m.new_sig(9), m.new_sig(9), m.new_sig(9), m.new_sig(9));
        let dff = |d: SigSpec, q: SigSpec| {
            CellKind::Ff(Ff {
                q,
                kind: FfKind::Dff {
                    clk: chipsmith_netlist::Ctrl::pos(clk),
                    d,
                    control: DffControl::Plain { en: None },
                },
            })
        };
        m.add_cell("ra", dff(da.clone(), qa.clone()));
        m.add_cell("rb", dff(db.clone(), qb.clone()));
        let y = m.new_sig(18);
        m.add_cell(
            "p",
            CellKind::Binary(Binary {
                op: BinaryOp::Mul,
                a: Operand {
                    sig: qa.clone(),
                    signed: false,
                },
                b: Operand {
                    sig: qb.clone(),
                    signed: false,
                },
                y,
            }),
        );
        // qb also drives an output port.
        let mut w = chipsmith_netlist::Wire::new("o", qb.clone());
        w.port = Some(Dir::Out);
        m.wires.push(w);
        assert_eq!(map(m), 1);
        let mult = m
            .cells
            .iter()
            .find_map(|c| match &c.kind {
                CellKind::Other(g) if g.ty == super::super::cells::MULT => Some(g),
                _ => None,
            })
            .unwrap();
        assert_eq!(mult.params["A_REG"], ParamValue::int(1));
        assert_eq!(mult.params["B_REG"], ParamValue::int(0));
        assert_eq!(mult.port("A").unwrap().sig, da);
        assert_eq!(mult.port("B").unwrap().sig, qb);
        assert_eq!(mult.port("CLK").unwrap().sig, vec![clk]);
        // ra is gone, rb stays.
        assert!(m.cells.iter().all(|c| c.name != "ra"));
        assert!(m.cells.iter().any(|c| c.name == "rb"));
    }
}
