//! chipsmith's 7-series packer: synthesis' primitives (`crate::synth`) to
//! the cells the placer moves (`crate::place::Design`) and what each of them
//! configures ([`Conf`]), replacing openXC7 nextpnr's packer.
//!
//! What the packer decides, the placer and the bitstream then take as
//! given:
//!
//! - every CARRY4 S input is a LUT's O6 in that bit's 6LUT: where S comes
//!   from anything else (a register, a pin, a constant, a LUT already tied
//!   to another carry bit), a feed-through LUT1 (or a constant LUT) is put
//!   in front of it. DI comes in on the bit's X bypass pin, never through a
//!   5LUT; a constant carry-in into the chain head is the PRECYINIT
//!   constant, a signal one comes in on AX;
//! - a RAM32M becomes its eight RAMD32s, `<ram>/DPR<k>_<h>`: memory k (A to
//!   D) in the k-th 6LUT (h = 1, data bit 1, out on O6) and 5LUT (h = 0,
//!   data bit 0, out on O5) of one SLICEM;
//! - each pin buffer (IBUF, OBUF, OBUFT, IOBUF) becomes the pin's IOB site,
//!   from the manifest's pin and prjxray-db's package pins;
//! - constants are two nets, driven by [`Conf::Gnd`] and [`Conf::Vcc`]
//!   cells that sit nowhere: the router ties each sink to its interconnect
//!   tile's GND_WIRE or VCC_WIRE. A register's CE tied 1 and SR tied 0 are
//!   no connection at all (the slice's CEUSEDMUX and SRUSEDMUX off), and so
//!   is a chain head's constant carry-in.

use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context, Result};
use chipsmith_netlist::{CellKind, Design as Netlist, Generic, ParamValue, SigBit, State};
use chipsmith_route::chipdb::ChipDb;
use rustc_hash::FxHashMap as HashMap;

use crate::place::{build, Cell, CellId, ControlSet, Design, Kind, Net, NetId};

/// What a cell configures, beside where it is and what it connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Conf {
    /// A logic LUT on pins A1 to A<k> (its pins in order), `init` a k-input
    /// table (`2^k` bits; k = 0 for a constant).
    Lut { init: u64 },
    /// A slice register: its power-up value and the value SR sets it to.
    Ff { init: bool, srval: bool },
    /// A CARRY4. `cyinit`: the chain head's constant carry-in (no CYINIT
    /// pin); `None` on later CARRY4s (CIN) and a head fed from a signal
    /// (CYINIT, on AX).
    Carry { cyinit: Option<bool> },
    /// One RAMD32 of a RAM32M: word `w` in bit `w`; written on the falling
    /// edge with `wclk_inv`.
    RamLut { init: u32, wclk_inv: bool },
    /// A pin's IOB, with its buffer: pins `O` (to the fabric), `I` (to the
    /// pin), `T` (high releases it), as the buffer has them.
    Pad,
    Bufg,
    Bscan,
    Ps7,
    Gnd,
    Vcc,
}

/// The packed design: the placer's cells and nets, each cell's
/// configuration, and the constant nets.
pub(crate) struct Packed {
    pub design: Design,
    pub conf: Vec<Conf>,
    pub gnd: NetId,
    pub vcc: NetId,
}

/// Ports a cell drives, by the placer's kind and pin name.
fn drives(kind: &Kind, pin: &str) -> bool {
    match kind {
        Kind::Lut { .. } => pin == "O6" || pin == "O5",
        Kind::Ff(_) => pin == "Q",
        Kind::Carry => pin.starts_with('O') || pin.starts_with("CO"),
        Kind::Pad { .. } => pin == "O",
        Kind::Bufg => pin == "O",
        Kind::Bscan { .. } => pin != "TDO",
        Kind::Ps7 => false,
        Kind::Gnd | Kind::Vcc => true,
    }
}

fn param_bits(g: &Generic, name: &str) -> u64 {
    match g.params.get(name) {
        Some(ParamValue::Bits(c)) => c
            .0
            .iter()
            .enumerate()
            .filter(|(_, s)| **s == State::One)
            .fold(0, |v, (i, _)| v | 1 << i),
        Some(ParamValue::Str(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn port<'a>(g: &'a Generic, name: &str) -> Option<&'a [SigBit]> {
    g.port(name).map(|p| p.sig.as_slice())
}

fn bit(g: &Generic, name: &str, i: usize) -> Option<SigBit> {
    port(g, name).and_then(|s| s.get(i).copied())
}

struct Packer {
    nets: HashMap<chipsmith_netlist::Net, NetId>,
    count: NetId,
    cells: Vec<Cell>,
    conf: Vec<Conf>,
    gnd: NetId,
    vcc: NetId,
}

impl Packer {
    fn net(&mut self, n: chipsmith_netlist::Net) -> NetId {
        if let Some(&id) = self.nets.get(&n) {
            return id;
        }
        let id = self.fresh();
        self.nets.insert(n, id);
        id
    }

    fn fresh(&mut self) -> NetId {
        self.count += 1;
        self.count - 1
    }

    /// A signal as a net: constants (x and z read as 0) on the constant
    /// nets.
    fn sig(&mut self, b: SigBit) -> NetId {
        match b {
            SigBit::Net(n) => self.net(n),
            SigBit::Const(State::One) => self.vcc,
            SigBit::Const(_) => self.gnd,
        }
    }

    fn push(&mut self, name: String, kind: Kind, pins: Vec<(String, NetId)>, conf: Conf) -> CellId {
        self.cells.push(Cell { name, kind, pins });
        self.conf.push(conf);
        self.cells.len() as CellId - 1
    }
}

/// Packs the synthesized top module; `pins` maps each top-level port bit
/// (`led[2]`) to its package pin (`G14`).
pub(crate) fn pack(netlist: &Netlist, pins: &BTreeMap<String, String>, db: &ChipDb) -> Result<Packed> {
    let m = netlist
        .modules
        .iter()
        .find(|m| !m.is_blackbox())
        .context("the netlist has no module")?;
    let mut p = Packer {
        nets: HashMap::default(),
        count: 2,
        cells: Vec::new(),
        conf: Vec::new(),
        gnd: 0,
        vcc: 1,
    };
    p.push("$gnd".into(), Kind::Gnd, vec![("G".into(), 0)], Conf::Gnd);
    p.push("$vcc".into(), Kind::Vcc, vec![("P".into(), 1)], Conf::Vcc);

    // Port bits by their pad net, for the pin buffers.
    let mut port_of: HashMap<chipsmith_netlist::Net, String> = HashMap::default();
    for (_, w, _) in m.ports() {
        for (i, b) in w.bits.iter().enumerate() {
            if let SigBit::Net(n) = b {
                let name = if w.bits.len() == 1 {
                    w.name.clone()
                } else {
                    format!("{}[{i}]", w.name)
                };
                port_of.entry(*n).or_insert(name);
            }
        }
    }
    let site_of_pin: HashMap<&str, &str> = db
        .pads
        .iter()
        .map(|pad| (pad.pin.as_str(), pad.bel.as_str()))
        .collect();
    let pad_site = |pad: SigBit, cell: &str| -> Result<(String, String)> {
        let port = pad
            .net()
            .and_then(|n| port_of.get(&n))
            .with_context(|| format!("pin buffer {cell} reaches no port"))?;
        let pin = pins
            .get(port)
            .with_context(|| format!("port {port} has no pin in the manifest"))?;
        let site = site_of_pin
            .get(pin.as_str())
            .with_context(|| format!("port {port}: pin {pin} isn't one of the package's"))?;
        Ok((site.to_string(), port.clone()))
    };

    let mut carries: Vec<CellId> = Vec::new();
    for c in &m.cells {
        let CellKind::Other(g) = &c.kind else {
            bail!("cell {} ({}) isn't a 7-series primitive", c.name, c.kind.type_name());
        };
        let name = c.name.clone();
        match g.ty.as_str() {
            "LUT1" | "LUT2" | "LUT3" | "LUT4" | "LUT5" | "LUT6" => {
                let k = (g.ty.as_bytes()[3] - b'0') as usize;
                let mut pins = Vec::new();
                for i in 0..k {
                    let b = bit(g, &format!("I{i}"), 0).with_context(|| format!("{name}: no I{i}"))?;
                    pins.push((format!("A{}", i + 1), p.sig(b)));
                }
                if let Some(SigBit::Net(o)) = bit(g, "O", 0) {
                    pins.push(("O6".into(), p.net(o)));
                }
                let init = param_bits(g, "INIT");
                p.push(name, Kind::Lut { dram: false }, pins, Conf::Lut { init });
            }
            "FDRE" | "FDSE" | "FDCE" | "FDPE" => {
                let sr_port = match g.ty.as_str() {
                    "FDRE" => "R",
                    "FDSE" => "S",
                    "FDCE" => "CLR",
                    _ => "PRE",
                };
                let mut pins = Vec::new();
                let clk = match bit(g, "C", 0) {
                    Some(SigBit::Net(n)) => p.net(n),
                    _ => bail!("register {name} has no clock"),
                };
                pins.push(("CK".into(), clk));
                let ce = match bit(g, "CE", 0).unwrap_or(SigBit::ONE) {
                    SigBit::Const(State::One) => None,
                    b => Some(p.sig(b)),
                };
                let sr = match bit(g, sr_port, 0).unwrap_or(SigBit::ZERO) {
                    SigBit::Const(State::Zero | State::X | State::Z) => None,
                    b => Some(p.sig(b)),
                };
                if let Some(ce) = ce {
                    pins.push(("CE".into(), ce));
                }
                if let Some(sr) = sr {
                    pins.push(("SR".into(), sr));
                }
                let d = bit(g, "D", 0).with_context(|| format!("{name}: no D"))?;
                pins.push(("D".into(), p.sig(d)));
                if let Some(SigBit::Net(q)) = bit(g, "Q", 0) {
                    pins.push(("Q".into(), p.net(q)));
                }
                let cs = ControlSet {
                    clk: Some(clk),
                    ce,
                    sr,
                    clk_inv: param_bits(g, "IS_C_INVERTED") == 1,
                    sync: matches!(g.ty.as_str(), "FDRE" | "FDSE"),
                };
                let conf = Conf::Ff {
                    init: param_bits(g, "INIT") == 1,
                    srval: matches!(g.ty.as_str(), "FDSE" | "FDPE"),
                };
                p.push(name, Kind::Ff(cs), pins, conf);
            }
            "CARRY4" => {
                let mut pins = Vec::new();
                let ci = bit(g, "CI", 0).unwrap_or(SigBit::ZERO);
                let cyinit = bit(g, "CYINIT", 0).unwrap_or(SigBit::ZERO);
                let head = matches!(ci, SigBit::Const(_));
                let conf_cyinit = match (head, cyinit) {
                    (false, _) => {
                        pins.push(("CIN".into(), p.sig(ci)));
                        None
                    }
                    (true, SigBit::Const(s)) => Some(s == State::One),
                    (true, b) => {
                        pins.push(("CYINIT".into(), p.sig(b)));
                        None
                    }
                };
                for k in 0..4 {
                    let s = bit(g, "S", k).unwrap_or(SigBit::ZERO);
                    pins.push((format!("S{k}"), p.sig(s)));
                    let di = bit(g, "DI", k).unwrap_or(SigBit::ZERO);
                    pins.push((format!("DI{k}"), p.sig(di)));
                    for (out, pin) in [("O", "O"), ("CO", "CO")] {
                        if let Some(SigBit::Net(n)) = bit(g, out, k) {
                            pins.push((format!("{pin}{k}"), p.net(n)));
                        }
                    }
                }
                let id = p.push(name, Kind::Carry, pins, Conf::Carry { cyinit: conf_cyinit });
                carries.push(id);
            }
            "RAM32M" => {
                let wclk_inv = param_bits(g, "IS_WCLK_INVERTED") == 1;
                let wclk = match bit(g, "WCLK", 0) {
                    Some(SigBit::Net(n)) => p.net(n),
                    _ => bail!("RAM {name} has no clock"),
                };
                let we = bit(g, "WE", 0).unwrap_or(SigBit::ONE);
                let we = p.sig(we);
                let waddr: Vec<NetId> = (0..5)
                    .map(|i| bit(g, "ADDRD", i).unwrap_or(SigBit::ZERO))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|b| p.sig(b))
                    .collect();
                for (k, l) in ['A', 'B', 'C', 'D'].into_iter().enumerate() {
                    let init = param_bits(g, &format!("INIT_{l}"));
                    for h in [1, 0] {
                        let mut pins = Vec::new();
                        for i in 0..5 {
                            let a = bit(g, &format!("ADDR{l}"), i).unwrap_or(SigBit::ZERO);
                            pins.push((format!("A{}", i + 1), p.sig(a)));
                        }
                        let di = bit(g, &format!("DI{l}"), h).unwrap_or(SigBit::ZERO);
                        pins.push(("DI".into(), p.sig(di)));
                        for (i, &wa) in waddr.iter().enumerate() {
                            pins.push((format!("WA{}", i + 1), wa));
                        }
                        pins.push(("WE".into(), we));
                        pins.push(("CLK".into(), wclk));
                        if let Some(SigBit::Net(o)) = bit(g, &format!("DO{l}"), h) {
                            let out = if h == 1 { "O6" } else { "O5" };
                            pins.push((out.into(), p.net(o)));
                        }
                        let bits = (0..32).fold(0u32, |v, w| v | ((init >> (2 * w + h) & 1) as u32) << w);
                        p.push(
                            format!("{name}/DPR{k}_{h}"),
                            Kind::Lut { dram: true },
                            pins,
                            Conf::RamLut {
                                init: bits,
                                wclk_inv,
                            },
                        );
                    }
                }
            }
            "IBUF" | "OBUF" | "OBUFT" | "IOBUF" => {
                let pad_port = match g.ty.as_str() {
                    "IBUF" => "I",
                    "IOBUF" => "IO",
                    _ => "O",
                };
                let pad = bit(g, pad_port, 0).with_context(|| format!("{name}: no pad"))?;
                let (site, port) = pad_site(pad, &name)?;
                let mut pins = Vec::new();
                match g.ty.as_str() {
                    "IBUF" => {
                        if let Some(SigBit::Net(o)) = bit(g, "O", 0) {
                            pins.push(("O".into(), p.net(o)));
                        }
                    }
                    ty => {
                        let i = bit(g, "I", 0).unwrap_or(SigBit::ZERO);
                        pins.push(("I".into(), p.sig(i)));
                        if ty != "OBUF" {
                            let t = bit(g, "T", 0).unwrap_or(SigBit::ZERO);
                            pins.push(("T".into(), p.sig(t)));
                        }
                        if ty == "IOBUF" {
                            if let Some(SigBit::Net(o)) = bit(g, "O", 0) {
                                pins.push(("O".into(), p.net(o)));
                            }
                        }
                    }
                }
                p.push(name, Kind::Pad { site, port }, pins, Conf::Pad);
            }
            "BUFG" => {
                let i = bit(g, "I", 0).with_context(|| format!("{name}: no I"))?;
                let mut pins = vec![("I0".into(), p.sig(i))];
                if let Some(SigBit::Net(o)) = bit(g, "O", 0) {
                    pins.push(("O".into(), p.net(o)));
                }
                p.push(name, Kind::Bufg, pins, Conf::Bufg);
            }
            "BSCANE2" => {
                let chain = match param_bits(g, "JTAG_CHAIN") {
                    0 => 1,
                    n => n as u32,
                };
                ensure!((1..=4).contains(&chain), "{name}: JTAG_CHAIN {chain}");
                let mut pins = Vec::new();
                for gp in &g.ports {
                    if let Some(&b) = gp.sig.first() {
                        if gp.name == "TDO" {
                            pins.push((gp.name.clone(), p.sig(b)));
                        } else if let SigBit::Net(n) = b {
                            pins.push((gp.name.clone(), p.net(n)));
                        }
                    }
                }
                p.push(name, Kind::Bscan { chain }, pins, Conf::Bscan);
            }
            "PS7" => {
                // Every PS7 input is tied to 0 where the bitstream is made
                // (`crate::phys`); a design driving or reading the PS7's
                // pins isn't packed yet.
                for gp in &g.ports {
                    ensure!(
                        gp.sig.iter().all(|b| matches!(b, SigBit::Const(State::Zero))),
                        "{name}: PS7 port {} is connected; only a PS7 with its inputs tied to 0 is supported",
                        gp.name
                    );
                }
                p.push(name, Kind::Ps7, Vec::new(), Conf::Ps7);
            }
            other => bail!("cell {name}: {other} isn't packed on the 7-series yet"),
        }
    }

    feed_throughs(&mut p, &carries);

    let mut nets: Vec<Net> = (0..p.count).map(|_| Net::default()).collect();
    for (i, c) in p.cells.iter().enumerate() {
        for (pin, net) in &c.pins {
            let n = &mut nets[*net as usize];
            if drives(&c.kind, pin) {
                ensure!(
                    n.driver.is_none(),
                    "net {} has two drivers ({} and {})",
                    net,
                    p.cells[n.driver.as_ref().unwrap().0 as usize].name,
                    c.name
                );
                n.driver = Some((i as CellId, pin.clone()));
            } else {
                n.users.push((i as CellId, pin.clone()));
            }
        }
    }
    let (gnd, vcc) = (p.gnd, p.vcc);
    let design = build(p.cells, nets)?;
    Ok(Packed {
        design,
        conf: p.conf,
        gnd,
        vcc,
    })
}

/// Gives every CARRY4 S input a LUT of its own in that bit's 6LUT: a LUT's
/// O6 the first time it feeds an S, else a feed-through LUT1 (a constant
/// LUT for a constant S).
fn feed_throughs(p: &mut Packer, carries: &[CellId]) {
    let mut driver: HashMap<NetId, CellId> = HashMap::default();
    for (i, c) in p.cells.iter().enumerate() {
        if c.kind == (Kind::Lut { dram: false }) {
            if let Some(o) = c.net("O6") {
                driver.insert(o, i as CellId);
            }
        }
    }
    let mut claimed: Vec<bool> = vec![false; p.cells.len()];
    for &carry in carries {
        for k in 0..4 {
            let pin = format!("S{k}");
            let s = p.cells[carry as usize].net(&pin).expect("every S is connected");
            if let Some(&l) = driver.get(&s) {
                if !claimed[l as usize] {
                    claimed[l as usize] = true;
                    continue;
                }
            }
            let out = p.fresh();
            let name = format!("{}$s{k}", p.cells[carry as usize].name);
            let (pins, init) = if s == p.gnd || s == p.vcc {
                (vec![("O6".into(), out)], u64::from(s == p.vcc))
            } else {
                (vec![("A1".into(), s), ("O6".into(), out)], 0b10)
            };
            p.push(name, Kind::Lut { dram: false }, pins, Conf::Lut { init });
            claimed.push(true);
            for (pn, net) in &mut p.cells[carry as usize].pins {
                if *pn == pin {
                    *net = out;
                }
            }
        }
    }
}
