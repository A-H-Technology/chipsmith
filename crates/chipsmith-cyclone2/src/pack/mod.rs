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

use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Map, Value};

pub use diff::diff_files;
use mask::{fix, invert_input, normalise_carry, substitute, BUFFERS};
use netlist::{CellId, IoeIn, Kind, NetId, Netlist, Port, LUT_INPUTS};
pub use yosys::bit_name;

use crate::m4k::{AddrPort, Mode, Pin, Width};
use crate::pin::{register_param, PowerUp};

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
    pub m4ks: usize,
    pub mults: usize,
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
    pack_m4k(nl, &mut stats)?;
    pack_mult(nl, &mut stats)?;
    pack_constants(nl, &mut stats)?;
    pack_lut1(nl, &mut stats);
    pack_lut_ff(nl, &mut stats)?;
    pack_carry(nl, &mut stats)?;
    pack_clocks(nl, &mut stats)?;
    // Last, so no constant folding takes these LUTs away again.
    unsigned_signs(nl);
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
    p == Port::Pad
        && matches!(
            nl.cell(c).kind,
            Kind::Ibuf { .. } | Kind::Obuf { .. } | Kind::Iobuf { .. }
        )
}

fn is_iobuf(nl: &Netlist, c: CellId) -> bool {
    matches!(nl.cell(c).kind, Kind::Iobuf { .. })
}

/// Name each pin buffer after the top-level port bit it serves, drop the
/// port cells and the pad side (it has no wire; the bel is the pad).
fn pack_io(nl: &mut Netlist) -> Result<()> {
    for c in nl.cells() {
        let name = nl.cell(c).name.clone();
        let cell = nl.cell(c);
        let pad_users = |n: NetId| -> Vec<CellId> {
            nl.users(n)
                .into_iter()
                .filter(|&(u, p)| p == Port::Pad && is_iobuf(nl, u))
                .map(|(u, _)| u)
                .collect()
        };
        let buffers: Vec<CellId> = match cell.kind {
            Kind::TopIn => cell
                .net(Port::O)
                .map(|o| {
                    nl.users(o)
                        .into_iter()
                        .filter(|&(u, _)| matches!(nl.cell(u).kind, Kind::Ibuf { .. }))
                        .map(|(u, _)| u)
                        .collect()
                })
                .unwrap_or_default(),
            // A tri-state output's IOBUF has the port net on its inout
            // PAD, so it is one of the net's users, not its driver.
            Kind::TopOut => cell
                .net(Port::I)
                .map(|i| {
                    let driver = nl
                        .net(i)
                        .driver
                        .filter(|&(d, _)| matches!(nl.cell(d).kind, Kind::Obuf { .. }))
                        .map(|(d, _)| d);
                    driver.into_iter().chain(pad_users(i)).collect()
                })
                .unwrap_or_default(),
            Kind::TopInOut => {
                let found: Vec<CellId> = [Port::I, Port::O]
                    .into_iter()
                    .filter_map(|p| cell.net(p))
                    .flat_map(|n| {
                        let driver = nl
                            .net(n)
                            .driver
                            .map(|(d, _)| d)
                            .filter(|&d| is_iobuf(nl, d));
                        driver.into_iter().chain(pad_users(n))
                    })
                    .collect();
                if found.is_empty() {
                    bail!(
                        "inout port {name} has no tri-state driver \
                         (only `assign p = oe ? d : 1'bz` is supported)"
                    );
                }
                found
            }
            _ => continue,
        };
        for b in buffers {
            if let Some(port) = nl.cell_mut(b).kind.pin_port() {
                *port = Some(name.clone());
            }
        }
    }

    let mut remove = Vec::new();
    for c in nl.cells() {
        if !matches!(nl.cell(c).kind, Kind::TopIn | Kind::TopOut | Kind::TopInOut) {
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
        let name = nl.cell(c).name.clone();
        let Some(port) = nl.cell_mut(c).kind.pin_port() else {
            continue;
        };
        if port.is_none() {
            bail!("pin buffer {name} is not connected to a top-level port");
        }
        nl.disconnect(c, Port::Pad);
        nl.remove_port(c, Port::Pad);
        // A bidir whose input side nothing reads is a tri-state output
        // (input buffer off, as Quartus' `output` mode).
        if is_iobuf(nl, c) {
            if let Some(o) = nl.cell(c).net(Port::Combout) {
                if nl.user_count(o) == 0 {
                    nl.disconnect(c, Port::Combout);
                }
            }
        }
    }
    Ok(())
}

/// M4K: the logical ports synth/m4k_map.v gives a CY2_M4K become the
/// block's physical pins (`m4k::Pin`): CLK -> CLK0, A_WE -> WE,
/// A_DATAIN[c] -> DI<c>, DATAOUT[c] -> DO<c>, address bit p of port A or B
/// -> `Pin::addr`. An n-bit address uses bits 12 - n ..= 11; libmap ties
/// the ones below to 0 and they are dropped, as are data bits past the
/// width and constant data inputs. A_WE never driven is a ROM.
fn pack_m4k(nl: &mut Netlist, stats: &mut Stats) -> Result<()> {
    // Driven by GND/VCC, or not at all; a net without a driver isn't.
    let constant = |nl: &Netlist, n: Option<NetId>| {
        n.is_none_or(|n| {
            nl.net(n)
                .driver
                .is_some_and(|(d, _)| matches!(nl.cell(d).kind, Kind::Const(_)))
        })
    };
    for c in nl.cells() {
        let Kind::M4k(shape) = nl.cell(c).kind else {
            continue;
        };
        stats.m4ks += 1;
        let name = nl.cell(c).name.clone();
        let width = shape.width();
        let (bits, low_addr) = (width.bits(), 12 - width.addr_bits());
        let dual = shape.mode() == Mode::SimpleDual;
        let we = nl.cell(c).net(Port::RamWe);
        let writes = !constant(nl, we);
        if let Some(we) = we.filter(|_| !writes) {
            if nl
                .net(we)
                .driver
                .is_some_and(|(d, _)| nl.cell(d).kind == Kind::Const(true))
            {
                bail!("M4K {name}: write enable tied high is not supported");
            }
        }
        if dual && !writes && width != Width::X36 {
            bail!("M4K {name}: a read-only simple dual port narrower than x36 is not supported");
        }
        // nextpnr iterates a cell's ports newest first.
        let old: Vec<(Port, Option<NetId>)> = nl.cell(c).ports().collect();
        let mut pins: Vec<(Pin, NetId)> = Vec::new();
        let mut outs: Vec<(Pin, NetId)> = Vec::new();
        for &(port, net) in old.iter().rev() {
            nl.disconnect(c, port);
            nl.remove_port(c, port);
            match port {
                Port::RamDataOut(k) => {
                    // Read bits nothing uses stay off the routing (and out of
                    // the atom width Quartus is given).
                    if k < bits && nl.has_users(net) {
                        outs.push((Pin::Do(k), net.unwrap()));
                    }
                }
                Port::Clk => {
                    let Some(n) = net.filter(|&n| !constant(nl, Some(n))) else {
                        bail!("M4K {name}: clock not driven");
                    };
                    pins.push((Pin::Clk0, n));
                }
                Port::RamWe => {
                    if let Some(n) = net.filter(|_| writes) {
                        pins.push((Pin::We, n));
                    }
                }
                Port::RamDataIn(k) => {
                    if let Some(n) = net.filter(|&n| writes && k < bits && !constant(nl, Some(n))) {
                        pins.push((Pin::Di(k), n));
                    }
                }
                Port::RamAddr(ab, p) => {
                    // A read-only x36 simple dual port reads on port B; port
                    // A then has nothing.
                    let unused = match ab {
                        AddrPort::A => dual && !writes,
                        AddrPort::B => !dual,
                    };
                    if unused || p < low_addr {
                        continue;
                    }
                    let Some(n) = net.filter(|&n| !constant(nl, Some(n))) else {
                        bail!(
                            "M4K {name}: address pin {} is a constant; memories shallower than \
                             their M4K configuration are not supported",
                            port.name()
                        );
                    };
                    pins.push((Pin::addr(ab, p), n));
                }
                other => bail!("M4K {name}: unknown port {}", other.name()),
            }
        }
        for (pin, n) in pins.into_iter().chain(outs) {
            let port = Port::M4k(pin);
            if nl.cell(c).has_port(port) {
                bail!("M4K {name}: pin {pin} used twice");
            }
            nl.add_port(c, port);
            nl.connect(c, port, n);
        }
    }
    Ok(())
}

/// Multipliers: a CY2_MULT's operands move onto the block's data inputs,
/// MSB-aligned in their row (`mult::Shape::pin`), product bit k onto
/// `Shape::out(k)`; unread product bits are dropped. An undriven data input
/// reads 1, so operand bits tied to 1 are left off. Bits tied to 0 and the
/// unused low pins of the row read 0 through the block's input inversion:
/// `DATA_ZERO`, a mask over the 36 data inputs (operand A's 0-17, B's
/// 18-35). The sign inputs get theirs in `unsigned_signs`. The registers'
/// clock, enable and clear move onto the block's control pins; an enable
/// tied on or a clear tied off is left unconnected, which reads the same.
fn pack_mult(nl: &mut Netlist, stats: &mut Stats) -> Result<()> {
    use crate::mult::{Operand, Pin as MPin};
    for c in nl.cells() {
        let Kind::Mult(shape, regs) = nl.cell(c).kind else {
            continue;
        };
        stats.mults += 1;
        let name = nl.cell(c).name.clone();
        let mut zero = [false; 2 * crate::mult::MAX_WIDTH];
        let index = |pin: MPin| match pin {
            MPin::A(k) => k as usize,
            MPin::B(k) => crate::mult::MAX_WIDTH + k as usize,
            _ => unreachable!("data pins only"),
        };
        for op in [Operand::A, Operand::B] {
            for k in 0..shape.pad(op) {
                zero[index(shape.pin(op, 0)) - shape.pad(op) + k] = true;
            }
        }
        let old: Vec<(Port, Option<NetId>)> = nl.cell(c).ports().collect();
        let mut pins: Vec<(MPin, NetId)> = Vec::new();
        for &(port, net) in old.iter().rev() {
            nl.disconnect(c, port);
            nl.remove_port(c, port);
            let operand = match port {
                Port::MultA(i) => Some((Operand::A, i)),
                Port::MultB(i) => Some((Operand::B, i)),
                _ => None,
            };
            let constant = |n: Option<NetId>| {
                n.and_then(|n| nl.net(n).driver).and_then(|(d, _)| match nl.cell(d).kind {
                    Kind::Const(v) => Some(v),
                    _ => None,
                })
            };
            match (port, operand) {
                (Port::Clk | Port::Ena | Port::Aclr, _) if !regs.any() => {
                    bail!("multiplier {name}: {} without a register", port.name())
                }
                (Port::Clk, _) => {
                    let Some(n) = net.filter(|&n| constant(Some(n)).is_none()) else {
                        bail!("multiplier {name}: clock not driven");
                    };
                    pins.push((MPin::Clk, n));
                }
                (Port::Ena, _) => match (net, constant(net)) {
                    (_, Some(true)) | (None, _) => {}
                    (_, Some(false)) => bail!("multiplier {name}: registers never enabled"),
                    (Some(n), None) => pins.push((MPin::Ena, n)),
                },
                (Port::Aclr, _) => match (net, constant(net)) {
                    (_, Some(false)) | (None, _) => {}
                    (_, Some(true)) => bail!("multiplier {name}: registers always cleared"),
                    (Some(n), None) => pins.push((MPin::Aclr, n)),
                },
                (Port::MultP(k), _) => {
                    if (k as usize) < shape.width(Operand::A) + shape.width(Operand::B)
                        && nl.has_users(net)
                    {
                        pins.push((shape.out(k as usize), net.unwrap()));
                    }
                }
                (_, Some((op, i))) => {
                    ensure!(
                        (i as usize) < shape.width(op),
                        "multiplier {name}: operand bit {} past its width",
                        port.name()
                    );
                    let Some(n) = net else {
                        bail!("multiplier {name}: {} not driven", port.name());
                    };
                    let pin = shape.pin(op, i as usize);
                    match nl.net(n).driver.map(|(d, _)| &nl.cell(d).kind) {
                        Some(Kind::Const(true)) => {}
                        Some(Kind::Const(false)) => zero[index(pin)] = true,
                        _ => pins.push((pin, n)),
                    }
                }
                (other, _) => bail!("multiplier {name}: unknown port {}", other.name()),
            }
        }
        for (pin, n) in pins {
            let port = Port::Mult(pin);
            nl.add_port(c, port);
            nl.connect(c, port, n);
        }
        let mask: String = zero.iter().rev().map(|&z| if z { '1' } else { '0' }).collect();
        nl.cell_mut(c)
            .params
            .insert("DATA_ZERO".into(), Value::String(mask));
    }
    Ok(())
}

/// An unsigned operand needs a 0 on its sign input, which reads 1 undriven:
/// a LUT with no inputs and an all-zero mask drives it, one for the whole
/// design, as Quartus' `~GND` cell does (one cell; cyclone2-re's export
/// names it `~GND` so Quartus keeps it where it is placed).
fn unsigned_signs(nl: &mut Netlist) {
    use crate::mult::{Operand, Pin as MPin};
    let mut gnd = None;
    for c in nl.cells() {
        let Kind::Mult(shape, _) = nl.cell(c).kind else {
            continue;
        };
        for op in [Operand::A, Operand::B].into_iter().filter(|&op| !shape.signed[op as usize]) {
            let net = *gnd.get_or_insert_with(|| {
                let cell = nl.create_cell("~GND".to_string(), Kind::Lut { mask: 0 });
                let net = nl.create_net("~GND".to_string());
                nl.add_port(cell, Port::Combout);
                nl.connect(cell, Port::Combout, net);
                net
            });
            let port = Port::Mult(MPin::Sign(op));
            nl.add_port(c, port);
            nl.connect(c, port, net);
        }
    }
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
                _ if nl.cell(c).kind.is_ioe_in(p) => {
                    if let Some(input) = nl.cell_mut(c).kind.ioe_in(p) {
                        if let IoeIn::Signal { reg: Some(_), .. } = input {
                            bail!(
                                "{}: a constant {} through its I/O register; synthesis keeps \
                                 such registers in LEs",
                                nl.cell(c).name,
                                p.name()
                            );
                        }
                        *input = IoeIn::Const(value);
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
        Kind::Obuf { .. } | Kind::Iobuf { .. } => nl.cell(u).kind.is_ioe_in(p),
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
            kind @ (Kind::Obuf { .. } | Kind::Iobuf { .. }) => match kind.ioe_in(p) {
                Some(IoeIn::Signal { invert, .. }) => *invert = !*invert,
                _ => unreachable!("a pin buffer load on a constant input"),
            },
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
                (kind, _) if kind.is_ioe_in(p) => {
                    nl.reconnect(u, p, input);
                    if let Some(IoeIn::Signal { invert, .. }) = nl.cell_mut(u).kind.ioe_in(p) {
                        *invert = inv;
                    }
                }
                // A plain buffer in front of a register: the register takes
                // the source on its sync load path instead.
                (Kind::Ff(_), _) if p == Port::Datain && !inv => nl.reconnect(u, p, input),
                // A buffer in front of an M4K input: the block takes the
                // source straight from its LAB line. An inverter stays: the
                // block inverts no data or address pin.
                (Kind::M4k(_), _) if !inv => nl.reconnect(u, p, input),
                // And into a multiplier's operands.
                (Kind::Mult(..), _) if !inv => nl.reconnect(u, p, input),
                // Likewise into the JTAG and serial-flash blocks, which
                // invert nothing either.
                (Kind::Hard(_), _) if !inv => nl.reconnect(u, p, input),
                // A buffer in front of a clock: the clock control takes the
                // source.
                (Kind::Ff(_), _) if p == Port::Clk && !inv => nl.reconnect(u, p, input),
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

/// A global clock control on every net that clocks a register or an M4K.
/// Which one is the placer's choice. The clock can come from a pin or from
/// the core (an LE, the JTAG block's TCK): a clock control takes either,
/// the core through its tile's LAB lines (cyclone2-re README, "Global clocks
/// from the core").
fn pack_clocks(nl: &mut Netlist, stats: &mut Stats) -> Result<()> {
    let is_clock = |nl: &Netlist, (u, p): (CellId, Port)| match nl.cell(u).kind {
        Kind::Ff(_) => p == Port::Clk,
        Kind::M4k(_) => p == Port::M4k(Pin::Clk0),
        Kind::Mult(..) => p == Port::Mult(crate::mult::Pin::Clk),
        Kind::Ibuf { .. } | Kind::Obuf { .. } | Kind::Iobuf { .. } => {
            matches!(p, Port::Inclk | Port::Outclk)
        }
        _ => false,
    };
    let clocks: Vec<NetId> = nl
        .nets()
        .into_iter()
        .filter(|&n| nl.users(n).into_iter().any(|u| is_clock(nl, u)))
        .collect();
    for ni in clocks {
        let name = nl.net(ni).name.clone();
        if nl.net(ni).driver.is_none() {
            bail!("clock {name} has no driver");
        }
        let cc = nl.create_cell(format!("{name}$clkctrl"), Kind::ClkCtrl);
        nl.add_port(cc, Port::Inclk);
        nl.add_port(cc, Port::Outclk);
        let g = nl.create_net(format!("{name}$global"));
        for (u, p) in nl.users(ni) {
            if is_clock(nl, (u, p)) {
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
            Kind::Ibuf { port, .. } | Kind::Obuf { port, .. } | Kind::Iobuf { port, .. } => {
                let port = port.as_ref().context("pin buffer without a port")?;
                attrs.insert("PORT".into(), Value::String(port.clone()));
                fn register(params: &mut Map<String, Value>, name: &str, reg: Option<PowerUp>) {
                    if let Some(p) = reg {
                        params.insert(name.into(), Value::String(p.param().into()));
                    }
                }
                let mut ioe_in = |input: IoeIn, constant: &str, invert: &str, reg: &str| match input
                {
                    IoeIn::Const(v) => {
                        params.insert(constant.into(), bit(v));
                    }
                    IoeIn::Signal {
                        invert: inv,
                        reg: r,
                    } => {
                        if inv {
                            params.insert(invert.into(), bit(true));
                        }
                        register(&mut params, reg, r);
                    }
                };
                match cell.kind {
                    Kind::Obuf { data, .. } => {
                        ioe_in(data, "OUT_CONST", "DATA_INVERT", register_param::OUTPUT);
                        "CY2_OBUF"
                    }
                    Kind::Iobuf {
                        data, oe, in_reg, ..
                    } => {
                        ioe_in(data, "OUT_CONST", "DATA_INVERT", register_param::OUTPUT);
                        ioe_in(oe, "OE_CONST", "OE_INVERT", register_param::OE);
                        register(&mut params, register_param::INPUT, in_reg);
                        "CY2_IOBUF"
                    }
                    Kind::Ibuf { reg, .. } => {
                        register(&mut params, register_param::INPUT, reg);
                        "CY2_IBUF"
                    }
                    _ => unreachable!("pin buffers"),
                }
            }
            Kind::ClkCtrl => "CLKCTRL",
            Kind::Hard(b) => b.cell_type(),
            Kind::Mult(shape, regs) => {
                use crate::mult::{Operand, Regs};
                for (k, v) in [
                    ("A_WIDTH", shape.width(Operand::A)),
                    ("B_WIDTH", shape.width(Operand::B)),
                    ("A_SIGNED", usize::from(shape.signed[0])),
                    ("B_SIGNED", usize::from(shape.signed[1])),
                ]
                .into_iter()
                .chain(Regs::PARAMS.into_iter().zip(regs.flags().map(usize::from)))
                {
                    params.insert(k.into(), Value::String(format!("{v:032b}")));
                }
                "CY2_MULT"
            }
            Kind::M4k(shape) => {
                // As yosys wrote them: a string and a 32-bit binary number.
                params.insert("MODE".into(), Value::String(shape.mode_name().into()));
                params.insert(
                    "WIDTH".into(),
                    Value::String(format!("{:032b}", shape.width().bits())),
                );
                "CY2_M4K"
            }
            Kind::Const(_) | Kind::TopIn | Kind::TopOut | Kind::TopInOut => {
                bail!("{}: left over after packing", cell.name)
            }
        };
        let mut dirs = Map::new();
        let mut conns = Map::new();
        for (p, net) in cell.ports() {
            dirs.insert(p.name().into(), cell.kind.dir(p).name().into());
            conns.insert(
                p.name().into_owned(),
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
                "direction": p.dir.name(),
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
