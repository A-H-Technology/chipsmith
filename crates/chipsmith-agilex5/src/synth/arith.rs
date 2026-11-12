//! `$alu` onto `A5_ARITH` chains, as tools/a5pnr/yosys/arith_map.v does it
//! (README (w)): a cell per bit with p = A ^ B ^ C, g = A & (B ^ C) over A =
//! a[i], B = b[i] and C = BI, after a start cell whose carry out is CI (the
//! hardware chain has no free carry-in). A BI that is a signal goes into B
//! through an XOR in front instead, since a5pnr's arithmetic half takes two
//! signals. A carry out that is read is the sum of one more cell (p = g = 0)
//! for the top bit, and the next bit's sum XOR its propagate below it.

use std::collections::BTreeMap;

use chipsmith_netlist::{
    Alu, CellKind, Const, Dir, Generic, GenericPort, Module, Net, ParamValue, SigBit, SigSpec,
    State,
};
use chipsmith_synth::rewrite::{Gates, Subst};
use chipsmith_synth::techmap::extend;

pub const ARITH: &str = "A5_ARITH";

/// Adders this short are cheaper as LUTs.
const MIN_WIDTH: usize = 2;

#[derive(Debug, Default, Clone, Copy)]
pub struct Counts {
    pub chains: usize,
    pub halves: usize,
}

fn cell(
    p: u16,
    g: u16,
    [a, b, c, d]: [SigBit; 4],
    ci: SigBit,
    so: Option<SigBit>,
    co: Option<SigBit>,
) -> CellKind {
    let bits = |v: u16| {
        ParamValue::Bits(Const(
            (0..16).map(|i| State::from(v >> i & 1 == 1)).collect(),
        ))
    };
    let port = |name: &str, dir, b: SigBit| GenericPort {
        name: name.into(),
        dir: Some(dir),
        sig: vec![b],
    };
    let mut ports = vec![
        port("A", Dir::In, a),
        port("B", Dir::In, b),
        port("C", Dir::In, c),
        port("D", Dir::In, d),
        port("CI", Dir::In, ci),
    ];
    ports.extend(so.map(|s| port("SO", Dir::Out, s)));
    ports.extend(co.map(|s| port("CO", Dir::Out, s)));
    CellKind::Other(Generic {
        ty: ARITH.into(),
        params: BTreeMap::from([("P".into(), bits(p)), ("G".into(), bits(g))]),
        ports,
    })
}

pub fn map(m: &mut Module) -> Counts {
    let mut read: std::collections::HashSet<Net> = Default::default();
    for c in &m.cells {
        c.kind.for_each_port(|p| {
            if p.dir.is_none_or(|d| d.reads()) {
                read.extend(p.bits.iter().filter_map(|b| b.net()));
            }
        });
    }
    for (_, w, d) in m.ports() {
        if d.drives() {
            read.extend(w.bits.iter().filter_map(|b| b.net()));
        }
    }
    let mut g = Gates::new(m, "arith");
    let cells = std::mem::take(&mut g.m.cells);
    let mut subst = Subst::new(g.m);
    let mut n = Counts::default();
    for c in cells {
        match c.kind {
            CellKind::Alu(alu) if alu.y.len() >= MIN_WIDTH => {
                chain(&mut g, &mut subst, &alu, &read, &mut n)
            }
            kind => g.m.cells.push(chipsmith_netlist::Cell { kind, ..c }),
        }
    }
    subst.apply(m);
    n
}

fn chain(
    g: &mut Gates,
    subst: &mut Subst,
    alu: &Alu,
    read: &std::collections::HashSet<Net>,
    n: &mut Counts,
) {
    let w = alu.y.len();
    let is_read = |b: SigBit| b.net().is_some_and(|x| read.contains(&x));
    let a = extend(&alu.a.sig, w, alu.a.signed);
    let b = extend(&alu.b.sig, w, alu.b.signed);
    // B as the chain sees it, and what goes on C.
    let (bb, c_in): (SigSpec, SigBit) = match alu.bi {
        SigBit::Const(_) => (b.clone(), alu.bi),
        bi => (b.iter().map(|&x| g.xor(x, bi)).collect(), SigBit::ZERO),
    };
    let (p, gen) = if c_in == SigBit::ZERO {
        (0x6666, 0x8888)
    } else {
        (0x9696, 0x2828)
    };
    let fresh = |g: &mut Gates| SigBit::Net(g.m.new_net());
    let mut carry = fresh(g);
    g.add(cell(
        0,
        0xAAAA,
        [alu.ci, SigBit::ZERO, SigBit::ZERO, SigBit::ZERO],
        SigBit::ZERO,
        None,
        Some(carry),
    ));
    let mut y = Vec::with_capacity(w);
    let mut prop = Vec::with_capacity(w);
    for i in 0..w {
        let (s, co) = (fresh(g), fresh(g));
        g.add(cell(
            p,
            gen,
            [a[i], bb[i], c_in, SigBit::ZERO],
            carry,
            Some(s),
            Some(co),
        ));
        y.push(s);
        // The propagate (X), as a gate: LUT mapping merges it with what reads it.
        let bx = if c_in == SigBit::ZERO {
            bb[i]
        } else {
            g.xor(bb[i], c_in)
        };
        prop.push(g.xor(a[i], bx));
        carry = co;
    }
    n.halves += w + 1;
    let top_read = is_read(alu.co[w - 1]);
    let top = if top_read {
        let s = fresh(g);
        g.add(cell(0, 0, [SigBit::ZERO; 4], carry, Some(s), None));
        n.halves += 1;
        s
    } else {
        SigBit::ZERO
    };
    n.chains += 1;
    for i in 0..w {
        let co = match i {
            i if !is_read(alu.co[i]) => continue,
            i if i + 1 < w => g.xor(y[i + 1], prop[i + 1]),
            _ => top,
        };
        if let SigBit::Net(x) = alu.co[i] {
            subst.set(x, co);
        }
    }
    for (outs, vals) in [(&alu.y, &y), (&alu.x, &prop)] {
        for (o, v) in outs.iter().zip(vals.iter()) {
            if let SigBit::Net(x) = o {
                subst.set(*x, *v);
            }
        }
    }
}

/// `(a + b) + c` or `(a + c) + b` for a single unsigned bit c, as
/// chipsmith's `alumacc` leaves it (two adders): one adder of a and b
/// with c as its carry-in, which the chain's start cell takes (yosys'
/// `alumacc` does the same: README (z14), arith 24 → 23 ALMs). The first
/// adder's sum must be read by the second alone, at the same width.
/// Returns how many were folded.
pub fn fold_carry_in(m: &mut Module) -> usize {
    let mut reads: std::collections::HashMap<Net, usize> = Default::default();
    for c in &m.cells {
        c.kind.for_each_port(|p| {
            if p.dir.is_none_or(|d| d.reads()) {
                for n in p.bits.iter().filter_map(|b| b.net()) {
                    *reads.entry(n).or_default() += 1;
                }
            }
        });
    }
    for (_, w, d) in m.ports() {
        if d.drives() {
            for n in w.bits.iter().filter_map(|b| b.net()) {
                *reads.entry(n).or_default() += 1;
            }
        }
    }
    let unread = |s: &SigSpec| {
        s.iter()
            .all(|b| b.net().is_none_or(|n| !reads.contains_key(&n)))
    };
    // An unsigned operand that is one bit, zero-extended.
    let one_bit = |o: &chipsmith_netlist::Operand| {
        (!o.signed && !o.sig.is_empty() && o.sig.iter().skip(1).all(|&b| b == SigBit::ZERO))
            .then(|| o.sig[0])
    };
    let plain =
        |z: &Alu| z.ci == SigBit::ZERO && z.bi == SigBit::ZERO && unread(&z.x) && unread(&z.co);
    let mut sum_of: std::collections::HashMap<SigSpec, usize> = Default::default();
    for (i, c) in m.cells.iter().enumerate() {
        if let CellKind::Alu(z) = &c.kind {
            if plain(z)
                && z.y
                    .iter()
                    .all(|b| b.net().is_none_or(|n| reads.get(&n) == Some(&1)))
            {
                sum_of.insert(z.y.clone(), i);
            }
        }
    }
    // (outer, inner, the operand of outer to replace with inner's wide one,
    // or None: inner takes outer's place; the carry-in)
    #[allow(clippy::type_complexity)]
    let mut folds: Vec<(
        usize,
        usize,
        Option<(bool, chipsmith_netlist::Operand)>,
        SigBit,
    )> = Vec::new();
    let mut used = std::collections::HashSet::new();
    for (i, c) in m.cells.iter().enumerate() {
        let CellKind::Alu(o) = &c.kind else { continue };
        if !plain(o) {
            continue;
        }
        for (first, wide, other) in [(true, &o.a, &o.b), (false, &o.b, &o.a)] {
            let Some(&z) = sum_of.get(&wide.sig) else {
                continue;
            };
            let CellKind::Alu(inner) = &m.cells[z].kind else {
                unreachable!()
            };
            if z == i
                || inner.y.len() != o.y.len()
                || wide.sig.len() != o.y.len()
                || used.contains(&z)
                || used.contains(&i)
            {
                continue;
            }
            let fold = if let Some(bit) = one_bit(other) {
                Some((None, bit))
            } else if let Some(bit) = one_bit(&inner.b) {
                Some((Some((first, inner.a.clone())), bit))
            } else {
                one_bit(&inner.a).map(|bit| (Some((first, inner.b.clone())), bit))
            };
            if let Some((op, bit)) = fold {
                used.insert(z);
                used.insert(i);
                folds.push((i, z, op, bit));
                break;
            }
        }
    }
    let n = folds.len();
    let mut gone = vec![false; m.cells.len()];
    for (outer, inner, op, ci) in folds {
        match op {
            // outer = sum + bit: inner adds the bit as its carry and drives outer's sum.
            None => {
                let CellKind::Alu(o) = &m.cells[outer].kind else {
                    unreachable!()
                };
                let y = o.y.clone();
                let CellKind::Alu(z) = &mut m.cells[inner].kind else {
                    unreachable!()
                };
                z.ci = ci;
                z.y = y;
                gone[outer] = true;
            }
            // inner = wide + bit: outer adds inner's wide operand and the bit.
            Some((first, wide)) => {
                let CellKind::Alu(o) = &mut m.cells[outer].kind else {
                    unreachable!()
                };
                if first {
                    o.a = wide;
                } else {
                    o.b = wide;
                }
                o.ci = ci;
                gone[inner] = true;
            }
        }
    }
    let mut k = gone.into_iter();
    m.cells.retain(|_| !k.next().unwrap());
    n
}
