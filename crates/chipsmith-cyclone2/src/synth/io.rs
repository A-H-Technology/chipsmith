//! Pin buffers on every top-level port bit (yosys' `iopadmap -bits` with
//! cyclone2.ys' CY2 cells), and LUTs onto LEs.
//!
//! A port wire keeps its name but now names the pad side, a fresh net
//! only the buffer touches; the nets the logic uses stay as they were.

use anyhow::{bail, Result};
use chipsmith_netlist::{CellKind, Dir, Module, SigBit, Tribuf};
use chipsmith_synth::rewrite::{Namer, Subst};

use super::cells::{ibuf, iobuf, obuf, Comb};

pub fn pads(m: &mut Module) -> Result<()> {
    let mut namer = Namer::new(m, "io");
    // Tri-state drivers of each net, which become IOBUFs on output and
    // inout ports.
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
    let mut used_tribufs = vec![false; m.cells.len()];
    let mut new_cells = Vec::new();
    let mut subst = Subst::new(m);
    for wi in 0..m.wires.len() {
        let Some(dir) = m.wires[wi].port else {
            continue;
        };
        for bi in 0..m.wires[wi].bits.len() {
            let b = m.wires[wi].bits[bi];
            let pad = SigBit::Net(m.new_net());
            let tri = b.net().and_then(|n| tribuf_of[n.index()]);
            let cell = match (dir, tri) {
                (Dir::In, _) => ibuf(pad, b),
                (Dir::Out | Dir::InOut, Some((ci, k))) => {
                    let CellKind::Tribuf(Tribuf { a, en, .. }) = &m.cells[ci].kind else {
                        unreachable!("indexed tri-state buffers");
                    };
                    let (data, en) = (a[k], *en);
                    used_tribufs[ci] = true;
                    // On an inout the pin is also read: what the logic
                    // read from the bus it now reads from the buffer.
                    let combout = (dir == Dir::InOut).then(|| {
                        let read = SigBit::Net(m.new_net());
                        if let SigBit::Net(n) = b {
                            subst.set(n, read);
                        }
                        read
                    });
                    iobuf(data, en, combout, pad)
                }
                (Dir::Out, None) => obuf(b, pad),
                (Dir::InOut, None) => bail!(
                    "inout port {} has no tri-state driver (only `assign p = oe ? d : 1'bz` is supported)",
                    m.wires[wi].name
                ),
            };
            m.wires[wi].bits[bi] = pad;
            new_cells.push(cell);
        }
    }
    let mut keep = used_tribufs.into_iter();
    m.cells.retain(|_| !keep.next().expect("one flag per cell"));
    for kind in new_cells {
        m.add_cell(namer.fresh(), kind);
    }
    subst.apply(m);
    Ok(())
}

/// `$lut` cells onto LEs (cyclone2-re's `cells_map.v`): a LUT narrower
/// than four repeats its table over the unused inputs, which are tied to 1
/// as an unconnected input reads.
pub fn luts(m: &mut Module) {
    for c in &mut m.cells {
        let CellKind::Lut(l) = &c.kind else { continue };
        let w = l.a.len();
        let mask = (0..16).fold(0u16, |mask, i| {
            let bit = l.table.0[i % (1 << w)] == chipsmith_netlist::State::One;
            mask | (u16::from(bit) << i)
        });
        let mut data = [SigBit::ONE; 4];
        data[..w].copy_from_slice(&l.a);
        c.kind = Comb {
            mask,
            data,
            cin: SigBit::ONE,
            combout: Some(l.y),
            cout: None,
        }
        .cell();
    }
}
