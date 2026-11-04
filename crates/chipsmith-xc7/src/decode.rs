//! A bitstream back to a netlist, to check the back end against the RTL:
//! prjxray's `bit2fasm` turns a `.bit` into FASM, and this reads that FASM
//! against the routing graph into 7-series primitives (LUT6/LUT5, CARRY4,
//! FDRE/FDSE/FDCE/FDPE, RAM32M, pin buffers, BUFG) that a simulator can run
//! beside the RTL.
//!
//! It reads the bits the way the hardware uses them, independently of how
//! chipsmith's writer (`crate::fasm`) chose them: every site input is traced
//! back through the enabled pips (and the always-on ones, which a bitstream
//! has no bits for) to a site output, a constant wire, or nothing (read as
//! 1); every used slice half becomes its four LUTs, registers and its carry
//! chain whatever is used of them. It understands what both chipsmith and
//! openXC7's nextpnr write, so it can be checked on nextpnr's bitstreams,
//! which have run on the board, before it judges chipsmith's.
//!
//! Block RAM is read per RAMB18 half: a RAMB36E1 is its two halves with
//! the data bits interleaved (its DIADI 2i is the lower half's DIADI i, 2i+1
//! the upper's, the same tile wires), so each half in use becomes a
//! RAMB18E1 with the pins and bits of its own (`bram_half`).
//!
//! Not decoded: BSCANs beyond their ports (each output becomes an input
//! port `__bscan_<PORT>`, TDO an output port `__bscan_TDO`, as the synthesis
//! tests cut them), the PS7, and slices whose LUTs are anything but logic or
//! a whole RAM32M.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Context, Result};
use chipsmith_netlist::{Design, Dir, Module, SigBit, State, Wire};
use chipsmith_route::chipdb::{ChipDb, NodeId};

use crate::synth::cells::{self, Carry4, Fd, Ram32m, Reset};
use crate::xray::pseudo_pip_features;

const LETTERS: [char; 4] = ['A', 'B', 'C', 'D'];

/// One FASM feature below its tile: the dotted name, and a vector's value
/// (bits from index `lo`).
struct Feature {
    name: String,
    /// Bits from index `lo`, the lowest first.
    value: Option<(u32, Vec<bool>)>,
}

fn parse(fasm: &str) -> Result<BTreeMap<String, Vec<Feature>>> {
    let mut out: BTreeMap<String, Vec<Feature>> = BTreeMap::new();
    for line in fasm.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (name, value) = match line.split_once('=') {
            Some((n, v)) => (n.trim(), Some(v.trim())),
            None => (line, None),
        };
        let (tile, rest) = name.split_once('.').with_context(|| format!("FASM line {line}"))?;
        let (rest, value) = match value {
            Some(v) => {
                let (base, range) = rest.split_once('[').context("a vector without a range")?;
                let lo: u32 = range
                    .trim_end_matches(']')
                    .rsplit(':')
                    .next()
                    .context("range")?
                    .parse()?;
                let bits = v.split_once("'b").context("a vector value")?.1;
                anyhow::ensure!(bits.chars().all(|c| c == '0' || c == '1'), "FASM value {v}");
                (base.to_string(), Some((lo, bits.chars().rev().map(|c| c == '1').collect())))
            }
            // A single set bit of a vector: `ALUT.INIT[17]`.
            None => match rest.strip_suffix(']').and_then(|r| r.rsplit_once('[')) {
                Some((base, bit)) if bit.chars().all(|c| c.is_ascii_digit()) => {
                    (base.to_string(), Some((bit.parse()?, vec![true])))
                }
                _ => (rest.to_string(), None),
            },
        };
        out.entry(tile.to_string()).or_default().push(Feature { name: rest, value });
    }
    Ok(out)
}

struct Decoder<'a> {
    db: &'a ChipDb,
    m: Module,
    /// Each node's driver, from the enabled pips.
    driver: HashMap<NodeId, NodeId>,
    /// Nets on site output pins.
    source: HashMap<NodeId, SigBit>,
    constant: HashMap<NodeId, bool>,
    names: HashMap<u32, HashMap<String, u32>>,
    /// Each driver's pip, by name, for `CHIPSMITH_DECODE_TRACE`.
    why: HashMap<NodeId, String>,
    /// Every site input the netlist reads (`resolve`'s starting nodes).
    read: std::cell::RefCell<HashSet<NodeId>>,
}

impl Decoder<'_> {
    fn wire(&mut self, tile: u32, name: &str) -> Option<u32> {
        let ty = self.db.tile_type_index(tile);
        let tt = self.db.tile_type(tile);
        self.names
            .entry(ty)
            .or_insert_with(|| (0..tt.wires.len() as u32).map(|w| (tt.wires.get(w).to_string(), w)).collect())
            .get(name)
            .copied()
    }

    fn pip(&mut self, tile: u32, dst: &str, src: &str) -> bool {
        let (Some(d), Some(s)) = (self.wire(tile, dst), self.wire(tile, src)) else {
            return false;
        };
        let (dn, sn) = (self.db.node(tile, d), self.db.node(tile, s));
        self.driver.insert(dn, sn);
        self.why.insert(dn, format!("{}.{dst}.{src}", self.db.tile_names[tile as usize]));
        true
    }

    fn pin(&self, site: &str, pin: &str) -> Result<NodeId> {
        crate::phys::pin_node(self.db, site, pin)
    }

    fn net(&mut self) -> SigBit {
        SigBit::Net(self.m.new_net())
    }

    /// What drives a site input: a site output's net, a constant, or (no
    /// driver at all) 1.
    fn resolve(&self, mut n: NodeId) -> SigBit {
        self.read.borrow_mut().insert(n);
        for _ in 0..10_000 {
            if let Some(&s) = self.source.get(&n) {
                return s;
            }
            if let Some(&c) = self.constant.get(&n) {
                return SigBit::Const(State::from(c));
            }
            match self.driver.get(&n) {
                Some(&d) => n = d,
                None => return SigBit::ONE,
            }
        }
        SigBit::X
    }

    fn resolve_pin(&self, site: &str, pin: &str) -> Result<SigBit> {
        Ok(self.resolve(self.pin(site, pin)?))
    }
}

/// The site of `tile` whose relative name starts with `prefix`.
fn site_in(db: &ChipDb, tile: u32, prefix: &str) -> Option<String> {
    let mut sites: Vec<(&String, &chipsmith_route::chipdb::Site)> =
        db.sites.iter().filter(|(_, s)| s.tile == tile && s.relative.starts_with(prefix)).collect();
    sites.sort_by(|a, b| a.0.cmp(b.0));
    sites.first().map(|(n, _)| n.to_string())
}

/// One slice half's decoded configuration.
#[derive(Default)]
struct Half {
    init: [u64; 4],
    ram: [bool; 4],
    /// `<L>LUT.DI1MUX.<L>I`: the 6LUT's data from its own `<L>I` pin, not
    /// from the next LUT's (A from B's, B from C's, C from D's).
    di1_own: [bool; 4],
    /// `WEMUX.CE`: write enable from the CE pin.
    we_ce: bool,
    outmux: [Option<String>; 4],
    ffmux: [Option<String>; 4],
    zini: [bool; 4],
    zrst: [bool; 4],
    /// The 5FFs': ZINI, ZRST, and `<L>5FFMUX` (`IN_A` O5, `IN_B` the
    /// bypass).
    zini5: [bool; 4],
    zrst5: [bool; 4],
    ff5mux: [Option<String>; 4],
    cy0_o5: [bool; 4],
    precyinit: Option<String>,
    clkinv: bool,
    sync: bool,
    srused: bool,
    ceused: bool,
}

/// Decodes `fasm` (bit2fasm's) into a netlist whose ports are the pins
/// `pins` names (port bit -> package pin); `family` is prjxray-db's
/// directory of the part's family, for its always-on pips.
pub fn decode(
    db: &ChipDb,
    family: &std::path::Path,
    fasm: &str,
    pins: &BTreeMap<String, String>,
) -> Result<Design> {
    Ok(decode_routed(db, family, fasm, pins)?.design)
}

/// A decoded bitstream with what it was traced through, for writing it
/// out placed and routed (`crate::vivado`).
pub struct Routed {
    pub design: Design,
    /// Each node's driver, through the enabled and the always-on pips.
    pub driver: HashMap<NodeId, NodeId>,
    /// The net on each site output pin.
    pub source: HashMap<NodeId, SigBit>,
    /// The site inputs the netlist reads.
    pub read: HashSet<NodeId>,
    /// Each slice half's site, by the key its cells are named after
    /// (`<tile>.<half>`).
    pub slice_sites: BTreeMap<String, String>,
}

/// [`decode`], keeping the routing.
pub fn decode_routed(
    db: &ChipDb,
    family: &std::path::Path,
    fasm: &str,
    pins: &BTreeMap<String, String>,
) -> Result<Routed> {
    let tiles = parse(fasm)?;
    let tile_of: HashMap<&str, u32> = db
        .tile_names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i as u32))
        .collect();
    let mut d = Decoder {
        db,
        m: Module::new("top"),
        driver: HashMap::new(),
        source: HashMap::new(),
        constant: HashMap::new(),
        names: HashMap::new(),
        why: HashMap::new(),
        read: Default::default(),
    };
    for tile in 0..db.width * db.height {
        for (w, v) in [
            ("GND_WIRE", false),
            ("VCC_WIRE", true),
            // A DSP tile's own ties.
            ("DSP_GND_L", false),
            ("DSP_GND_R", false),
            ("DSP_VCC_L", true),
            ("DSP_VCC_R", true),
        ] {
            if let Some(i) = d.wire(tile, w) {
                d.constant.insert(db.node(tile, i), v);
            }
        }
    }

    // Always-on pips: prjxray-db's `always` ppips, which a bitstream has
    // no bits for.
    let mut always: Vec<(NodeId, NodeId)> = Vec::new();
    let mut ppips: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for tile in 0..db.width * db.height {
        let tt = db.tile_type(tile);
        let base = tt.name.strip_suffix("_SING").unwrap_or(&tt.name).to_string();
        let list = ppips.entry(base.clone()).or_insert_with(|| {
            let path = family.join(format!("ppips_{}.db", base.to_lowercase()));
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .filter(|l| l.ends_with(" always"))
                .filter_map(|l| {
                    let key = l.split_whitespace().next()?;
                    let mut p = key.splitn(3, '.');
                    p.next()?;
                    Some((p.next()?.to_string(), p.next()?.to_string()))
                })
                .collect()
        });
        let list = list.clone();
        for (dst, src) in list {
            let undo = |w: &str| w.to_string();
            let (Some(dw), Some(sw)) = (d.wire(tile, &undo(&dst)), d.wire(tile, &undo(&src))) else {
                continue;
            };
            always.push((db.node(tile, dw), db.node(tile, sw)));
        }
    }

    // Pips with bits.
    let mut halves: BTreeMap<String, Half> = BTreeMap::new();
    let mut iobs: Vec<(String, bool, bool)> = Vec::new();
    let mut bufgs: Vec<String> = Vec::new();
    let mut bscans: Vec<String> = Vec::new();
    let mut rams: BTreeMap<(u32, usize), RamHalf> = BTreeMap::new();
    let mut dsps: BTreeMap<(u32, usize), DspSite> = BTreeMap::new();
    for (tname, feats) in &tiles {
        let Some(&tile) = tile_of.get(tname.as_str()) else {
            continue;
        };
        let ty = db.tile_type(tile).name.clone();
        let sing = tname.contains("_SING_");
        let names: HashSet<&str> = feats.iter().map(|f| f.name.as_str()).collect();
        // Pseudo pips, enabled by their site bits.
        let tt = db.tile_type(tile);
        let pseudo: Vec<(String, String)> = tt
            .pips
            .iter()
            .map(|&(s, dw)| (tt.wires.get(dw).to_string(), tt.wires.get(s).to_string()))
            .filter(|(dst, src)| pseudo_pip_features(&ty, dst, src).is_some())
            .collect();
        for (dst, src) in pseudo {
            let fs = pseudo_pip_features(&ty, &dst, &src).expect("filtered");
            let on = !fs.is_empty()
                && fs.iter().all(|f| {
                    names.contains(f.as_str()) || (sing && names.contains(f.replacen("Y0", "Y1", 1).as_str()))
                });
            if on {
                d.pip(tile, &dst, &src);
            }
        }
        for f in feats {
            let parts: Vec<&str> = f.name.split('.').collect();
            if parts.len() == 2 && f.value.is_none() && d.pip(tile, parts[0], parts[1]) {
                continue;
            }
            if sing && parts.len() == 2 && f.value.is_none() {
                // nextpnr's renaming in a top single-IOB tile.
                let (dst, src) = (parts[0].replacen("_1", "_0", 1), parts[1].replacen("_1", "_0", 1));
                let dst = dst.replacen("OLOGIC1", "OLOGIC0", 1);
                if d.pip(tile, &dst, &src) {
                    continue;
                }
            }
            let half = parts[0];
            if half.starts_with("SLICE") {
                let key = format!("{tname}.{half}");
                let h = halves.entry(key).or_default();
                let letter = |s: &str| LETTERS.iter().position(|&c| s.starts_with(c));
                match (&parts[1..], f.value.as_ref()) {
                    ([lut, "INIT"], Some((lo, v))) if lut.ends_with("LUT") => {
                        if let Some(k) = letter(lut) {
                            for (i, &b) in v.iter().enumerate() {
                                h.init[k] |= u64::from(b) << (*lo as usize + i);
                            }
                        }
                    }
                    ([lut, "RAM"], None) if lut.ends_with("LUT") => {
                        if let Some(k) = letter(lut) {
                            h.ram[k] = true;
                        }
                    }
                    ([lut, "DI1MUX", input], None) if lut.ends_with("LUT") && input.len() == 2 => {
                        if let Some(k) = letter(lut) {
                            h.di1_own[k] = true;
                        }
                    }
                    (["WEMUX", "CE"], None) => h.we_ce = true,
                    ([m, input], None) if m.ends_with("OUTMUX") => {
                        if let Some(k) = letter(m) {
                            h.outmux[k] = Some(input.to_string());
                        }
                    }
                    ([m, input], None) if m.ends_with("FFMUX") && m.len() == 6 => {
                        if let Some(k) = letter(m) {
                            h.ffmux[k] = Some(input.to_string());
                        }
                    }
                    ([ff, "ZINI"], None) if ff.len() == 4 => {
                        if let Some(k) = letter(ff) {
                            h.zini5[k] = true;
                        }
                    }
                    ([ff, "ZRST"], None) if ff.len() == 4 => {
                        if let Some(k) = letter(ff) {
                            h.zrst5[k] = true;
                        }
                    }
                    ([m, input], None) if m.len() == 7 && m.ends_with("5FFMUX") => {
                        if let Some(k) = letter(m) {
                            h.ff5mux[k] = Some(input.to_string());
                        }
                    }
                    ([ff, "ZINI"], None) => {
                        if let Some(k) = letter(ff) {
                            h.zini[k] = true;
                        }
                    }
                    ([ff, "ZRST"], None) => {
                        if let Some(k) = letter(ff) {
                            h.zrst[k] = true;
                        }
                    }
                    (["CARRY4", cy], None) => {
                        if let Some(k) = letter(cy) {
                            h.cy0_o5[k] = true;
                        }
                    }
                    (["PRECYINIT", input], None) => h.precyinit = Some(input.to_string()),
                    (["CLKINV"], None) => h.clkinv = true,
                    (["FFSYNC"], None) => h.sync = true,
                    (["SRUSEDMUX"], None) => h.srused = true,
                    (["CEUSEDMUX"], None) => h.ceused = true,
                    _ => {}
                }
            } else if let Some(y) = half.strip_prefix("IOB_Y") {
                let site = if sing {
                    site_in(db, tile, "IOB")
                } else {
                    let y: u32 = y.parse()?;
                    site_in(db, tile, &format!("IOB_X0Y{}", 1 - y))
                };
                let site = site.with_context(|| format!("{tname}.{half}: no site"))?;
                // bit2fasm also names the zero-encoded defaults of an
                // unused IOB (`SLEW.FAST`, `PULLTYPE.PULLDOWN`): only an
                // input or a drive strength marks one in use.
                let input = f.name.ends_with(".IN") || f.name.contains(".IN_ONLY");
                let output = f.name.contains(".DRIVE.");
                if !input && !output {
                    continue;
                }
                match iobs.iter_mut().find(|(s, _, _)| *s == site) {
                    Some(e) => {
                        e.1 |= input;
                        e.2 |= output;
                    }
                    None => iobs.push((site, input, output)),
                }
            } else if half == "DSP48" && parts.len() >= 3 {
                let k: usize = parts[1].strip_prefix("DSP_").context("a DSP site")?.parse()?;
                dsps.entry((tile, k)).or_default().read(&parts[2..], f.value.as_ref());
            } else if let Some(y) = half.strip_prefix("RAMB18_Y") {
                let h: usize = y.parse()?;
                let b = rams.entry((tile, h)).or_default();
                b.read(&parts[1..], f.value.as_ref());
            } else if half == "BUFGCTRL" && parts.get(2) == Some(&"IN_USE") {
                let site = db
                    .sites
                    .iter()
                    .find(|(_, s)| s.tile == tile && s.relative == parts[1])
                    .map(|(n, _)| n.clone())
                    .context("BUFGCTRL site")?;
                bufgs.push(site);
            } else if half == "BSCAN" {
                if let Some(n) = parts[1].strip_prefix("JTAG_CHAIN_") {
                    let n: u32 = n.parse()?;
                    bscans.push(format!("BSCAN_X0Y{}", n - 1));
                }
            }
        }
    }
    // Single-IOB I/O tiles: their IMUX into the OLOGIC has no bits
    // (nextpnr writes none there either), and bit2fasm reads none of their
    // other bits at all. Every pip into a wire with only one way in counts
    // as on: a pin's way through the tile (IBUF to ILOGIC to the
    // interconnect, OLOGIC to the pad) is that way. This tile's pips are
    // what the check can't see.
    for tile in 0..db.width * db.height {
        let tt = db.tile_type(tile);
        if !tt.name.contains("IOI3_SING") {
            continue;
        }
        let mut into: HashMap<u32, Vec<u32>> = HashMap::new();
        for &(s, dw) in &tt.pips {
            into.entry(dw).or_default().push(s);
        }
        for &(s, dw) in &tt.pips {
            let (src, dst) = (tt.wires.get(s), tt.wires.get(dw));
            let imux = (src.contains("IMUX") || src.contains("CTRL0")) && !dst.contains("CLK");
            if imux || into[&dw].len() == 1 {
                let dn = db.node(tile, dw);
                if let std::collections::hash_map::Entry::Vacant(e) = d.driver.entry(dn) {
                    e.insert(db.node(tile, s));
                    d.why.insert(dn, "single-IOB tile".into());
                }
            }
        }
    }
    for (dn, sn) in always {
        if let std::collections::hash_map::Entry::Vacant(e) = d.driver.entry(dn) {
            e.insert(sn);
            d.why.insert(dn, "always".into());
        }
    }

    // Site outputs first, so inputs resolve whatever the order.
    struct Nets {
        o6: [SigBit; 4],
        o5: [SigBit; 4],
        xor: [SigBit; 4],
        cy: [SigBit; 4],
        q: [SigBit; 4],
        q5: [SigBit; 4],
        mux: [SigBit; 4],
    }
    let mut slice_nets: BTreeMap<String, (String, Nets)> = BTreeMap::new();
    for (key, h) in &halves {
        let (tname, half) = key.split_once('.').expect("keyed so");
        let tile = tile_of[tname];
        let prefix = if half.ends_with("X1") { "SLICE_X1" } else { "SLICE_X0" };
        let site = site_in(db, tile, prefix).with_context(|| format!("{key}: no site"))?;
        let new4 = |d: &mut Decoder| -> [SigBit; 4] { std::array::from_fn(|_| d.net()) };
        let n = Nets {
            o6: new4(&mut d),
            o5: new4(&mut d),
            xor: new4(&mut d),
            cy: new4(&mut d),
            q: new4(&mut d),
            q5: new4(&mut d),
            mux: new4(&mut d),
        };
        for k in 0..4 {
            let l = LETTERS[k];
            d.source.insert(d.pin(&site, &l.to_string())?, n.o6[k]);
            d.source.insert(d.pin(&site, &format!("{l}Q"))?, n.q[k]);
            let mux = match h.outmux[k].as_deref() {
                Some("O6") => n.o6[k],
                Some("O5") => n.o5[k],
                Some("XOR") => n.xor[k],
                Some("CY") => n.cy[k],
                Some(m) if m.ends_with("5Q") => n.q5[k],
                _ => n.mux[k],
            };
            d.source.insert(d.pin(&site, &format!("{l}MUX"))?, mux);
        }
        if let Ok(cout) = d.pin(&site, "COUT") {
            d.source.insert(cout, n.cy[3]);
        }
        slice_nets.insert(key.clone(), (site, n));
    }
    rams.retain(|_, r| r.in_use);
    let mut ram_out: BTreeMap<(u32, usize), [Vec<SigBit>; 4]> = BTreeMap::new();
    for &(tile, h) in rams.keys() {
        ram_out.insert((tile, h), bram_sources(&mut d, tile, h)?);
    }
    let mut dsp_out: BTreeMap<(u32, usize), (String, Vec<(String, SigBit)>)> = BTreeMap::new();
    for &(tile, k) in dsps.keys() {
        dsp_out.insert((tile, k), dsp_sources(&mut d, tile, k)?);
    }
    let mut port_bits: Vec<(String, Dir, SigBit)> = Vec::new();
    let site_port: HashMap<&str, &str> = {
        let site_of_pin: HashMap<&str, &str> =
            db.pads.iter().map(|p| (p.pin.as_str(), p.bel.as_str())).collect();
        pins.iter()
            .filter_map(|(port, pin)| Some((*site_of_pin.get(pin.as_str())?, port.as_str())))
            .collect()
    };
    let mut pad_nets: HashMap<String, SigBit> = HashMap::new();
    // An input-only pin no port names (Vivado always configures PUDC_B as
    // one) drives nothing the check compares; anything it did drive would
    // read 1 and show.
    if let Some((site, ..)) = iobs.iter().find(|(s, _, out)| *out && !site_port.contains_key(s.as_str())) {
        bail!("pin {site} drives out but has no port");
    }
    iobs.retain(|(site, _, _)| site_port.contains_key(site.as_str()));
    for (site, input, output) in &iobs {
        let port = site_port
            .get(site.as_str())
            .with_context(|| format!("pin {site} is used but has no port"))?;
        let pad = d.net();
        pad_nets.insert(site.clone(), pad);
        let dir = match (input, output) {
            (true, true) => Dir::InOut,
            (true, false) => Dir::In,
            _ => Dir::Out,
        };
        port_bits.push((port.to_string(), dir, pad));
        if *input {
            let o = d.net();
            d.m.add_cell(format!("ibuf_{site}"), cells::ibuf(pad, o));
            d.source.insert(d.pin(site, "I")?, o);
        }
    }
    let mut bufg_out = Vec::new();
    for site in &bufgs {
        let o = d.net();
        d.source.insert(d.pin(site, "O")?, o);
        bufg_out.push(o);
    }
    for site in &bscans {
        let s = &db.sites[site];
        let bel = &db.tile_type(s.tile).bels[&s.relative];
        let mut outs: Vec<&String> = bel.pins.keys().filter(|p| *p != "TDO").collect();
        outs.sort();
        for p in outs {
            let n = d.net();
            d.source.insert(d.pin(site, p)?, n);
            port_bits.push((format!("__bscan_{p}"), Dir::In, n));
        }
    }

    if let Ok(t) = std::env::var("CHIPSMITH_DECODE_TRACE") {
        if let Some((site, pin)) = t.split_once(':') {
            let mut n = d.pin(site, pin)?;
            for _ in 0..40 {
                let src = d.source.get(&n).map(|s| format!("{s:?}"));
                eprintln!("trace {n}: {} source {src:?} const {:?}", d.why.get(&n).map_or("-", |s| s), d.constant.get(&n));
                match d.driver.get(&n) {
                    Some(&x) if src.is_none() => n = x,
                    _ => break,
                }
            }
        }
    }

    // Cells.
    for (key, h) in &halves {
        let (site, n) = &slice_nets[key];
        let pin = |d: &Decoder, p: String| d.resolve_pin(site, &p);
        let clk = pin(&d, "CLK".into())?;
        if h.ram.iter().any(|&r| r) {
            anyhow::ensure!(h.ram.iter().all(|&r| r), "{key}: a slice partly in RAM mode");
            let mut addr: [Vec<SigBit>; 4] = Default::default();
            for (k, a) in addr.iter_mut().enumerate() {
                for i in 1..=5 {
                    a.push(pin(&d, format!("{}{i}", LETTERS[k]))?);
                }
            }
            // Each LUT's DI1: its own <L>I pin, or the next LUT's DI1.
            let mut di1 = [SigBit::ZERO; 4];
            di1[3] = pin(&d, "DI".into())?;
            for k in (0..3).rev() {
                di1[k] = if h.di1_own[k] {
                    pin(&d, format!("{}I", LETTERS[k]))?
                } else {
                    di1[k + 1]
                };
            }
            let di = |d: &Decoder, k: usize| -> Result<[SigBit; 2]> {
                // Bit 0 (the 5LUT's half) from DI1, bit 1 from <L>X, as
                // Vivado's RAM32M has them.
                Ok([di1[k], pin(d, format!("{}X", LETTERS[k]))?])
            };
            let we = pin(&d, if h.we_ce { "CE" } else { "WE" }.into())?;
            let ram = Ram32m {
                raddr: [addr[0].clone(), addr[1].clone(), addr[2].clone()],
                waddr: addr[3].clone(),
                di: [di(&d, 0)?, di(&d, 1)?, di(&d, 2)?, di(&d, 3)?],
                dout: std::array::from_fn(|k| [n.o5[k], n.o6[k]]),
                wclk: clk,
                wclk_inv: h.clkinv,
                we,
                // Bit 2w + i of INIT_<k> is word w's bit i: bit i = 0 is
                // the 5LUT (table bits 0-31), 1 the 6LUT (32-63).
                init: std::array::from_fn(|k| {
                    (0..32).fold(0u64, |v, w| {
                        v | (h.init[k] >> w & 1) << (2 * w) | (h.init[k] >> (32 + w) & 1) << (2 * w + 1)
                    })
                }),
            };
            d.m.add_cell(format!("{key}.ram"), ram.cell());
            continue;
        }
        for k in 0..4 {
            let ins: Vec<SigBit> = (1..=6)
                .map(|i| pin(&d, format!("{}{i}", LETTERS[k])))
                .collect::<Result<_>>()?;
            d.m.add_cell(format!("{key}.{}6", LETTERS[k]), cells::lut(&ins, h.init[k], n.o6[k]));
            d.m.add_cell(
                format!("{key}.{}5", LETTERS[k]),
                cells::lut(&ins[..5], h.init[k] & 0xffff_ffff, n.o5[k]),
            );
        }
        let precy = h.precyinit.as_deref().unwrap_or("C0");
        let (ci, cyinit) = match precy {
            "CIN" => (pin(&d, "CIN".into())?, SigBit::ZERO),
            "AX" => (SigBit::ZERO, pin(&d, "AX".into())?),
            "C1" => (SigBit::ZERO, SigBit::ONE),
            _ => (SigBit::ZERO, SigBit::ZERO),
        };
        let di: [SigBit; 4] = std::array::from_fn(|k| {
            if h.cy0_o5[k] {
                n.o5[k]
            } else {
                pin(&d, format!("{}X", LETTERS[k])).unwrap_or(SigBit::ZERO)
            }
        });
        d.m.add_cell(
            format!("{key}.carry"),
            Carry4 {
                ci,
                cyinit,
                di,
                s: n.o6,
                o: n.xor,
                co: n.cy,
            }
            .cell(),
        );
        let ce = if h.ceused { pin(&d, "CE".into())? } else { SigBit::ONE };
        let sr = if h.srused { pin(&d, "SR".into())? } else { SigBit::ZERO };
        for k in 0..4 {
            let dsrc = match h.ffmux[k].as_deref() {
                Some("O6") => n.o6[k],
                Some("O5") => n.o5[k],
                Some("XOR") => n.xor[k],
                Some("CY") => n.cy[k],
                _ => pin(&d, format!("{}X", LETTERS[k]))?,
            };
            let reset = match (h.sync, h.zrst[k]) {
                (true, true) => Reset::SyncClear(sr),
                (true, false) => Reset::SyncSet(sr),
                (false, true) => Reset::AsyncClear(sr),
                (false, false) => Reset::AsyncPreset(sr),
            };
            d.m.add_cell(
                format!("{key}.{}ff", LETTERS[k]),
                Fd {
                    d: dsrc,
                    clk,
                    clk_inv: h.clkinv,
                    ce,
                    reset,
                    init: !h.zini[k],
                    q: n.q[k],
                }
                .cell(),
            );

            let d5 = match h.ff5mux[k].as_deref() {
                Some("IN_A") => n.o5[k],
                _ => pin(&d, format!("{}X", LETTERS[k]))?,
            };
            let reset5 = match (h.sync, h.zrst5[k]) {
                (true, true) => Reset::SyncClear(sr),
                (true, false) => Reset::SyncSet(sr),
                (false, true) => Reset::AsyncClear(sr),
                (false, false) => Reset::AsyncPreset(sr),
            };
            d.m.add_cell(
                format!("{key}.{}5ff", LETTERS[k]),
                Fd {
                    d: d5,
                    clk,
                    clk_inv: h.clkinv,
                    ce,
                    reset: reset5,
                    init: !h.zini5[k],
                    q: n.q5[k],
                }
                .cell(),
            );        }
    }
    for (key, s) in &dsps {
        let (site, outs) = &dsp_out[key];
        dsp_cell(&mut d, site, s, outs)?;
    }
    for ((tile, h), r) in &rams {
        bram_half(&mut d, *tile, *h, r, &ram_out[&(*tile, *h)])?;
    }
    for (site, o) in bufgs.iter().zip(&bufg_out) {
        let i = d.resolve_pin(site, "I0")?;
        d.m.add_cell(format!("bufg_{site}"), cells::bufg(i, *o));
    }
    for site in &bscans {
        let tdo = d.resolve_pin(site, "TDO")?;
        port_bits.push(("__bscan_TDO".into(), Dir::Out, tdo));
    }
    for (site, _, output) in &iobs {
        if !output {
            continue;
        }
        let pad = pad_nets[site];
        let data = d.resolve_pin(site, "O")?;
        // The IOB's T is its OLOGIC's TQ, which is T1 inverted unless
        // ZINV_T1 (the pseudo pip TQ <- T1) is set: an unused T1 reads 1,
        // so an unconfigured OLOGIC drives the pin.
        let ologic = site.replacen("IOB_", "OLOGIC_", 1);
        let r = d.resolve_pin(&ologic, "T1")?;
        let os = &db.sites[&ologic];
        let ioi = db.tile_names[os.tile as usize].as_str();
        let rel_y: u32 = os.relative.rsplit_once('Y').and_then(|(_, y)| y.parse().ok()).unwrap_or(0);
        let zinv = tiles.get(ioi).is_some_and(|fs| {
            fs.iter().any(|f| {
                f.name == format!("OLOGIC_Y{}.ZINV_T1", 1 - rel_y.min(1))
                    || (ioi.contains("_SING_") && f.name.ends_with(".ZINV_T1"))
            })
        });
        let t = if zinv {
            r
        } else {
            match r {
                SigBit::Const(State::One) => SigBit::ZERO,
                SigBit::Const(State::Zero) => SigBit::ONE,
                r => {
                    let n = d.net();
                    d.m.add_cell(format!("tinv_{site}"), cells::lut(&[r], 0b01, n));
                    n
                }
            }
        };
        match t {
            SigBit::Const(State::Zero) => d.m.add_cell(format!("obuf_{site}"), cells::obuf(data, pad)),
            t => d.m.add_cell(format!("obuft_{site}"), cells::obuft(data, t, pad)),
        };
    }

    // Ports, the bits of each grouped into its wire.
    let mut wires: BTreeMap<String, (Dir, BTreeMap<usize, SigBit>)> = BTreeMap::new();
    for (name, dir, bit) in port_bits {
        let (base, idx) = match name.strip_suffix(']').and_then(|n| n.rsplit_once('[')) {
            Some((b, i)) => (b.to_string(), i.parse::<usize>()?),
            None => (name, 0),
        };
        let w = wires.entry(base).or_insert((dir, BTreeMap::new()));
        w.1.insert(idx, bit);
    }
    for (name, (dir, bits)) in wires {
        let width = bits.keys().max().map_or(0, |m| m + 1);
        let sig: Vec<SigBit> = (0..width)
            .map(|i| bits.get(&i).copied().unwrap_or(SigBit::Z))
            .collect();
        if sig.iter().any(|b| !matches!(b, SigBit::Net(_))) && dir == Dir::In {
            bail!("input port {name} has missing bits");
        }
        let mut w = Wire::new(name, sig);
        w.port = Some(dir);
        d.m.wires.push(w);
    }
    let mut m = d.m;
    m.attributes.insert(
        chipsmith_netlist::attr::TOP.into(),
        chipsmith_netlist::ParamValue::int(1),
    );
    Ok(Routed {
        design: Design { modules: vec![m] },
        driver: d.driver,
        source: d.source,
        read: d.read.into_inner(),
        slice_sites: slice_nets.into_iter().map(|(k, (site, _))| (k, site)).collect(),
    })
}

/// One RAMB18 half's features.
#[derive(Default)]
struct RamHalf {
    in_use: bool,
    /// Per port: read and write widths from their bits (none set: x1).
    read: [Option<u32>; 2],
    write: [Option<u32>; 2],
    sdp_read: bool,
    sdp_write: bool,
    out_reg: [bool; 2],
    mode: [crate::bram::WriteMode; 2],
    rstreg_regce: [bool; 2],
    /// ZINV_<pin>: not inverted.
    zinv: HashSet<String>,
    zinit: [u64; 2],
    zsrval: [u64; 2],
    /// The half's data then parity bits.
    contents: Vec<bool>,
}

impl RamHalf {
    fn read(&mut self, parts: &[&str], value: Option<&(u32, Vec<bool>)>) {
        use crate::bram::WriteMode;
        if self.contents.is_empty() {
            self.contents = vec![false; 16384 + 2048];
        }
        let port = |s: &str| usize::from(s.ends_with('B'));
        let bits = |v: Option<&(u32, Vec<bool>)>| -> u64 {
            v.map_or(0, |(lo, b)| b.iter().enumerate().fold(0u64, |a, (i, &x)| a | u64::from(x) << (*lo as usize + i)))
        };
        match parts {
            ["IN_USE"] => self.in_use = true,
            ["SDP_READ_WIDTH_36"] => self.sdp_read = true,
            ["SDP_WRITE_WIDTH_36"] => self.sdp_write = true,
            [f] if f.starts_with("DO") && f.ends_with("_REG") => self.out_reg[usize::from(f.starts_with("DOB"))] = true,
            [f] if f.starts_with("READ_WIDTH_") || f.starts_with("WRITE_WIDTH_") => {
                let (name, w) = f.rsplit_once('_').expect("matched");
                let w: u32 = w.parse().unwrap_or(1);
                let k = port(name);
                if w > 1 {
                    if name.starts_with("READ") {
                        self.read[k] = Some(w);
                    } else {
                        self.write[k] = Some(w);
                    }
                }
            }
            [f] if f.starts_with("WRITE_MODE_") => {
                let k = usize::from(f.starts_with("WRITE_MODE_B"));
                if f.ends_with("READ_FIRST") {
                    self.mode[k] = WriteMode::ReadFirst;
                } else if f.ends_with("NO_CHANGE") {
                    self.mode[k] = WriteMode::NoChange;
                }
            }
            [f] if f.starts_with("RSTREG_PRIORITY_") && f.ends_with("_REGCE") => {
                self.rstreg_regce[usize::from(f.starts_with("RSTREG_PRIORITY_B"))] = true;
            }
            [f] if f.starts_with("ZINV_") => {
                self.zinv.insert(f["ZINV_".len()..].to_string());
            }
            [f] if f.starts_with("ZINIT_") => self.zinit[port(f)] = bits(value),
            [f] if f.starts_with("ZSRVAL_") => self.zsrval[port(f)] = bits(value),
            [f] if f.starts_with("INIT_") || f.starts_with("INITP_") => {
                let (pre, i) = f.split_once('_').expect("matched");
                let Ok(i) = usize::from_str_radix(i, 16) else { return };
                let base = if pre == "INITP" { 16384 } else { 0 } + 256 * i;
                if let Some((lo, v)) = value {
                    for (k, &b) in v.iter().enumerate() {
                        if let Some(slot) = self.contents.get_mut(base + *lo as usize + k) {
                            *slot = b;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// The site of half `h` of BRAM tile `tile`, and of the tile's RAMB36.
fn bram_sites(db: &ChipDb, tile: u32, h: usize) -> Result<(String, Option<String>)> {
    let half = db
        .sites
        .iter()
        .find(|(_, s)| s.tile == tile && s.relative == format!("RAMB18_X0Y{h}"))
        .map(|(n, _)| n.clone())
        .with_context(|| format!("tile {}: no RAMB18 half {h}", db.tile_names[tile as usize]))?;
    let full = db
        .sites
        .iter()
        .find(|(_, s)| s.tile == tile && s.ty == "RAMBFIFO36E1")
        .map(|(n, _)| n.clone());
    Ok((half, full))
}

/// Fresh nets for half `h`'s read data (DOADO, DOBDO, DOPADOP, DOPBDOP),
/// each the source of its RAMB18 site pin and of the RAMB36 pin that is
/// the same output (bit 2i + h of the RAMB36's).
fn bram_sources(d: &mut Decoder, tile: u32, h: usize) -> Result<[Vec<SigBit>; 4]> {
    let (half, full) = bram_sites(d.db, tile, h)?;
    let mut out: [Vec<SigBit>; 4] = Default::default();
    for (k, (name, n)) in [("DOADO", 16), ("DOBDO", 16), ("DOPADOP", 2), ("DOPBDOP", 2)].into_iter().enumerate() {
        for i in 0..n {
            let net = d.net();
            out[k].push(net);
            let pin = crate::phys::bram_pin(d.db, &half, &format!("{name}{i}"))?;
            d.source.insert(d.pin(&half, &pin)?, net);
            if let Some(full) = &full {
                if let Ok(node) = d.pin(full, &format!("{name}{}", 2 * i + h)) {
                    d.source.insert(node, net);
                }
            }
        }
    }
    Ok(out)
}

/// Half `h` of a BRAM tile as a RAMB18E1, its pins traced back like any
/// site's (`nextpnr`'s mapping of the primitive's pins, read backwards).
fn bram_half(d: &mut Decoder, tile: u32, h: usize, r: &RamHalf, dout: &[Vec<SigBit>; 4]) -> Result<()> {
    use crate::bram::{Config, Inversions, Port, Size, Width};
    let (site, _) = bram_sites(d.db, tile, h)?;
    let name = format!("{}.ramb18_y{h}", d.db.tile_names[tile as usize]);
    let pin = |d: &Decoder, p: &str| -> Result<SigBit> {
        let sp = crate::phys::bram_pin(d.db, &site, p)?;
        d.resolve_pin(&site, &sp)
    };
    let width = |w: Option<u32>| Width::from_bits(w.unwrap_or(1) as usize).context("a block RAM width");
    let sdp = r.sdp_read || r.sdp_write;
    let mut ports = [Port::unused(), Port::unused()];
    for k in 0..2 {
        ports[k].read = Some(width(r.read[k])?);
        ports[k].write = Some(width(r.write[k])?);
        ports[k].mode = r.mode[k];
        ports[k].out_reg = r.out_reg[k];
        ports[k].rstreg_first = !r.rstreg_regce[k];
        // In the read width's layout; SDP's wide read is each port's x18.
        let w = if r.sdp_read { Width::X18 } else { width(r.read[k])? };
        ports[k].init = crate::bram::from_half_bits(!r.zinit[k] & 0x3ffff, w);
        ports[k].srval = crate::bram::from_half_bits(!r.zsrval[k] & 0x3ffff, w);
    }
    if sdp {
        ports[0].read = Some(if r.sdp_read { Width::X36 } else { width(r.read[0])? });
        ports[1].write = Some(if r.sdp_write { Width::X36 } else { width(r.write[1])? });
        ports[0].write = None;
        ports[1].read = None;
        anyhow::ensure!(r.out_reg[0] == r.out_reg[1], "{name}: an SDP block with DOA_REG and DOB_REG apart");
        ports[1].out_reg = false;
    }
    let inv = |p: &str| !r.zinv.contains(p);
    let config = Config {
        size: Size::Half,
        sdp,
        ports,
        inv: Inversions {
            clk: [inv("CLKARDCLK"), inv("CLKBWRCLK")],
            en: [inv("ENARDEN"), inv("ENBWREN")],
            rstram: [inv("RSTRAMARSTRAM"), inv("RSTRAMB")],
            rstreg: [inv("RSTREGARSTREG"), inv("RSTREGB")],
        },
        contents: r.contents.clone(),
    };
    config.check().with_context(|| name.clone())?;
    let mut cell = crate::synth::cells::Ramb::new(config, [dout[0].clone(), dout[1].clone()], [dout[2].clone(), dout[3].clone()]);
    for (k, (clk, en, regce, rstram, rstreg, regclk)) in [
        ("CLKARDCLK", "ENARDEN", "REGCEAREGCE", "RSTRAMARSTRAM", "RSTREGARSTREG", "REGCLKARDRCLK"),
        ("CLKBWRCLK", "ENBWREN", "REGCEB", "RSTRAMB", "RSTREGB", "REGCLKB"),
    ]
    .into_iter()
    .enumerate()
    {
        cell.clk[k] = pin(d, clk)?;
        cell.en[k] = pin(d, en)?;
        cell.regce[k] = pin(d, regce)?;
        cell.rstram[k] = pin(d, rstram)?;
        cell.rstreg[k] = pin(d, rstreg)?;
        // The output register runs on REGCLK; the models on the port's
        // clock, so they must be one. (SDP: REGCLKB's register is the
        // read's, port A's.)
        let reg_on = if sdp { r.out_reg[0] } else { r.out_reg[k] };
        if reg_on {
            let want = if sdp { cell.clk[0] } else { cell.clk[k] };
            let got = pin(d, regclk)?;
            anyhow::ensure!(got == want, "{name}: {regclk} isn't the port's clock");
            anyhow::ensure!(
                inv(regclk) == if sdp { inv("CLKARDCLK") } else { [inv("CLKARDCLK"), inv("CLKBWRCLK")][k] },
                "{name}: {regclk} inverted apart from the clock"
            );
        }
        for i in 0..14 {
            cell.addr[k][i] = pin(d, &format!("{}{i}", ["ADDRARDADDR", "ADDRBWRADDR"][k]))?;
        }
        for i in 0..16 {
            cell.di[k][i] = pin(d, &format!("{}{i}", ["DIADI", "DIBDI"][k]))?;
        }
        for i in 0..2 {
            cell.dip[k][i] = pin(d, &format!("{}{i}", ["DIPADIP", "DIPBDIP"][k]))?;
        }
    }
    // Write enables, nextpnr's mapping backwards: WEA[i] on WEA2i (and
    // 2i+1), WEBWE[i] on WEBWE i, or 2i in SDP's wide write.
    for i in 0..2 {
        cell.we[0][i] = pin(d, &format!("WEA{}", 2 * i))?;
    }
    for i in 0..4 {
        let p = if r.sdp_write { 2 * i } else { i };
        cell.we[1][i] = pin(d, &format!("WEBWE{p}"))?;
    }
    let kind = cell.cell();
    d.m.add_cell(name, kind);
    Ok(())
}

/// One DSP48E1 site's features.
#[derive(Default)]
struct DspSite {
    /// (feature, value) as read.
    feats: Vec<(String, u64)>,
}

impl DspSite {
    fn read(&mut self, parts: &[&str], value: Option<&(u32, Vec<bool>)>) {
        let v = value.map_or(1, |(lo, b)| {
            b.iter().enumerate().fold(0u64, |a, (i, &x)| a | u64::from(x) << (*lo as usize + i))
        });
        let name = parts.join(".");
        let name = name.trim_end_matches("[0]").to_string();
        match self.feats.iter_mut().find(|(n, _)| *n == name) {
            Some((_, old)) => *old |= v,
            None => self.feats.push((name, v)),
        }
    }

    fn get(&self, name: &str) -> Option<u64> {
        self.feats.iter().find(|(n, _)| n == name).map(|&(_, v)| v)
    }

    fn config(&self) -> crate::dsp::Config {
        let has = |n: &str| self.get(n).is_some();
        let reg = |n: &str| -> u8 {
            if has(&format!("{n}_0")) {
                0
            } else if has(&format!("{n}_2")) {
                2
            } else {
                1
            }
        };
        let (areg, breg) = (reg("AREG"), reg("BREG"));
        let mut stages = [true; 10];
        for (s, name) in stages.iter_mut().zip(crate::dsp::STAGES) {
            *s = !has(&format!("Z{name}"));
        }
        let mut inverted = [0u8; 5];
        for (v, (pin, w)) in inverted.iter_mut().zip(crate::dsp::INVERTIBLE) {
            let z = self.get(&format!("ZIS_{pin}_INVERTED")).unwrap_or(0);
            *v = (!z & ((1 << w) - 1)) as u8;
        }
        crate::dsp::Config {
            areg,
            breg,
            // ACASCREG follows AREG but where ZAREG_2_ACASCREG_1 is clear.
            acascreg: if areg == 2 && !has("ZAREG_2_ACASCREG_1") { 1 } else { areg },
            bcascreg: if breg == 2 && !has("ZBREG_2_BCASCREG_1") { 1 } else { breg },
            stages,
            a_cascade: has("A_INPUT"),
            b_cascade: has("B_INPUT"),
            use_dport: has("USE_DPORT"),
            inverted,
            mask: self.get("MASK").unwrap_or(0),
            pattern: self.get("PATTERN").unwrap_or(0),
        }
    }
}

/// The DSP48E1 ports, inputs then outputs, with widths.
const DSP_INPUTS: [(&str, usize); 38] = [
    ("A", 30), ("ACIN", 30), ("ALUMODE", 4), ("B", 18), ("BCIN", 18), ("C", 48), ("CARRYCASCIN", 1),
    ("CARRYIN", 1), ("CARRYINSEL", 3), ("CEA1", 1), ("CEA2", 1), ("CEAD", 1), ("CEALUMODE", 1),
    ("CEB1", 1), ("CEB2", 1), ("CEC", 1), ("CECARRYIN", 1), ("CECTRL", 1), ("CED", 1), ("CEINMODE", 1),
    ("CEM", 1), ("CEP", 1), ("CLK", 1), ("D", 25), ("INMODE", 5), ("MULTSIGNIN", 1), ("OPMODE", 7),
    ("PCIN", 48), ("RSTA", 1), ("RSTALLCARRYIN", 1), ("RSTALUMODE", 1), ("RSTB", 1), ("RSTC", 1),
    ("RSTCTRL", 1), ("RSTD", 1), ("RSTINMODE", 1), ("RSTM", 1), ("RSTP", 1),
];
const DSP_OUTPUTS: [(&str, usize); 11] = [
    ("ACOUT", 30), ("BCOUT", 18), ("CARRYCASCOUT", 1), ("CARRYOUT", 4), ("MULTSIGNOUT", 1),
    ("OVERFLOW", 1), ("P", 48), ("PATTERNBDETECT", 1), ("PATTERNDETECT", 1), ("PCOUT", 48),
    ("UNDERFLOW", 1),
];

fn dsp_pin(name: &str, w: usize, i: usize) -> String {
    if w > 1 {
        format!("{name}{i}")
    } else {
        name.to_string()
    }
}

/// DSP `k` of `tile`'s site and fresh nets for its outputs, each its pin's
/// source.
fn dsp_sources(d: &mut Decoder, tile: u32, k: usize) -> Result<(String, Vec<(String, SigBit)>)> {
    let site = d
        .db
        .sites
        .iter()
        .find(|(_, s)| s.tile == tile && s.ty == "DSP48E1" && s.relative.ends_with(&k.to_string()))
        .map(|(n, _)| n.clone())
        .with_context(|| format!("tile {}: no DSP site {k}", d.db.tile_names[tile as usize]))?;
    let mut outs = Vec::new();
    for (name, w) in DSP_OUTPUTS {
        let mut bits = Vec::new();
        for i in 0..w {
            let net = d.net();
            if let Ok(node) = d.pin(&site, &dsp_pin(name, w, i)) {
                d.source.insert(node, net);
            }
            bits.push(net);
        }
        for (i, b) in bits.into_iter().enumerate() {
            outs.push((dsp_pin(name, w, i), b));
        }
    }
    Ok((site, outs))
}

/// A DSP48E1 of the site's features and pins. Its pins' inversions are
/// folded into the pins (a constant flipped, a signal through an
/// inverter), which yosys' model needs.
fn dsp_cell(d: &mut Decoder, site: &str, s: &DspSite, outs: &[(String, SigBit)]) -> Result<()> {
    use chipsmith_netlist::{CellKind, Dir as PDir, Generic, GenericPort};
    let mut config = s.config();
    let mut ports = Vec::new();
    let inv_of = |name: &str| -> Option<(usize, usize)> {
        crate::dsp::INVERTIBLE.iter().position(|(p, _)| *p == name).map(|i| (i, crate::dsp::INVERTIBLE[i].1))
    };
    for (name, w) in DSP_INPUTS {
        let mut sig = Vec::new();
        for i in 0..w {
            let mut b = d.resolve_pin(site, &dsp_pin(name, w, i))?;
            if let Some((k, _)) = inv_of(name) {
                if config.inverted[k] >> i & 1 == 1 {
                    b = match b {
                        SigBit::Const(State::One) => SigBit::ZERO,
                        SigBit::Const(State::Zero) => SigBit::ONE,
                        b => {
                            let n = d.net();
                            d.m.add_cell(format!("{site}.inv_{name}{i}"), cells::lut(&[b], 0b01, n));
                            n
                        }
                    };
                }
            }
            sig.push(b);
        }
        ports.push(GenericPort {
            name: name.to_string(),
            dir: Some(PDir::In),
            sig,
        });
    }
    config.inverted = [0; 5];
    for (name, w) in DSP_OUTPUTS {
        let sig: Vec<SigBit> = (0..w)
            .map(|i| {
                let pin = dsp_pin(name, w, i);
                outs.iter().find(|(p, _)| *p == pin).map_or(SigBit::X, |(_, b)| *b)
            })
            .collect();
        ports.push(GenericPort {
            name: name.to_string(),
            dir: Some(PDir::Out),
            sig,
        });
    }
    d.m.add_cell(
        format!("{site}.dsp"),
        CellKind::Other(Generic {
            ty: "DSP48E1".into(),
            params: config.params(),
            ports,
        }),
    );
    Ok(())
}
