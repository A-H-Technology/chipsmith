//! `e ? d : 1'bz` into `$tribuf` cells (yosys' `tribuf`), which a device
//! maps onto its pin buffers' output enables.

use chipsmith_netlist::{CellKind, Module, SigBit, Tribuf};

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
}
