//! `$mul` onto DSP48E1s, after the Cyclone II's multiplier mapping
//! (chipsmith-cyclone2 `synth/mult.rs`, ADR-0010): a product of two
//! non-constant operands at least two bits wide goes to DSPs while the
//! device has them; wider operands are cut into limbs and the partial
//! products summed with adders, which then map like any other.
//!
//! What is the DSP48E1's own: its multiplier is 25 x 18 signed, so a limb
//! is at most 24 x 17 bits unsigned, or 25 x 18 where it carries the
//! operand's sign (the top limb of a signed operand); the wider operand
//! goes on A unless the narrower one there takes fewer DSPs. Its
//! registers each have their own clock enable and synchronous reset (to
//! 0, over the enable) on one shared clock, and power up 0: a
//! rising-edge register whose output only this product reads becomes the
//! operand's register (AREG, BREG), a register that takes the
//! product of a one-DSP product becomes PREG, and a register after that one
//! too makes the first MREG. Everything else on the DSP is off: P = A * B
//! (OPMODE 0000101, ALUMODE 0000).

use std::collections::{BTreeMap, HashMap, HashSet};

use chipsmith_netlist::{
    Binary, BinaryOp, Cell, CellId, CellKind, Connectivity, Ctrl, DffControl, Dir, Endpoint, Ff,
    FfKind, Generic, GenericPort, Module, Net, Operand, ParamValue, Polarity, SigBit, SigSpec, State,
    SyncEnable, Word,
};
use chipsmith_synth::rewrite::{inits, Subst};

use super::cells::DSP48E1;

/// DSP48E1s on the xc7z010.
pub const DSPS: usize = 80;
/// A limb's bits on A and B: unsigned, and carrying a sign.
const A_UNSIGNED: usize = 24;
const A_SIGNED: usize = 25;
const B_UNSIGNED: usize = 17;
const B_SIGNED: usize = 18;

/// Maps the products the DSPs take, in netlist order; returns how many
/// DSPs the design uses, its own DSP48E1 instances included.
pub fn map(m: &mut Module) -> usize {
    let instantiated = m
        .cells
        .iter()
        .filter(|c| matches!(&c.kind, CellKind::Other(g) if g.ty == DSP48E1))
        .count();
    let mut left = DSPS.saturating_sub(instantiated);
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

/// A register stage's own controls: an active-high enable and synchronous
/// reset to 0 (over the enable), and the clock all stages share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Controls {
    clk: Ctrl,
    en: Option<SigBit>,
    rst: Option<SigBit>,
}

#[derive(Debug, Clone, Copy)]
struct RegBit {
    cell: CellId,
    bit: usize,
    controls: Controls,
    d: SigBit,
}

/// The registers a DSP could take, by their output bits.
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
            let positive = |c: &Ctrl| (c.polarity == Polarity::Pos).then_some(c.sig);
            let (en, rst) = match control {
                DffControl::Plain { en } => (en.map(|e| positive(&e)), None),
                DffControl::Sync { srst, value, en } if value.0.iter().all(|&s| s == State::Zero) => {
                    let en = match en {
                        SyncEnable::Always => None,
                        SyncEnable::ResetOverEnable(e) => Some(positive(e)),
                        SyncEnable::EnableOverReset(_) => continue,
                    };
                    (en, Some(positive(srst)))
                }
                _ => continue,
            };
            // An enable or reset of the wrong polarity: the DSP can't
            // invert them.
            let (en, rst) = match (en, rst) {
                (Some(None), _) | (_, Some(None)) => continue,
                (en, rst) => (en.flatten(), rst.flatten()),
            };
            // Falling-edge registers stay in the fabric: the DSP would
            // take them with IS_CLK_INVERTED, which yosys' DSP48E1 model
            // (the co-simulations') refuses. Remove once the co-simulation
            // folds the inversion into CLK the way the decoder does.
            if clk.polarity == Polarity::Neg {
                continue;
            }
            let controls = Controls { clk: *clk, en, rst };
            for (bit, (&q, &d)) in ff.q.iter().zip(d).enumerate() {
                let SigBit::Net(q) = q else { continue };
                // The DSP's registers power up 0.
                if init.get(&q) == Some(&State::One) {
                    continue;
                }
                by_q.insert(q, RegBit { cell, bit, controls, d });
            }
        }
        Registers { by_q }
    }
}

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
            if let DffControl::Sync { value, .. } = control {
                let mut i = 0;
                value.0.retain(|_| {
                    let k = !bits.contains(&i);
                    i += 1;
                    k
                });
            }
        }
        _ => unreachable!("only edge-triggered registers move into DSPs"),
    }
}

struct Context<'a> {
    cell: CellId,
    conn: &'a Connectivity,
    regs: &'a Registers,
    taken: &'a mut Taken,
    subst: &'a mut Subst,
}

impl Context<'_> {
    /// The register bits behind an operand's bits, if every net bit comes
    /// from a register only `reader` reads, all on one set of controls.
    fn regs_of(&self, bits: &[SigBit], reader: Option<CellId>) -> Option<(Controls, Vec<RegBit>)> {
        let mut controls = None;
        let mut regs = Vec::new();
        for b in bits {
            let SigBit::Net(n) = b else { continue };
            let r = *self.regs.by_q.get(n)?;
            if controls.is_some_and(|c| c != r.controls) {
                return None;
            }
            controls = Some(r.controls);
            let only = self.conn.users(*n).iter().all(|u| match (u, reader) {
                (Endpoint::Cell { cell, .. }, Some(rd)) => *cell == rd,
                _ => false,
            });
            if !only {
                return None;
            }
            regs.push(r);
        }
        Some((controls?, regs))
    }

    /// The register bits every used bit of `y` feeds (each with the bit's
    /// index), if they all are registers on one set of controls and
    /// nothing else reads the bits.
    fn regs_after(&self, y: &[SigBit]) -> Option<(Controls, Vec<(RegBit, usize)>)> {
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
    fn new(op: &Operand, signed: bool, width: usize) -> Factor {
        let mut bits: SigSpec = op.sig.iter().copied().take(width).collect();
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

    /// Limbs, least significant first: unsigned chunks of `unsigned` bits
    /// until what is left fits the port (`signed_width` if the operand is
    /// signed: the top limb carries the sign).
    fn limbs(&self, unsigned: usize, signed_width: usize) -> Vec<Factor> {
        let fits = if self.signed { signed_width } else { unsigned };
        let mut out = Vec::new();
        let mut rest: &[SigBit] = &self.bits;
        while rest.len() > fits {
            out.push(Factor {
                bits: rest[..unsigned].to_vec(),
                signed: false,
            });
            rest = &rest[unsigned..];
        }
        out.push(Factor {
            bits: rest.to_vec(),
            signed: self.signed,
        });
        out
    }
}

/// How one `$mul` maps: A's limbs, B's, and where
/// each limb starts.
#[derive(Debug)]
struct Plan {
    a: Vec<Factor>,
    b: Vec<Factor>,
    width: usize,
    y: SigSpec,
}

impl Plan {
    fn of(b: &Binary) -> Option<Plan> {
        let width = b.y.len();
        let signed = b.a.signed && b.b.signed;
        let mut fa = Factor::new(&b.a, signed, width);
        let mut fb = Factor::new(&b.b, signed, width);
        if fa.bits.len() < 2 || fb.bits.len() < 2 || fa.is_constant() || fb.is_constant() {
            return None;
        }
        if fa.bits.len() < fb.bits.len() {
            std::mem::swap(&mut fa, &mut fb);
        }
        let plan = |a: &Factor, b_: &Factor| Plan {
            a: a.limbs(A_UNSIGNED, A_SIGNED),
            b: b_.limbs(B_UNSIGNED, B_SIGNED),
            width,
            y: b.y.clone(),
        };
        // The narrower operand on A can take fewer DSPs (26 x 22 signed:
        // two, as Vivado builds it, against four).
        let (wide_on_a, narrow_on_a) = (plan(&fa, &fb), plan(&fb, &fa));
        let plan = if narrow_on_a.blocks() < wide_on_a.blocks() { narrow_on_a } else { wide_on_a };
        Some(plan).filter(|p| p.blocks() > 0)
    }

    fn shift(&self, i: usize, j: usize) -> usize {
        i * A_UNSIGNED + j * B_UNSIGNED
    }

    fn pairs(&self) -> Vec<(usize, usize)> {
        let mut v = Vec::new();
        for i in 0..self.a.len() {
            for j in 0..self.b.len() {
                if self.shift(i, j) < self.width {
                    v.push((i, j));
                }
            }
        }
        v
    }

    fn blocks(&self) -> usize {
        self.pairs().len()
    }

    fn build(&self, m: &mut Module, name: &str, cx: Context, out: &mut Vec<Cell>) -> SigSpec {
        let pairs = self.pairs();
        let single = pairs.len() == 1;
        // Each DSP: its limbs, configuration, operands and P.
        let mut dsps: Vec<((usize, usize), Dsp, [SigSpec; 2], SigSpec)> = Vec::new();
        for (i, j) in pairs {
            let (a, b) = (&self.a[i], &self.b[j]);
            let p = m.new_sig(48);
            let shift = self.shift(i, j);
            let part: SigSpec = (0..self.width)
                .map(|k| match k.checked_sub(shift) {
                    None => SigBit::ZERO,
                    // P is the product sign-extended to 48 bits (zero
                    // extended for unsigned limbs, whose ports hold a 0 on
                    // top): bits past the 43 the multiplier makes copy its
                    // top bit.
                    Some(k) => p[k.min(47)],
                })
                .collect();

            let mut dsp = Dsp::default();
            let mut clk: Option<Ctrl> = None;
            let mut operands = [a.bits.clone(), b.bits.clone()];
            for (k, f) in [a, b].into_iter().enumerate() {
                let Some((c, bits)) = cx.regs_of(&f.bits, Some(cx.cell)) else {
                    continue;
                };
                if clk.is_some_and(|have| have != c.clk) {
                    continue;
                }
                clk = Some(c.clk);
                dsp.stage[k] = Some(c);
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
                // The product's register, and one after it: MREG then PREG.
                if let Some((c, bits)) = cx.regs_after(&self.y) {
                    if clk.is_none_or(|have| have == c.clk) {
                        clk = Some(c.clk);
                        let qs: Vec<SigBit> = bits
                            .iter()
                            .map(|(r, _)| match &m.cells[r.cell].kind {
                                CellKind::Ff(ff) => ff.q[r.bit],
                                _ => unreachable!("a register"),
                            })
                            .collect();
                        let second = cx.regs_after(&qs).filter(|(c2, _)| c2.clk == c.clk);
                        match second {
                            Some((c2, bits2)) => {
                                dsp.stage[2] = Some(c);
                                dsp.stage[3] = Some(c2);
                                for (r, _) in &bits {
                                    cx.taken.take(*r);
                                }
                                for (r2, k2) in bits2 {
                                    let SigBit::Net(q) = (match &m.cells[r2.cell].kind {
                                        CellKind::Ff(ff) => ff.q[r2.bit],
                                        _ => unreachable!("a register"),
                                    }) else {
                                        unreachable!("an indexed register bit is a net")
                                    };
                                    // bits2 index the first stage's outputs;
                                    // they are the product's bits in order.
                                    let k = bits[k2].1;
                                    cx.subst.set(q, part[k]);
                                    cx.taken.take(r2);
                                }
                            }
                            None => {
                                dsp.stage[3] = Some(c);
                                for (r, k) in bits {
                                    let SigBit::Net(q) = (match &m.cells[r.cell].kind {
                                        CellKind::Ff(ff) => ff.q[r.bit],
                                        _ => unreachable!("a register"),
                                    }) else {
                                        unreachable!("an indexed register bit is a net")
                                    };
                                    cx.subst.set(q, part[k]);
                                    cx.taken.take(r);
                                }
                            }
                        }
                    }
                }
            }
            dsp.clk = clk;
            dsps.push(((i, j), dsp, operands, p));
        }
        // A's limb times two B limbs 17 bits apart in two DSPs that
        // cascade, as Vivado builds them: the upper adds the lower's P
        // shifted down 17 bits (PCIN >>> 17), so the two make one partial
        // product and one adder less in the fabric. Only DSPs without
        // registers: one with a stage the other lacks would add its PCIN a
        // cycle off.
        let cascades = |lo: &((usize, usize), Dsp, [SigSpec; 2], SigSpec), hi: &((usize, usize), Dsp, [SigSpec; 2], SigSpec)| {
            let ((i, j), (i2, j2)) = (lo.0, hi.0);
            i == i2 && j % 2 == 0 && j2 == j + 1 && lo.1.is_combinational() && hi.1.is_combinational()
        };
        let mut sum: Option<SigSpec> = None;
        let mut k = 0;
        while k < dsps.len() {
            let pair = k + 1 < dsps.len() && cascades(&dsps[k], &dsps[k + 1]);
            let ((i, j), _, _, _) = dsps[k];
            let shift = self.shift(i, j);
            let signed = |d: &((usize, usize), Dsp, [SigSpec; 2], SigSpec)| (self.a[d.0 .0].signed, self.b[d.0 .1].signed);
            let part: SigSpec = if pair {
                let (lo, hi) = (&dsps[k], &dsps[k + 1]);
                let pcout = m.new_sig(48);
                let (sa, sb) = signed(lo);
                out.push(Cell {
                    name: format!("{name}.dsp{i}_{j}"),
                    kind: lo.1.cell((&lo.2[0], sa), (&lo.2[1], sb), lo.3.clone(), Cascade::Out(pcout.clone())),
                    attributes: Default::default(),
                });
                let (sa, sb) = signed(hi);
                out.push(Cell {
                    name: format!("{name}.dsp{i}_{}", j + 1),
                    kind: hi.1.cell((&hi.2[0], sa), (&hi.2[1], sb), hi.3.clone(), Cascade::In(pcout)),
                    attributes: Default::default(),
                });
                // The lower's low 17 bits, then the upper's P (sign
                // extended to 48 bits as a single DSP's).
                (0..self.width)
                    .map(|b| match b.checked_sub(shift) {
                        None => SigBit::ZERO,
                        Some(b) if b < B_UNSIGNED => lo.3[b],
                        Some(b) => hi.3[(b - B_UNSIGNED).min(47)],
                    })
                    .collect()
            } else {
                let d = &dsps[k];
                let (sa, sb) = signed(d);
                out.push(Cell {
                    name: format!("{name}.dsp{i}_{j}"),
                    kind: d.1.cell((&d.2[0], sa), (&d.2[1], sb), d.3.clone(), Cascade::None),
                    attributes: Default::default(),
                });
                (0..self.width)
                    .map(|b| match b.checked_sub(shift) {
                        None => SigBit::ZERO,
                        Some(b) => d.3[b.min(47)],
                    })
                    .collect()
            };
            sum = Some(match sum {
                None => part,
                Some(acc) => {
                    let y = m.new_sig(self.width);
                    out.push(Cell {
                        name: format!("{name}.add{i}_{j}"),
                        kind: CellKind::Binary(Binary {
                            op: BinaryOp::Add,
                            a: Operand { sig: acc, signed: false },
                            b: Operand { sig: part, signed: false },
                            y: y.clone(),
                        }),
                        attributes: Default::default(),
                    });
                    y
                }
            });
            k += if pair { 2 } else { 1 };
        }
        sum.expect("a planned product has a DSP")
    }
}

/// A DSP48E1's place in a cascade.
enum Cascade {
    None,
    /// The lower of two: its PCOUT.
    Out(SigSpec),
    /// The upper: PCIN, which it adds shifted down 17 bits.
    In(SigSpec),
}

/// One DSP48E1's register stages: A, B, M, P.
#[derive(Debug, Default)]
struct Dsp {
    stage: [Option<Controls>; 4],
    clk: Option<Ctrl>,
}

impl Dsp {
    fn is_combinational(&self) -> bool {
        self.stage.iter().all(Option::is_none)
    }

    /// A DSP48E1 computing P = A * B (plus PCIN >>> 17 at a cascade's
    /// upper end), A and B extended to its 30 and 18 bits, its registers on
    /// `stage`.
    fn cell(&self, (a, a_signed): (&SigSpec, bool), (b, b_signed): (&SigSpec, bool), p: SigSpec, cascade: Cascade) -> CellKind {
        let port = |name: &str, dir, sig: SigSpec| GenericPort {
            name: name.into(),
            dir: Some(dir),
            sig,
        };
        let extend = |s: &SigSpec, signed: bool, width: usize| -> SigSpec {
            let fill = if signed { *s.last().expect("an operand bit") } else { SigBit::ZERO };
            (0..width).map(|k| s.get(k).copied().unwrap_or(fill)).collect()
        };
        let int = |v: i64| ParamValue::int(v);
        let flag = |s: usize| int(i64::from(self.stage[s].is_some()));
        let mut params = BTreeMap::from([
            ("AREG".to_string(), flag(0)),
            ("ACASCREG".to_string(), flag(0)),
            ("BREG".to_string(), flag(1)),
            ("BCASCREG".to_string(), flag(1)),
            ("MREG".to_string(), flag(2)),
            ("PREG".to_string(), flag(3)),
            ("A_INPUT".to_string(), ParamValue::Str("DIRECT".into())),
            ("B_INPUT".to_string(), ParamValue::Str("DIRECT".into())),
            ("USE_MULT".to_string(), ParamValue::Str("MULTIPLY".into())),
            ("USE_DPORT".to_string(), ParamValue::Str("FALSE".into())),
            ("USE_SIMD".to_string(), ParamValue::Str("ONE48".into())),
            (
                "IS_CLK_INVERTED".to_string(),
                ParamValue::Bits(chipsmith_netlist::Const(vec![State::from(
                    self.clk.is_some_and(|c| c.polarity == Polarity::Neg),
                )])),
            ),
        ]);
        for r in ["CREG", "DREG", "ADREG", "INMODEREG", "OPMODEREG", "ALUMODEREG", "CARRYINREG", "CARRYINSELREG"] {
            params.insert(r.into(), int(0));
        }
        let bit = |b: SigBit| vec![b];
        let konst = |v: u64, w: usize| -> SigSpec { (0..w).map(|k| SigBit::Const(State::from(v >> k & 1 == 1))).collect() };
        let ce = |s: usize| bit(self.stage[s].map_or(SigBit::ZERO, |c| c.en.unwrap_or(SigBit::ONE)));
        let rst = |s: usize| bit(self.stage[s].and_then(|c| c.rst).unwrap_or(SigBit::ZERO));
        let mut ports = vec![
            port("A", Dir::In, extend(a, a_signed, 30)),
            port("B", Dir::In, extend(b, b_signed, 18)),
            port("C", Dir::In, konst(0, 48)),
            port("D", Dir::In, konst(0, 25)),
            port("CLK", Dir::In, bit(self.clk.map_or(SigBit::ZERO, |c| c.sig))),
            // Z: 0, or PCIN >>> 17 (101); X and Y: the product.
            port(
                "OPMODE",
                Dir::In,
                konst(if matches!(cascade, Cascade::In(_)) { 0b101_0101 } else { 0b000_0101 }, 7),
            ),
            port("ALUMODE", Dir::In, konst(0, 4)),
            port("INMODE", Dir::In, konst(0, 5)),
            port("CARRYINSEL", Dir::In, konst(0, 3)),
            port("CARRYIN", Dir::In, bit(SigBit::ZERO)),
            port("CEA1", Dir::In, bit(SigBit::ZERO)),
            port("CEA2", Dir::In, ce(0)),
            port("CEB1", Dir::In, bit(SigBit::ZERO)),
            port("CEB2", Dir::In, ce(1)),
            port("CEM", Dir::In, ce(2)),
            port("CEP", Dir::In, ce(3)),
            port("RSTA", Dir::In, rst(0)),
            port("RSTB", Dir::In, rst(1)),
            port("RSTM", Dir::In, rst(2)),
            port("RSTP", Dir::In, rst(3)),
        ];
        for z in ["CEC", "CED", "CEAD", "CEALUMODE", "CECTRL", "CECARRYIN", "CEINMODE", "RSTC", "RSTD", "RSTALLCARRYIN", "RSTALUMODE", "RSTCTRL", "RSTINMODE"] {
            ports.push(port(z, Dir::In, bit(SigBit::ZERO)));
        }
        ports.push(port("P", Dir::Out, p));
        match cascade {
            Cascade::None => {}
            Cascade::Out(pcout) => ports.push(port("PCOUT", Dir::Out, pcout)),
            Cascade::In(pcin) => ports.push(port("PCIN", Dir::In, pcin)),
        }
        CellKind::Other(Generic {
            ty: DSP48E1.into(),
            params,
            ports,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mul(m: &mut Module, a: usize, b: usize, signed: bool, width: usize) -> Binary {
        let (a, b) = (m.new_sig(a), m.new_sig(b));
        Binary {
            op: BinaryOp::Mul,
            a: Operand { sig: a, signed },
            b: Operand { sig: b, signed },
            y: m.new_sig(width),
        }
    }

    #[test]
    fn operands_up_to_25_by_18_take_one_dsp() {
        let m = &mut Module::new("t");
        assert_eq!(Plan::of(&mul(m, 25, 18, true, 43)).unwrap().blocks(), 1);
        assert_eq!(Plan::of(&mul(m, 24, 17, false, 41)).unwrap().blocks(), 1);
        // Unsigned 25 bits don't fit A's 24 unsigned: a second limb.
        assert_eq!(Plan::of(&mul(m, 25, 17, false, 42)).unwrap().blocks(), 2);
        // The narrower operand goes on B.
        let p = Plan::of(&mul(m, 10, 20, false, 30)).unwrap();
        assert_eq!((p.a[0].bits.len(), p.b[0].bits.len()), (20, 10));
    }

    #[test]
    fn wide_operands_split_into_limbs_inside_the_result() {
        let m = &mut Module::new("t");
        // 32 x 32 -> 64: A in 24 + 8, B in 17 + 15: four DSPs.
        let p = Plan::of(&mul(m, 32, 32, false, 64)).unwrap();
        assert_eq!(p.blocks(), 4);
        // Signed: the top limb carries the sign.
        let p = Plan::of(&mul(m, 32, 32, true, 64)).unwrap();
        assert_eq!(
            p.a.iter().map(|f| (f.bits.len(), f.signed)).collect::<Vec<_>>(),
            [(24, false), (8, true)]
        );
        // 32 x 32 -> 32: the high x high product starts at bit 41.
        assert_eq!(Plan::of(&mul(m, 32, 32, false, 32)).unwrap().blocks(), 3);
        // 26 x 22 signed: the 22 bits fit A whole, the 26 split on B.
        let p = Plan::of(&mul(m, 26, 22, true, 48)).unwrap();
        assert_eq!((p.blocks(), p.a[0].bits.len()), (2, 22));
    }

    /// 32 x 32 -> 64 unsigned: each of A's two limbs times B's two in a
    /// cascaded pair (the upper's OPMODE adds PCIN >>> 17, its PCIN the
    /// lower's PCOUT), so one adder sums the two pairs, not three the four.
    #[test]
    fn b_limbs_cascade_in_pairs() {
        let mut m = Module::new("t");
        let b = mul(&mut m, 32, 32, false, 64);
        m.add_cell("p", CellKind::Binary(b));
        assert_eq!(map(&mut m), 4);
        let port = |g: &Generic, name: &str| g.ports.iter().find(|p| p.name == name).map(|p| p.sig.clone());
        let dsps: Vec<&Generic> = m
            .cells
            .iter()
            .filter_map(|c| match &c.kind {
                CellKind::Other(g) if g.ty == DSP48E1 => Some(g),
                _ => None,
            })
            .collect();
        let uppers: Vec<&&Generic> = dsps.iter().filter(|g| port(g, "PCIN").is_some()).collect();
        assert_eq!(uppers.len(), 2);
        for u in uppers {
            let opmode = port(u, "OPMODE").unwrap();
            let v = opmode.iter().enumerate().fold(0, |v, (k, b)| v | u64::from(*b == SigBit::ONE) << k);
            assert_eq!(v, 0b101_0101, "Z = PCIN >>> 17, X = Y = M");
            let pcin = port(u, "PCIN").unwrap();
            assert!(dsps.iter().any(|l| port(l, "PCOUT") == Some(pcin.clone())), "PCIN is a lower DSP's PCOUT");
        }
        let adders = m
            .cells
            .iter()
            .filter(|c| matches!(&c.kind, CellKind::Binary(b) if b.op == BinaryOp::Add))
            .count();
        assert_eq!(adders, 1);
    }
}
