//! Pin buffers on every top-level port bit (yosys' `iopadmap -bits` with
//! cyclone2.ys' CY2 cells), the registers the manifest moves into their
//! I/O cells, and LUTs onto LEs.
//!
//! A port wire keeps its name but now names the pad side, a fresh net
//! only the buffer touches; the nets the logic uses stay as they were.

use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context, Result};
use chipsmith_netlist::{
    Bit, CellKind, Ctrl, DffControl, Dir, Ff, FfKind, Generic, GenericPort, Module, ParamValue,
    Polarity, SigBit, State, SyncEnable, Tribuf,
};
use chipsmith_synth::rewrite::{inits, Gates, Namer, Subst};

use super::cells::{ibuf, iobuf, obuf, Comb, InReg, IoRegs, OutRegs};
use crate::pin::{IoRegMode, IoRegisters, PowerUp};

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

/// One I/O-cell register as `pads` reads it back: what it samples, its
/// clock (either edge), output and power-up value, and the cell controls
/// it takes, each either polarity: the side's clock enable and the cell's
/// asynchronous reset (to the power-up value).
#[derive(Debug, Clone, Copy)]
struct IoReg {
    d: SigBit,
    clk: Ctrl,
    q: SigBit,
    power_up: PowerUp,
    enable: Option<Ctrl>,
    areset: Option<Ctrl>,
}

fn polarity_param(c: Ctrl) -> ParamValue {
    ParamValue::Str(c.polarity.letter().to_string())
}

fn ioreg_cell(port: &str, role: Role, r: IoReg) -> CellKind {
    let p = |name: &str, dir, b| GenericPort {
        name: name.into(),
        dir: Some(dir),
        sig: vec![b],
    };
    let mut params = BTreeMap::from([
        ("PORT".to_string(), ParamValue::Str(port.into())),
        ("ROLE".to_string(), ParamValue::Str(role.name().into())),
        (
            "POWER_UP".to_string(),
            ParamValue::Str(r.power_up.param().into()),
        ),
        ("CLK_POLARITY".to_string(), polarity_param(r.clk)),
    ]);
    let mut ports = vec![
        p("D", Dir::In, r.d),
        p("CLK", Dir::In, r.clk.sig),
        p("Q", Dir::Out, r.q),
    ];
    for (name, c) in [("ENA", r.enable), ("ARESET", r.areset)] {
        if let Some(c) = c {
            ports.push(p(name, Dir::In, c.sig));
            params.insert(format!("{name}_POLARITY"), polarity_param(c));
        }
    }
    CellKind::Other(Generic {
        ty: IOREG.into(),
        params,
        ports,
    })
}

/// The name a port bit has in the packed design (nextpnr's
/// `get_bit_name`), which is how the manifest names it.
fn bit_name(w: &chipsmith_netlist::Wire, i: usize) -> String {
    crate::pack::bit_name(&w.name, i, w.bits.len(), w.offset, w.upto)
}

/// How a register goes into an I/O cell, as Quartus packs them
/// (cyclone2-re README, "I/O-cell register controls"): its clock on either
/// edge, its enable as the cell's clock enable, an asynchronous clear or
/// preset as the cell's reset. A synchronous reset Quartus makes into
/// logic on D (an output or OE register; an input register's D is the pad,
/// so Quartus leaves that one in the core): `logic`, where the copy samples
/// the original's next state ([`next_state`]), enable included.
#[derive(Debug, Clone, Copy)]
struct Plan {
    clk: Ctrl,
    d: SigBit,
    enable: Option<Ctrl>,
    /// The reset and the value it gives, which is the power-up value.
    areset: Option<(Ctrl, bool)>,
    logic: bool,
}

/// How register `ff` can go into an I/O cell as a `role` register; `Err`
/// says why it can't, and what to write instead.
fn plan(ff: &Ff<Bit>, role: Role) -> Result<Plan, &'static str> {
    let FfKind::Dff { clk, d, control } = &ff.kind else {
        return Err("it isn't an edge-triggered register");
    };
    let p = |enable: Option<Ctrl>, areset, logic| Plan {
        clk: *clk,
        d: *d,
        enable,
        areset,
        logic,
    };
    match control {
        DffControl::Plain { en } => Ok(p(*en, None, false)),
        DffControl::Async { arst, value, en } => Ok(p(*en, Some((*arst, *value)), false)),
        DffControl::Sync { .. } if role == Role::Input => Err(
            "it has a synchronous reset, which would be logic between the pad and the register \
             (Quartus leaves such a register in the core too): sample the pin in a register \
             without one (`always @(posedge clk) pin_q <= PIN;`) and reset the next register",
        ),
        DffControl::Sync { .. } => Ok(p(None, None, true)),
        DffControl::Load { .. } => Err(
            "it has an asynchronous load, which an I/O cell register hasn't: \
             put a plain register after this one",
        ),
        DffControl::SetClr { .. } => Err(
            "it has both an asynchronous set and an asynchronous clear, and an I/O cell \
             register has one asynchronous reset: put a plain register after this one",
        ),
    }
}

/// The register's next state on an edge, from its D, enable, synchronous
/// reset and its own Q: what a plain register sampling it holds, edge for
/// edge, given the same power-up value (and the same asynchronous reset,
/// which the cell does itself). That's how an output or OE register whose
/// enable or synchronous reset can't be the cell's goes into the cell: the
/// LE register stays and feeds the logic its Q.
fn next_state(g: &mut Gates, ff: &Ff<Bit>) -> SigBit {
    let FfKind::Dff { d, control, .. } = &ff.kind else {
        unreachable!("checked by `plan`")
    };
    // `c` active picks `on`.
    let pick = |g: &mut Gates, c: &Ctrl, on: SigBit, off: SigBit| match c.polarity {
        Polarity::Pos => g.mux(off, on, c.sig),
        Polarity::Neg => g.mux(on, off, c.sig),
    };
    let hold = |g: &mut Gates, en: &Ctrl, d: SigBit| pick(g, en, d, ff.q);
    let value = |v: &bool| if *v { SigBit::ONE } else { SigBit::ZERO };
    match control {
        DffControl::Plain { en: None } | DffControl::Async { en: None, .. } => *d,
        DffControl::Plain { en: Some(en) } | DffControl::Async { en: Some(en), .. } => {
            hold(g, en, *d)
        }
        DffControl::Sync { srst, value: v, en } => match en {
            SyncEnable::Always => pick(g, srst, value(v), *d),
            SyncEnable::ResetOverEnable(en) => {
                let held = hold(g, en, *d);
                pick(g, srst, value(v), held)
            }
            SyncEnable::EnableOverReset(en) => {
                let reset = pick(g, srst, value(v), *d);
                hold(g, en, reset)
            }
        },
        _ => unreachable!("checked by `plan`"),
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
/// often share one enable) and goes if nothing does. Either edge, an
/// enable and an asynchronous clear or preset go into the cell as Quartus
/// puts them there ([`plan`]); a synchronous reset on an output or OE
/// register, and an enable the output and OE registers can't share, are
/// logic on the copy's D ([`next_state`]), the original staying to feed it
/// its Q. Every request is the manifest's, so one that can't be met is an
/// error, not a register left in the core. Returns how many registers went
/// into I/O cells.
pub fn registers(m: &mut Module, requests: &BTreeMap<String, IoRegisters>) -> Result<usize> {
    if requests.is_empty() {
        return Ok(0);
    }
    let init = inits(m);
    let init_of = |q: SigBit| q.net().and_then(|n| init.get(&n)).copied();
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
    let ff = |ci: usize| -> &Ff<Bit> {
        let CellKind::FfGate(ff) = &m.cells[ci].kind else {
            unreachable!("indexed registers")
        };
        ff
    };
    // (port bit, role, register cell, its plan, whether it moves rather
    // than copies)
    let mut chosen: Vec<(String, Role, usize, Plan, bool)> = Vec::new();
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
        let check = |ci: usize, role: Role| -> Result<Plan> {
            let pl = plan(ff(ci), role).map_err(|why| {
                anyhow::anyhow!(
                    "{port}: its {} register {} can't go into the I/O cell: {why}",
                    role.name(),
                    m.cells[ci].name
                )
            })?;
            ensure!(
                !(pl.enable.is_none()
                    && pl.areset.is_none()
                    && !pl.logic
                    && matches!(pl.d, SigBit::Const(_))),
                "{port}: its {} register {} has constant data",
                role.name(),
                m.cells[ci].name
            );
            // The cell's reset restores the power-up value, so a register
            // given another one can't go in (Quartus drops the other).
            if let (Some((_, v)), Some(i)) = (pl.areset, init_of(ff(ci).q)) {
                ensure!(
                    (i == State::One) == v,
                    "{port}: its {} register {} powers up {} but its asynchronous reset gives {}, \
                     and an I/O cell's reset restores its power-up value: drop the initial value",
                    role.name(),
                    m.cells[ci].name,
                    u8::from(i == State::One),
                    u8::from(v)
                );
            }
            Ok(pl)
        };
        let driver = |sig: SigBit, role: Role, what: &str| -> Result<(usize, Plan)> {
            let ci = sig.net().and_then(|n| ff_of[n.index()]).with_context(|| {
                format!(
                    "{port}: an {} register is asked for, but no register drives its {what}",
                    role.name()
                )
            })?;
            Ok((ci, check(ci, role)?))
        };
        let first = chosen.len();
        if want.input {
            ensure!(
                dir != Dir::Out,
                "{port}: an input register is asked for on an output"
            );
            let samples = |ci: usize| match &m.cells[ci].kind {
                CellKind::FfGate(ff) => matches!(&ff.kind, FfKind::Dff { d, .. } if *d == b),
                _ => false,
            };
            // Of the registers sampling the pin, one that can go in; else
            // the first says why it can't.
            let candidates: Vec<usize> = (0..m.cells.len())
                .filter(|&ci| samples(ci) && !chosen.iter().any(|c| c.2 == ci))
                .collect();
            let sampler = *candidates.first().with_context(|| {
                format!("{port}: an input register is asked for, but no register samples the pin")
            })?;
            let ci = candidates
                .iter()
                .copied()
                .find(|&ci| check(ci, Role::Input).is_ok())
                .unwrap_or(sampler);
            let pl = check(ci, Role::Input)?;
            chosen.push((port.clone(), Role::Input, ci, pl, true));
        }
        let (data, en) = match (dir, tri) {
            (Dir::In, _) if want.output || want.output_enable => {
                bail!("{port}: output-side registers are asked for on an input")
            }
            (_, Some((a, en))) => (a, Some(en)),
            (_, None) => (b, None),
        };
        if want.output {
            let (ci, pl) = driver(data, Role::Output, "data")?;
            chosen.push((port.clone(), Role::Output, ci, pl, false));
        }
        if want.output_enable {
            let en = en.with_context(|| {
                format!("{port}: an output-enable register is asked for, but the pin is never tri-stated")
            })?;
            let (ci, pl) = driver(en, Role::OutputEnable, "output enable")?;
            chosen.push((port.clone(), Role::OutputEnable, ci, pl, false));
        }
        // What the cell's registers share: the output and OE registers one
        // clock (and edge) and one enable line, all three one reset.
        let pin = &mut chosen[first..];
        let out_side: Vec<usize> = (0..pin.len()).filter(|&i| pin[i].1 != Role::Input).collect();
        if let [o, e] = out_side[..] {
            ensure!(
                pin[o].3.clk == pin[e].3.clk,
                "{port}: its output and output-enable registers have different clocks or \
                 clock edges, and an I/O cell clocks both from one"
            );
            // Two different enables: Quartus leaves the OE register in the
            // core; the enables as logic keep both in the cell.
            if let (Some(a), Some(b)) = (pin[o].3.enable, pin[e].3.enable) {
                if a != b {
                    for i in [o, e] {
                        pin[i].3.logic = true;
                        pin[i].3.enable = None;
                    }
                }
            }
        }
        let mut resets = pin.iter().filter_map(|c| c.3.areset.map(|(r, _)| r));
        if let Some(r) = resets.next() {
            ensure!(
                resets.all(|x| x == r),
                "{port}: its I/O registers have different asynchronous resets, and an I/O cell \
                 has one: give them one reset, or put a plain register after one of them"
            );
        }
    }

    let mut gone = vec![false; m.cells.len()];
    let mut new_cells = Vec::new();
    let mut gates = Gates::new(m, "ioreg_next");
    for (port, role, ci, pl, moves) in &chosen {
        let CellKind::FfGate(ff) = &gates.m.cells[*ci].kind else {
            unreachable!("chosen registers")
        };
        let ff = ff.clone();
        let d = if pl.logic {
            next_state(&mut gates, &ff)
        } else {
            pl.d
        };
        let q = if *moves {
            gone[*ci] = true;
            ff.q
        } else {
            // The copy's output only marks it; `pads` reads its D.
            SigBit::Net(gates.m.new_net())
        };
        let power_up = match (pl.areset, init_of(ff.q)) {
            (Some((_, true)), _) | (None, Some(State::One)) => PowerUp::High,
            _ => PowerUp::Low,
        };
        let r = IoReg {
            d,
            clk: pl.clk,
            q,
            power_up,
            enable: pl.enable,
            areset: pl.areset.map(|(r, _)| r),
        };
        new_cells.push(ioreg_cell(port, *role, r));
    }
    drop(gates);
    // The next-state gates are kept.
    gone.resize(m.cells.len(), false);
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
        let sig = |p: &str| g.port(p).map(|p| p.sig[0]);
        let ctrl = |p: &str| -> Result<Option<Ctrl>> {
            let Some(sig) = sig(p) else { return Ok(None) };
            let polarity = s(&format!("{p}_POLARITY"))
                .chars()
                .next()
                .and_then(Polarity::from_letter)
                .context("I/O register control polarity")?;
            Ok(Some(Ctrl { sig, polarity }))
        };
        let role = match s("ROLE").as_str() {
            "input" => Role::Input,
            "output" => Role::Output,
            _ => Role::OutputEnable,
        };
        let r = IoReg {
            d: sig("D").context("I/O register port")?,
            clk: ctrl("CLK")?.context("I/O register clock")?,
            q: sig("Q").context("I/O register port")?,
            power_up: PowerUp::from_param(&s("POWER_UP")).context("I/O register power-up")?,
            enable: ctrl("ENA")?,
            areset: ctrl("ARESET")?,
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
    // Bus nets an inout names: an output reading one reads the pin.
    let mut on_inout = vec![false; m.net_count()];
    for (_, w, d) in m.ports() {
        if d == Dir::InOut {
            for n in w.bits.iter().filter_map(|b| b.net()) {
                on_inout[n.index()] = true;
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
            let mode = |r: IoReg| IoRegMode {
                power_up: r.power_up,
                enable: r.enable.is_some(),
                areset: r.areset.is_some(),
            };
            // `registers` checked what the cell's registers share.
            let regs = [in_reg, out_reg, oe_reg];
            let io_regs = IoRegs {
                input: in_reg.map(|r| InReg {
                    clk: r.clk,
                    q: r.q,
                    enable: r.enable,
                    power_up: r.power_up,
                    areset: r.areset.is_some(),
                }),
                output: out_reg.or(oe_reg).map(|r| OutRegs {
                    clk: r.clk,
                    enable: out_reg.and_then(|r| r.enable).or(oe_reg.and_then(|r| r.enable)),
                    data: out_reg.map(mode),
                    oe: oe_reg.map(mode),
                }),
                areset: regs.iter().flatten().find_map(|r| r.areset),
            };
            // A registered output or enable is what its register samples.
            let data_of = |d: SigBit| out_reg.map_or(d, |r| r.d);
            let en_of = |e: SigBit| oe_reg.map_or(e, |r| r.d);
            let pad = SigBit::Net(m.new_net());
            let tri = b.net().and_then(|n| tribuf_of[n.index()]);
            let cell = match (dir, tri) {
                (Dir::In, _) => ibuf(pad, b, io_regs),
                // `assign LEDR = GPIO`: the inout's input buffer, which the
                // substitution below puts in place of the bus.
                (Dir::Out, Some(_)) if b.net().is_some_and(|n| on_inout[n.index()]) => {
                    obuf(data_of(b), pad, io_regs)
                }
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
