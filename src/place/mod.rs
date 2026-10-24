//! chipsmith's placer: simulated annealing over LAB slots.
//!
//! Input is a packed design in nextpnr's JSON (`pack`, or `nextpnr-himbaechel
//! --pack-only --write`): LUTs, registers, pin buffers and clock controls,
//! with carry polarity and register packing already settled. Output is the
//! same JSON with a `NEXTPNR_BEL` on every cell, which the router and cram
//! read.
//!
//! What moves is an LE (a LUT and the register its output feeds, or either
//! alone) or a whole carry chain, which starts at LE 0 of a LAB and runs
//! LE k -> k+1, LE 15 -> LE 0 of the LAB below. A LAB holds 16 LEs and has
//! to satisfy the rules `lab_ok` checks; pins sit where the pin assignments
//! say, clock controls are the nearest free one the clock pin reaches. M4K
//! RAM blocks move between the chip's M4K sites, swapping with each other.

pub mod xc7;
pub mod xc7_delay;

use std::borrow::Cow;
use std::collections::VecDeque;

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

use crate::hard;
use crate::m4k;
use crate::route::chipdb::{ChipDb, NodeId, Pad, PadIo};
use crate::timing::{self, Arc, Ps};

const LES_PER_LAB: usize = 16;
/// Signals from outside a LAB arrive on its 38 local lines, each LE input
/// reaching about half of them. nextpnr's cyclone2 uarch measured 34 as the
/// most that keeps a full LAB routable on picorv32.
const MAX_LAB_INPUTS: usize = 34;
const LUT_INPUTS: [Port; 4] = [Port::DataA, Port::DataB, Port::DataC, Port::DataD];

/// Every cell port the packed netlist uses: a closed set, so cells keep
/// their nets in an array indexed by it rather than a map keyed by name.
/// M4K pins, of which there are many but only on a few cells, are kept
/// apart (`Cell::m4k`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Port {
    DataA,
    DataB,
    DataC,
    DataD,
    Cin,
    Cout,
    Combout,
    Datain,
    Sdata,
    Clk,
    Ena,
    Aclr,
    Sclr,
    Regout,
    Inclk,
    Outclk,
    Dataout,
    Oe,
    M4k(m4k::Pin),
    Hard(hard::Pin),
}

impl Port {
    const FIXED: [Port; 18] = [
        Port::DataA,
        Port::DataB,
        Port::DataC,
        Port::DataD,
        Port::Cin,
        Port::Cout,
        Port::Combout,
        Port::Datain,
        Port::Sdata,
        Port::Clk,
        Port::Ena,
        Port::Aclr,
        Port::Sclr,
        Port::Regout,
        Port::Inclk,
        Port::Outclk,
        Port::Dataout,
        Port::Oe,
    ];

    /// The netlist's name of each port, in `FIXED` order.
    const NAMES: [&'static str; 18] = [
        "DATAA", "DATAB", "DATAC", "DATAD", "CIN", "COUT", "COMBOUT", "DATAIN", "SDATA", "CLK",
        "ENA", "ACLR", "SCLR", "REGOUT", "INCLK", "OUTCLK", "DATAOUT", "OE",
    ];

    fn parse(name: &str) -> Option<Port> {
        match Self::NAMES.iter().position(|&n| n == name) {
            Some(i) => Some(Self::FIXED[i]),
            None => m4k::Pin::parse(name)
                .map(Port::M4k)
                .or_else(|| hard::Pin::parse(name).map(Port::Hard)),
        }
    }

    /// Slot in `Cell::ports`; None for an M4K pin.
    fn index(self) -> Option<usize> {
        Some(match self {
            Port::DataA => 0,
            Port::DataB => 1,
            Port::DataC => 2,
            Port::DataD => 3,
            Port::Cin => 4,
            Port::Cout => 5,
            Port::Combout => 6,
            Port::Datain => 7,
            Port::Sdata => 8,
            Port::Clk => 9,
            Port::Ena => 10,
            Port::Aclr => 11,
            Port::Sclr => 12,
            Port::Regout => 13,
            Port::Inclk => 14,
            Port::Outclk => 15,
            Port::Dataout => 16,
            Port::Oe => 17,
            Port::M4k(_) | Port::Hard(_) => return None,
        })
    }

    fn name(self) -> Cow<'static, str> {
        match self.index() {
            Some(i) => Self::NAMES[i].into(),
            None => match self {
                Port::M4k(pin) => pin.to_string().into(),
                Port::Hard(pin) => pin.name().into(),
                _ => unreachable!("only M4K and hard-block pins have no slot"),
            },
        }
    }
}

/// A net, renumbered densely so per-net data lives in `Vec`s.
type NetId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Lut,
    Ff,
    Ibuf,
    Obuf,
    /// Tri-state output or bidir.
    Iobuf,
    ClkCtrl,
    M4k,
    Hard(hard::Block),
}

#[derive(Debug)]
struct Cell {
    name: String,
    kind: Kind,
    ports: [Option<NetId>; Port::FIXED.len()],
    /// An M4K's connected pins.
    m4k: Vec<(m4k::Pin, NetId)>,
    /// A hard block's connected pins.
    hard: Vec<(hard::Pin, NetId)>,
    ena_inv: bool,
    sclr_inv: bool,
    /// Pin buffers: the top-level port they serve (`LEDR[0]`).
    port: Option<String>,
}

impl Cell {
    fn net(&self, port: Port) -> Option<NetId> {
        match (port.index(), port) {
            (Some(i), _) => self.ports[i],
            (None, Port::M4k(pin)) => self.m4k.iter().find(|(p, _)| *p == pin).map(|&(_, n)| n),
            (None, Port::Hard(pin)) => self.hard.iter().find(|(p, _)| *p == pin).map(|&(_, n)| n),
            (None, _) => None,
        }
    }

    fn connections(&self) -> impl Iterator<Item = (Port, NetId)> + '_ {
        Port::FIXED
            .into_iter()
            .filter_map(|p| Some((p, self.net(p)?)))
            .chain(self.m4k.iter().map(|&(pin, n)| (Port::M4k(pin), n)))
            .chain(self.hard.iter().map(|&(pin, n)| (Port::Hard(pin), n)))
    }
}

/// One LE's worth of logic.
#[derive(Debug, Clone, Copy)]
struct Le {
    lut: Option<usize>,
    ff: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub les: usize,
    pub chains: usize,
    pub initial_cost: u64,
    pub cost: u64,
    pub moves: u64,
    /// Estimated critical path, from placement distances alone.
    pub critical: Ps,
}

/// A position: tile coordinates, used for wirelength.
type Xy = (i32, i32);

struct Design {
    cells: Vec<Cell>,
    les: Vec<Le>,
    le_of: Vec<Option<usize>>,
    chains: Vec<Vec<usize>>,
    chain_of: Vec<Option<usize>>,
    /// M4K cells, and each cell's index among them.
    rams: Vec<usize>,
    ram_of: Vec<Option<usize>>,
    nets_of_ram: Vec<Vec<usize>>,
    /// Nets that count for wirelength: their cells. Clock nets (driven by
    /// a clock control) don't; they ride the global network.
    nets: Vec<Vec<usize>>,
    nets_of_le: Vec<Vec<usize>>,
    /// Driving cell of each net, for "is this LAB input local".
    driver: Vec<Option<usize>>,
    /// Every driver-to-load connection, for timing.
    conns: Vec<Conn>,
    timing: timing::Graph,
}

/// One driver-to-load connection.
#[derive(Debug, Clone, Copy)]
struct Conn {
    from_cell: usize,
    to_cell: usize,
    /// Timing nodes of the two ports.
    from: u32,
    to: u32,
    /// The wirelength net it belongs to, if any.
    net: Option<usize>,
    /// Needs no routing: a carry chain link, or a LUT into its own register.
    internal: bool,
}

fn read_design(json: &Value) -> Result<Design> {
    let mut cells = Vec::new();
    let mut net_ids: HashMap<u64, NetId> = HashMap::default();
    for (name, c) in json["modules"]["top"]["cells"]
        .as_object()
        .context("no cells")?
    {
        let kind = match c["type"].as_str().unwrap_or("") {
            "LCELL_COMB" => Kind::Lut,
            "LCELL_FF" => Kind::Ff,
            "CY2_IBUF" => Kind::Ibuf,
            "CY2_OBUF" => Kind::Obuf,
            "CY2_IOBUF" => Kind::Iobuf,
            "CLKCTRL" => Kind::ClkCtrl,
            "CY2_M4K" => Kind::M4k,
            other => match hard::Block::from_cell_type(other) {
                Some(b) => Kind::Hard(b),
                None => bail!("{name}: cell type {other} has nowhere to go"),
            },
        };
        let mut ports = [None; Port::FIXED.len()];
        let mut m4k = Vec::new();
        let mut hard = Vec::new();
        for (port, bits) in c["connections"].as_object().context("connections")? {
            if let Some(net) = bits
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_u64)
            {
                let p =
                    Port::parse(port).with_context(|| format!("{name}: unknown port {port}"))?;
                let next = net_ids.len() as NetId;
                let id = *net_ids.entry(net).or_insert(next);
                match (p.index(), p) {
                    (Some(i), _) => ports[i] = Some(id),
                    (None, Port::M4k(pin)) if kind == Kind::M4k => m4k.push((pin, id)),
                    (None, Port::Hard(pin)) if matches!(kind, Kind::Hard(b) if b.pins().contains(&pin)) => {
                        hard.push((pin, id))
                    }
                    _ => bail!("{name}: port {port} on a {kind:?}"),
                }
            }
        }
        let flag = |p: &str| {
            c["parameters"][p]
                .as_str()
                .is_some_and(|v| v.chars().any(|ch| ch == '1'))
        };
        cells.push(Cell {
            name: name.clone(),
            kind,
            ports,
            m4k,
            hard,
            ena_inv: flag("ENA_INV"),
            sclr_inv: flag("SCLR_INV"),
            port: c["attributes"]["PORT"].as_str().map(str::to_string),
        });
    }

    let net_count = net_ids.len();
    let mut driver: Vec<Option<usize>> = vec![None; net_count];
    let mut driver_port: Vec<Option<Port>> = vec![None; net_count];
    let mut users: Vec<Vec<(usize, Port)>> = vec![Vec::new(); net_count];
    for (i, c) in cells.iter().enumerate() {
        for (port, net) in c.connections() {
            let output = matches!(
                (c.kind, port),
                (Kind::Lut, Port::Combout | Port::Cout)
                    | (Kind::Ff, Port::Regout)
                    | (Kind::Ibuf | Kind::Iobuf, Port::Combout)
                    | (Kind::ClkCtrl, Port::Outclk)
            ) || matches!(port, Port::M4k(pin) if pin.is_output())
                || matches!(port, Port::Hard(pin) if pin.is_output());
            if output {
                ensure!(
                    driver[net as usize].replace(i).is_none(),
                    "{}: drives a net that already has a driver",
                    c.name
                );
                driver_port[net as usize] = Some(port);
            } else {
                users[net as usize].push((i, port));
            }
        }
    }

    // LEs: a register whose data comes from a LUT shares that LUT's LE.
    let mut le_of = vec![None; cells.len()];
    let mut les = Vec::new();
    for (i, c) in cells.iter().enumerate().filter(|(_, c)| c.kind == Kind::Ff) {
        let lut = match c.net(Port::Datain) {
            Some(net) => {
                let d =
                    driver[net as usize].with_context(|| format!("{}: DATAIN undriven", c.name))?;
                ensure!(
                    cells[d].kind == Kind::Lut && cells[d].net(Port::Combout) == Some(net),
                    "{}: DATAIN must come from a LUT",
                    c.name
                );
                ensure!(
                    le_of[d].is_none(),
                    "{} feeds two registers' DATAIN",
                    cells[d].name
                );
                Some(d)
            }
            None => None,
        };
        le_of[i] = Some(les.len());
        if let Some(d) = lut {
            le_of[d] = Some(les.len());
        }
        les.push(Le { lut, ff: Some(i) });
    }
    for (i, c) in cells.iter().enumerate() {
        if c.kind == Kind::Lut && le_of[i].is_none() {
            le_of[i] = Some(les.len());
            les.push(Le {
                lut: Some(i),
                ff: None,
            });
        }
    }

    // Carry chains, from their heads (COUT used, CIN not driven).
    let mut chains = Vec::new();
    let mut chain_of = vec![None; les.len()];
    let cout_user = |cell: usize| -> Result<Option<usize>> {
        let Some(net) = cells[cell].net(Port::Cout) else {
            return Ok(None);
        };
        match users[net as usize].as_slice() {
            [] => Ok(None),
            [(next, Port::Cin)] => Ok(Some(*next)),
            _ => bail!(
                "carry out of {} must feed exactly one carry in",
                cells[cell].name
            ),
        }
    };
    for (i, c) in cells.iter().enumerate() {
        let head = c.kind == Kind::Lut
            && c.net(Port::Cin)
                .is_none_or(|n| driver[n as usize].is_none());
        if !head || cout_user(i)?.is_none() {
            continue;
        }
        let mut chain = vec![le_of[i].unwrap()];
        let mut at = i;
        while let Some(next) = cout_user(at)? {
            chain.push(le_of[next].unwrap());
            at = next;
        }
        for &le in &chain {
            chain_of[le] = Some(chains.len());
        }
        chains.push(chain);
    }

    let clock_nets: HashSet<NetId> = cells
        .iter()
        .filter(|c| c.kind == Kind::ClkCtrl)
        .filter_map(|c| c.net(Port::Outclk))
        .collect();
    let mut by_net: Vec<Vec<usize>> = vec![Vec::new(); net_count];
    for (i, c) in cells.iter().enumerate() {
        for (port, net) in c.connections() {
            // Carry wires are fixed by the chain itself.
            if !clock_nets.contains(&net) && port != Port::Cin && port != Port::Cout {
                by_net[net as usize].push(i);
            }
        }
    }
    let mut keyed: Vec<(Vec<usize>, NetId)> = by_net
        .into_iter()
        .enumerate()
        .map(|(id, mut v)| {
            v.sort();
            v.dedup();
            (v, id as NetId)
        })
        .filter(|(v, _)| v.len() > 1)
        .collect();
    keyed.sort();
    let mut net_index: Vec<Option<usize>> = vec![None; net_count];
    for (i, (_, id)) in keyed.iter().enumerate() {
        net_index[*id as usize] = Some(i);
    }
    let nets: Vec<Vec<usize>> = keyed.into_iter().map(|(v, _)| v).collect();
    let rams: Vec<usize> = (0..cells.len())
        .filter(|&i| cells[i].kind == Kind::M4k)
        .collect();
    let mut ram_of = vec![None; cells.len()];
    for (r, &c) in rams.iter().enumerate() {
        ram_of[c] = Some(r);
    }
    let mut nets_of_le = vec![Vec::new(); les.len()];
    let mut nets_of_ram = vec![Vec::new(); rams.len()];
    for (n, members) in nets.iter().enumerate() {
        for &c in members {
            let of = match (le_of[c], ram_of[c]) {
                (Some(le), _) => &mut nets_of_le[le],
                (_, Some(r)) => &mut nets_of_ram[r],
                _ => continue,
            };
            if of.last() != Some(&n) {
                of.push(n);
            }
        }
    }

    // Timing: cell arcs, then every connection. A LUT's carry delay depends
    // on its LE index, which a chain fixes (head at LE 0); elsewhere it
    // doesn't matter.
    let mut timing = timing::Graph::default();
    for (i, c) in cells.iter().enumerate() {
        let ty = match c.kind {
            Kind::Lut => "LCELL_COMB",
            Kind::Ff => "LCELL_FF",
            Kind::M4k => "CY2_M4K",
            _ => continue,
        };
        let le = le_of[i]
            .and_then(|le| Some((le, chain_of[le]?)))
            .map_or(0, |(le, ch)| {
                chains[ch].iter().position(|&x| x == le).unwrap() % LES_PER_LAB
            });
        let ports: Vec<Cow<str>> = c.connections().map(|(p, _)| p.name()).collect();
        let ports: Vec<&str> = ports.iter().map(|p| p.as_ref()).collect();
        timing.add_cell(&c.name, ty, &ports, le as u8);
    }
    let mut conns = Vec::new();
    for (net, loads) in users.iter().enumerate() {
        let (Some(d), Some(dp)) = (driver[net], driver_port[net]) else {
            continue;
        };
        for &(c, p) in loads {
            conns.push(Conn {
                from_cell: d,
                to_cell: c,
                from: timing.node(&cells[d].name, &dp.name()),
                to: timing.node(&cells[c].name, &p.name()),
                net: net_index[net],
                internal: p == Port::Cin
                    || (p == Port::Datain && le_of[d].is_some() && le_of[d] == le_of[c]),
            });
        }
    }

    Ok(Design {
        cells,
        les,
        le_of,
        chains,
        chain_of,
        rams,
        ram_of,
        nets_of_ram,
        nets,
        nets_of_le,
        driver,
        conns,
        timing,
    })
}

/// The LABs and M4K sites of the chip.
struct Grid {
    /// LAB index -> its tile.
    tiles: Vec<u32>,
    xy: Vec<Xy>,
    at: HashMap<Xy, usize>,
    /// M4K site index -> its tile, and its position.
    ram_tiles: Vec<u32>,
    ram_xy: Vec<Xy>,
}

/// The M4K bel's name in its tile.
const M4K_BEL: &str = "M4K";

impl Grid {
    fn new(db: &ChipDb) -> Self {
        let (mut tiles, mut xy, mut at) = (Vec::new(), Vec::new(), HashMap::default());
        let (mut ram_tiles, mut ram_xy) = (Vec::new(), Vec::new());
        for tile in 0..db.width * db.height {
            let (x, y) = db.tile_xy(tile);
            let bels = &db.tile_type(tile).bels;
            if bels.contains_key("LE0.LUT") {
                at.insert((x as i32, y as i32), tiles.len());
                xy.push((x as i32, y as i32));
                tiles.push(tile);
            } else if bels.get(M4K_BEL).is_some_and(|b| b.ty == "M4K") {
                ram_xy.push((x as i32, y as i32));
                ram_tiles.push(tile);
            }
        }
        Grid {
            tiles,
            xy,
            at,
            ram_tiles,
            ram_xy,
        }
    }

    /// The LABs a chain of `len` LEs starting at `lab` occupies.
    fn chain_labs(&self, lab: usize, len: usize) -> Option<Vec<usize>> {
        let (x, y) = self.xy[lab];
        (0..len.div_ceil(LES_PER_LAB) as i32)
            .map(|i| self.at.get(&(x, y - i)).copied())
            .collect()
    }
}

/// Fixed cells: pins and clock controls, with their bel names.
fn fixed_cells(
    db: &ChipDb,
    design: &Design,
    pins: &std::collections::BTreeMap<String, String>,
) -> Result<HashMap<usize, (u32, String)>> {
    let mut out = HashMap::default();
    let pads: HashMap<&str, &Pad> = db.pads.iter().map(|p| (p.pin.as_str(), p)).collect();
    for (i, c) in design.cells.iter().enumerate() {
        let drives = match c.kind {
            Kind::Ibuf => false,
            Kind::Obuf | Kind::Iobuf => true,
            Kind::Lut | Kind::Ff | Kind::ClkCtrl | Kind::M4k => continue,
            // One of each per device, at a fixed bel.
            Kind::Hard(b) => {
                let ((x, y), bel) = b.bel();
                let tile = db.tile_index(x, y).context("hard block off chip")?;
                ensure!(
                    db.tile_type(tile).bels.contains_key(bel),
                    "the chip database has no {bel} bel at X{x}Y{y}; it predates cyclone2-re's hard blocks"
                );
                ensure!(
                    !out.values()
                        .any(|(t, b): &(u32, String)| *t == tile && b == bel),
                    "{}: a second {} in the design",
                    c.name,
                    b.cell_type()
                );
                out.insert(i, (tile, bel.to_string()));
                continue;
            }
        };
        let port = c
            .port
            .as_ref()
            .with_context(|| format!("{}: pin buffer without a top-level port", c.name))?;
        let pin = pins
            .get(port)
            .with_context(|| format!("no pin assignment for port {port}"))?;
        let ball = pin.strip_prefix("PIN_").unwrap_or(pin);
        let pad = pads
            .get(ball)
            .with_context(|| format!("port {port}: {pin} is not a user I/O pin"))?;
        let (tile, bel) = match (pad.io, drives) {
            (PadIo::ClockInput, true) => {
                bail!("port {port}: {pin} is a dedicated clock input, it can't drive")
            }
            (PadIo::Bidir, _) | (PadIo::ClockInput, false) => (pad.tile, pad.bel.as_str()),
        };
        ensure!(
            !out.values()
                .any(|(t, b): &(u32, String)| *t == tile && b == bel),
            "port {port}: {pin} is already used"
        );
        out.insert(i, (tile, bel.to_string()));
    }

    // INCLK node of every clock control.
    let mut inclk: HashMap<NodeId, (u32, String)> = HashMap::default();
    for tile in 0..db.width * db.height {
        for (name, bel) in &db.tile_type(tile).bels {
            if bel.ty == "CLKCTRL" {
                if let Some(&w) = bel.pins.get("INCLK") {
                    inclk.insert(db.node(tile, w), (tile, name.clone()));
                }
            }
        }
    }
    for (i, c) in design
        .cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind == Kind::ClkCtrl)
    {
        let net = c
            .net(Port::Inclk)
            .with_context(|| format!("{}: INCLK unconnected", c.name))?;
        let pin = design.driver[net as usize].context("clock control input undriven")?;
        ensure!(
            design.cells[pin].kind == Kind::Ibuf,
            "{}: clocks must come straight from a pin",
            c.name
        );
        let (tile, bel) = &out[&pin];
        let w = db.tile_type(*tile).bels[bel].pins["COMBOUT"];
        // Nearest free clock control the pin reaches, as nextpnr's packer picks it.
        let start = db.node(*tile, w);
        let mut seen = HashSet::from_iter([start]);
        let mut queue = VecDeque::from([(start, 0)]);
        let mut found = None;
        while let Some((n, depth)) = queue.pop_front() {
            if let Some(cc) = inclk.get(&n) {
                if !out.values().any(|v| v == cc) {
                    found = Some(cc.clone());
                    break;
                }
            }
            if depth > 8 {
                continue;
            }
            for e in db.downhill(n) {
                if seen.insert(e.dst) {
                    queue.push_back((e.dst, depth + 1));
                }
            }
        }
        out.insert(
            i,
            found.with_context(|| {
                format!(
                    "no free clock control reachable from {}",
                    design.cells[pin].name
                )
            })?,
        );
    }
    Ok(out)
}

/// The placement being annealed.
struct State<'a> {
    design: &'a Design,
    grid: &'a Grid,
    /// LE in each slot of each LAB.
    slots: Vec<[Option<usize>; LES_PER_LAB]>,
    /// (LAB, LE index) of each LE.
    pos: Vec<(usize, usize)>,
    /// Position of each fixed cell (pins, clock controls).
    fixed_xy: Vec<Option<Xy>>,
    /// M4K site of each M4K, and the M4K at each site.
    ram_site: Vec<usize>,
    site_ram: Vec<Option<usize>>,
    /// Each net's weight: 1, plus more the more timing-critical it is.
    weight: Vec<f32>,
    /// Each net's weighted wirelength.
    net_cost: Vec<f32>,
}

impl<'a> State<'a> {
    fn cell_xy(&self, cell: usize) -> Xy {
        let d = self.design;
        match (d.le_of[cell], d.ram_of[cell]) {
            (Some(le), _) => self.grid.xy[self.pos[le].0],
            (_, Some(r)) => self.grid.ram_xy[self.ram_site[r]],
            _ => self.fixed_xy[cell].expect("every cell is an LE, an M4K or fixed"),
        }
    }

    /// Half-perimeter wirelength; x counts routing and logic columns
    /// separately, so it is halved.
    fn cost(&self, net: usize) -> f32 {
        self.weight[net] * self.hpwl(net) as f32
    }

    fn hpwl(&self, net: usize) -> u32 {
        let mut it = self.design.nets[net].iter().map(|&c| self.cell_xy(c));
        let (x0, y0) = it.next().unwrap();
        let (mut lx, mut hx, mut ly, mut hy) = (x0, x0, y0, y0);
        for (x, y) in it {
            lx = lx.min(x);
            hx = hx.max(x);
            ly = ly.min(y);
            hy = hy.max(y);
        }
        ((hx - lx) / 2 + (hy - ly)) as u32
    }

    fn total(&self) -> f64 {
        self.net_cost.iter().map(|&c| c as f64).sum()
    }

    /// Unweighted wirelength, for reporting.
    fn wirelength(&self) -> u64 {
        (0..self.design.nets.len())
            .map(|n| self.hpwl(n) as u64)
            .sum()
    }

    /// A connection's delay as the placement stands: nothing inside an LE
    /// or along a carry chain, an LE's own LAB feedback line within a LAB,
    /// otherwise LE buffer and LAB line plus R4/C4 wire per LAB crossed
    /// (timing::wire_delay spread over the 4 LABs an R4/C4 spans).
    fn conn_delay(&self, c: &Conn) -> Ps {
        if c.internal {
            return 0.0;
        }
        let ((x0, y0), (x1, y1)) = (self.cell_xy(c.from_cell), self.cell_xy(c.to_cell));
        let (dx, dy) = ((x0 - x1).abs() / 2, (y0 - y1).abs());
        if dx == 0 && dy == 0 {
            return timing::wire_delay("LOCAL_LINE");
        }
        timing::wire_delay("LE_BUFFER")
            + timing::wire_delay("LOCAL_INTERCONNECT")
            + dx as Ps * timing::wire_delay("R4") / 4.0
            + dy as Ps * timing::wire_delay("C4") / 4.0
    }

    /// Re-run timing on the current placement and reweight every net by
    /// its most critical connection. Returns the estimated critical path.
    fn reweight(&mut self) -> Ps {
        let d = self.design;
        let arcs: Vec<Arc> = d
            .conns
            .iter()
            .map(|c| Arc {
                from: c.from,
                to: c.to,
                delay: self.conn_delay(c),
            })
            .collect();
        let analysis = d.timing.analyse(&arcs);
        self.weight.iter_mut().for_each(|w| *w = 1.0);
        for (c, a) in d.conns.iter().zip(&arcs) {
            if let Some(n) = c.net {
                let w = 1.0 + TIMING_WEIGHT * analysis.criticality(a).powf(CRIT_EXP);
                self.weight[n] = self.weight[n].max(w);
            }
        }
        for n in 0..d.nets.len() {
            self.net_cost[n] = self.cost(n);
        }
        analysis.critical
    }

    /// nextpnr's `isBelLocationValid` for one LAB. Hot: runs on every
    /// move, so it allocates nothing.
    fn lab_ok(&self, lab: usize) -> bool {
        let d = self.design;
        let cells = &d.cells;
        type Ctl = (Option<NetId>, bool);
        // A third distinct domain already fails, so three slots suffice.
        let mut domains: [(Option<NetId>, Ctl); 3] = [(None, (None, false)); 3];
        let mut n_domains = 0;
        let (mut ena, mut aclr, mut sclr): (Option<Ctl>, Option<NetId>, Option<Ctl>) =
            (None, None, None);
        let mut sync_kind = 0;
        let mut inputs = [0 as NetId; MAX_LAB_INPUTS + 1];
        let mut n_inputs = 0;
        let here = |net: NetId| {
            d.driver[net as usize]
                .and_then(|c| d.le_of[c])
                .is_some_and(|le| self.pos[le].0 == lab)
        };
        // False once the LAB has too many outside inputs.
        let mut add = |net: Option<NetId>| -> bool {
            let Some(n) = net else { return true };
            if here(n) || inputs[..n_inputs].contains(&n) {
                return true;
            }
            if n_inputs == MAX_LAB_INPUTS {
                return false;
            }
            inputs[n_inputs] = n;
            n_inputs += 1;
            true
        };
        let one = |slot: &mut Option<Ctl>, v: Ctl| match (v.0, *slot) {
            (None, _) => true,
            (Some(_), None) => {
                *slot = Some(v);
                true
            }
            (Some(_), Some(s)) => s == v,
        };
        for le in self.slots[lab].iter().flatten() {
            let Le { lut, ff } = d.les[*le];
            if let Some(l) = lut {
                for p in LUT_INPUTS {
                    if !add(cells[l].net(p)) {
                        return false;
                    }
                }
            }
            let Some(f) = ff else { continue };
            let c = &cells[f];
            for p in [Port::Sdata, Port::Ena, Port::Sclr, Port::Aclr] {
                if !add(c.net(p)) {
                    return false;
                }
            }
            let kind =
                c.net(Port::Sdata).is_some() as u8 | (c.net(Port::Sclr).is_some() as u8) << 1;
            if kind != 0 && sync_kind != 0 && kind != sync_kind {
                return false;
            }
            sync_kind |= kind;
            let e: Ctl = (c.net(Port::Ena), c.ena_inv);
            let domain = (c.net(Port::Clk), e);
            if !domains[..n_domains].contains(&domain) {
                if n_domains == 2 {
                    return false;
                }
                domains[n_domains] = domain;
                n_domains += 1;
            }
            if !one(&mut ena, e) || !one(&mut sclr, (c.net(Port::Sclr), c.sclr_inv)) {
                return false;
            }
            if let Some(a) = c.net(Port::Aclr) {
                if aclr.is_some_and(|x| x != a) {
                    return false;
                }
                aclr = Some(a);
            }
        }
        true
    }

    fn put(&mut self, le: usize, lab: usize, k: usize) {
        self.slots[lab][k] = Some(le);
        self.pos[le] = (lab, k);
    }
}

/// xorshift64*: deterministic, so the same design places the same way.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A move, kept so it can be undone.
enum Undo {
    /// Every LE that moved, and the LAB and slot it was in.
    Les(Vec<(usize, usize, usize)>),
    /// Every M4K that moved, and the site it was on.
    Rams(Vec<(usize, usize)>),
}

impl<'a> State<'a> {
    fn initial(design: &'a Design, grid: &'a Grid, fixed_xy: Vec<Option<Xy>>) -> Result<Self> {
        let mut s = State {
            design,
            grid,
            slots: vec![[None; LES_PER_LAB]; grid.tiles.len()],
            pos: vec![(usize::MAX, 0); design.les.len()],
            fixed_xy,
            ram_site: vec![usize::MAX; design.rams.len()],
            site_ram: vec![None; grid.ram_tiles.len()],
            weight: vec![1.0; design.nets.len()],
            net_cost: vec![0.0; design.nets.len()],
        };
        // Centre outwards.
        let (cx, cy) = grid
            .xy
            .iter()
            .fold((0i64, 0i64), |(a, b), &(x, y)| (a + x as i64, b + y as i64));
        let centre = (
            (cx / grid.xy.len() as i64) as i32,
            (cy / grid.xy.len() as i64) as i32,
        );
        let mut order: Vec<usize> = (0..grid.tiles.len()).collect();
        order.sort_by_key(|&l| {
            let (x, y) = grid.xy[l];
            (x - centre.0).abs() / 2 + (y - centre.1).abs()
        });

        for chain in &design.chains {
            let spot = order.iter().find_map(|&lab| {
                let labs = grid.chain_labs(lab, chain.len())?;
                let free = chain
                    .iter()
                    .enumerate()
                    .all(|(i, _)| s.slots[labs[i / LES_PER_LAB]][i % LES_PER_LAB].is_none());
                free.then_some(labs)
            });
            let labs = spot.context("no room for a carry chain")?;
            for (i, &le) in chain.iter().enumerate() {
                s.put(le, labs[i / LES_PER_LAB], i % LES_PER_LAB);
            }
            for &lab in &labs {
                ensure!(
                    s.lab_ok(lab),
                    "a carry chain's registers don't fit one LAB's controls"
                );
            }
        }

        let mut next = 0;
        for le in 0..design.les.len() {
            if design.chain_of[le].is_some() {
                continue;
            }
            let mut placed = false;
            for &lab in &order[next..] {
                let Some(k) = s.slots[lab].iter().position(Option::is_none) else {
                    continue;
                };
                s.put(le, lab, k);
                if s.lab_ok(lab) {
                    placed = true;
                    break;
                }
                s.slots[lab][k] = None;
            }
            ensure!(placed, "the design doesn't fit");
            // Skip LABs that are full for good.
            while next < order.len() && s.slots[order[next]].iter().all(Option::is_some) {
                next += 1;
            }
        }

        ensure!(
            design.rams.len() <= grid.ram_tiles.len(),
            "the design has {} M4K blocks, the chip {}",
            design.rams.len(),
            grid.ram_tiles.len()
        );
        let mut sites: Vec<usize> = (0..grid.ram_tiles.len()).collect();
        sites.sort_by_key(|&i| {
            let (x, y) = grid.ram_xy[i];
            ((x - centre.0).abs() / 2 + (y - centre.1).abs(), i)
        });
        for (r, &site) in sites.iter().take(design.rams.len()).enumerate() {
            s.ram_site[r] = site;
            s.site_ram[site] = Some(r);
        }
        for n in 0..design.nets.len() {
            s.net_cost[n] = s.cost(n);
        }
        Ok(s)
    }

    /// Try moving `le` (with its chain, if any) to `lab`/`k`. Returns the
    /// undo record, or None if the move is impossible or illegal.
    fn try_move(&mut self, le: usize, lab: usize, k: usize) -> Option<Undo> {
        let d = self.design;
        if let Some(c) = d.chain_of[le] {
            let chain = &d.chains[c];
            let labs = self.grid.chain_labs(lab, chain.len())?;
            let target = |i: usize| (labs[i / LES_PER_LAB], i % LES_PER_LAB);
            // Only into slots that are free or the chain's own.
            for i in 0..chain.len() {
                let (l, kk) = target(i);
                if self.slots[l][kk].is_some_and(|o| d.chain_of[o] != Some(c)) {
                    return None;
                }
            }
            let moved: Vec<(usize, usize, usize)> = chain
                .iter()
                .map(|&x| (x, self.pos[x].0, self.pos[x].1))
                .collect();
            for &x in chain {
                let (l, kk) = self.pos[x];
                self.slots[l][kk] = None;
            }
            for (i, &x) in chain.iter().enumerate() {
                let (l, kk) = target(i);
                self.put(x, l, kk);
            }
            let mut touched: Vec<usize> = moved.iter().map(|&(_, l, _)| l).chain(labs).collect();
            touched.sort();
            touched.dedup();
            let undo = Undo::Les(moved);
            if touched.iter().all(|&l| self.lab_ok(l)) {
                return Some(undo);
            }
            self.undo(&undo);
            return None;
        }
        let other = self.slots[lab][k];
        if other.is_some_and(|o| d.chain_of[o].is_some()) || other == Some(le) {
            return None;
        }
        let (from, fk) = self.pos[le];
        let mut undo = vec![(le, from, fk)];
        self.slots[from][fk] = None;
        if let Some(o) = other {
            undo.push((o, lab, k));
            self.put(o, from, fk);
        }
        self.put(le, lab, k);
        let undo = Undo::Les(undo);
        if self.lab_ok(lab) && (from == lab || self.lab_ok(from)) {
            return Some(undo);
        }
        self.undo(&undo);
        None
    }

    /// Move M4K `r` to `site`, swapping with the one there.
    fn try_move_ram(&mut self, r: usize, site: usize) -> Option<Undo> {
        let from = self.ram_site[r];
        if from == site {
            return None;
        }
        let mut moved = vec![(r, from)];
        if let Some(o) = self.site_ram[site] {
            moved.push((o, site));
            self.put_ram(o, from);
        } else {
            self.site_ram[from] = None;
        }
        self.put_ram(r, site);
        Some(Undo::Rams(moved))
    }

    fn put_ram(&mut self, r: usize, site: usize) {
        self.ram_site[r] = site;
        self.site_ram[site] = Some(r);
    }

    fn undo(&mut self, undo: &Undo) {
        match undo {
            Undo::Les(moved) => {
                for &(le, _, _) in moved {
                    let (l, k) = self.pos[le];
                    if self.slots[l][k] == Some(le) {
                        self.slots[l][k] = None;
                    }
                }
                for &(le, l, k) in moved {
                    self.put(le, l, k);
                }
            }
            Undo::Rams(moved) => {
                for &(r, _) in moved {
                    let site = self.ram_site[r];
                    if self.site_ram[site] == Some(r) {
                        self.site_ram[site] = None;
                    }
                }
                for &(r, site) in moved {
                    self.put_ram(r, site);
                }
            }
        }
    }

    fn nets_of(&self, undo: &Undo) -> Vec<usize> {
        let d = self.design;
        let mut nets: Vec<usize> = match undo {
            Undo::Les(moved) => moved
                .iter()
                .flat_map(|&(le, _, _)| d.nets_of_le[le].iter().copied())
                .collect(),
            Undo::Rams(moved) => moved
                .iter()
                .flat_map(|&(r, _)| d.nets_of_ram[r].iter().copied())
                .collect(),
        };
        nets.sort();
        nets.dedup();
        nets
    }
}

/// How much more a fully critical net's wirelength counts than an uncritical
/// one's, and how sharply criticality is weighted (VPR's criticality_exp).
const TIMING_WEIGHT: f32 = 8.0;
const CRIT_EXP: f32 = 4.0;

fn anneal(s: &mut State, rng: &mut Rng) -> u64 {
    let d = s.design;
    let movable: Vec<usize> = (0..d.les.len())
        .filter(|&le| d.chain_of[le].is_none_or(|c| d.chains[c][0] == le))
        .collect();
    if movable.is_empty() && d.rams.is_empty() {
        return 0;
    }
    let max_r = s
        .grid
        .xy
        .iter()
        .map(|&(x, y)| (x / 2).max(y))
        .max()
        .unwrap_or(1) as f64;
    let mut r = max_r;
    let objects = movable.len() + d.rams.len();
    let moves_per_t = (2.0 * (objects as f64).powf(4.0 / 3.0)).max(200.0) as usize;
    let mut total_moves = 0u64;

    let attempt = |s: &mut State, rng: &mut Rng, r: f64, t: f64| -> Option<f64> {
        let pick = rng.below(objects);
        let ri = r.max(1.0) as i32;
        let undo = if let Some(&le) = movable.get(pick) {
            let (x, y) = s.grid.xy[s.pos[le].0];
            let tx = x + 2 * (rng.below(2 * ri as usize + 1) as i32 - ri);
            let ty = y + rng.below(2 * ri as usize + 1) as i32 - ri;
            let &lab = s.grid.at.get(&(tx, ty))?;
            let k = if d.chain_of[le].is_some() {
                0
            } else {
                rng.below(LES_PER_LAB)
            };
            s.try_move(le, lab, k)?
        } else {
            // M4K columns are far apart: pick among the sites in range
            // rather than a point that is almost never one.
            let ram = pick - movable.len();
            let (x, y) = s.grid.ram_xy[s.ram_site[ram]];
            let near: Vec<usize> = (0..s.grid.ram_xy.len())
                .filter(|&i| {
                    let (sx, sy) = s.grid.ram_xy[i];
                    (sx - x).abs() / 2 <= ri && (sy - y).abs() <= ri
                })
                .collect();
            let site = near[rng.below(near.len())];
            s.try_move_ram(ram, site)?
        };
        let nets = s.nets_of(&undo);
        let before: f64 = nets.iter().map(|&n| s.net_cost[n] as f64).sum();
        let after_costs: Vec<f32> = nets.iter().map(|&n| s.cost(n)).collect();
        let delta = after_costs.iter().map(|&c| c as f64).sum::<f64>() - before;
        if delta <= 0.0 || (t > 0.0 && rng.unit() < (-delta / t).exp()) {
            for (&n, &c) in nets.iter().zip(&after_costs) {
                s.net_cost[n] = c;
            }
            Some(delta)
        } else {
            s.undo(&undo);
            None
        }
    };

    // Starting temperature: 20 standard deviations of random moves' deltas.
    let mut deltas = Vec::new();
    for _ in 0..movable.len().max(50) {
        if let Some(dc) = attempt(s, rng, r, f64::INFINITY) {
            deltas.push(dc);
        }
    }
    let mean = deltas.iter().sum::<f64>() / deltas.len().max(1) as f64;
    let var = deltas.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / deltas.len().max(1) as f64;
    let mut t = 20.0 * var.sqrt();

    loop {
        s.reweight();
        let mut accepted = 0;
        for _ in 0..moves_per_t {
            total_moves += 1;
            if attempt(s, rng, r, t).is_some() {
                accepted += 1;
            }
        }
        let rate = accepted as f64 / moves_per_t as f64;
        t *= match rate {
            r if r > 0.96 => 0.5,
            r if r > 0.8 => 0.9,
            r if r > 0.15 => 0.95,
            _ => 0.8,
        };
        r = (r * (1.0 - 0.44 + rate)).clamp(1.0, max_r);
        let cost = s.total();
        if t < 0.005 * cost / d.nets.len().max(1) as f64 {
            break;
        }
    }
    // A final greedy pass at zero temperature.
    for _ in 0..moves_per_t {
        total_moves += 1;
        attempt(s, rng, 1.0, 0.0);
    }
    total_moves
}

/// Place `packed`, write `placed`. `pins` maps top-level ports (`LEDR[0]`)
/// to pins (`PIN_AE23`).
pub fn place_file(
    db: &ChipDb,
    packed: &Path,
    pins: &std::collections::BTreeMap<String, String>,
    placed: &Path,
) -> Result<Stats> {
    let text =
        std::fs::read_to_string(packed).with_context(|| format!("reading {}", packed.display()))?;
    let mut json: Value = serde_json::from_str(&text)?;
    let design = read_design(&json)?;
    let fixed = fixed_cells(db, &design, pins)?;

    let grid = Grid::new(db);
    let mut fixed_xy = vec![None; design.cells.len()];
    for (&c, &(tile, _)) in &fixed {
        let (x, y) = db.tile_xy(tile);
        fixed_xy[c] = Some((x as i32, y as i32));
    }
    let mut state = State::initial(&design, &grid, fixed_xy)?;
    let initial_cost = state.wirelength();
    let moves = anneal(&mut state, &mut Rng(0x9e37_79b9_7f4a_7c15));
    let critical = state.reweight();

    let cells = json["modules"]["top"]["cells"]
        .as_object_mut()
        .context("no cells")?;
    for (i, c) in design.cells.iter().enumerate() {
        let (tile, bel) = match (design.le_of[i], design.ram_of[i]) {
            (Some(le), _) => {
                let (lab, k) = state.pos[le];
                let half = if c.kind == Kind::Lut { "LUT" } else { "FF" };
                (grid.tiles[lab], format!("LE{k}.{half}"))
            }
            (_, Some(r)) => (grid.ram_tiles[state.ram_site[r]], M4K_BEL.to_string()),
            _ => fixed[&i].clone(),
        };
        let (x, y) = db.tile_xy(tile);
        let attrs = cells
            .get_mut(&c.name)
            .context("cell vanished")?
            .as_object_mut()
            .context("cell is not an object")?
            .entry("attributes")
            .or_insert_with(|| Value::Object(Default::default()));
        attrs["NEXTPNR_BEL"] = Value::String(format!("X{x}Y{y}/{bel}"));
    }
    std::fs::write(placed, serde_json::to_string(&json)?)
        .with_context(|| format!("writing {}", placed.display()))?;
    Ok(Stats {
        les: design.les.len(),
        chains: design.chains.len(),
        initial_cost,
        cost: state.wirelength(),
        moves,
        critical,
    })
}
