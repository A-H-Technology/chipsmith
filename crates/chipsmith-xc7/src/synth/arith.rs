//! `$alu` onto CARRY4 chains, as yosys' `arith_map.v` (`_80_xilinx_alu`):
//! per bit the propagate `S = A ^ B'` (a gate, which LUT mapping makes the
//! LUT in front of the chain, merged with whatever drives A and B), the
//! generate `DI = A`, and `Y` the chain's sums. A constant carry-in or a
//! signal one enters at the head's CYINIT. A carry out is the chain's CO,
//! unless that bit's sum is read too: a slice leaves each bit's sum and
//! carry on one pin (`<L>MUX`), so then the carry comes from the next bit,
//! `O[i+1] ^ S[i+1]` (a LUT), or for the top bit from the sum of one more
//! chain bit with S and DI 0. `X` is the propagates.

use std::collections::HashSet;

use chipsmith_netlist::{Alu, CellKind, Module, Net, SigBit, SigSpec};
use chipsmith_synth::rewrite::{Gates, Subst};
use chipsmith_synth::techmap::extend;

use super::cells::Carry4;

/// Adders this short are cheaper as plain LUTs (`arith_map.v`'s
/// `_TECHMAP_FAIL_`).
const MIN_WIDTH: usize = 3;

/// Maps every `$alu` at least [`MIN_WIDTH`] wide; returns how many CARRY4s.
pub fn map(m: &mut Module) -> usize {
    let mut read: HashSet<Net> = HashSet::new();
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
    let mut g = Gates::new(m, "carry");
    let cells = std::mem::take(&mut g.m.cells);
    let mut subst = Subst::new(g.m);
    let mut carries = 0;
    for c in cells {
        match c.kind {
            CellKind::Alu(alu) if alu.y.len() >= MIN_WIDTH => {
                carries += chain(&mut g, &mut subst, &alu, &read);
            }
            kind => g.m.cells.push(chipsmith_netlist::Cell { kind, ..c }),
        }
    }
    subst.apply(m);
    carries
}

fn chain(g: &mut Gates, subst: &mut Subst, alu: &Alu, read: &HashSet<Net>) -> usize {
    let w = alu.y.len();
    let is_read = |b: SigBit| b.net().is_some_and(|n| read.contains(&n));
    let both = |i: usize| is_read(alu.y[i]) && is_read(alu.co[i]);
    let a = extend(&alu.a.sig, w, alu.a.signed);
    let b = extend(&alu.b.sig, w, alu.b.signed);
    let bb: SigSpec = b
        .iter()
        .map(|&x| {
            let nx = g.not(x);
            g.mux(x, nx, alu.bi)
        })
        .collect();
    let s: SigSpec = (0..w).map(|i| g.xor(a[i], bb[i])).collect();
    // One more bit for the top carry, when its sum is read as well.
    let n = (w + usize::from(both(w - 1))).div_ceil(4);
    let mut y = Vec::with_capacity(4 * n);
    let mut co = Vec::with_capacity(4 * n);
    for k in 0..n {
        let bit = |v: &SigSpec, j: usize| v.get(4 * k + j).copied().unwrap_or(SigBit::ZERO);
        let o: [SigBit; 4] = std::array::from_fn(|_| SigBit::Net(g.m.new_net()));
        let c: [SigBit; 4] = std::array::from_fn(|_| SigBit::Net(g.m.new_net()));
        let (ci, cyinit) = match co.last() {
            None => (SigBit::ZERO, alu.ci),
            Some(&prev) => (prev, SigBit::ZERO),
        };
        g.add(
            Carry4 {
                ci,
                cyinit,
                di: std::array::from_fn(|j| bit(&a, j)),
                s: std::array::from_fn(|j| bit(&s, j)),
                o,
                co: c,
            }
            .cell(),
        );
        y.extend(o);
        co.extend(c);
    }
    let co: SigSpec = (0..w)
        .map(|i| match i {
            i if !both(i) => co[i],
            i if i + 1 < w => g.xor(y[i + 1], s[i + 1]),
            // The extra bit's sum: S and DI 0, so O is the carry into it.
            i => y[i + 1],
        })
        .collect();
    for (outs, vals) in [(&alu.y, &y), (&alu.x, &s), (&alu.co, &co)] {
        for (o, v) in outs.iter().zip(vals.iter()) {
            if let SigBit::Net(net) = o {
                subst.set(*net, *v);
            }
        }
    }
    n
}
