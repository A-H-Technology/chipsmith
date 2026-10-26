//! Pin buffers on every top-level port bit (yosys' `iopadmap -bits` with
//! cyclone2.ys' CY2 cells), the registers the manifest moves into their
//! I/O cells, and LUTs onto LEs.
//!
//! A port wire keeps its name but now names the pad side, a fresh net
//! only the buffer touches; the nets the logic uses stay as they were.

use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context, Result};
use chipsmith_netlist::{
    Bit, CellKind, DffControl, Dir, Ff, FfKind, Generic, GenericPort, Module, ParamValue, Polarity,
    SigBit, State, Tribuf,
};
use chipsmith_synth::rewrite::{inits, Namer, Subst};

use super::cells::{ibuf, iobuf, obuf, Comb, IoRegs, OutRegs};
use crate::pin::{IoRegisters, PowerUp};

/// A register bound for a pin's I/O cell, between choosing it
/// (`registers`) and building the pin's buffer (`pads`): D, CLK, Q, and
/// parameters naming the port bit, the role and the power-up value.
const IOREG: &str = "$chipsmith_ioreg";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    Input,
    Output,
    OutputEnable,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Input => "input",
            Role::Output => "output",
            Role::OutputEnable => "output-enable",
        }
    }
}

/// One I/O-cell register as `pads` reads it back.
#[derive(Debug, Clone, Copy)]
struct IoReg {
    d: SigBit,
    clk: SigBit,
    q: SigBit,
    power_up: PowerUp,
}

fn ioreg_cell(port: &str, role: Role, r: IoReg) -> CellKind {
    let p = |name: &str, dir, b| GenericPort {
        name: name.into(),
        dir: Some(dir),
        sig: vec![b],
    };
    CellKind::Other(Generic {
        ty: IOREG.into(),
        params: BTreeMap::from([
            ("PORT".to_string(), ParamValue::Str(port.into())),
            ("ROLE".to_string(), ParamValue::Str(role.name().into())),
            (
                "POWER_UP".to_string(),
                ParamValue::Str(r.power_up.param().into()),
            ),
        ]),
        ports: vec![
            p("D", Dir::In, r.d),
            p("CLK", Dir::In, r.clk),
            p("Q", Dir::Out, r.q),
        ],
    })
}

/// The name a port bit has in the packed design (nextpnr's
/// `get_bit_name`), which is how the manifest names it.
fn bit_name(w: &chipsmith_netlist::Wire, i: usize) -> String {
    crate::pack::bit_name(&w.name, i, w.bits.len(), w.offset, w.upto)
}

/// A rising-edge register with no enable and no reset: what an I/O cell
/// register is. `Err` says why another one isn't.
fn plain(ff: &Ff<Bit>) -> Result<(SigBit, SigBit), &'static str> {
    match &ff.kind {
        FfKind::Dff {
            clk,
            d,
            control: DffControl::Plain { en: None },
        } if clk.polarity == Polarity::Pos => Ok((clk.sig, *d)),
        FfKind::Dff {
            control: DffControl::Plain { en: None },
            ..
        } => Err("it is clocked on the falling edge"),
        FfKind::Dff {
            control: DffControl::Plain { en: Some(_) },
            ..
        } => Err("it has a clock enable"),
        FfKind::Dff { .. } => Err("it has a reset"),
        _ => Err("it isn't an edge-triggered register"),
    }
}

/// Moves the registers `requests` names (per port bit, as the manifest
/// writes them) into those pins' I/O cells, as Quartus'
/// `FAST_INPUT_REGISTER` and friends do. Runs on gate-level registers,
/// before LE registers make a power-up 1 into inverters.
///
/// An input register is the register sampling the pin, moved. An output
/// or OE register is a copy of the register driving the pin's data or
/// enable: the original stays for whatever else reads it (several pins
/// often share one enable) and goes if nothing does. Either way the
/// register must be one an I/O cell has: rising edge, no enable, no reset.
/// Returns how many registers went into I/O cells.
pub fn registers(m: &mut Module, requests: &BTreeMap<String, IoRegisters>) -> Result<usize> {
    if requests.is_empty() {
        return Ok(0);
    }
    let init = inits(m);
    let power_up = |q: SigBit| match q.net().and_then(|n| init.get(&n)) {
        Some(State::One) => PowerUp::High,
        _ => PowerUp::Low,
    };
    let mut bits: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (wi, w) in m.wires.iter().enumerate() {
        if w.port.is_some() {
            for bi in 0..w.bits.len() {
                bits.insert(bit_name(w, bi), (wi, bi));
            }
        }
    }
    let mut ff_of = vec![None; m.net_count()];
    let mut tribuf_of = vec![None; m.net_count()];
    for (i, c) in m.cells.iter().enumerate() {
        match &c.kind {
            CellKind::FfGate(ff) => {
                if let SigBit::Net(q) = ff.q {
                    ff_of[q.index()] = Some(i);
                }
            }
            CellKind::Tribuf(t) => {
                for (k, y) in t.y.iter().enumerate() {
                    if let SigBit::Net(n) = y {
                        tribuf_of[n.index()] = Some((i, k));
                    }
                }
            }
            _ => {}
        }
    }
    // (port bit, role, register cell, whether it moves rather than copies)
    let mut chosen: Vec<(String, Role, usize, bool)> = Vec::new();
    for (port, want) in requests {
        let &(wi, bi) = bits.get(port).with_context(|| {
            format!("{port}: I/O registers asked for a port bit the design doesn't have")
        })?;
        let w = &m.wires[wi];
        let dir = w.port.expect("a port");
        let b = w.bits[bi];
        let tri = b.net().and_then(|n| tribuf_of[n.index()]).map(|(ci, k)| {
            let CellKind::Tribuf(Tribuf { a, en, .. }) = &m.cells[ci].kind else {
                unreachable!("indexed tri-state buffers");
            };
            (a[k], *en)
        });
        let check = |ci: usize, role: Role| -> Result<()> {
            let CellKind::FfGate(ff) = &m.cells[ci].kind else {
                unreachable!("indexed registers")
            };
            let (_, d) = plain(ff).map_err(|why| {
                anyhow::anyhow!(
                    "{port}: its {} register {} can't go into the I/O cell: {why}",
                    role.name(),
                    m.cells[ci].name
                )
            })?;
            ensure!(
                !matches!(d, SigBit::Const(_)),
                "{port}: its {} register {} has constant data",
                role.name(),
                m.cells[ci].name
            );
            Ok(())
        };
        let driver = |sig: SigBit, role: Role, what: &str| -> Result<usize> {
            let ci = sig.net().and_then(|n| ff_of[n.index()]).with_context(|| {
                format!(
                    "{port}: an {} register is asked for, but no register drives its {what}",
                    role.name()
                )
            })?;
            check(ci, role)?;
            Ok(ci)
        };
        if want.input {
            ensure!(
                dir != Dir::Out,
                "{port}: an input register is asked for on an output"
            );
            let samples = |ci: usize| match &m.cells[ci].kind {
                CellKind::FfGate(ff) => {
                    matches!(&ff.kind, FfKind::Dff { d, .. } if *d == b) && plain(ff).is_ok()
                }
                _ => false,
            };
            let ci = (0..m.cells.len())
                .find(|&ci| samples(ci) && !chosen.iter().any(|c| c.2 == ci))
                .with_context(|| {
                    format!(
                        "{port}: an input register is asked for, but no register \
                         (rising edge, no enable or reset) samples the pin"
                    )
                })?;
            chosen.push((port.clone(), Role::Input, ci, true));
        }
        let (data, en) = match (dir, tri) {
            (Dir::In, _) if want.output || want.output_enable => {
                bail!("{port}: output-side registers are asked for on an input")
            }
            (_, Some((a, en))) => (a, Some(en)),
            (_, None) => (b, None),
        };
        let out_clk = if want.output {
            let ci = driver(data, Role::Output, "data")?;
            chosen.push((port.clone(), Role::Output, ci, false));
            Some(ci)
        } else {
            None
        };
        if want.output_enable {
            let en = en.with_context(|| {
                format!("{port}: an output-enable register is asked for, but the pin is never tri-stated")
            })?;
            let ci = driver(en, Role::OutputEnable, "output enable")?;
            if let Some(o) = out_clk {
                let clk = |ci: usize| match &m.cells[ci].kind {
                    CellKind::FfGate(ff) => plain(ff).ok().map(|(c, _)| c),
                    _ => None,
                };
                ensure!(
                    clk(o) == clk(ci),
                    "{port}: its output and output-enable registers have different clocks, \
                     and an I/O cell clocks both from one"
                );
            }
            chosen.push((port.clone(), Role::OutputEnable, ci, false));
        }
    }

    let mut gone = vec![false; m.cells.len()];
    let mut new_cells = Vec::new();
    for (port, role, ci, moves) in &chosen {
        let CellKind::FfGate(ff) = &m.cells[*ci].kind else {
            unreachable!("chosen registers")
        };
        let (clk, d) = plain(ff).expect("checked");
        let ff_q = ff.q;
        let q = if *moves {
            gone[*ci] = true;
            ff_q
        } else {
            // The copy's output only marks it; `pads` reads its D.
            SigBit::Net(m.new_net())
        };
        let r = IoReg {
            d,
            clk,
            q,
            power_up: power_up(ff_q),
        };
        new_cells.push(ioreg_cell(port, *role, r));
    }
    let mut keep = gone.into_iter();
    m.cells.retain(|_| !keep.next().expect("one flag per cell"));
    let mut namer = Namer::new(m, "ioreg");
    let count = new_cells.len();
    for kind in new_cells {
        m.add_cell(namer.fresh(), kind);
    }
    Ok(count)
}

/// The registers `registers` chose, by port bit and role.
fn chosen_registers(m: &mut Module) -> Result<BTreeMap<(String, Role), IoReg>> {
    let mut out = BTreeMap::new();
    let mut keep = Vec::new();
    for c in std::mem::take(&mut m.cells) {
        let CellKind::Other(g) = &c.kind else {
            keep.push(c);
            continue;
        };
        if g.ty != IOREG {
            keep.push(c);
            continue;
        }
        let s = |k: &str| {
            g.params
                .get(k)
                .and_then(ParamValue::str)
                .unwrap_or("")
                .to_string()
        };
        let sig = |p: &str| g.port(p).map(|p| p.sig[0]).context("I/O register port");
        let role = match s("ROLE").as_str() {
            "input" => Role::Input,
            "output" => Role::Output,
            _ => Role::OutputEnable,
        };
        let r = IoReg {
            d: sig("D")?,
            clk: sig("CLK")?,
            q: sig("Q")?,
            power_up: PowerUp::from_param(&s("POWER_UP")).context("I/O register power-up")?,
        };
        out.insert((s("PORT"), role), r);
    }
    m.cells = keep;
    Ok(out)
}

pub fn pads(m: &mut Module) -> Result<()> {
    let mut regs = chosen_registers(m)?;
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
    // Nets something other than a tri-state drives.
    let mut driven = vec![false; m.net_count()];
    for c in &m.cells {
        if matches!(c.kind, CellKind::Tribuf(_)) {
            continue;
        }
        c.kind.for_each_port(|p| {
            if p.dir.is_none_or(|d| d.drives()) {
                for b in p.bits {
                    if let SigBit::Net(n) = b {
                        driven[n.index()] = true;
                    }
                }
            }
        });
    }
    for (_, w, d) in m.ports() {
        if d == Dir::In {
            for b in &w.bits {
                if let SigBit::Net(n) = b {
                    driven[n.index()] = true;
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
            let name = bit_name(&m.wires[wi], bi);
            let mut take = |role| regs.remove(&(name.clone(), role));
            let (in_reg, out_reg, oe_reg) = (
                take(Role::Input),
                take(Role::Output),
                take(Role::OutputEnable),
            );
            let io_regs = IoRegs {
                input: in_reg.map(|r| (r.clk, r.q, r.power_up)),
                output: out_reg.or(oe_reg).map(|r| OutRegs {
                    clk: r.clk,
                    data: out_reg.map(|r| r.power_up),
                    oe: oe_reg.map(|r| r.power_up),
                }),
            };
            // A registered output or enable is what its register samples.
            let data_of = |d: SigBit| out_reg.map_or(d, |r| r.d);
            let en_of = |e: SigBit| oe_reg.map_or(e, |r| r.d);
            let pad = SigBit::Net(m.new_net());
            let tri = b.net().and_then(|n| tribuf_of[n.index()]);
            let cell = match (dir, tri) {
                (Dir::In, _) => ibuf(pad, b, io_regs),
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
                    iobuf(data_of(data), en_of(en), combout, pad, io_regs)
                }
                // Always released.
                (Dir::Out | Dir::InOut, None)
                    if matches!(b, SigBit::Const(State::Z | State::X)) =>
                {
                    iobuf(SigBit::ZERO, SigBit::ZERO, None, pad, io_regs)
                }
                (Dir::Out, None) => obuf(data_of(b), pad, io_regs),
                // Never driven: an input, the output buffer off.
                (Dir::InOut, None) if b.net().is_some_and(|n| !driven[n.index()]) => {
                    let read = SigBit::Net(m.new_net());
                    if let SigBit::Net(n) = b {
                        subst.set(n, read);
                    }
                    iobuf(SigBit::ZERO, SigBit::ZERO, Some(read), pad, io_regs)
                }
                // Always driven: an output.
                (Dir::InOut, None) => iobuf(data_of(b), SigBit::ONE, None, pad, io_regs),
            };
            m.wires[wi].bits[bi] = pad;
            new_cells.push(cell);
        }
    }
    if let Some((port, _)) = regs.keys().next() {
        bail!("{port}: an I/O register was chosen for a bit that is no pin");
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
