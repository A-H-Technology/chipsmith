//! chipsmith's packer: yosys's netlist (synth/cyclone2.ys cells) into the
//! packed design the placer reads, in nextpnr's JSON
//! (`nextpnr-himbaechel --pack-only --write` writes the same).
//!
//! Ported from cyclone2-re's nextpnr uarch (`nextpnr/cyclone2/cyclone2.cc`,
//! `pack()`). Every rule exists so the bitstream matches what Quartus
//! assembles from the same placement: constants and one-input LUTs are
//! folded the way quartus_map does it, carry chains are rewritten in
//! Quartus' polarity, registers are paired with LUTs or put on the
//! sync-load path the way the fitter packs them. Pins and clock controls
//! get no location here; the placer decides those (`place::fixed_cells`).

mod diff;
mod mask;
mod netlist;
mod yosys;

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

pub use diff::diff_files;
use mask::{fix, invert_input, normalise_carry, substitute, BUFFERS};
use netlist::{CellId, Kind, NetId, Netlist, OutData, Port, LUT_INPUTS};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub luts: usize,
    pub ffs: usize,
    /// Registers on the sync-load path (SDATA) rather than beside a LUT.
    pub sdata: usize,
    pub chains: usize,
    pub clocks: usize,
    /// Registers that never leave their power-up 0, removed.
    pub stuck_ffs: usize,
    /// LUTs left with no inputs, removed.
    pub const_luts: usize,
    /// One-input LUT loads folded, and the LUTs that left.
    pub folded: usize,
    pub removed_luts: usize,
}

/// Pack `netlist` (yosys JSON), write `packed`.
pub fn pack_file(netlist: &Path, packed: &Path) -> Result<Stats> {
    let text = std::fs::read_to_string(netlist)
        .with_context(|| format!("reading {}", netlist.display()))?;
    let json: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", netlist.display()))?;
    let mut nl = yosys::import(&json)?;
    let stats = pack(&mut nl)?;
    std::fs::write(packed, serde_json::to_string(&to_json(&nl)?)?)
        .with_context(|| format!("writing {}", packed.display()))?;
    Ok(stats)
}

fn pack(nl: &mut Netlist) -> Result<Stats> {
    let mut stats = Stats::default();
    pack_io(nl)?;
    pack_constants(nl, &mut stats)?;
    pack_lut1(nl, &mut stats);
    pack_lut_ff(nl, &mut stats)?;
    pack_carry(nl, &mut stats)?;
    pack_clocks(nl, &mut stats)?;
    for c in nl.cells() {
        match nl.cell(c).kind {
            Kind::Lut { .. } => stats.luts += 1,
            Kind::Ff(_) => stats.ffs += 1,
            _ => {}
        }
    }
    Ok(stats)
}

/// An insertion-ordered set iterated newest first: nextpnr's `pool`.
#[derive(Default)]
struct Pool {
    order: Vec<usize>,
    set: HashSet<usize>,
}

impl Pool {
    fn insert(&mut self, k: usize) {
        if self.set.insert(k) {
            self.order.push(k);
        }
    }
    fn contains(&self, k: usize) -> bool {
        self.set.contains(&k)
    }
    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.order.iter().rev().copied()
    }
}

fn is_pad(nl: &Netlist, (c, p): (CellId, Port)) -> bool {
    p == Port::Pad && matches!(nl.cell(c).kind, Kind::Ibuf { .. } | Kind::Obuf { .. })
}

/// Name each pin buffer after the top-level port bit it serves, drop the
/// port cells and the pad side (it has no wire; the bel is the pad).
fn pack_io(nl: &mut Netlist) -> Result<()> {
    for c in nl.cells() {
        let name = nl.cell(c).name.clone();
        let buffers: Vec<CellId> = match nl.cell(c).kind {
            Kind::TopIn => nl
                .cell(c)
                .net(Port::O)
                .map(|o| nl.users(o).into_iter().map(|(u, _)| u).collect())
                .unwrap_or_default(),
            Kind::TopOut => nl
                .cell(c)
                .net(Port::I)
                .and_then(|i| nl.net(i).driver)
                .map(|(d, _)| vec![d])
                .unwrap_or_default(),
            _ => continue,
        };
        for b in buffers {
            match &mut nl.cell_mut(b).kind {
                Kind::Ibuf { port } | Kind::Obuf { port, .. } => *port = Some(name.clone()),
                _ => {}
            }
        }
    }

    let mut remove = Vec::new();
    for c in nl.cells() {
        if !matches!(nl.cell(c).kind, Kind::TopIn | Kind::TopOut) {
            continue;
        }
        let name = &nl.cell(c).name;
        if let Some(d) = nl.cell(c).net(Port::I).and_then(|i| nl.net(i).driver) {
            if !is_pad(nl, d) {
                bail!(
                    "top-level port {name} is driven by {}.{}, not a pin buffer",
                    nl.cell(d.0).name,
                    d.1.name()
                );
            }
        }
        if let Some(o) = nl.cell(c).net(Port::O) {
            if let Some(u) = nl.users(o).into_iter().find(|&u| !is_pad(nl, u)) {
                bail!(
                    "top-level port {name} drives {}.{}, not a pin buffer",
                    nl.cell(u.0).name,
                    u.1.name()
                );
            }
        }
        nl.disconnect(c, Port::I);
        nl.disconnect(c, Port::O);
        remove.push(c);
    }
    for c in remove {
        nl.erase_cell(c);
    }

    for c in nl.cells() {
        let cell = nl.cell(c);
        let port = match &cell.kind {
            Kind::Ibuf { port } | Kind::Obuf { port, .. } => port,
            _ => continue,
        };
        if port.is_none() {
            bail!(
                "pin buffer {} is not connected to a top-level port",
                cell.name
            );
        }
        nl.disconnect(c, Port::Pad);
        nl.remove_port(c, Port::Pad);
    }
    Ok(())
}

/// Constants have no driver in the fabric: LUT inputs are folded into the
/// mask, output pins get a constant output setting. Registers that can
/// never leave their power-up 0 (data, enable or clock stuck, clear stuck
/// on) are constants too, as quartus_map treats them ("stuck at GND"), and
/// so are LUTs left with no inputs; both propagate.
fn pack_constants(nl: &mut Netlist, stats: &mut Stats) -> Result<()> {
    let const_driver = |nl: &Netlist, n: NetId| match nl.net(n).driver {
        Some((d, _)) => match nl.cell(d).kind {
            Kind::Const(v) => Some((d, v)),
            _ => None,
        },
        None => None,
    };
    let mut work: Vec<(NetId, bool)> = nl
        .nets()
        .into_iter()
        .filter_map(|n| Some((n, const_driver(nl, n)?.1)))
        .collect();
    let (mut dead_cells, mut dead_nets) = (Pool::default(), Pool::default());
    let mut const_one_ff = Vec::new();
    while let Some((n, value)) = work.pop() {
        if dead_nets.contains(n) {
            continue;
        }
        for (c, p) in nl.users(n) {
            if dead_cells.contains(c) {
                continue;
            }
            let make_const = |nl: &Netlist, work: &mut Vec<(NetId, bool)>, port, v| {
                if let Some(net) = nl.cell(c).net(port) {
                    work.push((net, v));
                }
            };
            match nl.cell(c).kind {
                Kind::Lut { .. } if p.lut_index().is_some() || p == Port::Cin => {
                    // On DATAC, DATAD and CIN a 1 is only disconnected: it
                    // is what an unconnected input reads, and yosys ties
                    // every unused input to 1. Folding it would rewrite the
                    // carry half of the mask, which COUT reads with
                    // DATAD = 0, or for DATAC the LUT input C it shares with
                    // CIN. Anything else is folded, which is also what
                    // Quartus does with the mask. CIN only acts as LUT input
                    // C (and the carry LUT's C); folding it makes both
                    // independent of the hardware cin.
                    if !value || matches!(p, Port::DataA | Port::DataB) {
                        let bit = p.lut_index().unwrap_or(2);
                        let m = fix(nl.cell(c).mask(), bit, value);
                        nl.cell_mut(c).set_mask(m);
                    }
                    nl.disconnect(c, p);
                    let cell = nl.cell(c);
                    let any = cell.net(Port::Cin).is_some()
                        || LUT_INPUTS.iter().any(|&q| cell.net(q).is_some());
                    if !any {
                        // The LUT is a constant: what it reads with every
                        // remaining (unconnected) input at 1, and DATAD = 0
                        // for COUT.
                        let m = cell.mask();
                        make_const(nl, &mut work, Port::Combout, (m >> 15) & 1 == 1);
                        make_const(nl, &mut work, Port::Cout, (m >> 7) & 1 == 1);
                        dead_cells.insert(c);
                        stats.const_luts += 1;
                    }
                }
                Kind::Obuf { .. } if p == Port::Dataout => {
                    if let Kind::Obuf { data, .. } = &mut nl.cell_mut(c).kind {
                        *data = OutData::Const(value);
                    }
                    nl.disconnect(c, p);
                }
                Kind::Ff(_)
                    if (p == Port::Ena && value)
                        || (matches!(p, Port::Aclr | Port::Sclr) && !value) =>
                {
                    nl.disconnect(c, p); // the idle level
                }
                Kind::Ff(_) if p == Port::Datain && value => {
                    nl.disconnect(c, p);
                    const_one_ff.push(c);
                }
                Kind::Ff(_) => {
                    // Data 0, never enabled, never clocked or always
                    // cleared: it stays at its power-up 0.
                    make_const(nl, &mut work, Port::Regout, false);
                    dead_cells.insert(c);
                    stats.stuck_ffs += 1;
                }
                _ => bail!(
                    "constant {} on {}.{} is not supported",
                    value as u8,
                    nl.cell(c).name,
                    p.name()
                ),
            }
        }
        dead_nets.insert(n);
        if let Some((d, _)) = const_driver(nl, n) {
            dead_cells.insert(d);
        }
    }
    for c in dead_cells.iter() {
        nl.erase_cell(c);
    }
    for n in dead_nets.iter() {
        if nl.net_exists(n) && nl.user_count(n) == 0 {
            nl.erase_net(n);
        }
    }
    // A register loading a constant 1 gets a LUT that outputs it.
    for ff in const_one_ff {
        if dead_cells.contains(ff) {
            continue;
        }
        let name = nl.cell(ff).name.clone();
        let lut = nl.create_cell(format!("{name}$const"), Kind::Lut { mask: 0xFFFF });
        for p in LUT_INPUTS
            .into_iter()
            .chain([Port::Cin, Port::Combout, Port::Cout])
        {
            nl.add_port(lut, p);
        }
        let n = nl.create_net(format!("{name}$const_net"));
        nl.connect(lut, Port::Combout, n);
        nl.connect(ff, Port::Datain, n);
    }
    Ok(())
}

fn is_plain_lut(nl: &Netlist, c: CellId) -> bool {
    let cell = nl.cell(c);
    cell.kind.is_lut() && cell.net(Port::Cin).is_none() && !nl.has_users(cell.net(Port::Cout))
}

/// The LUT driving `n` from its COMBOUT, if it uses no carry.
fn plain_lut_driver(nl: &Netlist, n: NetId) -> Option<CellId> {
    match nl.net(n).driver {
        Some((d, Port::Combout)) if is_plain_lut(nl, d) => Some(d),
        _ => None,
    }
}

fn feeds_register_data(nl: &Netlist, lut: CellId) -> bool {
    nl.cell(lut).net(Port::Combout).is_some_and(|o| {
        nl.users(o)
            .into_iter()
            .any(|(u, p)| nl.cell(u).kind.is_ff() && p == Port::Datain)
    })
}

/// An inverter in front of a register can't fold into it, and Quartus
/// would re-create it somewhere of its own choosing. Instead invert the LUT
/// driving the inverter's input and undo that on every other load, when
/// they all can take it; the inverter is then a plain buffer.
fn push_inversion_back(nl: &mut Netlist, inverter: CellId, input: NetId) -> bool {
    let Some(d) = plain_lut_driver(nl, input) else {
        return false;
    };
    let loads: Vec<(CellId, Port)> = nl
        .users(input)
        .into_iter()
        .filter(|&(u, _)| u != inverter)
        .collect();
    let takes_inversion = |&(u, p): &(CellId, Port)| match nl.cell(u).kind {
        Kind::Lut { .. } => p.lut_index().is_some(),
        Kind::Obuf { .. } => p == Port::Dataout,
        Kind::Ff(_) => matches!(p, Port::Ena | Port::Sclr),
        _ => false,
    };
    if !loads.iter().all(takes_inversion) {
        return false;
    }
    let m = !nl.cell(d).mask();
    nl.cell_mut(d).set_mask(m);
    for (u, p) in loads {
        let cell = nl.cell_mut(u);
        match &mut cell.kind {
            Kind::Lut { mask } => *mask = invert_input(*mask, p.lut_index().unwrap()),
            Kind::Obuf {
                data: OutData::Signal { invert },
                ..
            } => *invert = !*invert,
            Kind::Ff(cfg) if p == Port::Ena => cfg.ena_inv = !cfg.ena_inv,
            Kind::Ff(cfg) => cfg.sclr_inv = !cfg.sclr_inv,
            _ => unreachable!("checked above"),
        }
    }
    true
}

/// quartus_map drops every one-input LUT (buffer or inverter) of an atom
/// netlist as a "redundant logic cell" and folds the inversion into
/// whatever it feeds: other LUTs' masks, an output pin's inverter
/// (IO_DATA_INVERT), a LAB's clock-enable / sync-clear inversion. Doing the
/// same keeps the netlist one Quartus would assemble unchanged. What feeds
/// a register's data input stays: it is the register's own LUT (a feeder,
/// which Quartus re-creates itself).
fn pack_lut1(nl: &mut Netlist, stats: &mut Stats) {
    let mut dead = Vec::new();
    for c in nl.cells() {
        let Kind::Lut { mask } = nl.cell(c).kind else {
            continue;
        };
        let cell = nl.cell(c);
        let connected: Vec<usize> = (0..4)
            .filter(|&i| cell.net(LUT_INPUTS[i]).is_some())
            .collect();
        let &[only] = connected.as_slice() else {
            continue;
        };
        if !is_plain_lut(nl, c) {
            continue;
        }
        if mask != BUFFERS[only] && mask != !BUFFERS[only] {
            continue;
        }
        let mut inv = mask != BUFFERS[only];
        let input = cell.net(LUT_INPUTS[only]).unwrap();
        if inv && feeds_register_data(nl, c) {
            if push_inversion_back(nl, c, input) {
                inv = false;
            } else if let Some(d) = plain_lut_driver(nl, input) {
                // Otherwise make the inverter an inverted copy of its
                // driver: a real LUT Quartus keeps where it is.
                nl.disconnect(c, LUT_INPUTS[only]);
                for p in LUT_INPUTS {
                    if let Some(n) = nl.cell(d).net(p) {
                        nl.add_port(c, p);
                        nl.connect(c, p, n);
                    }
                }
                let m = !nl.cell(d).mask();
                nl.cell_mut(c).set_mask(m);
                continue;
            }
        }
        let Some(o) = nl.cell(c).net(Port::Combout) else {
            continue;
        };
        for (u, p) in nl.users(o) {
            let load = nl.cell(u);
            match (&load.kind, p.lut_index()) {
                (Kind::Lut { mask: m }, Some(i)) => {
                    let m = *m;
                    let j = (0..4)
                        .rev()
                        .find(|&k| k != i && load.net(LUT_INPUTS[k]) == Some(input));
                    nl.disconnect(u, p);
                    match j {
                        Some(j) => nl.cell_mut(u).set_mask(substitute(m, i, Some(j), inv)),
                        None => {
                            if inv {
                                nl.cell_mut(u).set_mask(invert_input(m, i));
                            }
                            nl.connect(u, p, input);
                        }
                    }
                }
                (Kind::Obuf { .. }, _) if p == Port::Dataout => {
                    nl.reconnect(u, p, input);
                    if let Kind::Obuf { data, .. } = &mut nl.cell_mut(u).kind {
                        *data = OutData::Signal { invert: inv };
                    }
                }
                // A plain buffer in front of a register: the register takes
                // the source on its sync load path instead.
                (Kind::Ff(_), _) if p == Port::Datain && !inv => nl.reconnect(u, p, input),
                (Kind::Ff(_), _) if matches!(p, Port::Ena | Port::Sclr) => {
                    nl.reconnect(u, p, input);
                    if let Kind::Ff(cfg) = &mut nl.cell_mut(u).kind {
                        if p == Port::Ena {
                            cfg.ena_inv = inv;
                        } else {
                            cfg.sclr_inv = inv;
                        }
                    }
                }
                _ => continue,
            }
            stats.folded += 1;
        }
        if nl.user_count(o) == 0 {
            nl.disconnect(c, LUT_INPUTS[only]);
            nl.disconnect(c, Port::Combout);
            dead.push(c);
        }
    }
    stats.removed_luts = dead.len();
    for c in dead {
        nl.erase_cell(c);
    }
}

/// A register driven by a LUT sits beside it (DATAIN is that LUT's
/// output). Any other register takes its data through the synchronous load
/// path instead: SDATA (the LE's input C) with the LAB's sload tied high,
/// the fitter's own register packing. That leaves the LUT free and needs
/// no feeder LUT (which quartus_map would delete).
fn pack_lut_ff(nl: &mut Netlist, stats: &mut Stats) -> Result<()> {
    let ffs: Vec<CellId> = nl
        .cells()
        .into_iter()
        .filter(|&c| nl.cell(c).kind.is_ff())
        .collect();
    let mut paired = HashSet::new();
    for ff in ffs {
        let d = nl
            .cell(ff)
            .net(Port::Datain)
            .with_context(|| format!("register {} has no data input", nl.cell(ff).name))?;
        if let Some((l, Port::Combout)) = nl.net(d).driver {
            if nl.cell(l).kind.is_lut() && paired.insert(l) {
                continue;
            }
        }
        nl.disconnect(ff, Port::Datain);
        nl.add_port(ff, Port::Sdata);
        nl.connect(ff, Port::Sdata, d);
        stats.sdata += 1;
    }
    Ok(())
}

/// Carry chains: LE k -> k+1 in a LAB, LE 15 -> LE 0 of the LAB below; the
/// placer keeps them together. Here: check them and give them Quartus'
/// carry polarity.
fn pack_carry(nl: &mut Netlist, stats: &mut Stats) -> Result<()> {
    for c in nl.cells() {
        let cell = nl.cell(c);
        if !cell.kind.is_lut() {
            continue;
        }
        if cell
            .net(Port::Cin)
            .is_some_and(|n| nl.net(n).driver.is_some())
        {
            continue; // not a chain head
        }
        if !nl.has_users(cell.net(Port::Cout)) {
            continue;
        }
        let mut chain = vec![c];
        loop {
            let last = *chain.last().unwrap();
            let co = nl.cell(last).net(Port::Cout);
            if !nl.has_users(co) {
                break;
            }
            match nl.users(co.unwrap()).as_slice() {
                [(next, Port::Cin)] if !chain.contains(next) => chain.push(*next),
                _ => bail!(
                    "carry out of {} must feed exactly one carry in",
                    nl.cell(last).name
                ),
            }
        }
        for &x in &chain {
            let cell = nl.cell(x);
            if cell.net(Port::DataC).is_some() && cell.net(Port::Cin).is_some() {
                bail!("{} uses both DATAC and CIN", cell.name);
            }
        }
        for (i, &x) in chain.iter().enumerate() {
            let cell = nl.cell(x);
            let last_unused = i + 1 == chain.len() && !nl.has_users(cell.net(Port::Cout));
            let sum_used = nl.has_users(cell.net(Port::Combout));
            let m = normalise_carry(cell.mask(), i, last_unused, sum_used);
            nl.cell_mut(x).set_mask(m);
        }
        stats.chains += 1;
    }
    Ok(())
}

/// A global clock control on every net that clocks a register. Which one
/// is the placer's choice.
fn pack_clocks(nl: &mut Netlist, stats: &mut Stats) -> Result<()> {
    let clocks: Vec<NetId> = nl
        .nets()
        .into_iter()
        .filter(|&n| {
            nl.users(n)
                .iter()
                .any(|&(u, p)| nl.cell(u).kind.is_ff() && p == Port::Clk)
        })
        .collect();
    for ni in clocks {
        let name = nl.net(ni).name.clone();
        match nl.net(ni).driver {
            Some((d, _)) if matches!(nl.cell(d).kind, Kind::Ibuf { .. }) => {}
            _ => bail!("clock {name} must come straight from a pin"),
        }
        let cc = nl.create_cell(format!("{name}$clkctrl"), Kind::ClkCtrl);
        nl.add_port(cc, Port::Inclk);
        nl.add_port(cc, Port::Outclk);
        let g = nl.create_net(format!("{name}$global"));
        for (u, p) in nl.users(ni) {
            if nl.cell(u).kind.is_ff() && p == Port::Clk {
                nl.reconnect(u, p, g);
            }
        }
        nl.connect(cc, Port::Inclk, ni);
        nl.connect(cc, Port::Outclk, g);
        stats.clocks += 1;
    }
    Ok(())
}

fn hide_name(name: &str) -> u8 {
    name.starts_with('$') as u8
}

fn bit(v: bool) -> Value {
    Value::String(if v { "1" } else { "0" }.into())
}

/// The packed netlist as nextpnr writes it.
fn to_json(nl: &Netlist) -> Result<Value> {
    let id = |n: NetId| nl.ids.get(&nl.net(n).name);
    let mut cells = Map::new();
    for c in nl.cells() {
        let cell = nl.cell(c);
        let mut params: Map<String, Value> = cell.params.clone().into_iter().collect();
        let mut attrs: Map<String, Value> = cell.attrs.clone().into_iter().collect();
        let ty = match &cell.kind {
            Kind::Lut { mask } => {
                params.insert("LUT_MASK".into(), Value::String(format!("{mask:016b}")));
                "LCELL_COMB"
            }
            Kind::Ff(cfg) => {
                if cfg.ena_inv {
                    params.insert("ENA_INV".into(), bit(true));
                }
                if cfg.sclr_inv {
                    params.insert("SCLR_INV".into(), bit(true));
                }
                if cell.has_port(Port::Sdata) {
                    params.insert("SLOAD".into(), bit(true));
                }
                "LCELL_FF"
            }
            Kind::Ibuf { port } | Kind::Obuf { port, .. } => {
                let port = port.as_ref().context("pin buffer without a port")?;
                attrs.insert("PORT".into(), Value::String(port.clone()));
                match cell.kind {
                    Kind::Obuf {
                        data: OutData::Const(v),
                        ..
                    } => {
                        params.insert("OUT_CONST".into(), bit(v));
                        "CY2_OBUF"
                    }
                    Kind::Obuf {
                        data: OutData::Signal { invert },
                        ..
                    } => {
                        if invert {
                            params.insert("DATA_INVERT".into(), bit(true));
                        }
                        "CY2_OBUF"
                    }
                    _ => "CY2_IBUF",
                }
            }
            Kind::ClkCtrl => "CLKCTRL",
            Kind::Const(_) | Kind::TopIn | Kind::TopOut => {
                bail!("{}: left over after packing", cell.name)
            }
        };
        let mut dirs = Map::new();
        let mut conns = Map::new();
        for (p, net) in cell.ports() {
            let dir = if cell.kind.is_output(p) {
                "output"
            } else {
                "input"
            };
            dirs.insert(p.name().into(), dir.into());
            conns.insert(
                p.name().into(),
                json!(net.map(id).into_iter().collect::<Vec<_>>()),
            );
        }
        cells.insert(
            cell.name.clone(),
            json!({
                "hide_name": hide_name(&cell.name),
                "type": ty,
                "parameters": params,
                "attributes": attrs,
                "port_directions": dirs,
                "connections": conns,
            }),
        );
    }
    let mut netnames = Map::new();
    for n in nl.nets() {
        let net = nl.net(n);
        netnames.insert(
            net.name.clone(),
            json!({
                "hide_name": hide_name(&net.name),
                "bits": [id(n)],
                "attributes": net.attrs,
            }),
        );
    }
    let mut ports = Map::new();
    for p in &nl.top_ports {
        let bits: Vec<Value> = p
            .bits
            .iter()
            .map(|b| b.map_or(Value::Null, |n| json!(id(n))))
            .collect();
        ports.insert(
            p.name.clone(),
            json!({
                "direction": if p.output { "output" } else { "input" },
                "bits": bits,
            }),
        );
    }
    Ok(json!({
        "creator": "chipsmith pack",
        "modules": {
            "top": {
                "settings": {},
                "attributes": nl.module_attrs,
                "ports": ports,
                "cells": cells,
                "netnames": netnames,
            }
        }
    }))
}

#[cfg(test)]
mod tests;
