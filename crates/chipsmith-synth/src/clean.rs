//! Removing cells nothing reads (yosys' `opt_clean`), and with `purge`
//! the wires that only name internal nets.

use chipsmith_netlist::{attr, is_set, CellKind, Module, Net, SigBit};

/// Whether a cell may go when nothing reads its outputs. Cells with side
/// effects or with outputs the IR can't see (instances of unknown modules,
/// hard blocks, `keep`) stay; so do the device cells a pass lists in
/// `removable` by type name.
fn removable(kind: &CellKind, removable_types: &[&str]) -> bool {
    match kind {
        CellKind::Other(g) => removable_types.contains(&g.ty.as_str()),
        // A memory with a write port but no read port can't be observed.
        _ => true,
    }
}

/// Removes every cell whose outputs nothing (a kept cell, a module port)
/// reads, transitively. `removable_types` names the device cells that are
/// pure functions of their inputs.
pub fn clean(m: &mut Module, removable_types: &[&str]) {
    let n = m.net_count();
    // Driver cells of each net (several for a tri-state bus).
    let mut drivers: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, c) in m.cells.iter().enumerate() {
        c.kind.for_each_port(|p| {
            if p.dir.is_none_or(|d| d.drives()) {
                for b in p.bits {
                    if let SigBit::Net(net) = b {
                        drivers[net.index()].push(i);
                    }
                }
            }
        });
    }
    let mut live = vec![false; m.cells.len()];
    let mut stack: Vec<usize> = Vec::new();
    let mut seen_net = vec![false; n];
    let mut need = |net: Net, stack: &mut Vec<usize>, live: &mut Vec<bool>| {
        if std::mem::replace(&mut seen_net[net.index()], true) {
            return;
        }
        for &d in &drivers[net.index()] {
            if !std::mem::replace(&mut live[d], true) {
                stack.push(d);
            }
        }
    };
    for (_, w, dir) in m.ports() {
        if dir.drives() {
            for b in &w.bits {
                if let SigBit::Net(net) = b {
                    need(*net, &mut stack, &mut live);
                }
            }
        }
    }
    for w in &m.wires {
        if is_set(&w.attributes, attr::KEEP) {
            for b in &w.bits {
                if let SigBit::Net(net) = b {
                    need(*net, &mut stack, &mut live);
                }
            }
        }
    }
    for (i, c) in m.cells.iter().enumerate() {
        if is_set(&c.attributes, attr::KEEP) || !removable(&c.kind, removable_types) {
            if !std::mem::replace(&mut live[i], true) {
                stack.push(i);
            }
        }
    }
    while let Some(i) = stack.pop() {
        let mut inputs = Vec::new();
        m.cells[i].kind.for_each_port(|p| {
            if p.dir.is_none_or(|d| d.reads()) {
                inputs.extend(p.bits.iter().filter_map(|b| b.net()));
            }
        });
        for net in inputs {
            need(net, &mut stack, &mut live);
        }
    }
    let mut it = live.into_iter();
    m.cells.retain(|_| it.next().expect("one flag per cell"));
}

/// Drops every wire that isn't a port and isn't `keep` (`opt_clean
/// -purge`): internal names go, the netlist stays the same.
pub fn purge_wires(m: &mut Module) {
    m.wires
        .retain(|w| w.port.is_some() || is_set(&w.attributes, attr::KEEP));
}
