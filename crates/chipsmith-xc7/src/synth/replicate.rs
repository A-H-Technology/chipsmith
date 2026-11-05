//! Registers read by more than [`MAX_FANOUT`] cell inputs are copied, each
//! copy taking a run of the readers, as Vivado's synthesis does (sha256's
//! round counter `t_ctr[4]` in six registers). One register's net to 201
//! loads spans the device: its first connection was 3.5 to 5.2 ns of
//! sha256's critical path. A copy shares the original's D, clock, enable
//! and reset, so the copies hold the same value on every cycle.
//!
//! The readers go in runs of the netlist's order, which keeps a word's
//! cells together (the LUT mapper writes a word's bits one after another),
//! so the placer can put each copy beside the readers it drives.

use std::collections::{HashMap, HashSet};

use chipsmith_netlist::{CellKind, Module, Net, SigBit};

/// Above this many reading pins a register is copied, one copy per this
/// many (by whole cells).
pub const MAX_FANOUT: usize = 32;

fn is_register(kind: &CellKind) -> bool {
    matches!(kind, CellKind::Other(g) if matches!(g.ty.as_str(), "FDRE" | "FDSE" | "FDCE" | "FDPE"))
}

/// Returns how many copies were added.
pub fn registers(m: &mut Module, max_fanout: usize) -> usize {
    let mut q_of: HashMap<Net, usize> = HashMap::new();
    for (i, c) in m.cells.iter().enumerate() {
        if !is_register(&c.kind) {
            continue;
        }
        c.kind.for_each_port(|p| {
            if p.name == "Q" {
                if let Some(n) = p.bits.first().and_then(|b| b.net()) {
                    q_of.insert(n, i);
                }
            }
        });
    }
    // Per register, its readers in cell order and their pin counts.
    let mut readers: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
    // A register that clocks something (riscv-bram's CPU clock, a register
    // toggling) stays one: each copy would be a clock of its own.
    let mut clocks: HashSet<usize> = HashSet::new();
    for (i, c) in m.cells.iter().enumerate() {
        let mut pins: HashMap<usize, usize> = HashMap::new();
        let ty = match &c.kind {
            CellKind::Other(g) => g.ty.as_str(),
            _ => "",
        };
        c.kind.for_each_port(|p| {
            if p.dir.is_some_and(|d| !d.reads()) {
                return;
            }
            for n in p.bits.iter().filter_map(|b| b.net()) {
                if let Some(&r) = q_of.get(&n) {
                    *pins.entry(r).or_default() += 1;
                    if super::io::clock_port(ty, p.name) {
                        clocks.insert(r);
                    }
                }
            }
        });
        for (r, k) in pins {
            readers.entry(r).or_default().push((i, k));
        }
    }
    // Module ports read the original, and are no cell's to move.
    let mut regs: Vec<usize> = readers
        .iter()
        .filter(|(r, v)| !clocks.contains(r) && v.iter().map(|&(_, k)| k).sum::<usize>() > max_fanout)
        .map(|(&r, _)| r)
        .collect();
    regs.sort_unstable();
    let mut added = 0;
    for r in regs {
        let mut cells = readers.remove(&r).expect("readers");
        cells.sort_unstable();
        let total: usize = cells.iter().map(|&(_, k)| k).sum();
        let copies = total.div_ceil(max_fanout);
        // Runs of about equal pin counts.
        let per = total.div_ceil(copies);
        let q = q_of
            .iter()
            .find_map(|(&n, &i)| (i == r).then_some(n))
            .expect("a register's Q");
        let mut run = 0;
        let mut in_run = 0;
        let mut copy_q: Option<Net> = None;
        for (cell, k) in cells {
            if in_run >= per {
                run += 1;
                in_run = 0;
                copy_q = None;
            }
            in_run += k;
            if run == 0 || cell == r {
                continue;
            }
            let new_q = match copy_q {
                Some(n) => n,
                None => {
                    let n = m.new_net();
                    let mut kind = m.cells[r].kind.clone();
                    kind.for_each_bit_mut(|b| {
                        if *b == SigBit::Net(q) {
                            *b = SigBit::Net(n);
                        }
                    });
                    let name = format!("{}_rep{run}", m.cells[r].name);
                    let attributes = m.cells[r].attributes.clone();
                    let id = m.add_cell(name, kind);
                    m.cells[id].attributes = attributes;
                    copy_q = Some(n);
                    added += 1;
                    n
                }
            };
            m.cells[cell].kind.for_each_bit_mut(|b| {
                if *b == SigBit::Net(q) {
                    *b = SigBit::Net(new_q);
                }
            });
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_netlist::{Dir, Generic, GenericPort};

    fn generic(ty: &str, ports: Vec<(&str, Dir, SigBit)>) -> CellKind {
        CellKind::Other(Generic {
            ty: ty.into(),
            params: Default::default(),
            ports: ports
                .into_iter()
                .map(|(name, dir, b)| GenericPort {
                    name: name.into(),
                    dir: Some(dir),
                    sig: vec![b].into_iter().collect(),
                })
                .collect(),
        })
    }

    /// 100 LUTs reading one register become four runs, each on its own
    /// register with the original's D; a LUT's two pins on it stay on one.
    #[test]
    fn a_wide_register_is_split_in_runs() {
        let mut m = Module::new("top");
        let [d, c, q] = [0; 3].map(|_| SigBit::Net(m.new_net()));
        m.add_cell(
            "r",
            generic(
                "FDRE",
                vec![("C", Dir::In, c), ("CE", Dir::In, SigBit::ONE), ("R", Dir::In, SigBit::ZERO), ("D", Dir::In, d), ("Q", Dir::Out, q)],
            ),
        );
        for i in 0..100 {
            let o = SigBit::Net(m.new_net());
            let i1 = if i == 40 { q } else { d };
            m.add_cell(format!("l{i}"), generic("LUT2", vec![("I0", Dir::In, q), ("I1", Dir::In, i1), ("O", Dir::Out, o)]));
        }
        assert_eq!(registers(&mut m, MAX_FANOUT), 3);
        let regs: Vec<_> = m.cells.iter().filter(|c| is_register(&c.kind)).collect();
        assert_eq!(regs.len(), 4);
        let mut loads: HashMap<SigBit, usize> = HashMap::new();
        for c in &m.cells {
            if let CellKind::Other(g) = &c.kind {
                if g.ty == "LUT2" {
                    let i0 = g.ports[0].sig[0];
                    assert_eq!(g.ports[1].sig[0] == i0, c.name == "l40", "{}: both pins on one copy", c.name);
                    *loads.entry(i0).or_default() += 1;
                }
            }
        }
        for r in regs {
            let CellKind::Other(g) = &r.kind else { unreachable!() };
            assert_eq!(g.ports[3].sig[0], d, "the copies share D");
            let n = loads[&g.ports[4].sig[0]];
            assert!((20..=32).contains(&n), "{}: {n} readers", r.name);
        }
    }

    /// A register that clocks others (a clock from a toggling register)
    /// stays one register however many it clocks.
    #[test]
    fn a_clock_is_not_copied() {
        let mut m = Module::new("top");
        let [d, c, q] = [0; 3].map(|_| SigBit::Net(m.new_net()));
        let fdre = |c: SigBit, d: SigBit, q: SigBit| {
            generic(
                "FDRE",
                vec![("C", Dir::In, c), ("CE", Dir::In, SigBit::ONE), ("R", Dir::In, SigBit::ZERO), ("D", Dir::In, d), ("Q", Dir::Out, q)],
            )
        };
        m.add_cell("half", fdre(c, d, q));
        for i in 0..100 {
            let o = SigBit::Net(m.new_net());
            m.add_cell(format!("r{i}"), fdre(q, d, o));
        }
        assert_eq!(registers(&mut m, MAX_FANOUT), 0);
    }
}
