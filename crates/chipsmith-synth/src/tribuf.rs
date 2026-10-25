//! Tri-state drivers as `$tribuf` cells, one per driven net: `e ? d :
//! 1'bz` multiplexers become `$tribuf` (yosys' `tribuf`), and several
//! tri-states driving one net become one (`tribuf -merge`), enabled when
//! any is and driving what the enabled one drives. A device maps them onto
//! its pin buffers' output enables.

use std::collections::BTreeMap;

use chipsmith_netlist::{CellKind, Module, Net, SigBit, Tribuf};

use crate::rewrite::Gates;

fn all_z(s: &[SigBit]) -> bool {
    !s.is_empty() && s.iter().all(|&b| b == SigBit::Z)
}

pub fn tribuf(m: &mut Module) {
    let mut g = Gates::new(m, "tribuf");
    for i in 0..g.m.cells.len() {
        let CellKind::Mux(x) = &g.m.cells[i].kind else {
            continue;
        };
        let (a, b, s, y) = (x.a.clone(), x.b.clone(), x.s, x.y.clone());
        let (a, en) = if all_z(&a) {
            (b, s)
        } else if all_z(&b) {
            (a, g.not(s))
        } else {
            continue;
        };
        g.m.cells[i].kind = CellKind::Tribuf(Tribuf { a, en, y });
    }
    merge(&mut g);
}

/// One `$tribuf` per bit for the nets several drive, each bit's drivers
/// merged: a bus is driven by whichever driver is enabled.
fn merge(g: &mut Gates) {
    let mut drivers: BTreeMap<Net, Vec<(SigBit, SigBit)>> = BTreeMap::new();
    for c in &g.m.cells {
        if let CellKind::Tribuf(t) = &c.kind {
            for (k, y) in t.y.iter().enumerate() {
                if let SigBit::Net(n) = y {
                    drivers.entry(*n).or_default().push((t.a[k], t.en));
                }
            }
        }
    }
    drivers.retain(|_, d| d.len() > 1);
    if drivers.is_empty() {
        return;
    }
    // Every tri-state with a bit on a shared net goes; its other bits are
    // kept as one-bit tri-states.
    let shared = |t: &Tribuf| t.y.iter().any(|b| b.net().is_some_and(|n| drivers.contains_key(&n)));
    let mut keep = Vec::new();
    for c in std::mem::take(&mut g.m.cells) {
        match &c.kind {
            CellKind::Tribuf(t) if shared(t) => {
                for (k, y) in t.y.iter().enumerate() {
                    if !y.net().is_some_and(|n| drivers.contains_key(&n)) {
                        keep.push(CellKind::Tribuf(Tribuf {
                            a: vec![t.a[k]],
                            en: t.en,
                            y: vec![*y],
                        }));
                    }
                }
            }
            _ => g.m.cells.push(c),
        }
    }
    for kind in keep {
        g.add(kind);
    }
    for (net, ds) in drivers {
        let (mut a, mut en) = ds[0];
        for &(da, de) in &ds[1..] {
            a = g.mux(a, da, de);
            en = g.or(en, de);
        }
        g.add(CellKind::Tribuf(Tribuf {
            a: vec![a],
            en,
            y: vec![SigBit::Net(net)],
        }));
    }
}
