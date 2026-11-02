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
    value: Option<(u32, u64)>,
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
                (base.to_string(), Some((lo, u64::from_str_radix(bits, 2)?)))
            }
            // A single set bit of a vector: `ALUT.INIT[17]`.
            None => match rest.strip_suffix(']').and_then(|r| r.rsplit_once('[')) {
                Some((base, bit)) if bit.chars().all(|c| c.is_ascii_digit()) => {
                    (base.to_string(), Some((bit.parse()?, 1)))
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
    };
    for tile in 0..db.width * db.height {
        for (w, v) in [("GND_WIRE", false), ("VCC_WIRE", true)] {
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
                match (&parts[1..], f.value) {
                    ([lut, "INIT"], Some((lo, v))) if lut.ends_with("LUT") => {
                        if let Some(k) = letter(lut) {
                            h.init[k] |= v << lo;
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
                _ => n.mux[k],
            };
            d.source.insert(d.pin(&site, &format!("{l}MUX"))?, mux);
        }
        if let Ok(cout) = d.pin(&site, "COUT") {
            d.source.insert(cout, n.cy[3]);
        }
        slice_nets.insert(key.clone(), (site, n));
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
            // Each 6LUT's data: its own pin, or the next LUT's data.
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
                Ok([pin(d, format!("{}X", LETTERS[k]))?, di1[k]])
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
        }
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
    Ok(Design { modules: vec![m] })
}
