//! Pins and clocks, as yosys' `synth_xilinx` buffers them: every port bit
//! through an IBUF, OBUF, OBUFT or IOBUF (`iopadmap -bits`), every net a
//! register or RAM is clocked by through a BUFG (`clkbufmap`).

use anyhow::Result;
use chipsmith_netlist::{CellKind, Dir, Module, Net, SigBit, State, Tribuf};
use chipsmith_synth::rewrite::{Gates, Subst};

use super::cells;

/// Puts a buffer between each port bit and the logic: the port's wire
/// then names the pad side.
pub fn pads(m: &mut Module) -> Result<()> {
    let mut tribuf_of = vec![None; m.net_count()];
    for (i, c) in m.cells.iter().enumerate() {
        if let CellKind::Tribuf(t) = &c.kind {
            for (k, y) in t.y.iter().enumerate() {
                if let SigBit::Net(n) = y {
                    tribuf_of[n.index()] = Some((i, k));
                }
            }
        }
    }
    let mut driven = vec![false; m.net_count()];
    for c in &m.cells {
        if matches!(c.kind, CellKind::Tribuf(_)) {
            continue;
        }
        c.kind.for_each_port(|p| {
            if p.dir.is_none_or(|d| d.drives()) {
                for n in p.bits.iter().filter_map(|b| b.net()) {
                    driven[n.index()] = true;
                }
            }
        });
    }
    for (_, w, d) in m.ports() {
        if d == Dir::In {
            for n in w.bits.iter().filter_map(|b| b.net()) {
                driven[n.index()] = true;
            }
        }
    }
    let mut on_inout = vec![false; m.net_count()];
    for (_, w, d) in m.ports() {
        if d == Dir::InOut {
            for n in w.bits.iter().filter_map(|b| b.net()) {
                on_inout[n.index()] = true;
            }
        }
    }
    let mut used_tribufs = vec![false; m.cells.len()];
    let mut subst = Subst::new(m);
    let mut new_cells: Vec<CellKind> = Vec::new();
    let mut g = Gates::new(m, "io");
    for wi in 0..g.m.wires.len() {
        let Some(dir) = g.m.wires[wi].port else {
            continue;
        };
        for bi in 0..g.m.wires[wi].bits.len() {
            let b = g.m.wires[wi].bits[bi];
            let pad = SigBit::Net(g.m.new_net());
            let tri = b.net().and_then(|n| tribuf_of[n.index()]);
            let cell = match (dir, tri) {
                (Dir::In, _) => cells::ibuf(pad, b),
                // `assign out = bus`: what the inout's buffer reads.
                (Dir::Out, Some(_)) if b.net().is_some_and(|n| on_inout[n.index()]) => {
                    cells::obuf(b, pad)
                }
                (Dir::Out | Dir::InOut, Some((ci, k))) => {
                    let CellKind::Tribuf(Tribuf { a, en, .. }) = &g.m.cells[ci].kind else {
                        unreachable!("indexed tri-state buffers");
                    };
                    let (data, en) = (a[k], *en);
                    used_tribufs[ci] = true;
                    let t = g.not(en);
                    if dir == Dir::InOut {
                        let read = SigBit::Net(g.m.new_net());
                        if let SigBit::Net(n) = b {
                            subst.set(n, read);
                        }
                        cells::iobuf(data, t, read, pad)
                    } else {
                        cells::obuft(data, t, pad)
                    }
                }
                (Dir::Out, None) if matches!(b, SigBit::Const(State::Z | State::X)) => {
                    cells::obuft(SigBit::ZERO, SigBit::ONE, pad)
                }
                (Dir::Out, None) => cells::obuf(b, pad),
                (Dir::InOut, None) if b.net().is_some_and(|n| !driven[n.index()]) => {
                    let read = SigBit::Net(g.m.new_net());
                    if let SigBit::Net(n) = b {
                        subst.set(n, read);
                    }
                    cells::iobuf(SigBit::ZERO, SigBit::ONE, read, pad)
                }
                (Dir::InOut, None) if matches!(b, SigBit::Const(State::Z | State::X)) => {
                    cells::obuft(SigBit::ZERO, SigBit::ONE, pad)
                }
                (Dir::InOut, None) => cells::obuf(b, pad),
            };
            g.m.wires[wi].bits[bi] = pad;
            new_cells.push(cell);
        }
    }
    for cell in new_cells {
        g.add(cell);
    }
    let mut keep = used_tribufs.into_iter().chain(std::iter::repeat(false));
    m.cells.retain(|_| !keep.next().expect("endless"));
    subst.apply(m);
    Ok(())
}

/// The clock pins of the cells synthesis makes. A block RAM's or a DSP's
/// clock left off here stays on the pin's net, which the router then
/// takes through the general interconnect beside the BUFG.
fn clock_port(ty: &str, port: &str) -> bool {
    matches!(
        (ty, port),
        ("FDRE" | "FDSE" | "FDCE" | "FDPE", "C")
            | ("RAM32M", "WCLK")
            | ("RAMB18E1" | "RAMB36E1", "CLKARDCLK" | "CLKBWRCLK")
            | ("DSP48E1", "CLK")
    )
}

/// Puts a BUFG in front of the clock pins of every net that clocks
/// something and isn't a BUFG's output already; returns how many.
pub fn clock_buffers(m: &mut Module) -> usize {
    let mut buffered = vec![false; m.net_count()];
    for c in &m.cells {
        if let CellKind::Other(g) = &c.kind {
            if g.ty == cells::BUFG {
                if let Some(n) = g.port("O").and_then(|p| p.sig[0].net()) {
                    buffered[n.index()] = true;
                }
            }
        }
    }
    let mut clocks: Vec<Net> = Vec::new();
    for c in &m.cells {
        if let CellKind::Other(g) = &c.kind {
            for p in &g.ports {
                if clock_port(&g.ty, &p.name) {
                    if let Some(n) = p.sig[0].net() {
                        if !buffered[n.index()] && !clocks.contains(&n) {
                            clocks.push(n);
                        }
                    }
                }
            }
        }
    }
    let mut out = std::collections::HashMap::new();
    for &n in &clocks {
        out.insert(n, SigBit::Net(m.new_net()));
    }
    for c in &mut m.cells {
        if let CellKind::Other(g) = &mut c.kind {
            let ty = g.ty.clone();
            for p in &mut g.ports {
                if clock_port(&ty, &p.name) {
                    if let Some(o) = p.sig[0].net().and_then(|n| out.get(&n)) {
                        p.sig[0] = *o;
                    }
                }
            }
        }
    }
    let mut g = Gates::new(m, "clk");
    for &n in &clocks {
        g.add(cells::bufg(SigBit::Net(n), out[&n]));
    }
    clocks.len()
}
