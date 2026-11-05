//! From the placed design to the physical one: which site pin each cell
//! port is, so which nets need the interconnect between which graph nodes
//! ([`RouteNet`]), and how every used site is configured ([`SliceConf`],
//! [`PadConf`], ...) for the FASM writer (`crate::fasm`).
//!
//! The packer and placer have already fixed the shapes (`crate::pack`), so
//! the site routing follows from them:
//!
//! - a logic LUT in the k-th 6LUT reads its inputs on the pins `<L>1..<L>6`
//!   their drivers reach soonest, the most critical first ([`route_pins`]),
//!   its table permuted to match, and drives pin `<L>` from O6;
//! - a logic LUT in the k-th 5LUT (paired with the 6LUT's, `crate::pack`)
//!   reads the same pins, drives `<L>MUX` from O5, and `<L>6` is tied high;
//!   the 5FF beside it takes its D from O5 (`<L>5FFMUX.IN_A`) or `<L>X`
//!   (`IN_B`) and drives `<L>MUX` (`<L>OUTMUX.<L>5Q`);
//! - a register's D comes from the 6LUT beside it (`<L>FFMUX.O6`), the
//!   CARRY4's sum of its bit (`.XOR`), or the `<L>X` bypass pin; Q is
//!   `<L>Q`; clock, CE and SR are the slice's `CLK`, `CE` and `SR`;
//! - a CARRY4 takes S from the 6LUTs, DI from the `<L>X` pins, a signal
//!   carry-in on `AX` and CIN from the slice below; a sum or carry out read
//!   outside the slice leaves on `<L>MUX` (`<L>OUTMUX.XOR` or `.CY`);
//! - a RAM32M's memory k reads its address on `<L>1..<L>5` (`<L>6` tied
//!   high, which splits the LUT into the two 32-bit RAMs), writes data bit 1
//!   from `<L>I` (`DI` for D) and bit 0 from `<L>X`, and drives `<L>` (O6)
//!   and `<L>MUX` (O5); the write address is D's, inside the slice;
//! - a pin's buffer drives the fabric from the IOB's `I` and takes `O` and
//!   `T` (through the I/O tile's ILOGIC and OLOGIC, which the router passes
//!   through, `crate::xray`);
//! - a BUFG is a BUFGCTRL with CE0, S0, IGNORE0 on and CE1, S1, IGNORE1
//!   off, all six pins tied to VCC and the off ones inverted, as openXC7's
//!   nextpnr ties them;
//! - every PS7 input is tied to 0;
//! - a block RAM's pins are its site's ([`bram_pin`]: the lower RAMB18
//!   site is a FIFO18E1 one, whose pins are named for the FIFO).
//!
//! Constants: each sink of the constant nets is reached from the nearest
//! GND_WIRE or VCC_WIRE (by graph hops), one routed net per source.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use anyhow::{bail, ensure, Context, Result};
use chipsmith_route::chipdb::{ChipDb, NodeId};
use chipsmith_route::timing::Ps;

use crate::pack::{Conf, Packed};
use crate::place::{Bel, CellId, Kind, NetId, Placement};

/// One net for the router.
#[derive(Debug, Clone)]
pub(crate) struct RouteNet {
    pub name: String,
    pub source: NodeId,
    pub sinks: Vec<NodeId>,
    /// The packed net it carries.
    pub net: NetId,
    /// Per sink, the cell ports it feeds.
    pub sink_ports: Vec<Vec<(CellId, String)>>,
}

/// A register's own bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FfConf {
    /// Powers up 0.
    pub zini: bool,
    /// SR resets it to 0.
    pub zrst: bool,
}

/// The CARRY4's carry into bit 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreCyInit {
    Cin,
    Ax,
    Zero,
    One,
}

/// One slice's configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SliceConf {
    pub tile: u32,
    /// The FASM half name: `SLICEM_X0`, `SLICEL_X0` or `SLICEL_X1`.
    pub half: String,
    /// Each LUT's 64-bit table, A1 the lowest index bit.
    pub lut_init: [Option<u64>; 4],
    /// The LUT is two 32 x 1 RAMs (`RAM`, `SMALL`).
    pub lut_ram: [bool; 4],
    pub ff: [Option<FfConf>; 4],
    /// The 5LUTs' 32-bit tables, where a logic LUT is in one.
    pub lut5_init: [Option<u64>; 4],
    pub ff5: [Option<FfConf>; 4],
    /// `<L>5FFMUX`'s input: `IN_A` (O5) or `IN_B` (the bypass).
    pub ff5mux: [Option<&'static str>; 4],
    /// `<L>FFMUX`'s input: `O6`, `XOR` or the bypass (`AX`, ...).
    pub ffmux: [Option<&'static str>; 4],
    /// `<L>OUTMUX`'s input onto `<L>MUX`: `O5`, `XOR`, `CY` or `5Q` (the
    /// 5FF's output, `<L>5Q`).
    pub outmux: [Option<&'static str>; 4],
    pub carry: Option<PreCyInit>,
    pub clkinv: bool,
    pub ffsync: bool,
    pub srused: bool,
    pub ceused: bool,
}

/// A pin in use: its IOB site and which way it carries signals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PadConf {
    pub site: String,
    pub input: bool,
    pub output: bool,
}

/// A used block RAM site: its tile and half (0 lower, 1 upper; a RAMB36E1
/// both) and configuration.
#[derive(Debug, Clone)]
pub(crate) struct BramConf {
    pub tile: u32,
    pub half: Option<usize>,
    pub config: crate::bram::Config,
}

/// A used DSP48E1 site: its tile, which of the tile's two (`DSP_0` the
/// lower), and configuration.
#[derive(Debug, Clone)]
pub(crate) struct DspConf {
    pub tile: u32,
    pub which: usize,
    pub config: crate::dsp::Config,
}

/// A used PLLE2_ADV or MMCME2_ADV: its tile, what it holds, and whether
/// RST is routed from a signal.
#[derive(Debug, Clone)]
pub(crate) struct CmtSite {
    pub tile: u32,
    pub config: crate::cmt::Config,
    pub reset: bool,
}

pub(crate) struct Physical {
    pub nets: Vec<RouteNet>,
    pub brams: Vec<BramConf>,
    pub dsps: Vec<DspConf>,
    pub cmts: Vec<CmtSite>,
    pub slices: BTreeMap<String, SliceConf>,
    pub pads: Vec<PadConf>,
    pub bufgs: Vec<String>,
    /// BSCAN sites and their JTAG chain.
    pub bscans: Vec<(String, u32)>,
}

const LETTERS: [char; 4] = ['A', 'B', 'C', 'D'];

/// A site pin's graph node.
pub(crate) fn pin_node(db: &ChipDb, site: &str, pin: &str) -> Result<NodeId> {
    let s = db.sites.get(site).with_context(|| format!("no site {site}"))?;
    let bel = db
        .tile_type(s.tile)
        .bels
        .get(&s.relative)
        .with_context(|| format!("site {site} ({}) isn't in its tile's type", s.relative))?;
    let wire = *bel
        .pins
        .get(pin)
        .with_context(|| format!("site {site} has no pin {pin}"))?;
    Ok(db.node(s.tile, wire))
}

/// The site pin of block RAM `site` that its RAMB18E1/RAMB36E1 pin `pin`
/// is: the pin itself, or on the lower RAMB18 site (a FIFO18E1) the pin
/// whose wire is named for it (`BRAM_FIFO18_<pin>`).
pub(crate) fn bram_pin(db: &ChipDb, site: &str, pin: &str) -> Result<String> {
    let s = db.sites.get(site).with_context(|| format!("no site {site}"))?;
    let tt = db.tile_type(s.tile);
    let bel = tt.bels.get(&s.relative).with_context(|| format!("site {site}"))?;
    if bel.pins.contains_key(pin) && s.ty != "FIFO18E1" {
        return Ok(pin.to_string());
    }
    let suffix = format!("_{pin}");
    bel.pins
        .iter()
        .find(|(_, &w)| {
            let name = tt.wires.get(w);
            name.strip_prefix("BRAM_FIFO18").is_some_and(|rest| rest == suffix)
                || name.strip_prefix("BRAM_RAMB18").is_some_and(|rest| rest == suffix)
        })
        .map(|(p, _)| p.clone())
        .with_context(|| format!("block RAM site {site} has no pin for {pin}"))
}

/// Where a cell port connects: inside its site, or at a site pin.
#[derive(Debug, Clone, PartialEq, Eq)]
enum End {
    Internal,
    Pin(String, String),
}

/// The 64-bit table of a LUT whose input `i` is on pin `pins[i]` (1 to 6).
fn expand(init: u64, pins: &[usize]) -> u64 {
    (0..64u64).fold(0, |t, j| {
        let idx = pins.iter().enumerate().fold(0, |a, (i, &p)| a | (j >> (p - 1) & 1) << i);
        t | ((init >> idx) & 1) << j
    })
}

/// The pin of a logic LUT's `r`th input (from 0), fastest first: a site
/// pin's delay falls from A1 (0.52 ns, prjxray-db's slow corner) to A6
/// (0.005 ns). With the 5LUT in use (a pair, or a LUT5 there) A6 is tied
/// high, so both LUTs take theirs from A5 down.
fn lut_pin(five_used: bool, r: usize) -> usize {
    if five_used { 5 - r } else { 6 - r }
}

struct Builder<'a> {
    db: &'a ChipDb,
    p: &'a Packed,
    sites: &'a [Option<(String, Bel)>],
    /// The (site, letter)s whose 5LUT holds a cell.
    five_used: HashSet<(String, usize)>,
    /// Each (site, letter)'s logic LUT inputs' pins (A1 = 0, ...; pins 1 to
    /// 6), chosen by route delay ([`route_pins`]).
    pins: HashMap<(String, usize), Vec<usize>>,
}

impl Builder<'_> {
    /// The pin of LUT input `i` (from 0) of the logic LUT(s) at `site`,
    /// letter `k`.
    fn lut_pin(&self, site: &str, k: usize, i: usize) -> usize {
        let key = (site.to_string(), k);
        match self.pins.get(&key).and_then(|p| p.get(i)) {
            Some(&p) => p,
            None => lut_pin(self.five_used.contains(&key), i),
        }
    }
}

/// Searches stop after settling this many nodes: a source further away
/// reaches every pin about as fast, and takes them by [`lut_pin`]'s order.
const PIN_SEARCH_NODES: usize = 3000;

/// Each (site, letter)'s logic LUT inputs on the pins their drivers reach
/// soonest, the most critical input first (a pair's input by the more
/// critical of its two LUTs' connections), so the critical connections
/// get theirs whatever it costs the others: an assignment making the
/// criticality-weighted sum of the delays least was tried, and lost
/// bram-selftest 9% (README). The fastest site pin isn't always the
/// nearest: zybo-jtag-counter's register reached the `D6` beside it by a
/// long line out and back, where pins chosen for their own delay alone
/// put it. Delays are the routing graph's (each node's, as the router
/// times them), with no other net in the way; a pin no search reached
/// counts as far, and ties go to [`lut_pin`]'s order.
fn route_pins(b: &Builder, placement: &Placement, node_delay: &[Ps]) -> Result<HashMap<(String, usize), Vec<usize>>> {
    let d = &b.p.design;
    // Per (site, letter): per input, its net and criticality.
    let mut inputs: HashMap<(String, usize), Vec<Option<(NetId, f32)>>> = HashMap::new();
    for (c, cell) in d.cells.iter().enumerate() {
        if cell.kind != (Kind::Lut { dram: false }) {
            continue;
        }
        let Some((site, Bel::Lut6(k) | Bel::Lut5(k))) = &placement.sites[c] else {
            continue;
        };
        let v = inputs.entry((site.clone(), *k)).or_default();
        for (port, net) in &cell.pins {
            let Some(i) = port.strip_prefix('A').and_then(|i| i.parse::<usize>().ok()) else {
                continue;
            };
            if v.len() < i {
                v.resize(i, None);
            }
            let x = placement.lut_criticality.get(&(c as CellId, port.clone())).copied().unwrap_or(0.0);
            let crit = v[i - 1].map_or(x, |(_, y)| x.max(y));
            v[i - 1] = Some((*net, crit));
        }
    }
    let mut sources: HashMap<NetId, Option<NodeId>> = HashMap::new();
    let mut source = |net: NetId| -> Result<Option<NodeId>> {
        if let Some(&s) = sources.get(&net) {
            return Ok(s);
        }
        let s = match &d.nets[net as usize].driver {
            Some((drv, port)) if net != b.p.gnd && net != b.p.vcc => {
                let (site, pin, _) = b.output(*drv, port)?;
                pin_node(b.db, &site, &pin).ok()
            }
            _ => None,
        };
        sources.insert(net, s);
        Ok(s)
    };
    let mut keys: Vec<(String, usize)> = inputs.keys().cloned().collect();
    keys.sort();
    let mut out = HashMap::new();
    for key in keys {
        let v = &inputs[&key];
        let five = b.five_used.contains(&key);
        let candidates: Vec<usize> = (0..if five { 5 } else { 6 }).map(|r| lut_pin(five, r)).collect();
        let nodes: Vec<NodeId> = candidates
            .iter()
            .map(|&p| pin_node(b.db, &key.0, &format!("{}{p}", LETTERS[key.1])))
            .collect::<Result<_>>()?;
        let mut order: Vec<usize> = (0..v.len()).filter(|&i| v[i].is_some()).collect();
        order.sort_by(|&x, &y| {
            let (cx, cy) = (v[x].map_or(0.0, |t| t.1), v[y].map_or(0.0, |t| t.1));
            cy.total_cmp(&cx).then(x.cmp(&y))
        });
        let mut free = vec![true; candidates.len()];
        let mut pins = vec![0; v.len()];
        for i in order {
            let (net, _) = v[i].expect("filtered");
            let delays = match source(net)? {
                Some(s) => search(b.db, node_delay, s, &nodes),
                None => vec![Ps::INFINITY; nodes.len()],
            };
            let best = (0..candidates.len())
                .filter(|&j| free[j])
                .min_by(|&x, &y| delays[x].total_cmp(&delays[y]).then(x.cmp(&y)))
                .context("a LUT with more inputs than pins")?;
            free[best] = false;
            pins[i] = candidates[best];
        }
        // An input nothing drives (none in a mapped LUT) takes what's left.
        for (i, p) in pins.iter_mut().enumerate() {
            if v[i].is_none() {
                let j = free.iter().position(|&f| f).context("a LUT with more inputs than pins")?;
                free[j] = false;
                *p = candidates[j];
            }
        }
        out.insert(key, pins);
    }
    Ok(out)
}

/// The delay from `from` to each of `to` by Dijkstra over the routing
/// graph, infinite for one not settled within [`PIN_SEARCH_NODES`].
fn search(db: &ChipDb, node_delay: &[Ps], from: NodeId, to: &[NodeId]) -> Vec<Ps> {
    use std::cmp::Reverse;
    let mut out = vec![Ps::INFINITY; to.len()];
    let mut left = to.len();
    let mut best: HashMap<NodeId, Ps> = HashMap::from([(from, 0.0)]);
    let mut heap = std::collections::BinaryHeap::from([(Reverse(0u32), from)]);
    let mut settled = 0;
    while let Some((Reverse(at), n)) = heap.pop() {
        let at = f32::from_bits(at);
        if best.get(&n).is_some_and(|&b| b < at) {
            continue;
        }
        for (j, &t) in to.iter().enumerate() {
            if t == n && out[j].is_infinite() {
                out[j] = at;
                left -= 1;
            }
        }
        settled += 1;
        if left == 0 || settled >= PIN_SEARCH_NODES {
            break;
        }
        for e in db.downhill(n) {
            let next = at + node_delay.get(e.dst as usize).copied().unwrap_or(0.0);
            if best.get(&e.dst).is_none_or(|&b| next < b) {
                best.insert(e.dst, next);
                // Non-negative floats order as their bits.
                heap.push((Reverse(next.to_bits()), e.dst));
            }
        }
    }
    out
}

impl Builder<'_> {
    fn bram_pin(&self, site: &str, pin: &str) -> Result<String> {
        bram_pin(self.db, site, pin)
    }

    fn at(&self, c: CellId) -> Result<(&str, Bel)> {
        let (site, bel) = self.sites[c as usize]
            .as_ref()
            .with_context(|| format!("{} isn't placed", self.p.design.cells[c as usize].name))?;
        Ok((site.as_str(), *bel))
    }

    /// The cell driving `net` through `port`, if it is in `site` at `bel`.
    fn driven_from(&self, net: NetId, site: &str, bel: Bel, port: &str) -> bool {
        let Some((drv, p)) = &self.p.design.nets[net as usize].driver else {
            return false;
        };
        p == port && self.sites[*drv as usize].as_ref().is_some_and(|(s, b)| s == site && *b == bel)
    }

    /// Where a cell's input `port` (on `net`) comes in.
    fn input(&self, c: CellId, port: &str, net: NetId) -> Result<End> {
        let cell = &self.p.design.cells[c as usize];
        let (site, bel) = self.at(c)?;
        let pin = |name: String| Ok(End::Pin(site.to_string(), name));
        match (&cell.kind, bel) {
            (Kind::Lut { dram: false }, Bel::Lut6(k) | Bel::Lut5(k)) => {
                let i: usize = port.strip_prefix('A').and_then(|i| i.parse().ok()).context("a LUT input")?;
                pin(format!("{}{}", LETTERS[k], self.lut_pin(site, k, i - 1)))
            }
            (Kind::Lut { dram: true }, Bel::Lut6(k) | Bel::Lut5(k)) => {
                let six = matches!(bel, Bel::Lut6(_));
                match port {
                    // The 5LUT's half (a RAM32M's bit 0) is written from
                    // <L>I (D's from DI), the 6LUT's from <L>X: Vivado's
                    // RAM32M, checked by importing chipsmith's routing into
                    // it (`crate::vivado`), whose bitstream then sets the
                    // same bits.
                    "DI" if !six && k == 3 => pin("DI".into()),
                    "DI" if !six => pin(format!("{}I", LETTERS[k])),
                    "DI" => pin(format!("{}X", LETTERS[k])),
                    "WE" => pin("WE".into()),
                    "CLK" => pin("CLK".into()),
                    p if p.starts_with("WA") => Ok(End::Internal),
                    p => {
                        let i = p.strip_prefix('A').context("a RAM address")?;
                        pin(format!("{}{i}", LETTERS[k]))
                    }
                }
            }
            (Kind::Ff(_), Bel::Ff5(k)) => match port {
                "D" if self.driven_from(net, site, Bel::Lut5(k), "O6") => Ok(End::Internal),
                "D" => pin(format!("{}X", LETTERS[k])),
                "CK" => pin("CLK".into()),
                "CE" => pin("CE".into()),
                "SR" => pin("SR".into()),
                _ => bail!("{}: register port {port}", cell.name),
            },
            (Kind::Ff(_), Bel::Ff(k)) => match port {
                "D" => {
                    if self.driven_from(net, site, Bel::Lut6(k), "O6")
                        || self.driven_from(net, site, Bel::Carry4, &format!("O{k}"))
                    {
                        Ok(End::Internal)
                    } else {
                        pin(format!("{}X", LETTERS[k]))
                    }
                }
                "CK" => pin("CLK".into()),
                "CE" => pin("CE".into()),
                "SR" => pin("SR".into()),
                _ => bail!("{}: register port {port}", cell.name),
            },
            (Kind::Carry, Bel::Carry4) => match port {
                "CIN" => Ok(End::Internal),
                "CYINIT" => pin("AX".into()),
                p if p.starts_with('S') => {
                    let k: usize = p[1..].parse()?;
                    ensure!(
                        self.driven_from(net, site, Bel::Lut6(k), "O6"),
                        "{}: S{k} isn't the 6LUT beside it",
                        cell.name
                    );
                    Ok(End::Internal)
                }
                p if p.starts_with("DI") => {
                    let k: usize = p[2..].parse()?;
                    pin(format!("{}X", LETTERS[k]))
                }
                _ => bail!("{}: CARRY4 port {port}", cell.name),
            },
            // The buffer's I (what the pin drives) is the IOB's O.
            (Kind::Pad { site, .. }, _) => match port {
                "I" => Ok(End::Pin(site.clone(), "O".into())),
                "T" => Ok(End::Pin(site.clone(), "T".into())),
                _ => bail!("{}: pin buffer port {port}", cell.name),
            },
            (Kind::Bufg, _) | (Kind::Bscan { .. }, _) | (Kind::Cmt { .. }, _) => pin(port.to_string()),
            (Kind::Bram { .. }, _) => pin(self.bram_pin(site, port)?),
            (Kind::Dsp { .. }, _) => pin(port.to_string()),
            (kind, bel) => bail!("{}: {kind:?} at {bel:?}", cell.name),
        }
    }

    /// Where a cell's output `port` leaves its site, and the output mux it
    /// sets: `(site, pin, (letter, OUTMUX input))`.
    fn output(&self, c: CellId, port: &str) -> Result<(String, String, Option<(usize, &'static str)>)> {
        let cell = &self.p.design.cells[c as usize];
        let (site, bel) = self.at(c)?;
        let s = site.to_string();
        Ok(match (&cell.kind, bel) {
            (Kind::Lut { .. }, Bel::Lut6(k)) => (s, LETTERS[k].to_string(), None),
            (Kind::Lut { .. }, Bel::Lut5(k)) => (s, format!("{}MUX", LETTERS[k]), Some((k, "O5"))),
            (Kind::Ff(_), Bel::Ff5(k)) => (s, format!("{}MUX", LETTERS[k]), Some((k, "5Q"))),
            (Kind::Ff(_), Bel::Ff(k)) => (s, format!("{}Q", LETTERS[k]), None),
            (Kind::Carry, Bel::Carry4) => {
                let (k, mux) = if let Some(k) = port.strip_prefix("CO") {
                    (k.parse::<usize>()?, "CY")
                } else {
                    (port[1..].parse::<usize>()?, "XOR")
                };
                (s, format!("{}MUX", LETTERS[k]), Some((k, mux)))
            }
            (Kind::Pad { site, .. }, _) => (site.clone(), "I".into(), None),
            (Kind::Bufg, _) | (Kind::Bscan { .. }, _) | (Kind::Cmt { .. }, _) => (s, port.to_string(), None),
            (Kind::Bram { .. }, _) => (s.clone(), self.bram_pin(&s, port)?, None),
            (Kind::Dsp { .. }, _) => (s, port.to_string(), None),
            (kind, bel) => bail!("{}: output {port} of {kind:?} at {bel:?}", cell.name),
        })
    }
}

/// The PS7's input pins, from prjxray-db's `site_type_PS7.json` (the site's
/// pins with wires in its tile).
fn ps7_inputs(db: &ChipDb, family: &std::path::Path) -> Result<Vec<String>> {
    let path = family.join("site_type_PS7.json");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let s = db.sites.get("PS7_X0Y0").context("no PS7 site")?;
    let bel = &db.tile_type(s.tile).bels[&s.relative];
    let mut out: Vec<String> = json["site_pins"]
        .as_object()
        .context("site_pins")?
        .iter()
        .filter(|(name, p)| p["direction"] == "IN" && bel.pins.contains_key(name.as_str()))
        .map(|(name, _)| name.clone())
        .collect();
    out.sort();
    Ok(out)
}

/// For every node, the nearest node whose wire is `name` (`GND_WIRE` or
/// `VCC_WIRE`) upstream of it, by pips: a breadth-first search from all of
/// them at once.
fn nearest_source(db: &ChipDb, names: &[&str]) -> Vec<NodeId> {
    let mut from = vec![NodeId::MAX; db.nodes()];
    let mut queue = VecDeque::new();
    let mut by_type: HashMap<u32, Vec<u32>> = HashMap::new();
    for tile in 0..db.width * db.height {
        let ty = db.tile_type_index(tile);
        let wires = by_type
            .entry(ty)
            .or_insert_with(|| names.iter().filter_map(|n| db.tile_type(tile).wire(n)).collect())
            .clone();
        for w in wires {
            let n = db.node(tile, w);
            if from[n as usize] == NodeId::MAX {
                from[n as usize] = n;
                queue.push_back(n);
            }
        }
    }
    while let Some(n) = queue.pop_front() {
        for e in db.downhill(n) {
            if from[e.dst as usize] == NodeId::MAX {
                from[e.dst as usize] = from[n as usize];
                queue.push_back(e.dst);
            }
        }
    }
    from
}

/// The physical design: `family` is prjxray-db's directory for the part's
/// family (`.../zynq7`), for the PS7's pins.
pub(crate) fn build(
    db: &ChipDb,
    p: &Packed,
    placement: &Placement,
    family: &std::path::Path,
    node_delay: &[Ps],
) -> Result<Physical> {
    let d = &p.design;
    let b = Builder {
        db,
        p,
        sites: &placement.sites,
        five_used: placement
            .sites
            .iter()
            .flatten()
            .filter_map(|(site, bel)| match bel {
                Bel::Lut5(k) => Some((site.clone(), *k)),
                _ => None,
            })
            .collect(),
        pins: HashMap::new(),
    };
    let pins = route_pins(&b, placement, node_delay)?;
    let b = Builder { pins, ..b };
    let mut slices: BTreeMap<String, SliceConf> = BTreeMap::new();
    let slice = |slices: &mut BTreeMap<String, SliceConf>, site: &str| -> Result<()> {
        if !slices.contains_key(site) {
            let s = db.sites.get(site).with_context(|| format!("no site {site}"))?;
            let even = s.relative.starts_with("SLICE_X0");
            let half = match (s.ty.as_str(), even) {
                ("SLICEM", _) => "SLICEM_X0",
                (_, true) => "SLICEL_X0",
                (_, false) => "SLICEL_X1",
            };
            slices.insert(
                site.to_string(),
                SliceConf {
                    tile: s.tile,
                    half: half.to_string(),
                    ..Default::default()
                },
            );
        }
        Ok(())
    };

    // Constant sinks: (node, constant net, ports). A PS7 input the fabric
    // can't reach at all (a dedicated PS pin) is left alone; nextpnr ties
    // only the others, the same 1,770.
    let mut const_sinks: Vec<(NodeId, NetId, Vec<(CellId, String)>)> = Vec::new();
    let mut optional: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    let mut nets: Vec<RouteNet> = Vec::new();
    for (n, net) in d.nets.iter().enumerate() {
        let n = n as NetId;
        let mut sinks: Vec<(NodeId, Vec<(CellId, String)>)> = Vec::new();
        for (u, port) in &net.users {
            if let End::Pin(site, pin) = b.input(*u, port, n)? {
                let node = pin_node(db, &site, &pin)
                    .with_context(|| format!("{}.{port}", d.cells[*u as usize].name))?;
                match sinks.iter_mut().find(|(s, _)| *s == node) {
                    Some((_, ports)) => ports.push((*u, port.clone())),
                    None => sinks.push((node, vec![(*u, port.clone())])),
                }
            }
        }
        if sinks.is_empty() {
            continue;
        }
        if n == p.gnd || n == p.vcc {
            const_sinks.extend(sinks.into_iter().map(|(node, ports)| (node, n, ports)));
            continue;
        }
        let (drv, port) = net
            .driver
            .as_ref()
            .with_context(|| format!("a net read by {} has no driver", d.cells[net.users[0].0 as usize].name))?;
        let (site, pin, mux) = b.output(*drv, port)?;
        if let Some((k, input)) = mux {
            slice(&mut slices, &site)?;
            let s = slices.get_mut(&site).expect("added");
            ensure!(
                s.outmux[k].is_none_or(|m| m == input),
                "{}: {}MUX is wanted for both {} and {input}",
                site,
                LETTERS[k],
                s.outmux[k].unwrap()
            );
            s.outmux[k] = Some(input);
        }
        let source = pin_node(db, &site, &pin).with_context(|| format!("{}.{port}", d.cells[*drv as usize].name))?;
        nets.push(RouteNet {
            name: format!("{}.{port}", d.cells[*drv as usize].name),
            source,
            sinks: sinks.iter().map(|s| s.0).collect(),
            net: n,
            sink_ports: sinks.into_iter().map(|s| s.1).collect(),
        });
    }

    // Site configuration.
    let mut pads = Vec::new();
    let mut bufgs = Vec::new();
    let mut bscans = Vec::new();
    let mut ps7 = false;
    let mut brams = Vec::new();
    let mut dsps = Vec::new();
    let mut cmts = Vec::new();
    for (c, cell) in d.cells.iter().enumerate() {
        let c = c as CellId;
        match (&cell.kind, &p.conf[c as usize]) {
            (Kind::Lut { dram: false }, Conf::Lut { init }) => {
                let (site, bel) = b.at(c)?;
                let inputs = cell.pins.iter().filter(|(p, _)| p.starts_with('A')).count();
                let (Bel::Lut6(k) | Bel::Lut5(k)) = bel else {
                    bail!("{}: a logic LUT in {bel:?}", cell.name);
                };
                let pins: Vec<usize> = (0..inputs).map(|i| b.lut_pin(site, k, i)).collect();
                slice(&mut slices, site)?;
                let s = slices.get_mut(site).unwrap();
                match bel {
                    Bel::Lut6(k) => s.lut_init[k] = Some(expand(*init, &pins)),
                    Bel::Lut5(k) => {
                        s.lut5_init[k] = Some(expand(*init, &pins) & 0xffff_ffff);
                        const_sinks.push((pin_node(db, site, &format!("{}6", LETTERS[k]))?, p.vcc, Vec::new()));
                    }
                    _ => bail!("{}: a logic LUT in {bel:?}", cell.name),
                }
            }
            (Kind::Lut { dram: true }, Conf::RamLut { init, wclk_inv }) => {
                let (site, bel) = b.at(c)?;
                slice(&mut slices, site)?;
                let s = slices.get_mut(site).unwrap();
                let (k, shift) = match bel {
                    Bel::Lut6(k) => (k, 32),
                    Bel::Lut5(k) => (k, 0),
                    _ => bail!("{}: a RAM LUT in {bel:?}", cell.name),
                };
                let t = s.lut_init[k].get_or_insert(0);
                *t |= u64::from(*init) << shift;
                s.lut_ram[k] = true;
                s.clkinv = *wclk_inv;
                if shift == 32 {
                    const_sinks.push((pin_node(db, site, &format!("{}6", LETTERS[k]))?, p.vcc, Vec::new()));
                }
            }
            (Kind::Ff(cs), Conf::Ff { init, srval }) => {
                let (site, bel) = b.at(c)?;
                let conf = FfConf {
                    zini: !init,
                    zrst: !srval,
                };
                slice(&mut slices, site)?;
                let dnet = cell.net("D").context("a register's D")?;
                let k = match bel {
                    Bel::Ff(k) => k,
                    Bel::Ff5(k) => {
                        let inner = b.input(c, "D", dnet)? == End::Internal;
                        let s = slices.get_mut(site).unwrap();
                        s.ff5[k] = Some(conf);
                        s.ff5mux[k] = Some(if inner { "IN_A" } else { "IN_B" });
                        s.clkinv = cs.clk_inv;
                        s.ffsync = cs.sync;
                        s.srused = cs.sr.is_some();
                        s.ceused = cs.ce.is_some();
                        continue;
                    }
                    _ => bail!("{}: a register in {bel:?}", cell.name),
                };
                let s = slices.get_mut(site).unwrap();
                s.ff[k] = Some(conf);
                s.clkinv = cs.clk_inv;
                s.ffsync = cs.sync;
                s.srused = cs.sr.is_some();
                s.ceused = cs.ce.is_some();
                s.ffmux[k] = Some(match b.input(c, "D", dnet)? {
                    End::Pin(..) => ["AX", "BX", "CX", "DX"][k],
                    End::Internal if b.driven_from(dnet, site, Bel::Lut6(k), "O6") => "O6",
                    End::Internal => "XOR",
                });
            }
            (Kind::Carry, Conf::Carry { cyinit }) => {
                let (site, _) = b.at(c)?;
                slice(&mut slices, site)?;
                slices.get_mut(site).unwrap().carry = Some(match (cell.net("CIN"), cyinit) {
                    (Some(_), _) => PreCyInit::Cin,
                    (None, Some(false)) => PreCyInit::Zero,
                    (None, Some(true)) => PreCyInit::One,
                    (None, None) => PreCyInit::Ax,
                });
            }
            (Kind::Pad { site, .. }, Conf::Pad) => pads.push(PadConf {
                site: site.clone(),
                input: cell.net("O").is_some(),
                output: cell.net("I").is_some(),
            }),
            (Kind::Bufg, Conf::Bufg) => {
                let (site, _) = b.at(c)?;
                for pin in ["CE0", "S0", "IGNORE0", "CE1", "S1", "IGNORE1"] {
                    const_sinks.push((pin_node(db, site, pin)?, p.vcc, Vec::new()));
                }
                bufgs.push(site.to_string());
            }
            (Kind::Bscan { chain }, Conf::Bscan) => bscans.push((b.at(c)?.0.to_string(), *chain)),
            (Kind::Ps7, Conf::Ps7) => ps7 = true,
            (Kind::Cmt { .. }, Conf::Cmt(conf)) => {
                let (site, _) = b.at(c)?;
                cmts.push(CmtSite {
                    tile: db.sites[site].tile,
                    config: conf.config.clone(),
                    reset: cell.net("RST").is_some(),
                });
            }
            (Kind::Dsp { .. }, Conf::Dsp(config)) => {
                let (site, _) = b.at(c)?;
                let s = &db.sites[site];
                dsps.push(DspConf {
                    tile: s.tile,
                    which: usize::from(!s.relative.ends_with('0')),
                    config: (**config).clone(),
                });
            }
            (Kind::Bram { .. }, Conf::Bram(config)) => {
                let (site, _) = b.at(c)?;
                let s = &db.sites[site];
                let half = match s.ty.as_str() {
                    "RAMBFIFO36E1" => None,
                    _ if s.relative.ends_with('0') => Some(0),
                    _ => Some(1),
                };
                brams.push(BramConf {
                    tile: s.tile,
                    half,
                    config: (**config).clone(),
                });
            }
            (Kind::Gnd, Conf::Gnd) | (Kind::Vcc, Conf::Vcc) => {}
            (kind, conf) => bail!("{}: {kind:?} configured as {conf:?}", cell.name),
        }
    }
    if ps7 {
        for pin in ps7_inputs(db, family)? {
            let node = pin_node(db, "PS7_X0Y0", &pin)?;
            optional.insert(node);
            const_sinks.push((node, p.gnd, Vec::new()));
        }
    }

    // Constant sinks, each from its nearest source of the constant.
    // A DSP tile ties its own pins (DSP_GND_L, ...), as Vivado does.
    for (constant, wire, names) in [
        (p.gnd, "GND_WIRE", ["GND_WIRE", "DSP_GND_L", "DSP_GND_R"]),
        (p.vcc, "VCC_WIRE", ["VCC_WIRE", "DSP_VCC_L", "DSP_VCC_R"]),
    ] {
        let mine: Vec<&(NodeId, NetId, Vec<(CellId, String)>)> =
            const_sinks.iter().filter(|s| s.1 == constant).collect();
        if mine.is_empty() {
            continue;
        }
        let from = nearest_source(db, &names);
        let mut groups: HashMap<NodeId, Vec<(NodeId, Vec<(CellId, String)>)>> = HashMap::new();
        for (node, _, ports) in mine {
            let src = from[*node as usize];
            if src == NodeId::MAX && optional.contains(node) {
                continue;
            }
            ensure!(
                src != NodeId::MAX,
                "no {wire} reaches {}",
                ports
                    .first()
                    .map_or_else(|| format!("node {node}"), |(c, p)| format!("{}.{p}", d.cells[*c as usize].name))
            );
            let g = groups.entry(src).or_default();
            match g.iter_mut().find(|(n, _)| n == node) {
                Some((_, ps)) => ps.extend(ports.iter().cloned()),
                None => g.push((*node, ports.clone())),
            }
        }
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by_key(|g| g.0);
        for (src, sinks) in groups {
            nets.push(RouteNet {
                name: format!("{wire}#{src}"),
                source: src,
                sinks: sinks.iter().map(|s| s.0).collect(),
                net: constant,
                sink_ports: sinks.into_iter().map(|s| s.1).collect(),
            });
        }
    }

    Ok(Physical {
        nets,
        brams,
        dsps,
        cmts,
        slices,
        pads,
        bufgs,
        bscans,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_luts_repeat_over_the_unused_inputs() {
        // A1 & A2 on a LUT6: every 4 bits 0b1000.
        assert_eq!(expand(0b1000, &[1, 2]), 0x8888_8888_8888_8888);
        // A constant 1.
        assert_eq!(expand(1, &[]), u64::MAX);
        // A feed-through of A1.
        assert_eq!(expand(0b10, &[1]), 0xaaaa_aaaa_aaaa_aaaa);
        // ... of A6: the upper half.
        assert_eq!(expand(0b10, &[6]), 0xffff_ffff_0000_0000);
        // I0 & !I1 with I0 on A6, I1 on A5.
        assert_eq!(expand(0b0010, &[6, 5]), 0x0000_ffff_0000_0000);
    }
}
