//! nextpnr frontend: a routed himbaechel JSON (`nextpnr-himbaechel --write`,
//! uarch cyclone2, or chipsmith's router, `route.rs`) -> physical netlist.
//! The parsed design (`Routed`) is public: cyclone2-re's
//! `cram quartus-export` writes it out as Quartus constraints, so its oracle
//! can assemble exactly this placement and routing.
//!
//! Wire and bel names follow the chip database (cyclone2-re's `chipdb.rs`,
//! which documents them).

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use super::device::{Device, Node};

use super::device::element_name;
use super::hard::{self, Asmi, Hard, HardInput, Jtag};
use super::io::{
    Dir, InReg, Input, IoDb, IoReg, OutData, Output, OutputEnable, PinUse, Standard, Vccio,
    BLOCK_CLK_N, BLOCK_CLK_P,
};
use super::logic::{
    clock_slots, Aclr, ClkCtrl, Ctl, Domain, GlobalClock, LabConfig, LeConfig, LeSource, Logic,
    Lut, Reg, Sload, BLOCK_INPUT_MUX, CORE_INPUT,
};
use super::m4k::{self, Content, Controls, CtlSrc, M4k, Mode, Width};
use super::mult::{self as bmult, DataIn, DataPin, Mult, Sign};
use super::phys::{PhysDesign, Pip, RoutedNet};
use crate::mult::{self as cmult, Operand};
use crate::pin::{assignment, register_of, register_param, PinSettings, Pull};

/// A nextpnr wire, by what it is in the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Wire {
    /// A routing-graph element.
    Node(Node),
    /// LOCAL_LINE k of LAB (x, y): LE k's direct output into its own LAB.
    LocalLine {
        x: u32,
        y: u32,
        k: u32,
    },
    /// A pin of LE k (LUT input A..D, COMB, Q, CLK).
    Le {
        x: u32,
        y: u32,
        k: u32,
        pin: LePin,
    },
    /// Output data / output enable / pad-to-core of the I/O element at
    /// logic tile (x, y), bel z.
    Ioe {
        x: u32,
        y: u32,
        z: u32,
        pin: IoePin,
    },
    ClkIn {
        g: u32,
    },
    /// The input of a PLL's external clock output's clock control.
    PllOutIn {
        x: u32,
        y: u32,
    },
    /// LAB control line k (rcf LAB_CONTROL_MUX).
    Ctrl {
        x: u32,
        y: u32,
        k: u32,
    },
    /// A pin of the M4K block at (x, y), or one of its control inputs.
    M4k {
        x: u32,
        y: u32,
        pin: M4kPin,
    },
    /// An input of the multiplier at (X39, y), behind its source mux
    /// (data, signs, enable) or its fast mux (clock, clear).
    Mult {
        y: u32,
        pin: cmult::Pin,
    },
    /// Control input k (0 clock, 2 clear) of the multiplier at (X39, y): a
    /// LAB line into the fast mux.
    MultCtl {
        y: u32,
        k: u8,
    },
    /// An input of the JTAG or serial-flash block.
    Hard(HardInput),
    /// A control input of the PLL in tile (x, y).
    PllIn {
        x: u32,
        y: u32,
        input: super::pll::Control,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum M4kPin {
    /// An input behind a source mux over the block's LAB lines.
    In(m4k::Input),
    /// Control input k (`RAM_PIA_TO_INPUT_YGR` k, rcf
    /// `M4K_LEFT_CONTROL_INPUT`), fed from a LAB line.
    Ctl(u8),
    /// A control pin behind a fast mux: `m4k::CONTROL_PINS[i]`.
    Fast(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LePin {
    /// Physical LE input A..D (fed by the LAB's lines).
    Input(usize),
    /// Logical LUT input DATAA..DATAD (a cell pin), fed by any physical one.
    Logical(usize),
    Comb,
    Q,
    Clk,
    Ena,
    Aclr,
    Sclr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IoePin {
    DataOut,
    Oe,
    CombOut,
    /// The input register's clock and output.
    InClk,
    RegOut,
    /// The output and OE registers' clock.
    OutClk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bel {
    Lut {
        x: u32,
        y: u32,
        k: u32,
    },
    Ff {
        x: u32,
        y: u32,
        k: u32,
    },
    Ioe {
        x: u32,
        y: u32,
        z: u32,
    },
    ClkCtrl {
        g: u32,
    },
    /// A PLL's external clock output's clock control.
    PllOut {
        x: u32,
        y: u32,
    },
    M4k {
        x: u32,
        y: u32,
    },
    Mult {
        y: u32,
    },
    Jtag,
    Asmi,
    Pll {
        x: u32,
        y: u32,
    },
}

#[derive(Debug)]
pub struct Cell {
    pub ty: String,
    pub bel: Bel,
    pub params: HashMap<String, String>,
    pub attrs: HashMap<String, String>,
    /// port -> net id (only connected ports)
    pub conns: HashMap<String, u64>,
}

#[derive(Debug)]
pub struct PipUse {
    pub src: Wire,
    pub dst: Wire,
}

#[derive(Debug, Default)]
pub struct Net {
    pub name: String,
    pub pips: Vec<PipUse>,
}

pub struct Routed {
    pub cells: BTreeMap<String, Cell>,
    pub nets: BTreeMap<u64, Net>,
    /// Top-level port (bus) name -> direction.
    pub ports: HashMap<String, PortDir>,
    /// The top module's attributes (the unused-pin mode, `crate::pin`).
    pub attrs: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDir {
    In,
    Out,
    InOut,
}

fn tile_xy(t: &str) -> Result<(u32, u32)> {
    let r = t.strip_prefix('X').with_context(|| format!("tile {t}"))?;
    let (x, y) = r.split_once('Y').with_context(|| format!("tile {t}"))?;
    Ok((x.parse()?, y.parse()?))
}

fn num(s: &str, prefix: &str) -> Option<u32> {
    s.strip_prefix(prefix)?.parse().ok()
}

fn find(dev: &Device, x: u32, y: u32, e: i16, z: i16, idx: u32) -> Result<Node> {
    let v: Vec<Node> = dev
        .at(x as i16, y as i16, z, idx as i16)
        .iter()
        .copied()
        .filter(|&n| dev.element_enum(n) == e)
        .collect();
    match v[..] {
        [n] => Ok(n),
        _ => bail!("{} {} at X{x} Y{y} S{z} I{idx}", v.len(), element_name(e)),
    }
}

fn wire(dev: &Device, tx: u32, ty: u32, name: &str) -> Result<Wire> {
    if let Some(n) = num(name, "G") {
        return Ok(Wire::Node(Node(n)));
    }
    let (x, y) = (tx / 2, ty);
    if let Some(l) = num(name, "LI") {
        return Ok(Wire::Node(find(dev, x, y, 276, 0, l)?));
    }
    if let Some(i) = num(name, "LEB") {
        return Ok(Wire::Node(find(dev, x, y, 45, 0, i)?));
    }
    if let Some(b) = num(name, "BCB") {
        return Ok(Wire::Node(find(dev, x, y, 560, 0, b)?));
    }
    if let Some(k) = num(name, "MB") {
        return Ok(Wire::Node(find(dev, x, y, 278, 0, k)?));
    }
    if let Some(k) = num(name, "DB") {
        return Ok(Wire::Node(find(dev, x, y, 280, 0, k)?));
    }
    if let Some(pin) = name.strip_prefix("MULT.") {
        if let Some(k) = num(pin, "CTL") {
            return Ok(Wire::MultCtl { y, k: k as u8 });
        }
        let pin = cmult::Pin::parse(pin)
            .filter(|p| !p.is_output())
            .with_context(|| format!("wire {name}"))?;
        return Ok(Wire::Mult { y, pin });
    }
    if let Some(pin) = name.strip_prefix("M4K.") {
        let pin = if let Some(k) = num(pin, "CTL") {
            M4kPin::Ctl(k as u8)
        } else if let Some(i) = m4k::CONTROL_PINS.iter().position(|p| p.0 == pin) {
            M4kPin::Fast(i as u8)
        } else {
            M4kPin::In(m4k::Input::from_pin_name(pin).with_context(|| format!("wire {name}"))?)
        };
        return Ok(Wire::M4k { x, y, pin });
    }
    if let Some(k) = num(name, "LL") {
        return Ok(Wire::LocalLine { x, y, k });
    }
    if let Some(k) = num(name, "CTRL") {
        return Ok(Wire::Ctrl { x, y, k });
    }
    if let Some((bel, pin)) = name.split_once('.') {
        if let Some(k) = num(bel, "LE") {
            let pin = match pin {
                "IA" => LePin::Logical(0),
                "IB" => LePin::Logical(1),
                "IC" => LePin::Logical(2),
                "ID" => LePin::Logical(3),
                "A" => LePin::Input(0),
                "B" => LePin::Input(1),
                "C" => LePin::Input(2),
                "D" => LePin::Input(3),
                "COMB" => LePin::Comb,
                "Q" => LePin::Q,
                "CLK" => LePin::Clk,
                "ENA" => LePin::Ena,
                "ACLR" => LePin::Aclr,
                "SCLR" => LePin::Sclr,
                _ => bail!("wire {name}"),
            };
            return Ok(Wire::Le { x, y, k, pin });
        }
        if let Some(z) = num(bel, "IOE") {
            let pin = match pin {
                "DATAOUT" => IoePin::DataOut,
                "OE" => IoePin::Oe,
                "COMBOUT" => IoePin::CombOut,
                "INCLK" => IoePin::InClk,
                "REGOUT" => IoePin::RegOut,
                "OUTCLK" => IoePin::OutClk,
                _ => bail!("wire {name}"),
            };
            return Ok(Wire::Ioe { x, y, z, pin });
        }
        if let Some(g) = num(bel, "CLKCTRL") {
            return Ok(Wire::ClkIn { g });
        }
        if bel == "PLLOUT" {
            return Ok(Wire::PllOutIn { x, y });
        }
        if let Some(h) = HardInput::ALL
            .into_iter()
            .find(|h| h.bel_pin() == (bel, pin))
        {
            return Ok(Wire::Hard(h));
        }
        if bel == "PLL" {
            if let Some(input) = super::pll::Control::ALL
                .into_iter()
                .find(|c| c.pin() == pin)
            {
                return Ok(Wire::PllIn { x, y, input });
            }
        }
    }
    bail!("unknown wire {name} in tile X{tx}Y{ty}")
}

fn bel(tx: u32, ty: u32, name: &str) -> Result<Bel> {
    let (x, y) = (tx / 2, ty);
    if let Some(k) = name.strip_suffix(".LUT").and_then(|b| num(b, "LE")) {
        return Ok(Bel::Lut { x, y, k });
    }
    if let Some(k) = name.strip_suffix(".FF").and_then(|b| num(b, "LE")) {
        return Ok(Bel::Ff { x, y, k });
    }
    if let Some(z) = num(name, "IOE") {
        return Ok(Bel::Ioe { x, y, z });
    }
    if let Some(g) = num(name, "CLKCTRL") {
        return Ok(Bel::ClkCtrl { g });
    }
    if name == "PLLOUT" {
        return Ok(Bel::PllOut { x, y });
    }
    if name == "M4K" {
        return Ok(Bel::M4k { x, y });
    }
    if name == "MULT" {
        ensure!(x == bmult::X, "multiplier bel at X{x}");
        return Ok(Bel::Mult { y });
    }
    if name == "JTAG" {
        return Ok(Bel::Jtag);
    }
    if name == "ASMI" {
        return Ok(Bel::Asmi);
    }
    if name == "PLL" {
        return Ok(Bel::Pll { x, y });
    }
    bail!("unknown bel {name}")
}

fn strs(v: &Value) -> HashMap<String, String> {
    v.as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

pub fn read(dev: &Device, path: &Path) -> Result<Routed> {
    let j: Value = serde_json::from_str(
        &std::fs::read_to_string(path).with_context(|| path.display().to_string())?,
    )?;
    let m = j["modules"]["top"].as_object().context("no module top")?;
    let mut cells = BTreeMap::new();
    for (name, c) in m["cells"].as_object().context("cells")? {
        let attrs = strs(&c["attributes"]);
        let b = attrs
            .get("NEXTPNR_BEL")
            .with_context(|| format!("{name} is not placed"))?;
        let (t, bn) = b.split_once('/').context("bel name")?;
        let (tx, ty) = tile_xy(t)?;
        let mut conns = HashMap::new();
        for (p, bits) in c["connections"].as_object().context("connections")? {
            if let Some(id) = bits
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_u64)
            {
                conns.insert(p.clone(), id);
            }
        }
        cells.insert(
            name.clone(),
            Cell {
                ty: c["type"].as_str().unwrap_or("").to_string(),
                bel: bel(tx, ty, bn)?,
                params: strs(&c["parameters"]),
                attrs,
                conns,
            },
        );
    }
    let mut nets: BTreeMap<u64, Net> = BTreeMap::new();
    for (name, n) in m["netnames"].as_object().context("netnames")? {
        let Some(id) = n["bits"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_u64)
        else {
            continue;
        };
        let net = nets.entry(id).or_default();
        let routing = n["attributes"]["ROUTING"].as_str().unwrap_or("");
        if net.name.is_empty() || !routing.is_empty() {
            net.name = name.clone();
        }
        let parts: Vec<&str> = routing.split(';').collect();
        for t in parts.chunks(3) {
            let [_, pip, _] = t else { continue };
            if pip.is_empty() {
                continue;
            }
            let mut f = pip.split('/');
            let (Some(tile), Some(dst), Some(src)) = (f.next(), f.next(), f.next()) else {
                bail!("pip {pip}")
            };
            let (tx, ty) = tile_xy(tile)?;
            net.pips.push(PipUse {
                src: wire(dev, tx, ty, src)?,
                dst: wire(dev, tx, ty, dst)?,
            });
        }
    }
    let mut ports = HashMap::new();
    for (name, p) in m["ports"].as_object().context("ports")? {
        let d = match p["direction"].as_str() {
            Some("input") => PortDir::In,
            Some("output") => PortDir::Out,
            Some("inout") => PortDir::InOut,
            d => bail!("port {name}: direction {d:?}"),
        };
        ports.insert(name.clone(), d);
    }
    Ok(Routed {
        cells,
        nets,
        ports,
        attrs: strs(&m["attributes"]),
    })
}

/// Logical -> physical LUT input of each LE (from the permutation pips).
pub type Perm = HashMap<(u32, u32, u32), [Option<usize>; 4]>;

pub fn perms(nets: &BTreeMap<u64, Net>) -> Perm {
    let mut out: Perm = HashMap::new();
    for p in nets.values().flat_map(|n| &n.pips) {
        if let (
            Wire::Le {
                pin: LePin::Input(ph),
                ..
            },
            Wire::Le {
                x,
                y,
                k,
                pin: LePin::Logical(lg),
            },
        ) = (p.src, p.dst)
        {
            out.entry((x, y, k)).or_default()[lg] = Some(ph);
        }
    }
    out
}

/// The truth table over physical inputs. Unrouted logical inputs keep their
/// own position: the mask doesn't depend on them, except in carry LEs,
/// where C is the carry in and D picks sum or carry and both stay put.
fn physical_mask(logical: u16, perm: [Option<usize>; 4]) -> u16 {
    let mut out = 0u16;
    for idx_p in 0..16 {
        let mut idx_l = 0;
        for (q, p) in perm.iter().enumerate() {
            idx_l |= (idx_p >> p.unwrap_or(q) & 1) << q;
        }
        out |= (logical >> idx_l & 1) << idx_p;
    }
    out
}

pub fn flag(c: &Cell, p: &str) -> bool {
    c.params.get(p).is_some_and(|v| v.contains('1'))
}

pub fn lut_mask(c: &Cell) -> Result<u16> {
    let s = c.params.get("LUT_MASK").context("LUT_MASK")?;
    Ok(u16::from_str_radix(s, 2)?)
}

/// What a packed CY2_M4K cell (`pack::pack_m4k`, or nextpnr/cyclone2's:
/// physical pin names, the shape's parameters, INIT) configures, before
/// routing.
pub struct M4kShape {
    pub shape: crate::m4k::Shape,
    /// Whether port A (WE) and port B (RE) write.
    pub writes: [bool; 2],
    pub content: Content,
}

impl M4kShape {
    pub fn of(c: &Cell) -> Result<M4kShape> {
        let p = |k: &str| c.params.get(k).with_context(|| format!("M4K without {k}"));
        let used = |col: u32| {
            c.conns.contains_key(&format!("DI{col}")) || c.conns.contains_key(&format!("DO{col}"))
        };
        // A cell from the yosys flow: the data bits it uses are the
        // physical width, less the top P/9 columns of a x9/x18/x36 port when
        // no data pin there is connected. That is the atom width Quartus is
        // given (x8, x16, x32 are physically x9, x18, x36 with bit k in
        // column k, VQM experiment q1).
        let legacy_bits = |w: Width| {
            let w = w.bits() as u32;
            let bits = if w >= 9 && (w - w / 9..w).all(|col| !used(col)) {
                w - w / 9
            } else {
                w
            };
            bits as u8
        };
        let shape = crate::m4k::Shape::parse(
            |k| c.params.get(k).map(|v| v.trim_end().to_owned()),
            |k| {
                c.params
                    .get(k)
                    .and_then(|v| u64::from_str_radix(v.trim(), 2).ok())
            },
            legacy_bits,
        )?;
        let writes = [c.conns.contains_key("WE"), c.conns.contains_key("RE")];
        // INIT: MSB first, bit l = linear position l (skipping every ninth
        // position for x1/x2/x4, as the block does). Bits no port sees stay
        // 0, as in Quartus' contents.
        let init = p("INIT")?.trim().as_bytes();
        ensure!(init.len() == m4k::BITS, "M4K INIT has {} bits", init.len());
        let mut content = Content::default();
        for l in 0..m4k::BITS {
            if init[init.len() - 1 - l] == b'1' && shape.holds(l) {
                content.set_linear(l);
            }
        }
        Ok(M4kShape {
            shape,
            writes,
            content,
        })
    }

    pub fn mode(&self) -> Result<Mode> {
        self.shape.mode(self.writes[0])
    }
}

/// Package pin at an I/O bel.
pub fn pin_at(io: &IoDb, x: u32, y: u32, z: u32) -> Result<&super::io::Pin> {
    let clock = z >= 8;
    io.pins
        .values()
        .find(|p| {
            (p.xyz.0 as u32, p.xyz.1 as u32, p.xyz.2 as u32) == (x, y, z % 8)
                && clock == [BLOCK_CLK_P, BLOCK_CLK_N].contains(&p.ioe.block)
        })
        .with_context(|| format!("no pin at X{x} Y{y} IOE{z}"))
}

fn le_at(les: &mut BTreeMap<(u32, u32, u32), LeConfig>, x: u32, y: u32, k: u32) -> &mut LeConfig {
    les.entry((x, y, k)).or_insert_with(|| LeConfig {
        x,
        y,
        le: k,
        lut: None,
        inputs: [None; 4],
        carry_in: false,
        reg_feedback: false,
        reg: None,
        comb_out: [false; 3],
    })
}

impl Routed {
    /// Direction of the top-level port a pin buffer's `PORT` (`name` or
    /// `name[i]`) belongs to.
    pub fn port_dir(&self, port: &str) -> Result<PortDir> {
        let (b, _) = split_port(port);
        self.ports
            .get(port)
            .or_else(|| self.ports.get(b))
            .copied()
            .with_context(|| format!("no top-level port {port}"))
    }

    pub fn to_phys(&self, dev: &Device, io: &IoDb) -> Result<PhysDesign> {
        let mut les: BTreeMap<(u32, u32, u32), LeConfig> = BTreeMap::new();
        let mut clk_buf: HashMap<(u32, u32, u32), u8> = HashMap::new();
        let mut ena: HashMap<(u32, u32, u32), u8> = HashMap::new();
        let mut aclr: HashMap<(u32, u32, u32), Aclr> = HashMap::new();
        let mut sclr: HashMap<(u32, u32, u32), u8> = HashMap::new();
        let mut pia: HashMap<(u32, u32, u32), LeSource> = HashMap::new();
        let mut clk_in: HashMap<u32, (Node, u8)> = HashMap::new();
        // A clock control fed from the core: LAB line -> BLOCK_INPUT_MUX
        // (532) -> CLKBUF_IN 4, both graph edges without bits of their own;
        // the line is chosen by the clock control's COREIN mux.
        let mut core_in: HashMap<Node, u8> = HashMap::new();
        let mut data_line: HashMap<(u32, u32, u32), u32> = HashMap::new();
        // Pins driven by a PLL's external clock output.
        let mut pll_data: std::collections::HashSet<(u32, u32, u32)> = Default::default();
        let mut oe_line: HashMap<(u32, u32, u32), u32> = HashMap::new();
        let mut datain: HashMap<(u32, u32, u32), [bool; 2]> = HashMap::new();
        // The input register's pad-to-core paths, and the row-clock buffer
        // of each IOE's input and output-side registers.
        let mut reg_datain: HashMap<(u32, u32, u32), [bool; 2]> = HashMap::new();
        let mut io_clk: HashMap<(u32, u32, u32, IoePin), u32> = HashMap::new();
        let mut ram_in: HashMap<(u32, u32), BTreeMap<m4k::Input, u8>> = HashMap::new();
        let mut ram_pia: HashMap<(u32, u32), [Option<u8>; 8]> = HashMap::new();
        let mut ram_ctl: HashMap<(u32, u32, u8), CtlSrc> = HashMap::new();
        let mut hard_in: HashMap<HardInput, hard::Line> = HashMap::new();
        let mut mult_in: HashMap<(u32, cmult::Pin), bmult::Line> = HashMap::new();
        let mut mult_ctl_line: HashMap<(u32, u8), bmult::Line> = HashMap::new();
        let mut mult_ctl: HashMap<(u32, cmult::Pin), bmult::Ctl> = HashMap::new();
        let mut pll_in: HashMap<(u32, u32, super::pll::Control), u8> = HashMap::new();
        let mut nets = Vec::new();
        for n in self.nets.values() {
            let mut pips = Vec::new();
            for p in &n.pips {
                match (p.src, p.dst) {
                    (Wire::Node(s), Wire::Node(d)) => {
                        pips.push(
                            Pip::new(dev, s, d)
                                .with_context(|| format!("{}: no edge {s:?} -> {d:?}", n.name))?,
                        );
                        if dev.element_enum(s) == 276 && dev.element_enum(d) == BLOCK_INPUT_MUX {
                            let t = &dev.routing.templates
                                [dev.routing.elements[s.0 as usize].template as usize];
                            core_in.insert(d, t.index as u8);
                        }
                    }
                    (
                        Wire::Le {
                            pin: LePin::Input(_),
                            ..
                        },
                        Wire::Le {
                            pin: LePin::Logical(_),
                            ..
                        },
                    ) => {}
                    // M4K inputs and control inputs: a LAB line of the block
                    // through its source mux; control pins: a control input
                    // or a row clock through their fast mux.
                    (
                        Wire::Node(s),
                        Wire::M4k {
                            x,
                            y,
                            pin: pin @ (M4kPin::In(_) | M4kPin::Ctl(_)),
                        },
                    ) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[s.0 as usize].template as usize];
                        ensure!(
                            t.element_enum == 276,
                            "{}: M4K input from {}",
                            n.name,
                            element_name(t.element_enum)
                        );
                        let line = t.index as u8;
                        match pin {
                            M4kPin::In(i) => {
                                ram_in.entry((x, y)).or_default().insert(i, line);
                            }
                            M4kPin::Ctl(k) => {
                                ram_pia.entry((x, y)).or_default()[k as usize] = Some(line)
                            }
                            M4kPin::Fast(_) => unreachable!(),
                        }
                    }
                    // Multiplier clock and clear from a row clock (their
                    // fast mux) or from a control input fed by a line.
                    (
                        Wire::Node(s),
                        Wire::Mult {
                            y,
                            pin: pin @ (cmult::Pin::Clk | cmult::Pin::Aclr),
                        },
                    ) => {
                        ensure!(
                            dev.element_enum(s) == 560,
                            "{}: multiplier {pin} from {}",
                            n.name,
                            element_name(dev.element_enum(s))
                        );
                        mult_ctl.insert((y, pin), bmult::Ctl::global(dev.index(s) as u32)?);
                    }
                    (Wire::MultCtl { y, k }, Wire::Mult { pin, .. }) => {
                        let line = *mult_ctl_line.get(&(y, k)).with_context(|| {
                            format!("{}: multiplier control input {k} not fed", n.name)
                        })?;
                        mult_ctl.insert((y, pin), bmult::Ctl::Line(line));
                    }
                    (Wire::Node(s), Wire::MultCtl { y, k }) => {
                        ensure!(
                            dev.element_enum(s) == 276,
                            "{}: multiplier control input from {}",
                            n.name,
                            element_name(dev.element_enum(s))
                        );
                        mult_ctl_line.insert((y, k), bmult::Line::new(dev.index(s) as u32)?);
                    }
                    // Multiplier data, sign and enable inputs: a LAB line of
                    // the block through its source mux.
                    (Wire::Node(s), Wire::Mult { y, pin }) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[s.0 as usize].template as usize];
                        ensure!(
                            t.element_enum == 276,
                            "{}: multiplier input from {}",
                            n.name,
                            element_name(t.element_enum)
                        );
                        mult_in.insert((y, pin), bmult::Line::new(t.index as u32)?);
                    }
                    (Wire::Node(s), Wire::PllIn { x, y, input }) => {
                        ensure!(
                            dev.element_enum(s) == 276,
                            "{}: PLL {input:?} from {}",
                            n.name,
                            element_name(dev.element_enum(s))
                        );
                        pll_in.insert((x, y, input), dev.index(s) as u8);
                    }
                    (Wire::Node(s), Wire::Hard(h)) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[s.0 as usize].template as usize];
                        ensure!(
                            t.element_enum == 276,
                            "{}: {h:?} from {}",
                            n.name,
                            element_name(t.element_enum)
                        );
                        hard_in.insert(h, hard::Line::new(t.index as u32)?);
                    }
                    (
                        Wire::M4k {
                            pin: M4kPin::Ctl(k),
                            ..
                        },
                        Wire::M4k {
                            x,
                            y,
                            pin: M4kPin::Fast(f),
                        },
                    ) => {
                        ram_ctl.insert((x, y, f), CtlSrc::Control(k));
                    }
                    (
                        Wire::Node(s),
                        Wire::M4k {
                            x,
                            y,
                            pin: M4kPin::Fast(f),
                        },
                    ) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[s.0 as usize].template as usize];
                        ensure!(
                            t.element_enum == 560,
                            "{}: M4K control from {}",
                            n.name,
                            element_name(t.element_enum)
                        );
                        ram_ctl.insert((x, y, f), CtlSrc::Global(t.index as u8));
                    }
                    (
                        src,
                        dst @ (Wire::Le {
                            pin: LePin::Input(_),
                            ..
                        }
                        | Wire::Ctrl { .. }),
                    ) => {
                        let s = match src {
                            Wire::Node(s) => {
                                let t = &dev.routing.templates
                                    [dev.routing.elements[s.0 as usize].template as usize];
                                ensure!(
                                    t.element_enum == 276,
                                    "{}: LE input from {}",
                                    n.name,
                                    element_name(t.element_enum)
                                );
                                LeSource::Line(t.index as u8)
                            }
                            Wire::LocalLine { k: j, .. } => LeSource::Feedback(j as u8),
                            w => bail!("{}: LE input from {w:?}", n.name),
                        };
                        match dst {
                            Wire::Le {
                                x,
                                y,
                                k,
                                pin: LePin::Input(i),
                            } => le_at(&mut les, x, y, k).inputs[i] = Some(s),
                            Wire::Ctrl { x, y, k } => {
                                pia.insert((x, y, k), s);
                            }
                            _ => unreachable!(),
                        }
                    }
                    (Wire::Ctrl { k: line, .. }, Wire::Le { x, y, k, pin }) => {
                        let line = line as u8;
                        match pin {
                            LePin::Ena => ena.insert((x, y, k), line),
                            LePin::Sclr => sclr.insert((x, y, k), line),
                            LePin::Aclr => aclr.insert((x, y, k), Aclr::Pia(line)).map(|_| 0),
                            p => bail!("{}: control line into LE pin {p:?}", n.name),
                        };
                    }
                    (
                        Wire::Node(s),
                        Wire::Le {
                            x,
                            y,
                            k,
                            pin: LePin::Aclr,
                        },
                    ) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[s.0 as usize].template as usize];
                        aclr.insert((x, y, k), Aclr::Global(t.index as u8));
                    }
                    (Wire::Le { x, y, k, pin }, dst) if matches!(pin, LePin::Comb | LePin::Q) => {
                        let o = match dst {
                            Wire::Node(d) => {
                                let t = &dev.routing.templates
                                    [dev.routing.elements[d.0 as usize].template as usize];
                                ensure!(
                                    t.element_enum == 45,
                                    "{}: LE output into {}",
                                    n.name,
                                    element_name(t.element_enum)
                                );
                                t.index as u32 - 2 * k
                            }
                            Wire::LocalLine { .. } => 2,
                            w => bail!("{}: LE output into {w:?}", n.name),
                        };
                        le_at(&mut les, x, y, k).comb_out[o as usize] = pin == LePin::Comb;
                    }
                    (
                        Wire::Node(s),
                        Wire::Le {
                            x,
                            y,
                            k,
                            pin: LePin::Clk,
                        },
                    ) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[s.0 as usize].template as usize];
                        clk_buf.insert((x, y, k), t.index as u8);
                    }
                    // Counter 2 into the clock control: no select bits
                    // (observed: Quartus sets none).
                    (Wire::Node(_), Wire::PllOutIn { .. }) => {}
                    (
                        Wire::Node(s),
                        Wire::Ioe {
                            x,
                            y,
                            z,
                            pin: IoePin::DataOut,
                        },
                    ) if dev.element_enum(s) == 303 => {
                        pll_data.insert((x, y, z));
                    }
                    (Wire::Node(s), Wire::ClkIn { g }) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[s.0 as usize].template as usize];
                        clk_in.insert(g, (s, t.index as u8));
                    }
                    (
                        Wire::Node(s),
                        Wire::Ioe {
                            x,
                            y,
                            z,
                            pin: pin @ (IoePin::DataOut | IoePin::Oe),
                        },
                    ) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[s.0 as usize].template as usize];
                        ensure!(
                            t.element_enum == 276,
                            "{}: pin {pin:?} from {}",
                            n.name,
                            element_name(t.element_enum)
                        );
                        let m = if pin == IoePin::Oe {
                            &mut oe_line
                        } else {
                            &mut data_line
                        };
                        m.insert((x, y, z), t.index as u32);
                    }
                    (
                        Wire::Ioe {
                            x,
                            y,
                            z,
                            pin: IoePin::CombOut,
                        },
                        Wire::Node(d),
                    ) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[d.0 as usize].template as usize];
                        if t.element_enum == 301 {
                            datain.entry((x, y, z)).or_default()[t.index as usize] = true;
                        }
                    }
                    (
                        Wire::Ioe {
                            x,
                            y,
                            z,
                            pin: IoePin::RegOut,
                        },
                        Wire::Node(d),
                    ) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[d.0 as usize].template as usize];
                        ensure!(
                            t.element_enum == 301,
                            "{}: input register into {}",
                            n.name,
                            element_name(t.element_enum)
                        );
                        reg_datain.entry((x, y, z)).or_default()[t.index as usize] = true;
                    }
                    (
                        Wire::Node(s),
                        Wire::Ioe {
                            x,
                            y,
                            z,
                            pin: pin @ (IoePin::InClk | IoePin::OutClk),
                        },
                    ) => {
                        let t = &dev.routing.templates
                            [dev.routing.elements[s.0 as usize].template as usize];
                        ensure!(
                            t.element_enum == 560,
                            "{}: I/O register clock from {}",
                            n.name,
                            element_name(t.element_enum)
                        );
                        io_clk.insert((x, y, z, pin), t.index as u32);
                    }
                    (s, d) => bail!("{}: unexpected pip {s:?} -> {d:?}", n.name),
                }
            }
            nets.push(RoutedNet {
                name: n.name.clone(),
                pips,
            });
        }

        let perm = perms(&self.nets);
        let mut clkctrls = Vec::new();
        let mut pins = Vec::new();
        let mut m4ks = Vec::new();
        let mut mults = Vec::new();
        let mut hard = Hard::default();
        let mut plls = Vec::new();
        let mut ff_domain: HashMap<(u32, u32, u32), Domain> = HashMap::new();
        let sdata: std::collections::HashSet<(u32, u32, u32)> = self
            .cells
            .values()
            .filter_map(|c| match c.bel {
                Bel::Ff { x, y, k } if c.conns.contains_key("SDATA") => Some((x, y, k)),
                _ => None,
            })
            .collect();
        let sclr_inv: std::collections::HashSet<(u32, u32, u32)> = self
            .cells
            .values()
            .filter_map(|c| match c.bel {
                Bel::Ff { x, y, k } if flag(c, "SCLR_INV") => Some((x, y, k)),
                _ => None,
            })
            .collect();
        for (name, c) in &self.cells {
            match (c.ty.as_str(), c.bel) {
                ("LCELL_COMB", Bel::Lut { x, y, k }) => {
                    let l = le_at(&mut les, x, y, k);
                    l.lut = Some(Lut {
                        mask: physical_mask(
                            lut_mask(c)?,
                            perm.get(&(x, y, k)).copied().unwrap_or_default(),
                        ),
                    });
                    l.carry_in = c.conns.contains_key("CIN");
                }
                ("LCELL_FF", Bel::Ff { x, y, k }) => {
                    let b = *clk_buf
                        .get(&(x, y, k))
                        .with_context(|| format!("{name}: clock not routed"))?;
                    let e = ena.get(&(x, y, k)).map(|&line| Ctl {
                        line,
                        inverted: flag(c, "ENA_INV"),
                    });
                    ff_domain.insert((x, y, k), (b, flag(c, "CLK_INV"), e));
                }
                ("CY2_M4K", Bel::M4k { x, y }) => {
                    let shape = M4kShape::of(c).with_context(|| name.clone())?;
                    let ctl = |pin: &str| -> Option<CtlSrc> {
                        let f = m4k::CONTROL_PINS.iter().position(|p| p.0 == pin)? as u8;
                        ram_ctl.get(&(x, y, f)).copied()
                    };
                    let controls = Controls {
                        pia: ram_pia.get(&(x, y)).copied().unwrap_or_default(),
                        clock: [ctl("CLK0"), ctl("CLK1")],
                        enable: [ctl("ENA0"), ctl("ENA1")],
                        clear: [ctl("CLR0"), ctl("CLR1")],
                        write_enable: ctl("WE"),
                        b_write_enable: ctl("RE"),
                        ..Default::default()
                    };
                    ensure!(controls.clock[0].is_some(), "{name}: clock not routed");
                    for (pin, routed, wanted) in [
                        ("WE", controls.write_enable.is_some(), shape.writes[0]),
                        ("RE", controls.b_write_enable.is_some(), shape.writes[1]),
                    ] {
                        ensure!(routed == wanted, "{name}: {pin} not routed");
                    }
                    for k in 0..2 {
                        let pin = format!("ENA{k}");
                        ensure!(
                            controls.enable[k].is_some() == c.conns.contains_key(&pin),
                            "{name}: {pin} not routed"
                        );
                    }
                    m4ks.push(M4k {
                        x,
                        y,
                        mode: shape.mode().with_context(|| name.clone())?,
                        controls,
                        inputs: ram_in.remove(&(x, y)).unwrap_or_default(),
                        content: shape.content,
                    });
                }
                ("CY2_MULT", Bel::Mult { y }) => {
                    mults.push(mult_of(c, y, &mult_in, &mult_ctl).with_context(|| name.clone())?);
                }
                ("CY2_PLL", Bel::Pll { x, y }) => {
                    use super::pll::{Control, Pll, Site};
                    let site = Site::ALL
                        .into_iter()
                        .find(|s| s.xy() == (x, y))
                        .with_context(|| format!("{name}: no PLL at X{x} Y{y}"))?;
                    let used = [0, 1, 2].map(|k| c.conns.contains_key(&format!("CLK{k}")));
                    let solved = crate::pll::Request::from_params(&c.params, used)
                        .and_then(|r| r.solve())
                        .with_context(|| name.clone())?;
                    // The clock pin: the CLK_BUFFER its INCLK net starts at,
                    // by its index (z) along the side.
                    let net = c
                        .conns
                        .get("INCLK")
                        .with_context(|| format!("{name}: INCLK unconnected"))?;
                    let inclk_pin = self.nets[net]
                        .pips
                        .iter()
                        .find_map(|p| match p.src {
                            Wire::Node(s) if dev.element_enum(s) == 289 => Some(
                                dev.routing.templates
                                    [dev.routing.elements[s.0 as usize].template as usize]
                                    .z as u8,
                            ),
                            _ => None,
                        })
                        .with_context(|| format!("{name}: INCLK not routed from a clock pin"))?;
                    let control = |c: Control| pll_in.get(&(x, y, c)).copied();
                    plls.push(Pll {
                        site,
                        config: solved.config,
                        inclk_pin,
                        areset: control(Control::Areset),
                        pfdena: control(Control::Pfdena),
                    });
                }
                ("CY2_JTAG", Bel::Jtag) => {
                    hard.jtag = Some(Jtag {
                        tdouser: hard_in.get(&HardInput::Tdouser).copied(),
                    })
                }
                ("CY2_ASMI", Bel::Asmi) => {
                    let need = |h: HardInput| {
                        hard_in
                            .get(&h)
                            .copied()
                            .with_context(|| format!("{name}: {h:?} not routed"))
                    };
                    hard.asmi = Some(Asmi {
                        dclk: need(HardInput::Dclk)?,
                        sce: need(HardInput::Sce)?,
                        sdo: need(HardInput::Sdo)?,
                    });
                }
                ("PLLOUT", Bel::PllOut { .. }) => {}
                ("CLKCTRL", Bel::ClkCtrl { g }) => {
                    let (node, input) = *clk_in
                        .get(&g)
                        .with_context(|| format!("{name}: input not routed"))?;
                    // The BLOCK_INPUT_MUX feeding a core input, and the line
                    // that feeds it.
                    let core_line = core_in
                        .iter()
                        .find_map(|(&m, &line)| dev.edge(m, node).is_some().then_some(line));
                    ensure!(
                        (input == CORE_INPUT) == core_line.is_some(),
                        "{name}: clock control input {input} without its core path, or the other way round"
                    );
                    clkctrls.push(ClkCtrl {
                        g: g as u8,
                        input,
                        core_line,
                        enable: None,
                        // The Quartus export writes `ena_register_mode =
                        // "none"` (cyclone2-re's quartus/export.rs).
                        enable_registered: false,
                    });
                }
                ("CY2_IBUF" | "CY2_OBUF" | "CY2_IOBUF", Bel::Ioe { x, y, z }) => {
                    let p = pin_at(io, x, y, z)?;
                    let settings = PinSettings::from_params(&c.params)
                        .with_context(|| format!("{name}: I/O settings"))?;
                    let (standard, drive) = settings.standard.ioe();
                    // An I/O register, clocked from the row-clock buffer the
                    // routing reached its clock pin through.
                    let reg = |param: &str, clk: IoePin| -> Result<Option<IoReg>> {
                        let Some(power_up) = register_of(&c.params, param)? else {
                            return Ok(None);
                        };
                        let clock = *io_clk
                            .get(&(x, y, z, clk))
                            .with_context(|| format!("{name}: {param} clock not routed"))?;
                        Ok(Some(IoReg::plain(clock, power_up.high())))
                    };
                    let in_reg = reg(register_param::INPUT, IoePin::InClk)?;
                    let input = Input {
                        comb: datain.get(&(x, y, z)).copied().unwrap_or_default(),
                        reg: in_reg.map(|reg| InReg {
                            reg,
                            paths: reg_datain.get(&(x, y, z)).copied().unwrap_or_default(),
                        }),
                    };
                    let data = || -> Result<OutData> {
                        Ok(
                            match (c.params.get("OUT_CONST"), data_line.get(&(x, y, z))) {
                                (None, None) if pll_data.contains(&(x, y, z)) => OutData::PllClock,
                                (Some(v), _) => OutData::Const(v.ends_with('1')),
                                (None, Some(&line)) => OutData::Line {
                                    line,
                                    inverted: flag(c, "DATA_INVERT"),
                                    reg: reg(register_param::OUTPUT, IoePin::OutClk)?,
                                },
                                _ => bail!("{name}: output data neither routed nor constant"),
                            },
                        )
                    };
                    let output = || -> Result<Output> {
                        let enable = match (
                            c.ty.as_str(),
                            c.params.get("OE_CONST"),
                            oe_line.get(&(x, y, z)),
                        ) {
                            ("CY2_OBUF", _, _) => OutputEnable::Always,
                            (_, Some(v), _) if v.ends_with('1') => OutputEnable::Always,
                            // Never driven: what Quartus makes of such an
                            // inout, an open drain releasing the pad.
                            (_, Some(_), _) => {
                                return Ok(Output {
                                    drive,
                                    data: OutData::Const(true),
                                    enable: OutputEnable::OpenDrain,
                                })
                            }
                            (_, None, Some(&line)) => OutputEnable::Line {
                                line,
                                inverted: flag(c, "OE_INVERT"),
                                reg: reg(register_param::OE, IoePin::OutClk)?,
                            },
                            (_, None, None) => {
                                bail!("{name}: output enable neither routed nor constant")
                            }
                        };
                        Ok(Output {
                            drive,
                            data: data()?,
                            enable,
                        })
                    };
                    // As in Quartus, the port's direction decides: an
                    // `output` keeps its input buffer off, every bit of an
                    // `inout` is a bidir (whether or not the core reads it).
                    let port = c
                        .attrs
                        .get("PORT")
                        .with_context(|| format!("{name}: no PORT"))?;
                    let dir = match (c.ty.as_str(), self.port_dir(port)?) {
                        ("CY2_IBUF", PortDir::In) => Dir::In(input),
                        ("CY2_OBUF" | "CY2_IOBUF", PortDir::Out) => Dir::Out(output()?),
                        ("CY2_OBUF" | "CY2_IOBUF", PortDir::InOut) => Dir::Bidir(input, output()?),
                        (t, d) => bail!("{name}: {t} on an {d:?} port"),
                    };
                    ensure!(
                        dir.output().is_none() || settings.standard.drives(),
                        "{name}: a {} pin can't drive",
                        settings.standard.quartus_name()
                    );
                    pins.push(PinUse {
                        pin: p.name.clone(),
                        port: port.clone(),
                        standard,
                        pull_up: settings.pull == Pull::Up,
                        bus_hold: settings.pull == Pull::BusHold,
                        dir,
                    });
                }
                (t, b) => bail!("{name}: cell type {t} on {b:?}"),
            }
        }
        let mut labs: BTreeMap<(u32, u32), LabConfig> = BTreeMap::new();
        let mut by_lab: BTreeMap<(u32, u32), Vec<Domain>> = BTreeMap::new();
        for (&(x, y, _), d) in &ff_domain {
            by_lab.entry((x, y)).or_default().push(*d);
        }
        for (&xy, v) in &by_lab {
            let slots = clock_slots(v).with_context(|| format!("LAB {xy:?}"))?;
            let lab = labs.entry(xy).or_default();
            for (j, d) in slots.iter().enumerate() {
                if let Some(d) = d {
                    lab.clocks[j] = Some(GlobalClock {
                        buf: d.0,
                        inverted: d.1,
                    });
                    lab.enables[j] = d.2;
                }
            }
            for (&(x, y, k), d) in ff_domain.iter().filter(|((x, y, _), _)| (*x, *y) == xy) {
                let clock = slots.iter().position(|s| *s == Some(*d)).unwrap() as u8;
                let a = aclr.get(&(x, y, k)).copied();
                let sc = sclr.get(&(x, y, k)).map(|&line| Ctl {
                    line,
                    inverted: sclr_inv.contains(&(x, y, k)),
                });
                if let Some(a) = a {
                    ensure!(lab.aclr.is_none_or(|o| o == a), "LAB {xy:?}: two clears");
                    lab.aclr = Some(a);
                }
                if let Some(c) = sc {
                    ensure!(
                        lab.sync.sclr.is_none_or(|o| o == c),
                        "LAB {xy:?}: two sync clears"
                    );
                    lab.sync.sclr = Some(c);
                }
                // A cleared register takes LAB clear line 0 (the model's
                // only one).
                // Registers on the sync load path load SDATA (input C) every
                // cycle through the LAB's sload, tied high.
                let sload = sdata.contains(&(x, y, k));
                if sload {
                    lab.sync.sload = Some(Sload::Const);
                }
                le_at(&mut les, x, y, k).reg = Some(Reg {
                    clock,
                    aclr: a.map(|_| 0),
                    sync: sc.is_some() || sload,
                });
            }
        }
        for l in les.values() {
            labs.entry((l.x, l.y)).or_default();
        }
        for (&(x, y, k), &src) in &pia {
            labs.entry((x, y)).or_default().pia[k as usize] = Some(src);
        }
        Ok(PhysDesign {
            logic: Logic {
                les: les.into_values().collect(),
                clkctrls,
                labs,
            },
            pins,
            unused_pins: match self.attrs.get(assignment::UNUSED_PINS) {
                None => crate::pin::UnusedPins::default().ioe(),
                Some(v) => crate::pin::UnusedPins::from_quartus_name(v)
                    .with_context(|| format!("unused-pin mode {v:?}"))?
                    .ioe(),
            },
            // The DE2's banks, all at 3.3 V (`crate::pin`).
            vccio: Vccio::Board(Standard::Lvttl33),
            m4ks,
            mults,
            hard,
            plls,
            nets,
        })
    }
}

/// A placed `CY2_MULT`: its shape and `DATA_INVERT` mask (the packer's,
/// `pack_mult`) and the lines routed into its pins.
/// A placed `CY2_MULT`'s shape, from the packer's parameters (32-bit
/// binary strings, as yosys writes integers).
pub fn mult_shape(c: &Cell) -> Result<cmult::Shape> {
    let int = |k: &str| mult_param(c, k);
    cmult::Shape::new(
        int("A_WIDTH")? as usize,
        int("B_WIDTH")? as usize,
        [int("A_SIGNED")? != 0, int("B_SIGNED")? != 0],
    )
}

/// The packer's `DATA_INVERT` mask over the 36 data inputs (A's 0-17, B's
/// 18-35): the pins the block inverts, driven (a folded inverter) or not
/// (a 0).
pub fn mult_inverted(c: &Cell) -> Result<u64> {
    mult_param(c, "DATA_INVERT")
}

fn mult_param(c: &Cell, k: &str) -> Result<u64> {
    let v = c.params.get(k).with_context(|| format!("{k} missing"))?;
    u64::from_str_radix(v.trim_end(), 2).with_context(|| format!("{k} = {v}"))
}

/// The register flags of a placed `CY2_MULT`.
pub fn mult_regs(c: &Cell) -> Result<cmult::Regs> {
    let mut flags = [false; 3];
    for (f, k) in flags.iter_mut().zip(cmult::Regs::PARAMS) {
        *f = c.params.get(k).map_or(Ok(0), |_| mult_param(c, k))? != 0;
    }
    Ok(cmult::Regs::from_flags(flags))
}

fn mult_of(
    c: &Cell,
    y: u32,
    lines: &HashMap<(u32, cmult::Pin), bmult::Line>,
    ctl: &HashMap<(u32, cmult::Pin), bmult::Ctl>,
) -> Result<Mult> {
    let shape = mult_shape(c)?;
    let regs = mult_regs(c)?;
    let inverted = mult_inverted(c)?;
    let mut data = BTreeMap::new();
    // Inverted pins no line drives read 0; the lines routed below
    // replace the entry of a pin that has one.
    for k in 0..bmult::DATA as u32 {
        if inverted >> k & 1 == 1 {
            data.insert(DataPin::new(k)?, DataIn::Zero);
        }
    }
    let line_in = |k: u32, line: bmult::Line| DataIn::Line {
        line,
        inverted: inverted >> k & 1 == 1,
    };
    let mut sign = [Sign::Signed; 2];
    let mut enable = None;
    for (&(at, pin), &line) in lines.iter().filter(|((at, _), _)| *at == y) {
        debug_assert_eq!(at, y);
        match pin {
            cmult::Pin::Ena => enable = Some(line),
            cmult::Pin::Clk | cmult::Pin::Aclr => unreachable!("fast-muxed pins aren't lines"),
            cmult::Pin::A(k) => {
                data.insert(DataPin::new(k as u32)?, line_in(k as u32, line));
            }
            cmult::Pin::B(k) => {
                let k = (cmult::MAX_WIDTH + k as usize) as u32;
                data.insert(DataPin::new(k)?, line_in(k, line));
            }
            cmult::Pin::Sign(op) => sign[op as usize] = Sign::Line(line),
            cmult::Pin::Out(_) => unreachable!("outputs aren't mux inputs"),
        }
    }
    for op in [Operand::A, Operand::B] {
        ensure!(
            shape.signed[op as usize] || sign[op as usize] != Sign::Signed,
            "unsigned operand {op:?} without a routed sign"
        );
    }
    // The block's registers: the operand's rows (row 0 in nine-bit mode,
    // both in 18-bit mode) and the product's.
    let registers = if regs.any() {
        let rows: [bool; 2] = [true, !shape.nine_bit()];
        let clock = *ctl
            .get(&(y, cmult::Pin::Clk))
            .context("registered multiplier without a routed clock")?;
        Some(super::mult::Registers {
            clock,
            enable,
            clear: ctl.get(&(y, cmult::Pin::Aclr)).copied(),
            input: rows.map(|r| [r && regs.a, r && regs.b]),
            sign: [false; 2],
            output: rows.map(|r| r && regs.p),
        })
    } else {
        None
    };
    Ok(Mult {
        y,
        nine_bit: shape.nine_bit(),
        data,
        sign,
        registers,
    })
}

/// `name[3]` -> ("name", Some(3)).
pub fn split_port(p: &str) -> (&str, Option<u32>) {
    if let Some((b, i)) = p.strip_suffix(']').and_then(|s| s.split_once('[')) {
        if let Ok(i) = i.parse() {
            return (b, Some(i));
        }
    }
    (p, None)
}
