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
//! - a block RAM (RAMB18E1, RAMB36E1) becomes its site's pins, as openXC7's
//!   nextpnr maps them (`pack.cc`, `pack_bram`), but a RAMB18E1's write
//!   enables as Vivado maps them: WE[i] on two pins (2i, 2i+1) for a port
//!   18 wide or SDP's 36, WE[0] on all four for a narrower one, WEBWE4-7
//!   outside SDP's wide write and the unused WEAs 0; the ADDR*TIEHIGH
//!   pins high; a RAMB36E1's
//!   controls, addresses and write enables on both halves' pins (`L`,
//!   `U`), ADDR[15] on `L15` alone. The output register's clock
//!   (REGCLK) is the port's, where the register is used;
//! - two LUTs whose inputs fit five pins share one LUT6 ([`pair_luts`]):
//!   one out of O6, the other out of O5 (A6 tied high), the second LUT's
//!   register in the letter's 5FF;
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
    Lut {
        init: u64,
    },
    /// A slice register: its power-up value and the value SR sets it to.
    Ff {
        init: bool,
        srval: bool,
    },
    /// A CARRY4. `cyinit`: the chain head's constant carry-in (no CYINIT
    /// pin); `None` on later CARRY4s (CIN) and a head fed from a signal
    /// (CYINIT, on AX).
    Carry {
        cyinit: Option<bool>,
    },
    /// One RAMD32 of a RAM32M: word `w` in bit `w`; written on the falling
    /// edge with `wclk_inv`.
    RamLut {
        init: u32,
        wclk_inv: bool,
    },
    /// A pin's IOB, with its buffer: pins `O` (to the fabric), `I` (to the
    /// pin), `T` (high releases it), as the buffer has them.
    Pad,
    /// A block RAM; its pins are its site's.
    Bram(Box<crate::bram::Config>),
    /// A DSP48E1; its pins are its site's (`A0`, `OPMODE3`, `CLK`, ...).
    Dsp(Box<crate::dsp::Config>),
    /// A PLL or MMCM, and its input clock's period (CLKIN1_PERIOD).
    Cmt(Box<CmtConf>),
    Bufg,
    Bscan,
    Ps7,
    Gnd,
    Vcc,
}

/// A clock management tile's block: what its bits hold, and the period of
/// the clock on CLKIN1 that the outputs' frequencies follow from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CmtConf {
    pub config: crate::cmt::Config,
    pub clkin_period_ps: u32,
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
        Kind::Cmt { .. } => pin.starts_with("CLKOUT") || pin == "CLKFBOUT" || pin == "LOCKED",
        Kind::Bscan { .. } => pin != "TDO",
        Kind::Ps7 => false,
        Kind::Bram { .. } => pin.starts_with("DO"),
        Kind::Dsp { .. } => DSP_OUTPUTS.iter().any(|o| {
            pin.strip_prefix(o)
                .is_some_and(|i| i.chars().all(|c| c.is_ascii_digit()))
        }),
        Kind::Gnd | Kind::Vcc => true,
    }
}

fn param_bits(g: &Generic, name: &str) -> u64 {
    match g.params.get(name) {
        Some(ParamValue::Bits(c)) => {
            c.0.iter()
                .enumerate()
                .filter(|(_, s)| **s == State::One)
                .fold(0, |v, (i, _)| v | 1 << i)
        }
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
pub(crate) fn pack(
    netlist: &Netlist,
    pins: &BTreeMap<String, String>,
    db: &ChipDb,
) -> Result<Packed> {
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
            bail!(
                "cell {} ({}) isn't a 7-series primitive",
                c.name,
                c.kind.type_name()
            );
        };
        let name = c.name.clone();
        match g.ty.as_str() {
            "LUT1" | "LUT2" | "LUT3" | "LUT4" | "LUT5" | "LUT6" => {
                let k = (g.ty.as_bytes()[3] - b'0') as usize;
                let mut pins = Vec::new();
                for i in 0..k {
                    let b =
                        bit(g, &format!("I{i}"), 0).with_context(|| format!("{name}: no I{i}"))?;
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
                let id = p.push(
                    name,
                    Kind::Carry,
                    pins,
                    Conf::Carry {
                        cyinit: conf_cyinit,
                    },
                );
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
                        let bits =
                            (0..32).fold(0u32, |v, w| v | ((init >> (2 * w + h) & 1) as u32) << w);
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
            "RAMB18E1" | "RAMB36E1" => {
                let r = crate::synth::cells::Ramb::from_generic(g)
                    .with_context(|| format!("block RAM {name}"))?;
                let pins = bram_pins(&r)
                    .into_iter()
                    .filter_map(|(pin, b, out)| match (out, b) {
                        (true, SigBit::Net(n)) => Some((pin, p.net(n))),
                        (true, _) => None,
                        (false, b) => Some((pin, p.sig(b))),
                    })
                    .collect();
                let c = &r.config;
                let reg = if c.sdp {
                    [c.ports[0].out_reg; 2]
                } else {
                    [c.ports[0].out_reg, c.ports[1].out_reg]
                };
                let kind = Kind::Bram {
                    size: c.size,
                    reg,
                    sdp: c.sdp,
                };
                p.push(name, kind, pins, Conf::Bram(Box::new(r.config)));
            }
            "DSP48E1" => {
                let config = crate::dsp::Config::from_params(&g.params)
                    .with_context(|| format!("DSP {name}"))?;
                let mut pins = Vec::new();
                for gp in &g.ports {
                    let output = gp.dir == Some(chipsmith_netlist::Dir::Out);
                    for (i, &b) in gp.sig.iter().enumerate() {
                        let pin = if gp.sig.len() > 1 {
                            format!("{}{i}", gp.name)
                        } else {
                            gp.name.clone()
                        };
                        match (output, b) {
                            (true, SigBit::Net(n)) => pins.push((pin, p.net(n))),
                            (true, _) => {}
                            (false, b) => {
                                if matches!(b, SigBit::Const(_))
                                    && !dsp_input_matters(&config, g, &gp.name)
                                {
                                    continue;
                                }
                                pins.push((pin, p.sig(b)));
                            }
                        }
                    }
                }
                let kind = Kind::Dsp {
                    inreg: [config.areg > 0, config.breg > 0],
                    mreg: config.stage("MREG"),
                    preg: config.stage("PREG"),
                };
                p.push(name, kind, pins, Conf::Dsp(Box::new(config)));
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
            "PLLE2_ADV" | "MMCME2_ADV" => {
                let prim = crate::cmt::Prim::from_cell(&g.ty).expect("matched");
                let net_of = |port: &str| match bit(g, port, 0) {
                    Some(SigBit::Net(n)) => Some(n),
                    _ => None,
                };
                let used: Vec<bool> = (0..prim.outputs())
                    .map(|k| net_of(&format!("CLKOUT{k}")).is_some())
                    .collect();
                let config = crate::cmt::Config::from_params(prim, &g.params, &used)
                    .with_context(|| format!("cell {name}"))?;
                let period_ns: f64 = match g.params.get("CLKIN1_PERIOD") {
                    Some(ParamValue::Str(s)) => s
                        .trim()
                        .parse()
                        .with_context(|| format!("{name}: CLKIN1_PERIOD {s:?}"))?,
                    _ => bail!("{name}: no CLKIN1_PERIOD"),
                };
                let mut pins = Vec::new();
                for port in ["CLKIN1", "CLKFBIN", "CLKFBOUT"] {
                    let n = net_of(port).with_context(|| format!("{name}: {port} unconnected"))?;
                    pins.push((port.to_string(), p.net(n)));
                }
                // A reset tied 0 stays unrouted (`crate::cmt::Config::features`).
                match bit(g, "RST", 0) {
                    Some(SigBit::Net(n)) => pins.push(("RST".into(), p.net(n))),
                    None | Some(SigBit::Const(State::Zero)) => {}
                    Some(b) => bail!("{name}: RST tied to {b:?}: held in reset"),
                }
                for k in 0..prim.outputs() {
                    if let Some(n) = net_of(&format!("CLKOUT{k}")) {
                        pins.push((format!("CLKOUT{k}"), p.net(n)));
                    }
                }
                if let Some(n) = net_of("LOCKED") {
                    pins.push(("LOCKED".into(), p.net(n)));
                }
                let conf = CmtConf {
                    config,
                    clkin_period_ps: (period_ns * 1000.0).round() as u32,
                };
                p.push(name, Kind::Cmt { prim }, pins, Conf::Cmt(Box::new(conf)));
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
    let pairs = pair_luts(&mut p);

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
    let design = build(p.cells, nets, &pairs)?;
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
            let s = p.cells[carry as usize]
                .net(&pin)
                .expect("every S is connected");
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

/// A block RAM's site pins and what each carries: (pin, signal, output).
fn bram_pins(r: &crate::synth::cells::Ramb) -> Vec<(String, SigBit, bool)> {
    use crate::bram::Size;
    let c = &r.config;
    let full = c.size == Size::Full;
    let mut out: Vec<(String, SigBit, bool)> = Vec::new();
    // A RAMB36E1's controls and addresses go to both halves.
    let both = |out: &mut Vec<(String, SigBit, bool)>, base: &str, l: &str, u: &str, b: SigBit| {
        if full {
            out.push((format!("{base}{l}"), b, false));
            out.push((format!("{base}{u}"), b, false));
        } else {
            out.push((base.to_string(), b, false));
        }
    };
    let pairs = [
        ("CLKARDCLK", "CLKBWRCLK"),
        ("ENARDEN", "ENBWREN"),
        ("REGCEAREGCE", "REGCEB"),
        ("RSTREGARSTREG", "RSTREGB"),
    ];
    let sigs = [r.clk, r.en, r.regce, r.rstreg];
    for ((a, b), v) in pairs.into_iter().zip(sigs) {
        both(&mut out, a, "L", "U", v[0]);
        both(&mut out, b, "L", "U", v[1]);
    }
    // RAMB36's port A RSTRAM pin is named oddly.
    both(&mut out, "RSTRAMARSTRAM", "LRST", "U", r.rstram[0]);
    both(&mut out, "RSTRAMB", "L", "U", r.rstram[1]);
    // The output registers' clocks.
    let reg = [
        c.ports[0].out_reg,
        c.ports[1].out_reg || (c.sdp && c.ports[0].out_reg),
    ];
    if reg[0] {
        both(&mut out, "REGCLKARDRCLK", "L", "U", r.clk[0]);
    }
    if reg[1] {
        // SDP's read register runs on the read clock, port A's.
        let clk = if c.sdp { r.clk[0] } else { r.clk[1] };
        both(&mut out, "REGCLKB", "L", "U", clk);
    }
    for (k, name) in ["ADDRARDADDR", "ADDRBWRADDR"].into_iter().enumerate() {
        for (i, &b) in r.addr[k].iter().enumerate() {
            if full && i == 15 {
                out.push((format!("{name}L15"), b, false));
            } else if full {
                out.push((format!("{name}L{i}"), b, false));
                out.push((format!("{name}U{i}"), b, false));
            } else {
                out.push((format!("{name}{i}"), b, false));
            }
        }
    }
    if !full {
        for pin in [
            "ADDRATIEHIGH0",
            "ADDRATIEHIGH1",
            "ADDRBTIEHIGH0",
            "ADDRBTIEHIGH1",
        ] {
            out.push((pin.into(), SigBit::ONE, false));
        }
    }
    // Write enables.
    let wide_write = c.sdp && c.ports[1].write == Some(c.size.sdp());
    if full {
        for (i, &b) in r.we[0].iter().enumerate() {
            let b = if c.sdp { SigBit::ZERO } else { b };
            out.push((format!("WEAL{i}"), b, false));
            out.push((format!("WEAU{i}"), b, false));
        }
        for (i, &b) in r.we[1].iter().enumerate() {
            let b = if i >= 4 && !wide_write {
                SigBit::ZERO
            } else {
                b
            };
            out.push((format!("WEBWEL{i}"), b, false));
            out.push((format!("WEBWEU{i}"), b, false));
        }
    } else {
        // Each physical pin is a byte lane's enable. A port 18 wide (or
        // SDP's 36) has WE[i] on lanes 2i and 2i+1; a narrower one writes
        // one lane, picked by its address, so WE[0] goes on all four: as
        // Vivado's RAMB18E1. WEBWE[i] on lane i alone (nextpnr's
        // `pack_bram`) never wrote port B's odd lanes in true dual port,
        // seen on the Zybo (bram-selftest, 2026-11-06).
        let lanes = |we: &[SigBit], width: Option<crate::bram::Width>, n: usize| -> Vec<SigBit> {
            use crate::bram::Width;
            (0..n)
                .map(|j| match width {
                    None => SigBit::ZERO,
                    Some(Width::X18 | Width::X36 | Width::X72) => we[j / 2],
                    Some(_) => we[0],
                })
                .collect()
        };
        let wea = if c.sdp {
            lanes(&r.we[0], None, 4)
        } else {
            lanes(&r.we[0], c.ports[0].write, 4)
        };
        for (j, b) in wea.into_iter().enumerate() {
            out.push((format!("WEA{j}"), b, false));
        }
        let webwe = if wide_write {
            lanes(&r.we[1], Some(crate::bram::Width::X36), 8)
        } else {
            let mut v = lanes(&r.we[1], c.ports[1].write, 4);
            v.extend([SigBit::ZERO; 4]);
            v
        };
        for (j, b) in webwe.into_iter().enumerate() {
            out.push((format!("WEBWE{j}"), b, false));
        }
    }
    for (k, (di, dip, dout, doutp)) in [
        ("DIADI", "DIPADIP", "DOADO", "DOPADOP"),
        ("DIBDI", "DIPBDIP", "DOBDO", "DOPBDOP"),
    ]
    .into_iter()
    .enumerate()
    {
        // A port that never writes takes no data (SDP's wide write is on
        // both ports' pins).
        if c.ports[k].write.is_some() || (wide_write && k == 0) {
            for (i, &b) in r.di[k].iter().enumerate() {
                out.push((format!("{di}{i}"), b, false));
            }
            for (i, &b) in r.dip[k].iter().enumerate() {
                out.push((format!("{dip}{i}"), b, false));
            }
        }
        for (i, &b) in r.dout[k].iter().enumerate() {
            out.push((format!("{dout}{i}"), b, true));
        }
        for (i, &b) in r.doutp[k].iter().enumerate() {
            out.push((format!("{doutp}{i}"), b, true));
        }
    }
    out
}

/// The inputs two LUTs must share to pair. Measured on zybo-jtag-counter,
/// zybo-picorv32 and zybo-sha256 (LUT6 sites, slices, estimated MHz; 2026-11-02):
/// unpaired 97/57, 700/242/204.1, 2572/1076/61.1; one shared input 65/53,
/// 651/228/169.2, 2285/990/55.5; two 96/61, 664/233/192.1, 2289/993/59.8;
/// three 97/57, 678/244/193.7, 2413/1052/57.3. Two keeps most of the
/// saving at a few percent of Fmax; one costs picorv32 a sixth of it.
const PAIR_SHARED: usize = 2;

/// Pairs of logic LUTs for one LUT6 each (LUT6_2): (the one out of O6, the
/// one out of O5). Two LUTs pair when their inputs together are at most
/// five nets, sharing at least [`PAIR_SHARED`], neither feeds a carry chain, and the O5 one's output
/// can leave the slice: alone on the letter's MUX pin, or only into its
/// own register (whose output then takes that pin). Their registers, if
/// both have one, share a control set. Both LUTs' pins are rewritten to
/// the shared inputs (A1 up), their tables with them. Greedy, most shared
/// inputs first.
fn pair_luts(p: &mut Packer) -> Vec<(CellId, CellId)> {
    // Off for comparing: CHIPSMITH_XC7_PAIR=0.
    if std::env::var("CHIPSMITH_XC7_PAIR").is_ok_and(|v| v == "0") {
        return Vec::new();
    }
    let n = p.cells.len();
    let min_shared = PAIR_SHARED;
    let mut users: HashMap<NetId, Vec<(CellId, String)>> = HashMap::default();
    for (i, c) in p.cells.iter().enumerate() {
        for (pin, net) in &c.pins {
            if !drives(&c.kind, pin) {
                users
                    .entry(*net)
                    .or_default()
                    .push((i as CellId, pin.clone()));
            }
        }
    }
    let output = |c: &Cell| c.net("O6");
    let inputs = |c: &Cell| -> Vec<NetId> {
        c.pins
            .iter()
            .filter(|(pn, _)| pn.starts_with('A'))
            .map(|&(_, net)| net)
            .collect()
    };
    // The register a LUT's BLE would take (`place::build`): the first one
    // its output drives.
    let ff_of = |l: usize| -> Option<CellId> {
        let o = output(&p.cells[l])?;
        users
            .get(&o)?
            .iter()
            .filter(|(u, pn)| pn == "D" && matches!(p.cells[*u as usize].kind, Kind::Ff(_)))
            .map(|&(u, _)| u)
            .min()
    };
    let cs = |f: CellId| match p.cells[f as usize].kind {
        Kind::Ff(cs) => Some(cs),
        _ => None,
    };
    let mut candidate = vec![false; n];
    for (i, c) in p.cells.iter().enumerate() {
        if c.kind != (Kind::Lut { dram: false }) {
            continue;
        }
        let ins = inputs(c);
        let feeds_carry = output(c).and_then(|o| users.get(&o)).is_some_and(|us| {
            us.iter()
                .any(|(u, _)| p.cells[*u as usize].kind == Kind::Carry)
        });
        candidate[i] = !ins.is_empty()
            && ins.len() <= 5
            && !feeds_carry
            && !ins.iter().any(|&x| x == p.gnd || x == p.vcc);
    }
    // Whether LUT l can be the O5 one.
    let o5_ok = |l: usize| -> bool {
        let Some(o) = output(&p.cells[l]) else {
            return true;
        };
        match ff_of(l) {
            None => true,
            Some(f) => users
                .get(&o)
                .is_some_and(|us| us.len() == 1 && us[0].0 == f),
        }
    };
    let mut by_input: HashMap<NetId, Vec<usize>> = HashMap::default();
    for i in (0..n).filter(|&i| candidate[i]) {
        for x in inputs(&p.cells[i]) {
            by_input.entry(x).or_default().push(i);
        }
    }
    let mut paired = vec![false; n];
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for a in (0..n).filter(|&i| candidate[i]) {
        if paired[a] {
            continue;
        }
        let ia = inputs(&p.cells[a]);
        let mut best: Option<(usize, usize)> = None;
        for x in &ia {
            for &b in by_input.get(x).into_iter().flatten() {
                if b == a || paired[b] {
                    continue;
                }
                let ib = inputs(&p.cells[b]);
                let mut union = ia.clone();
                union.extend(ib.iter().filter(|y| !ia.contains(y)));
                if union.len() > 5 {
                    continue;
                }
                let shared = ia.len() + ib.len() - union.len();
                if shared < min_shared {
                    continue;
                }
                if let (Some(fa), Some(fb)) = (ff_of(a), ff_of(b)) {
                    if cs(fa) != cs(fb) {
                        continue;
                    }
                }
                if !o5_ok(a) && !o5_ok(b) {
                    continue;
                }
                if best
                    .is_none_or(|(s, c)| (shared, std::cmp::Reverse(b)) > (s, std::cmp::Reverse(c)))
                {
                    best = Some((shared, b));
                }
            }
        }
        let Some((_, b)) = best else { continue };
        paired[a] = true;
        paired[b] = true;
        pairs.push(if o5_ok(b) { (a, b) } else { (b, a) });
    }
    for &(l6, l5) in &pairs {
        // Both on the union's pins, in order.
        let mut union = inputs(&p.cells[l6]);
        let i5 = inputs(&p.cells[l5]);
        union.extend(
            i5.iter()
                .filter(|y| !union.contains(y))
                .copied()
                .collect::<Vec<_>>(),
        );
        for &l in &[l6, l5] {
            let old = inputs(&p.cells[l]);
            let Conf::Lut { init } = p.conf[l] else {
                unreachable!("a logic LUT")
            };
            // Bit idx of the new table: the old one at the old inputs' values.
            let table = (0..1u64 << union.len()).fold(0u64, |t, idx| {
                let old_idx = old.iter().enumerate().fold(0u64, |a, (k, x)| {
                    let pos = union.iter().position(|u| u == x).expect("in the union");
                    a | (idx >> pos & 1) << k
                });
                t | (init >> old_idx & 1) << idx
            });
            p.conf[l] = Conf::Lut { init: table };
            let out: Vec<(String, NetId)> = p.cells[l]
                .pins
                .iter()
                .filter(|(pn, _)| !pn.starts_with('A'))
                .cloned()
                .collect();
            let mut pins: Vec<(String, NetId)> = union
                .iter()
                .enumerate()
                .map(|(k, &x)| (format!("A{}", k + 1), x))
                .collect();
            pins.extend(out);
            p.cells[l].pins = pins;
        }
    }
    pairs
        .into_iter()
        .map(|(a, b)| (a as CellId, b as CellId))
        .collect()
}

/// A DSP48E1's outputs, by port name.
const DSP_OUTPUTS: [&str; 12] = [
    "P",
    "PCOUT",
    "ACOUT",
    "BCOUT",
    "CARRYOUT",
    "CARRYCASCOUT",
    "MULTSIGNOUT",
    "OVERFLOW",
    "UNDERFLOW",
    "PATTERNDETECT",
    "PATTERNBDETECT",
    "ECCPARITY",
];

/// Whether a DSP input tied to a constant has to be routed: no where its
/// value can't matter (a register stage that is off, the cascade inputs
/// with A and B direct, C and PCIN where a constant OPMODE selects
/// neither, D without the D port). Saves routing a hundred constants a DSP.
fn dsp_input_matters(c: &crate::dsp::Config, g: &Generic, port: &str) -> bool {
    let opmode: Option<u64> = port_const(g, "OPMODE");
    let z = opmode.map(|o| o >> 4 & 7);
    let y = opmode.map(|o| o >> 2 & 3);
    let x = opmode.map(|o| o & 3);
    let reg = |name: &str| c.stage(name);
    match port {
        "C" => !(z.is_some_and(|z| z != 3) && y.is_some_and(|y| y != 3)),
        "PCIN" => !(z.is_some_and(|z| z != 1 && z != 5)),
        "D" => c.use_dport,
        "ACIN" => c.a_cascade,
        "BCIN" => c.b_cascade,
        "CARRYCASCIN" | "MULTSIGNIN" => false,
        "CEA1" => c.areg == 2 || c.acascreg == 2,
        "CEB1" => c.breg == 2 || c.bcascreg == 2,
        "CEA2" | "RSTA" => c.areg > 0,
        "CEB2" | "RSTB" => c.breg > 0,
        "CEM" | "RSTM" => reg("MREG"),
        "CEP" | "RSTP" => reg("PREG"),
        "CEC" | "RSTC" => reg("CREG"),
        "CED" | "CEAD" | "RSTD" => c.use_dport && (reg("DREG") || reg("ADREG")),
        "CEALUMODE" | "RSTALUMODE" => reg("ALUMODEREG"),
        "CECTRL" | "RSTCTRL" => reg("OPMODEREG") || reg("CARRYINSELREG"),
        "CECARRYIN" | "RSTALLCARRYIN" => reg("CARRYINREG"),
        "CEINMODE" | "RSTINMODE" => reg("INMODEREG"),
        "CLK" => c.areg > 0 || c.breg > 0 || c.stages.iter().any(|&s| s),
        // A, B, OPMODE, ALUMODE, INMODE, CARRYIN, CARRYINSEL.
        _ => x.is_none_or(|x| x != 0) || !matches!(port, "A" | "B"),
    }
}

/// A port's value, if all its bits are constants.
fn port_const(g: &Generic, name: &str) -> Option<u64> {
    let sig = &g.port(name)?.sig;
    sig.iter().enumerate().try_fold(0u64, |v, (i, b)| match b {
        SigBit::Const(State::One) => Some(v | 1 << i),
        SigBit::Const(_) => Some(v),
        SigBit::Net(_) => None,
    })
}
