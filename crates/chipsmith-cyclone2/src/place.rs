//! chipsmith's placer: analytic global placement (`chipsmith_place`),
//! legalised into LAB slots, then a short simulated anneal over them.
//!
//! Input is a packed design in nextpnr's JSON (`pack`, or `nextpnr-himbaechel
//! --pack-only --write`): LUTs, registers, pin buffers and clock controls,
//! with carry polarity and register packing already settled. Output is the
//! same JSON with a `NEXTPNR_BEL` on every cell, which the router and the
//! bitstream read.
//!
//! What moves is an LE (a LUT and the register its output feeds, or either
//! alone) or a whole carry chain, which starts at LE 0 of a LAB and runs
//! LE k -> k+1, LE 15 -> LE 0 of the LAB below. A LAB holds 16 LEs and has
//! to satisfy the rules `lab_ok` checks; pins sit where the pin assignments
//! say, clock controls are the nearest free one the clock pin reaches. M4K
//! RAM blocks and multipliers move between the chip's sites of their kind,
//! swapping with each other.

use std::borrow::Cow;
use std::collections::VecDeque;

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

use crate::hard;
use crate::m4k;
use crate::mult;
use crate::pin::{assignment, register_param, PinSettings, Pull, UnusedPins};
use chipsmith_place::{Bins, Legalizer, Net, Object, Options, Pin, PinOf, Pos, Problem};
use chipsmith_route::chipdb::{ChipDb, NodeId, Pad, PadIo};
use chipsmith_route::timing::{self, Arc, Ps};

const LES_PER_LAB: usize = 16;
/// Signals from outside a LAB arrive on its 38 local lines, each LE input
/// reaching about half of them. nextpnr's cyclone2 uarch measured 34 as the
/// most that keeps a full LAB routable on picorv32.
const MAX_LAB_INPUTS: usize = 34;
const LUT_INPUTS: [Port; 4] = [Port::DataA, Port::DataB, Port::DataC, Port::DataD];

/// Every cell port the packed netlist uses: a closed set, so cells keep
/// their nets in an array indexed by it rather than a map keyed by name.
/// Block pins (M4K, multiplier, hard block), of which there are many but
/// only on a few cells, are kept apart (`Cell::pins`).
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
    Mult(mult::Pin),
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
                .or_else(|| mult::Pin::parse(name).map(Port::Mult))
                .or_else(|| hard::Pin::parse(name).map(Port::Hard)),
        }
    }

    /// Slot in `Cell::ports`; None for a block pin.
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
            Port::M4k(_) | Port::Mult(_) | Port::Hard(_) => return None,
        })
    }

    fn name(self) -> Cow<'static, str> {
        match self.index() {
            Some(i) => Self::NAMES[i].into(),
            None => match self {
                Port::M4k(pin) => pin.to_string().into(),
                Port::Mult(pin) => pin.to_string().into(),
                Port::Hard(pin) => pin.name().into(),
                _ => unreachable!("only block pins have no slot"),
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
    /// A PLL's external clock output (`PLLOUT`).
    PllOut,
    Block(Block),
    Hard(hard::Block),
}

/// A block that moves between the chip's sites of its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    M4k,
    Mult,
}

impl Block {
    const ALL: [Block; 2] = [Block::M4k, Block::Mult];

    /// The bel's name and type in its tile.
    fn bel(self) -> &'static str {
        match self {
            Block::M4k => "M4K",
            Block::Mult => "MULT",
        }
    }

    fn cell_type(self) -> &'static str {
        match self {
            Block::M4k => "CY2_M4K",
            Block::Mult => "CY2_MULT",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Block::M4k => "M4K blocks",
            Block::Mult => "multipliers",
        }
    }

    fn has_pin(self, port: Port) -> bool {
        matches!(
            (self, port),
            (Block::M4k, Port::M4k(_)) | (Block::Mult, Port::Mult(_))
        )
    }
}

#[derive(Debug)]
struct Cell {
    name: String,
    kind: Kind,
    ports: [Option<NetId>; Port::FIXED.len()],
    /// A block's or hard block's connected pins.
    pins: Vec<(Port, NetId)>,
    clk_inv: bool,
    ena_inv: bool,
    sclr_inv: bool,
    /// A multiplier's A_REG, B_REG, P_REG.
    mult_regs: [bool; 3],
    /// Pin buffers: the top-level port they serve (`LEDR[0]`).
    port: Option<String>,
    /// Pin buffers: which of their I/O-cell registers they use (input,
    /// output, OE).
    io_regs: [bool; 3],
}

impl Cell {
    fn net(&self, port: Port) -> Option<NetId> {
        match (port.index(), port) {
            (Some(i), _) => self.ports[i],
            (None, _) => self.pins.iter().find(|(p, _)| *p == port).map(|&(_, n)| n),
        }
    }

    fn connections(&self) -> impl Iterator<Item = (Port, NetId)> + '_ {
        Port::FIXED
            .into_iter()
            .filter_map(|p| Some((p, self.net(p)?)))
            .chain(self.pins.iter().copied())
    }
}

/// One LE's worth of logic.
#[derive(Debug, Clone, Copy)]
struct Le {
    lut: Option<usize>,
    ff: Option<usize>,
}

/// A control signal with its polarity.
type Ctl = (Option<NetId>, bool);

/// What `lab_ok` needs of an LE, gathered once.
#[derive(Debug, Clone, Copy)]
struct Sig {
    /// Nets the LE takes from its LAB's lines: the LUT's inputs, then the
    /// register's SDATA, ENA, SCLR and ACLR, those that are connected.
    inputs: [NetId; 8],
    n_inputs: u8,
    ff: Option<FfCtl>,
}

/// Why a LAB refuses its contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// More than `MAX_LAB_INPUTS` distinct signals from outside.
    Inputs,
    /// Registers with SDATA and others with SCLR.
    SyncKind,
    /// Two clocks.
    Clock,
    /// A third (clock, enable) domain.
    Domains,
    /// A second enable, synchronous clear or asynchronous clear.
    Control,
}

/// A register's LAB-wide controls.
#[derive(Debug, Clone, Copy)]
struct FfCtl {
    /// The clock net and whether it is inverted (the falling edge).
    clk: Ctl,
    ena: Ctl,
    sclr: Ctl,
    aclr: Option<NetId>,
    /// Bit 0: uses SDATA, bit 1: SCLR; a LAB's registers agree on it.
    sync_kind: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub les: usize,
    pub chains: usize,
    /// Rounds of analytic placement.
    pub rounds: usize,
    /// Wirelength after analytic placement, before the anneal.
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
    sigs: Vec<Sig>,
    le_of: Vec<Option<usize>>,
    chains: Vec<Vec<usize>>,
    chain_of: Vec<Option<usize>>,
    /// Block cells (M4Ks, multipliers), and each cell's index among them.
    blocks: Vec<usize>,
    block_of: Vec<Option<usize>>,
    nets_of_block: Vec<Vec<usize>>,
    /// Nets that count for wirelength: their cells. Clock nets (driven by
    /// a clock control) don't; they ride the global network.
    nets: Vec<Vec<usize>>,
    /// Each LE's nets, with how many of the LE's two cells are on each (a
    /// LUT and its own register share their net), so a move can update a
    /// net's bounding box once per member.
    nets_of_le: Vec<Vec<(usize, u8)>>,
    /// Driving cell of each net, and the LE it is in, for "is this LAB
    /// input local".
    driver: Vec<Option<usize>>,
    driver_le: Vec<Option<u32>>,
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
            "PLLOUT" => Kind::PllOut,
            other => match Block::ALL.into_iter().find(|b| b.cell_type() == other) {
                Some(b) => Kind::Block(b),
                None => match hard::Block::from_cell_type(other) {
                    Some(b) => Kind::Hard(b),
                    None => bail!("{name}: cell type {other} has nowhere to go"),
                },
            },
        };
        let mut ports = [None; Port::FIXED.len()];
        let mut pins = Vec::new();
        for (port, bits) in c["connections"].as_object().context("connections")? {
            if let Some(net) = bits
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_u64)
            {
                // A hard block's pins first: a PLL's CLK0 is no M4K's.
                let hard_pin = match kind {
                    Kind::Hard(b) => hard::Pin::parse(port).filter(|p| b.pins().contains(p)),
                    _ => None,
                };
                let p = hard_pin
                    .map(Port::Hard)
                    .or_else(|| Port::parse(port))
                    .with_context(|| format!("{name}: unknown port {port}"))?;
                let next = net_ids.len() as NetId;
                let id = *net_ids.entry(net).or_insert(next);
                match (p.index(), p) {
                    (Some(i), _) => ports[i] = Some(id),
                    (None, _) if matches!(kind, Kind::Block(b) if b.has_pin(p)) => {
                        pins.push((p, id))
                    }
                    (None, Port::Hard(pin)) if matches!(kind, Kind::Hard(b) if b.pins().contains(&pin)) => {
                        pins.push((p, id))
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
            pins,
            clk_inv: flag("CLK_INV"),
            ena_inv: flag("ENA_INV"),
            sclr_inv: flag("SCLR_INV"),
            mult_regs: mult::Regs::PARAMS.map(flag),
            port: c["attributes"]["PORT"].as_str().map(str::to_string),
            io_regs: [
                register_param::INPUT,
                register_param::OUTPUT,
                register_param::OE,
            ]
            .map(|p| c["parameters"][p].is_string()),
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
                    | (Kind::Ibuf | Kind::Iobuf, Port::Combout | Port::Regout)
                    | (Kind::ClkCtrl | Kind::PllOut, Port::Outclk)
            ) || matches!(port, Port::M4k(pin) if pin.is_output())
                || matches!(port, Port::Mult(pin) if pin.is_output())
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
    let blocks: Vec<usize> = (0..cells.len())
        .filter(|&i| matches!(cells[i].kind, Kind::Block(_)))
        .collect();
    let mut block_of = vec![None; cells.len()];
    for (r, &c) in blocks.iter().enumerate() {
        block_of[c] = Some(r);
    }
    let mut nets_of_le: Vec<Vec<(usize, u8)>> = vec![Vec::new(); les.len()];
    let mut nets_of_block = vec![Vec::new(); blocks.len()];
    for (n, members) in nets.iter().enumerate() {
        for &c in members {
            match (le_of[c], block_of[c]) {
                (Some(le), _) => match nets_of_le[le].last_mut() {
                    Some((last, count)) if *last == n => *count += 1,
                    _ => nets_of_le[le].push((n, 1)),
                },
                (_, Some(r)) => nets_of_block[r].push(n),
                _ => {}
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
            Kind::Block(b) => b.cell_type(),
            Kind::Ibuf | Kind::Obuf | Kind::Iobuf => {
                let [i, o, e] = c.io_regs;
                timing.add_ioe_registers(&c.name, i, o, e);
                continue;
            }
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
        if c.kind == Kind::Block(Block::Mult) {
            let [a, b, p] = c.mult_regs;
            timing.add_mult_registers(&c.name, a, b, p);
        }
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

    let sigs = les
        .iter()
        .map(|&Le { lut, ff }| {
            let mut sig = Sig {
                inputs: [0; 8],
                n_inputs: 0,
                ff: None,
            };
            let mut add = |net: Option<NetId>| {
                if let Some(n) = net {
                    sig.inputs[sig.n_inputs as usize] = n;
                    sig.n_inputs += 1;
                }
            };
            if let Some(l) = lut {
                for p in LUT_INPUTS {
                    add(cells[l].net(p));
                }
            }
            if let Some(f) = ff {
                let c = &cells[f];
                for p in [Port::Sdata, Port::Ena, Port::Sclr, Port::Aclr] {
                    add(c.net(p));
                }
                sig.ff = Some(FfCtl {
                    clk: (c.net(Port::Clk), c.clk_inv),
                    ena: (c.net(Port::Ena), c.ena_inv),
                    sclr: (c.net(Port::Sclr), c.sclr_inv),
                    aclr: c.net(Port::Aclr),
                    sync_kind: c.net(Port::Sdata).is_some() as u8
                        | (c.net(Port::Sclr).is_some() as u8) << 1,
                });
            }
            sig
        })
        .collect();
    let driver_le = driver
        .iter()
        .map(|d| d.and_then(|c| le_of[c]).map(|le| le as u32))
        .collect();

    Ok(Design {
        cells,
        les,
        sigs,
        le_of,
        chains,
        chain_of,
        blocks,
        block_of,
        nets_of_block,
        nets,
        nets_of_le,
        driver,
        driver_le,
        conns,
        timing,
    })
}

impl Design {
    /// What block `r` is (an M4K, a multiplier).
    fn block_kind(&self, r: usize) -> Block {
        match self.cells[self.blocks[r]].kind {
            Kind::Block(b) => b,
            _ => unreachable!("blocks are block cells"),
        }
    }
}

/// The LABs and block sites of the chip.
struct Grid {
    /// LAB index -> its tile.
    tiles: Vec<u32>,
    xy: Vec<Xy>,
    /// The LAB at each tile position, row-major; looked up on every move.
    at: Vec<Option<usize>>,
    width: i32,
    height: i32,
    /// Block site index -> its tile, its position and what it holds.
    site_tiles: Vec<u32>,
    site_xy: Vec<Xy>,
    site_block: Vec<Block>,
}

impl Grid {
    fn new(db: &ChipDb) -> Self {
        let (width, height) = (db.width as i32, db.height as i32);
        let (mut tiles, mut xy) = (Vec::new(), Vec::new());
        let mut at = vec![None; (width * height) as usize];
        let (mut site_tiles, mut site_xy, mut site_block) = (Vec::new(), Vec::new(), Vec::new());
        for tile in 0..db.width * db.height {
            let (x, y) = db.tile_xy(tile);
            let bels = &db.tile_type(tile).bels;
            if bels.contains_key("LE0.LUT") {
                at[(y as i32 * width + x as i32) as usize] = Some(tiles.len());
                xy.push((x as i32, y as i32));
                tiles.push(tile);
            } else if let Some(b) = Block::ALL
                .into_iter()
                .find(|b| bels.get(b.bel()).is_some_and(|bel| bel.ty == b.bel()))
            {
                site_xy.push((x as i32, y as i32));
                site_tiles.push(tile);
                site_block.push(b);
            }
        }
        Grid {
            tiles,
            xy,
            at,
            width,
            height,
            site_tiles,
            site_xy,
            site_block,
        }
    }

    fn lab_at(&self, (x, y): Xy) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        self.at[(y * self.width + x) as usize]
    }

    /// The LABs a chain of `len` LEs starting at `lab` occupies.
    fn chain_labs(&self, lab: usize, len: usize) -> Option<Vec<usize>> {
        let (x, y) = self.xy[lab];
        (0..len.div_ceil(LES_PER_LAB) as i32)
            .map(|i| self.lab_at((x, y - i)))
            .collect()
    }
}

/// A net's bounding box, with how many of its cells sit on each edge, so
/// a move inside the box or onto an edge updates it without a pass over
/// the net (VPR's incremental bounding boxes). A count reaching zero means
/// the edge moved inwards by an unknown amount: the box is then recomputed.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Bbox {
    lx: i32,
    hx: i32,
    ly: i32,
    hy: i32,
    n_lx: u32,
    n_hx: u32,
    n_ly: u32,
    n_hy: u32,
}

impl Bbox {
    fn of(mut points: impl Iterator<Item = Xy>) -> Bbox {
        let (x0, y0) = points.next().expect("a net has cells");
        let mut b = Bbox {
            lx: x0,
            hx: x0,
            ly: y0,
            hy: y0,
            n_lx: 1,
            n_hx: 1,
            n_ly: 1,
            n_hy: 1,
        };
        for (x, y) in points {
            Self::add(&mut b.lx, &mut b.hx, &mut b.n_lx, &mut b.n_hx, x);
            Self::add(&mut b.ly, &mut b.hy, &mut b.n_ly, &mut b.n_hy, y);
        }
        b
    }

    fn add(lo: &mut i32, hi: &mut i32, n_lo: &mut u32, n_hi: &mut u32, v: i32) {
        if v < *lo {
            *lo = v;
            *n_lo = 1;
        } else if v == *lo {
            *n_lo += 1;
        }
        if v > *hi {
            *hi = v;
            *n_hi = 1;
        } else if v == *hi {
            *n_hi += 1;
        }
    }

    /// One cell of the net moved from `old` to `new`. False when an edge
    /// count dropped to zero and the box has to be recomputed.
    fn moved(&mut self, old: Xy, new: Xy) -> bool {
        let mut ok = true;
        for (lo, hi, n_lo, n_hi, o, n) in [
            (
                &mut self.lx,
                &mut self.hx,
                &mut self.n_lx,
                &mut self.n_hx,
                old.0,
                new.0,
            ),
            (
                &mut self.ly,
                &mut self.hy,
                &mut self.n_ly,
                &mut self.n_hy,
                old.1,
                new.1,
            ),
        ] {
            if o == n {
                continue;
            }
            if o == *lo {
                *n_lo -= 1;
            }
            if o == *hi {
                *n_hi -= 1;
            }
            Self::add(lo, hi, n_lo, n_hi, n);
            ok &= *n_lo > 0 && *n_hi > 0;
        }
        ok
    }

    /// Half-perimeter wirelength; x counts routing and logic columns
    /// separately, so it is halved.
    fn hpwl(&self) -> u32 {
        ((self.hx - self.lx) / 2 + (self.hy - self.ly)) as u32
    }
}

/// What `fixed_cells` found.
enum Fixed {
    /// Pins and clock controls, with their bel names.
    Sites(HashMap<usize, (u32, String)>),
    /// The packer's guess that these PLL external clock outputs (`PLLOUT`
    /// cells) drive their PLL's clock output pin was wrong: their pins are
    /// ordinary ones (`globals_for_pll_pins`).
    OffPin(Vec<usize>),
}

/// Fixed cells: pins and clock controls, with their bel names.
fn fixed_cells(
    db: &ChipDb,
    design: &Design,
    pins: &std::collections::BTreeMap<String, PinPlan>,
) -> Result<Fixed> {
    let mut out = HashMap::default();
    let pads: HashMap<&str, &Pad> = db.pads.iter().map(|p| (p.pin.as_str(), p)).collect();
    for (i, c) in design.cells.iter().enumerate() {
        let drives = match c.kind {
            Kind::Ibuf => false,
            Kind::Obuf | Kind::Iobuf => true,
            Kind::Lut | Kind::Ff | Kind::ClkCtrl | Kind::PllOut | Kind::Block(_) => continue,
            // One of each per device, at a fixed bel; PLLs come below.
            Kind::Hard(b) => {
                let Some(((x, y), bel)) = b.bel() else {
                    continue;
                };
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
        let plan = pins
            .get(port)
            .with_context(|| format!("no pin assignment for port {port}"))?;
        let pin = &plan.pin;
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
        // What the manifest couldn't check: which way the port goes, and
        // which pins are clock inputs.
        ensure!(
            !drives || plan.settings.standard.drives(),
            "port {port}: {} is an input standard here (every DE2 bank is 3.3 V), and the port drives",
            plan.settings.standard.quartus_name()
        );
        ensure!(
            pad.io != PadIo::ClockInput || plan.settings.pull == Pull::None,
            "port {port}: {pin} is a dedicated clock input, which has neither a weak pull-up nor bus hold"
        );
        ensure!(
            pad.io != PadIo::ClockInput || c.io_regs == [false; 3],
            "port {port}: {pin} is a dedicated clock input, which has no I/O-cell registers"
        );
        ensure!(
            !out.values()
                .any(|(t, b): &(u32, String)| *t == tile && b == bel),
            "port {port}: {pin} is already used"
        );
        out.insert(i, (tile, bel.to_string()));
    }

    // INCLK node of every clock control, and of every PLL.
    let mut inclk: HashMap<NodeId, (u32, String)> = HashMap::default();
    let mut pll_inclk: HashMap<NodeId, (u32, String)> = HashMap::default();
    let mut pllout_inclk: HashMap<NodeId, (u32, String)> = HashMap::default();
    for tile in 0..db.width * db.height {
        for (name, bel) in &db.tile_type(tile).bels {
            let to = match bel.ty.as_str() {
                "CLKCTRL" => &mut inclk,
                "PLL" => &mut pll_inclk,
                "PLLOUT" => &mut pllout_inclk,
                _ => continue,
            };
            if let Some(&w) = bel.pins.get("INCLK") {
                to.insert(db.node(tile, w), (tile, name.clone()));
            }
        }
    }
    // The output node of a fixed cell's port.
    let out_node =
        |out: &HashMap<usize, (u32, String)>, cell: usize, port: Port| -> Result<NodeId> {
            let (tile, bel) = &out[&cell];
            let pin = match port {
                Port::Hard(p) => p.name(),
                _ => "COMBOUT",
            };
            let w = *db.tile_type(*tile).bels[bel]
                .pins
                .get(pin)
                .with_context(|| format!("bel {bel} has no pin {pin}"))?;
            Ok(db.node(*tile, w))
        };
    // The nearest free bel among `targets` that `start` reaches through
    // dedicated wiring (a few hops), as nextpnr's packer picks a clock
    // control for a clock pin.
    let nearest = |out: &HashMap<usize, (u32, String)>,
                   targets: &HashMap<NodeId, (u32, String)>,
                   start: NodeId|
     -> Option<(u32, String)> {
        let mut seen = HashSet::from_iter([start]);
        let mut queue = VecDeque::from([(start, 0)]);
        while let Some((n, depth)) = queue.pop_front() {
            if let Some(cc) = targets.get(&n) {
                if !out.values().any(|v| v == cc) {
                    return Some(cc.clone());
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
        None
    };
    // PLLs: the one whose input the clock pin reaches (each PLL takes the
    // dedicated clock pins of one side of the chip).
    for (i, c) in design
        .cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind == Kind::Hard(hard::Block::Pll))
    {
        let net = c
            .net(Port::Inclk)
            .with_context(|| format!("{}: PLL INCLK unconnected", c.name))?;
        let pin = design.driver[net as usize]
            .filter(|&d| design.cells[d].kind == Kind::Ibuf)
            .with_context(|| {
                format!(
                    "{}: a PLL's INCLK must come straight from a clock pin",
                    c.name
                )
            })?;
        let site = nearest(&out, &pll_inclk, out_node(&out, pin, Port::Combout)?).with_context(|| {
            format!(
                "{}: no free PLL reachable from {}: a PLL takes a dedicated clock pin of its side (CLOCK_50, PIN_N2, feeds PLL_1)",
                c.name, design.cells[pin].name
            )
        })?;
        out.insert(i, site);
    }
    // A PLL's external clock output: the one its counter 2 reaches, if
    // that is wired to the pin it drives.
    let mut off_pin = Vec::new();
    for (i, c) in design
        .cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind == Kind::PllOut)
    {
        let net = c
            .net(Port::Inclk)
            .with_context(|| format!("{}: INCLK unconnected", c.name))?;
        let d =
            design.driver[net as usize].with_context(|| format!("{}: input undriven", c.name))?;
        let port = design.cells[d]
            .connections()
            .find(|&(_, n)| n == net)
            .map(|(p, _)| p)
            .context("external clock output input")?;
        let site = nearest(&out, &pllout_inclk, out_node(&out, d, port)?).with_context(|| {
            format!(
                "{}: no external clock output reachable",
                design.cells[d].name
            )
        })?;
        let o = c
            .net(Port::Outclk)
            .with_context(|| format!("{}: OUTCLK unconnected", c.name))?;
        let pin = design
            .cells
            .iter()
            .position(|p| p.kind == Kind::Obuf && p.net(Port::Dataout) == Some(o))
            .with_context(|| format!("{}: drives no output pin", c.name))?;
        let node = |(tile, bel): &(u32, String), p: &str| {
            db.node(*tile, db.tile_type(*tile).bels[bel].pins[p])
        };
        let (from, to) = (node(&site, "OUTCLK"), node(&out[&pin], "DATAOUT"));
        let reached: Vec<NodeId> = db
            .downhill(from)
            .iter()
            .flat_map(|e| db.downhill(e.dst))
            .map(|e| e.dst)
            .collect();
        if !reached.contains(&to) {
            off_pin.push(i);
            continue;
        }
        out.insert(i, site);
    }
    if !off_pin.is_empty() {
        return Ok(Fixed::OffPin(off_pin));
    }
    // Clocks from pins first: each takes the nearest free clock control
    // its pin reaches, as nextpnr's packer picks it. Clocks from the core
    // then take any free one (every clock control has a core input), the
    // nearest to their driver if that is fixed, else the nearest to the
    // middle of the chip, where most of the logic driving it will be.
    let clkctrls: Vec<(usize, usize)> = design
        .cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind == Kind::ClkCtrl)
        .map(|(i, c)| {
            let net = c
                .net(Port::Inclk)
                .with_context(|| format!("{}: INCLK unconnected", c.name))?;
            let d = design.driver[net as usize]
                .with_context(|| format!("{}: clock control input undriven", c.name))?;
            Ok((i, d))
        })
        .collect::<Result<_>>()?;
    let dedicated = |d: usize| {
        matches!(
            design.cells[d].kind,
            Kind::Ibuf | Kind::Hard(hard::Block::Pll)
        )
    };
    let (from_pins, from_core): (Vec<_>, Vec<_>) =
        clkctrls.into_iter().partition(|&(_, d)| dedicated(d));
    for (i, d) in from_pins {
        let net = design.cells[i].net(Port::Inclk).unwrap();
        let port = design.cells[d]
            .connections()
            .find(|&(_, n)| n == net)
            .map(|(p, _)| p)
            .context("clock control input")?;
        let found = nearest(&out, &inclk, out_node(&out, d, port)?);
        out.insert(
            i,
            found.with_context(|| {
                format!(
                    "no free clock control reachable from {}",
                    design.cells[d].name
                )
            })?,
        );
    }
    let mut sites: Vec<(u32, String)> = inclk.into_values().collect();
    sites.sort();
    for (i, d) in from_core {
        let target = match out.get(&d) {
            Some((tile, _)) => {
                let (x, y) = db.tile_xy(*tile);
                (x as i64, y as i64)
            }
            None => (i64::from(db.width) / 2, i64::from(db.height) / 2),
        };
        let free = sites
            .iter()
            .filter(|s| !out.values().any(|v| v == *s))
            .min_by_key(|(tile, _)| {
                let (x, y) = db.tile_xy(*tile);
                (x as i64 - target.0).abs() + (y as i64 - target.1).abs()
            })
            .cloned()
            .with_context(|| format!("{}: all 16 global clocks are taken", design.cells[i].name))?;
        out.insert(i, free);
    }
    Ok(Fixed::Sites(out))
}

/// The placement being annealed.
#[derive(Clone)]
struct State<'a> {
    design: &'a Design,
    grid: &'a Grid,
    /// LE in each slot of each LAB.
    slots: Vec<[Option<usize>; LES_PER_LAB]>,
    /// (LAB, LE index) of each LE.
    pos: Vec<(usize, usize)>,
    /// Position of every cell: LEs and blocks as placed, pins and clock
    /// controls where they are fixed.
    xy: Vec<Xy>,
    /// Site of each block, and the block at each site.
    block_site: Vec<usize>,
    site_of: Vec<Option<usize>>,
    /// Each net's weight: 1, plus more the more timing-critical it is.
    weight: Vec<f32>,
    /// Each net's bounding box and weighted wirelength.
    bbox: Vec<Bbox>,
    net_cost: Vec<f32>,
    /// The move being evaluated: the boxes it changed, as they were, so a
    /// rejected move is undone without recomputing them; and nets that
    /// need a full recompute (`touched_gen` marks a net already saved).
    saved: Vec<(usize, Bbox)>,
    dirty: Vec<usize>,
    touched_gen: Vec<u32>,
    generation: u32,
}

impl<'a> State<'a> {
    fn cell_xy(&self, cell: usize) -> Xy {
        self.xy[cell]
    }

    fn cost(&self, net: usize) -> f32 {
        self.weight[net] * self.bbox[net].hpwl() as f32
    }

    fn compute_bbox(&self, net: usize) -> Bbox {
        Bbox::of(self.design.nets[net].iter().map(|&c| self.xy[c]))
    }

    fn total(&self) -> f64 {
        self.net_cost.iter().map(|&c| c as f64).sum()
    }

    /// Unweighted wirelength, for reporting.
    fn wirelength(&self) -> u64 {
        self.bbox.iter().map(|b| b.hpwl() as u64).sum()
    }

    /// Account for a move already applied to `slots`/`pos`/`xy`: update the
    /// boxes of every net touched and return the change in cost. The old
    /// boxes are kept for `revert_cost`; `commit_cost` makes the new costs
    /// permanent.
    fn apply_cost(&mut self, undo: &Undo) -> f64 {
        let d = self.design;
        self.generation += 1;
        self.saved.clear();
        self.dirty.clear();
        let g = self.generation;
        match undo {
            Undo::Les(moved) => {
                for &(le, old_lab, _) in moved {
                    let (old, new) = (self.grid.xy[old_lab], self.grid.xy[self.pos[le].0]);
                    for &(n, count) in &d.nets_of_le[le] {
                        for _ in 0..count {
                            self.net_moved(n, old, new, g);
                        }
                    }
                }
            }
            Undo::Blocks(moved) => {
                for &(r, old_site) in moved {
                    let (old, new) = (
                        self.grid.site_xy[old_site],
                        self.grid.site_xy[self.block_site[r]],
                    );
                    for &n in &d.nets_of_block[r] {
                        self.net_moved(n, old, new, g);
                    }
                }
            }
        }
        // A box an edge count emptied is recomputed once every cell has
        // its final position.
        for i in 0..self.dirty.len() {
            let n = self.dirty[i];
            self.bbox[n] = self.compute_bbox(n);
        }
        // Summed as the full recompute did (per net in index order, after
        // minus before), so the same seed still makes the same decisions.
        self.saved.sort_unstable_by_key(|&(n, _)| n);
        let after: f64 = self.saved.iter().map(|&(n, _)| self.cost(n) as f64).sum();
        let before: f64 = self
            .saved
            .iter()
            .map(|&(n, _)| self.net_cost[n] as f64)
            .sum();
        after - before
    }

    fn net_moved(&mut self, n: usize, old: Xy, new: Xy, g: u32) {
        if self.touched_gen[n] != g {
            self.touched_gen[n] = g;
            self.saved.push((n, self.bbox[n]));
        } else if self.dirty.contains(&n) {
            return;
        }
        if !self.bbox[n].moved(old, new) {
            self.dirty.push(n);
        }
    }

    fn revert_cost(&mut self) {
        for &(n, b) in &self.saved {
            self.bbox[n] = b;
        }
    }

    fn commit_cost(&mut self) {
        for i in 0..self.saved.len() {
            let n = self.saved[i].0;
            self.net_cost[n] = self.cost(n);
        }
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

    fn lab_ok(&self, lab: usize) -> bool {
        self.lab_check(lab).is_ok()
    }

    /// nextpnr's `isBelLocationValid` for one LAB. Hot: runs on every
    /// move, so it allocates nothing and reads each LE's precomputed `Sig`.
    fn lab_check(&self, lab: usize) -> Result<(), Refusal> {
        let d = self.design;
        // A third distinct domain already fails, so three slots suffice.
        let mut domains: [((Option<NetId>, bool), Ctl); 3] = [((None, false), (None, false)); 3];
        let mut n_domains = 0;
        let (mut ena, mut aclr, mut sclr): (Option<Ctl>, Option<NetId>, Option<Ctl>) =
            (None, None, None);
        let mut sync_kind = 0;
        let mut inputs = [0 as NetId; MAX_LAB_INPUTS];
        let mut n_inputs = 0;
        let one = |slot: &mut Option<Ctl>, v: Ctl| match (v.0, *slot) {
            (None, _) => true,
            (Some(_), None) => {
                *slot = Some(v);
                true
            }
            (Some(_), Some(s)) => s == v,
        };
        for le in self.slots[lab].iter().flatten() {
            let sig = &d.sigs[*le];
            for &n in &sig.inputs[..sig.n_inputs as usize] {
                // Driven inside the LAB, or already counted: no new line.
                let here = d.driver_le[n as usize].is_some_and(|le| self.pos[le as usize].0 == lab);
                if here || inputs[..n_inputs].contains(&n) {
                    continue;
                }
                if n_inputs == MAX_LAB_INPUTS {
                    return Err(Refusal::Inputs);
                }
                inputs[n_inputs] = n;
                n_inputs += 1;
            }
            let Some(ff) = &sig.ff else { continue };
            if ff.sync_kind != 0 && sync_kind != 0 && ff.sync_kind != sync_kind {
                return Err(Refusal::SyncKind);
            }
            sync_kind |= ff.sync_kind;
            // The clock's inversion is part of the domain (each LAB clock
            // has its own invert bit) and of the clock: one clock net and
            // one edge per LAB, though a LAB has two LAB clocks. Which of
            // them Quartus gives each of two clocks isn't predictable from
            // what we know (cyclone2-re README, "LAB clocks": no rule fits
            // all 15 measured LABs; for one net on both edges, chipsmith
            // examples/negedge-selftest's ten LABs fit no rule either), and
            // the bitstream has to say, so a LAB mixing clocks or edges
            // wouldn't assemble the same in Quartus. Remove once that
            // choice is modelled; until then two domains in a LAB share a
            // clock and an edge and differ in their enable.
            let domain = (ff.clk, ff.ena);
            if domains[..n_domains].iter().any(|d| d.0 != domain.0) {
                return Err(Refusal::Clock);
            }
            if !domains[..n_domains].contains(&domain) {
                if n_domains == 2 {
                    return Err(Refusal::Domains);
                }
                domains[n_domains] = domain;
                n_domains += 1;
            }
            if !one(&mut ena, ff.ena) || !one(&mut sclr, ff.sclr) {
                return Err(Refusal::Control);
            }
            if let Some(a) = ff.aclr {
                if aclr.is_some_and(|x| x != a) {
                    return Err(Refusal::Control);
                }
                aclr = Some(a);
            }
        }
        Ok(())
    }

    /// Take `le` off the chip again. `pos` must go too: `lab_ok` reads it
    /// to tell a LAB's own signals from outside inputs.
    fn unput(&mut self, le: usize) {
        let (lab, k) = self.pos[le];
        if lab != usize::MAX && self.slots[lab][k] == Some(le) {
            self.slots[lab][k] = None;
        }
        self.pos[le] = (usize::MAX, 0);
    }

    fn put(&mut self, le: usize, lab: usize, k: usize) {
        self.slots[lab][k] = Some(le);
        self.pos[le] = (lab, k);
        let Le { lut, ff } = self.design.les[le];
        for c in [lut, ff].into_iter().flatten() {
            self.xy[c] = self.grid.xy[lab];
        }
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
    /// Every block that moved, and the site it was on.
    Blocks(Vec<(usize, usize)>),
}

impl<'a> State<'a> {
    /// An empty placement: pins and clock controls where they are fixed,
    /// nothing else placed yet.
    fn new(design: &'a Design, grid: &'a Grid, fixed_xy: &[Option<Xy>]) -> Self {
        State {
            design,
            grid,
            slots: vec![[None; LES_PER_LAB]; grid.tiles.len()],
            pos: vec![(usize::MAX, 0); design.les.len()],
            xy: fixed_xy.iter().map(|p| p.unwrap_or((0, 0))).collect(),
            block_site: vec![usize::MAX; design.blocks.len()],
            site_of: vec![None; grid.site_tiles.len()],
            weight: vec![1.0; design.nets.len()],
            bbox: vec![Bbox::default(); design.nets.len()],
            net_cost: vec![0.0; design.nets.len()],
            saved: Vec::new(),
            dirty: Vec::new(),
            touched_gen: vec![0; design.nets.len()],
            generation: 0,
        }
    }

    /// Take every LE and M4K off the chip.
    fn clear(&mut self) {
        self.slots.iter_mut().for_each(|s| *s = [None; LES_PER_LAB]);
        self.pos.iter_mut().for_each(|p| *p = (usize::MAX, 0));
        self.block_site.iter_mut().for_each(|s| *s = usize::MAX);
        self.site_of.iter_mut().for_each(|s| *s = None);
    }

    /// Every box and cost from scratch, after a placement was built
    /// rather than moved into.
    fn recompute_costs(&mut self) {
        for n in 0..self.design.nets.len() {
            self.bbox[n] = self.compute_bbox(n);
            self.net_cost[n] = self.cost(n);
        }
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

    /// Move block `r` to `site` (one of its kind), swapping with the one
    /// there.
    fn try_move_block(&mut self, r: usize, site: usize) -> Option<Undo> {
        let from = self.block_site[r];
        if from == site {
            return None;
        }
        let mut moved = vec![(r, from)];
        if let Some(o) = self.site_of[site] {
            moved.push((o, site));
            self.put_block(o, from);
        } else {
            self.site_of[from] = None;
        }
        self.put_block(r, site);
        Some(Undo::Blocks(moved))
    }

    fn put_block(&mut self, r: usize, site: usize) {
        self.block_site[r] = site;
        self.site_of[site] = Some(r);
        self.xy[self.design.blocks[r]] = self.grid.site_xy[site];
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
            Undo::Blocks(moved) => {
                for &(r, _) in moved {
                    let site = self.block_site[r];
                    if self.site_of[site] == Some(r) {
                        self.site_of[site] = None;
                    }
                }
                for &(r, site) in moved {
                    self.put_block(r, site);
                }
            }
        }
    }
}

/// What the analytic placer moves: a cluster of LEs that will share a
/// LAB, a carry chain (by its head), or an M4K.
#[derive(Debug, Clone, Copy)]
enum Obj {
    Cluster(usize),
    Chain(usize),
    Block(usize),
}

/// Gather the LEs that are not in chains into LAB-sized clusters, as
/// VPR's packer does before placing: a LAB holds one set of register
/// controls and 34 outside inputs, so an LE-level analytic placement at
/// LAB density has nowhere legal to go; clusters of compatible,
/// connected LEs place at LAB granularity and always fit. Each cluster
/// grows from a seed by the candidate sharing the most nets with it
/// (high-fanout nets count little), as long as `lab_ok` accepts the
/// cluster in a LAB; the anneal moves single LEs afterwards.
fn cluster(d: &Design, grid: &Grid, fixed_xy: &[Option<Xy>]) -> Vec<Vec<usize>> {
    let chain_labs: usize = d.chains.iter().map(|c| c.len().div_ceil(LES_PER_LAB)).sum();
    let budget = grid.tiles.len().saturating_sub(chain_labs);
    let mut scratch = State::new(d, grid, fixed_xy);
    let clusters = cluster_with(d, &mut scratch, budget, false);
    if clusters.len() <= budget {
        return clusters;
    }
    cluster_with(d, &mut scratch, budget, true)
}

/// One clustering pass. With `fill`, a cluster whose connected
/// candidates run out is topped up with unclustered LUT-only LEs (VPR's
/// unrelated clustering): a LAB takes one enable, so on a design with
/// hundreds of enables (picorv32x24: 674) the registers whose LUTs went
/// into other clusters are otherwise left alone in a LAB each, and a
/// near-full design doesn't fit. Unrelated LEs in a LAB cost wirelength,
/// so this is the fallback, not the default.
fn cluster_with(d: &Design, scratch: &mut State, budget: usize, fill: bool) -> Vec<Vec<usize>> {
    // LUT-only LEs in index order (synthesis keeps related logic close
    // in it), tried as fillers from a cursor that only moves forward.
    let fillers: Vec<usize> = (0..d.les.len())
        .filter(|&le| d.les[le].ff.is_none() && d.chain_of[le].is_none())
        .collect();
    let mut cursor = 0;
    let mut clustered = vec![false; d.les.len()];
    for ch in &d.chains {
        for &le in ch {
            clustered[le] = true;
        }
    }
    // Registers first: they are what constrains a LAB.
    let mut seeds: Vec<usize> = (0..d.les.len()).filter(|&le| !clustered[le]).collect();
    seeds.sort_by_key(|&le| (d.les[le].ff.is_none(), le));
    let mut clusters = Vec::new();
    let mut gain: HashMap<usize, f32> = HashMap::default();
    let mut refused: HashSet<usize> = HashSet::default();
    for seed in seeds {
        if clustered[seed] {
            continue;
        }
        let mut members = vec![seed];
        clustered[seed] = true;
        scratch.put(seed, 0, 0);
        gain.clear();
        refused.clear();
        let add_gains = |le: usize, gain: &mut HashMap<usize, f32>, clustered: &[bool]| {
            for &(n, _) in &d.nets_of_le[le] {
                let fanout = d.nets[n].len();
                for &c in &d.nets[n] {
                    if let Some(other) = d.le_of[c] {
                        if !clustered[other] {
                            *gain.entry(other).or_insert(0.0) += 1.0 / (fanout - 1) as f32;
                        }
                    }
                }
            }
        };
        add_gains(seed, &mut gain, &clustered);
        while members.len() < LES_PER_LAB {
            let best = gain
                .iter()
                .filter(|(le, _)| !clustered[**le] && !refused.contains(le))
                .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(a.0)))
                .map(|(&le, _)| le);
            let le = match best {
                Some(le) => le,
                None if fill => {
                    while cursor < fillers.len() && clustered[fillers[cursor]] {
                        cursor += 1;
                    }
                    // A few tries past the cursor, so one LE the LAB
                    // refuses (too many inputs) doesn't end the search.
                    let Some(&le) = fillers[cursor..]
                        .iter()
                        .filter(|&&le| !clustered[le] && !refused.contains(&le))
                        .take(1)
                        .next()
                    else {
                        break;
                    };
                    if refused.len() > 4 * LES_PER_LAB {
                        break;
                    }
                    le
                }
                None => break,
            };
            let k = members.len();
            scratch.put(le, 0, k);
            if scratch.lab_ok(0) {
                members.push(le);
                clustered[le] = true;
                add_gains(le, &mut gain, &clustered);
            } else {
                scratch.unput(le);
                refused.insert(le);
            }
        }
        for &le in &members {
            scratch.unput(le);
        }
        clusters.push(members);
    }
    merge_clusters(d, scratch, &mut clusters);
    clusters.retain(|c| !c.is_empty());
    if clusters.len() > budget {
        pack_clusters(scratch, &mut clusters, budget);
    }
    clusters
}

/// When the clusters still need more LABs than the chip has left after
/// the carry chains: small clusters packed first-fit into any other
/// cluster with room whose union a LAB accepts, connected or not, until
/// they fit. Mostly registers with an enable of their own (picorv32x24:
/// 203 singletons among 674 enables); a LAB takes two (clock, enable)
/// domains, so they pair up. Unrelated logic in one LAB costs
/// wirelength, so this runs only when the design wouldn't fit otherwise.
fn pack_clusters(scratch: &mut State, clusters: &mut Vec<Vec<usize>>, budget: usize) {
    let mut small: Vec<usize> = (0..clusters.len())
        .filter(|&i| clusters[i].len() < LES_PER_LAB)
        .collect();
    small.sort_by_key(|&i| (clusters[i].len(), i));
    let mut live = clusters.len();
    for a in 0..small.len() {
        if live <= budget {
            break;
        }
        let i = small[a];
        if clusters[i].is_empty() {
            continue;
        }
        // Into the fullest one it fits, so LABs fill up.
        for &j in small[a + 1..].iter().rev() {
            if clusters[j].is_empty() || clusters[i].len() + clusters[j].len() > LES_PER_LAB {
                continue;
            }
            for (k, &le) in clusters[j].iter().chain(&clusters[i]).enumerate() {
                scratch.put(le, 0, k);
            }
            let ok = scratch.lab_ok(0);
            for &le in clusters[j].iter().chain(&clusters[i]) {
                scratch.unput(le);
            }
            if ok {
                let moved = std::mem::take(&mut clusters[i]);
                clusters[j].extend(moved);
                live -= 1;
                break;
            }
        }
    }
    clusters.retain(|c| !c.is_empty());
}

/// Clusters a seed's controls or the input limit closed early, merged
/// into others whose union a LAB still accepts, smallest first, into the
/// one they share most nets with. The analytic placer gives every
/// cluster a LAB of its own, so a design near full (picorv32x24: 1,794
/// clusters plus its chains against 2,076 LABs) only fits once the
/// half-empty ones are combined.
fn merge_clusters(d: &Design, scratch: &mut State, clusters: &mut [Vec<usize>]) {
    let mut cluster_of = vec![usize::MAX; d.les.len()];
    for (i, c) in clusters.iter().enumerate() {
        for &le in c {
            cluster_of[le] = i;
        }
    }
    let mut order: Vec<usize> = (0..clusters.len()).collect();
    order.sort_by_key(|&i| (clusters[i].len(), i));
    let mut gain: HashMap<usize, f32> = HashMap::default();
    for i in order {
        let size = clusters[i].len();
        if size == 0 || size >= LES_PER_LAB {
            continue;
        }
        gain.clear();
        for &le in &clusters[i] {
            for &(n, _) in &d.nets_of_le[le] {
                let fanout = d.nets[n].len();
                // A clock or reset says nothing about where a cluster belongs.
                if fanout > 2 * LES_PER_LAB {
                    continue;
                }
                for &c in &d.nets[n] {
                    let Some(j) = d.le_of[c].map(|o| cluster_of[o]) else {
                        continue;
                    };
                    if j != usize::MAX && j != i && size + clusters[j].len() <= LES_PER_LAB {
                        *gain.entry(j).or_insert(0.0) += 1.0 / (fanout - 1) as f32;
                    }
                }
            }
        }
        let mut candidates: Vec<(usize, f32)> = gain.iter().map(|(&j, &g)| (j, g)).collect();
        candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        for (j, _) in candidates {
            for (k, &le) in clusters[j].iter().chain(&clusters[i]).enumerate() {
                scratch.put(le, 0, k);
            }
            let ok = scratch.lab_ok(0);
            for &le in clusters[j].iter().chain(&clusters[i]) {
                scratch.unput(le);
            }
            if ok {
                let moved = std::mem::take(&mut clusters[i]);
                for &le in &moved {
                    cluster_of[le] = j;
                }
                clusters[j].extend(moved);
                break;
            }
        }
    }
}

/// The design as `chipsmith_place` sees it: positions in halved x (the
/// wirelength's unit), one bin per LAB tile holding 16 LEs, M4Ks in a
/// class of their own that only the legaliser places.
struct Analytic {
    problem: Problem,
    objects: Vec<Obj>,
    clusters: Vec<Vec<usize>>,
}

/// A tile position in placement units: the centre of its bin.
fn unit((x, y): Xy) -> Pos {
    ((x / 2) as f32 + 0.5, y as f32 + 0.5)
}

fn analytic_problem(
    d: &Design,
    grid: &Grid,
    fixed_xy: &[Option<Xy>],
    clusters: &[Vec<usize>],
) -> Analytic {
    let mut objects = Vec::new();
    // Each LE's object and its row offset along a chain (LE k of a chain
    // is in the LAB k / 16 below the head's).
    let mut obj_of_le: Vec<Option<(u32, f32)>> = vec![None; d.les.len()];
    for (i, members) in clusters.iter().enumerate() {
        for &le in members {
            obj_of_le[le] = Some((objects.len() as u32, 0.0));
        }
        objects.push(Obj::Cluster(i));
    }
    for (c, chain) in d.chains.iter().enumerate() {
        let o = objects.len() as u32;
        objects.push(Obj::Chain(c));
        for (k, &x) in chain.iter().enumerate() {
            obj_of_le[x] = Some((o, -((k / LES_PER_LAB) as f32)));
        }
    }
    let block_obj: Vec<u32> = (0..d.blocks.len())
        .map(|r| {
            objects.push(Obj::Block(r));
            objects.len() as u32 - 1
        })
        .collect();
    let mut fixed = Vec::new();
    let mut fixed_index: Vec<Option<u32>> = vec![None; d.cells.len()];
    for (c, xy) in fixed_xy.iter().enumerate() {
        if let Some(xy) = xy {
            fixed_index[c] = Some(fixed.len() as u32);
            fixed.push(unit(*xy));
        }
    }
    let nets = d
        .nets
        .iter()
        .map(|members| {
            // One pin per object and offset: a cluster's LEs on the same
            // net are one point.
            let mut pins: Vec<Pin> = Vec::new();
            for &c in members {
                let (of, dy) = match (d.le_of[c], d.block_of[c]) {
                    (Some(le), _) => {
                        let (o, dy) = obj_of_le[le].expect("every LE has an object");
                        (PinOf::Object(o), dy)
                    }
                    (_, Some(r)) => (PinOf::Object(block_obj[r]), 0.0),
                    _ => (
                        PinOf::Fixed(fixed_index[c].expect("a cell is an LE, an M4K or fixed")),
                        0.0,
                    ),
                };
                let pin = Pin { of, dx: 0.0, dy };
                if !pins.contains(&pin) {
                    pins.push(pin);
                }
            }
            Net { weight: 1.0, pins }
        })
        .collect();
    let width = grid.width as usize / 2 + 1;
    let height = grid.height as usize;
    let mut capacity = vec![0.0; width * height];
    for &(x, y) in &grid.xy {
        // One LAB's worth; an LE is 1/16 of it.
        capacity[y as usize * width + (x / 2) as usize] += 1.0;
    }
    let problem = Problem {
        objects: objects
            .iter()
            .map(|o| match *o {
                // A LAB each, however full: two clusters rarely agree on
                // their controls, and the anneal merges those that do.
                Obj::Cluster(_) => Object {
                    class: 0,
                    area: 1.0,
                    height: 1,
                },
                Obj::Chain(c) => Object {
                    class: 0,
                    area: d.chains[c].len() as f32 / LES_PER_LAB as f32,
                    height: d.chains[c].len().div_ceil(LES_PER_LAB) as u32,
                },
                Obj::Block(_) => Object {
                    class: 1,
                    area: 1.0,
                    height: 1,
                },
            })
            .collect(),
        fixed,
        nets,
        bins: Bins {
            width,
            height,
            capacity,
        },
    };
    Analytic {
        problem,
        objects,
        clusters: clusters.to_vec(),
    }
}

/// Snaps the analytic placement onto LAB slots and M4K sites: chains
/// first (longest first), then LEs most critical first, each into the
/// nearest LAB with a free slot that `lab_ok` accepts, searching outwards
/// from its target.
struct LabLegalizer<'s, 'a> {
    state: &'s mut State<'a>,
    an: &'s Analytic,
    /// Bin offsets in order of distance.
    spiral: Vec<(i32, i32)>,
    /// Set when something found no LAB at all.
    failed: bool,
    /// Print each round's numbers to stderr (`CHIPSMITH_PLACE_TRACE`).
    trace: bool,
    round: usize,
    started: std::time::Instant,
    /// Trace counters: LABs skipped for being full, LABs refused by
    /// reason (`Refusal` order).
    full: usize,
    refused: [usize; 5],
}

impl<'s, 'a> LabLegalizer<'s, 'a> {
    fn new(state: &'s mut State<'a>, an: &'s Analytic) -> Self {
        let (w, h) = (an.problem.bins.width as i32, an.problem.bins.height as i32);
        let mut spiral: Vec<(i32, i32)> = (-h..=h)
            .flat_map(|dy| (-w..=w).map(move |dx| (dx, dy)))
            .collect();
        spiral.sort_by_key(|&(dx, dy)| (dx.abs() + dy.abs(), dy, dx));
        LabLegalizer {
            state,
            an,
            spiral,
            failed: false,
            trace: false,
            round: 0,
            started: std::time::Instant::now(),
            full: 0,
            refused: [0; 5],
        }
    }

    /// The LABs in a bin: tile columns 2b and 2b + 1.
    fn labs_in(&self, bx: i32, by: i32) -> impl Iterator<Item = usize> + '_ {
        let g = self.state.grid;
        [2 * bx, 2 * bx + 1]
            .into_iter()
            .filter_map(move |x| g.lab_at((x, by)))
    }

    /// A cluster goes into the nearest LAB with room for all of it that
    /// `lab_ok` accepts with what is already there.
    fn place_cluster(&mut self, i: usize, (tx, ty): Pos) -> bool {
        let members = &self.an.clusters[i];
        let (bx, by) = (tx.floor() as i32, ty.floor() as i32);
        for s in 0..self.spiral.len() {
            let (dx, dy) = self.spiral[s];
            let labs: Vec<usize> = self.labs_in(bx + dx, by + dy).collect();
            for lab in labs {
                let st = &mut *self.state;
                let free: Vec<usize> = (0..LES_PER_LAB)
                    .filter(|&k| st.slots[lab][k].is_none())
                    .collect();
                if free.len() < members.len() {
                    self.full += 1;
                    continue;
                }
                for (&le, &k) in members.iter().zip(&free) {
                    st.put(le, lab, k);
                }
                match st.lab_check(lab) {
                    Ok(()) => return true,
                    Err(why) => {
                        for &le in members {
                            st.unput(le);
                        }
                        self.refused[why as usize] += 1;
                    }
                }
            }
        }
        false
    }

    fn place_chain(&mut self, c: usize, (tx, ty): Pos) -> bool {
        let chain = &self.state.design.chains[c];
        let (bx, by) = (tx.floor() as i32, ty.floor() as i32);
        for i in 0..self.spiral.len() {
            let (dx, dy) = self.spiral[i];
            let labs: Vec<usize> = self.labs_in(bx + dx, by + dy).collect();
            for head in labs {
                let s = &mut *self.state;
                let Some(labs) = s.grid.chain_labs(head, chain.len()) else {
                    continue;
                };
                let free = (0..chain.len())
                    .all(|i| s.slots[labs[i / LES_PER_LAB]][i % LES_PER_LAB].is_none());
                if !free {
                    continue;
                }
                for (i, &le) in chain.iter().enumerate() {
                    s.put(le, labs[i / LES_PER_LAB], i % LES_PER_LAB);
                }
                if labs.iter().all(|&l| s.lab_ok(l)) {
                    return true;
                }
                for &le in chain {
                    s.unput(le);
                }
            }
        }
        false
    }

    fn place_block(&mut self, r: usize, (tx, ty): Pos) -> bool {
        let s = &mut *self.state;
        let kind = s.design.block_kind(r);
        let site = (0..s.grid.site_xy.len())
            .filter(|&i| s.grid.site_block[i] == kind && s.site_of[i].is_none())
            .min_by(|&a, &b| {
                let dist = |i: usize| {
                    let (x, y) = unit(s.grid.site_xy[i]);
                    (x - tx).abs() + (y - ty).abs()
                };
                dist(a).total_cmp(&dist(b)).then(a.cmp(&b))
            });
        match site {
            Some(site) => {
                s.put_block(r, site);
                true
            }
            None => false,
        }
    }
}

impl Legalizer for LabLegalizer<'_, '_> {
    fn legalize(&mut self, targets: &[Pos]) -> Vec<Pos> {
        let d = self.state.design;
        let solved = std::time::Instant::now();
        self.state.clear();
        // Most constrained and most critical first.
        let mut order: Vec<usize> = (0..self.an.objects.len()).collect();
        // Chains, then the biggest clusters: the hardest to fit go first.
        let key = |o: usize| -> (u8, i64, usize) {
            match self.an.objects[o] {
                Obj::Chain(c) => (0, -(d.chains[c].len() as i64), o),
                Obj::Cluster(i) => (1, -(self.an.clusters[i].len() as i64), o),
                Obj::Block(_) => (2, 0, o),
            }
        };
        order.sort_by_key(|&o| key(o));
        let mut legal = targets.to_vec();
        let mut displaced = (0.0f32, 0.0f32, 0usize);
        let mut chain_displaced = 0.0f32;
        self.full = 0;
        self.refused = [0; 5];
        for o in order {
            let target = targets[o];
            let ok = match self.an.objects[o] {
                Obj::Cluster(i) => self.place_cluster(i, target),
                Obj::Chain(c) => self.place_chain(c, target),
                Obj::Block(r) => self.place_block(r, target),
            };
            if !ok {
                self.failed = true;
                if self.trace {
                    eprintln!(
                        "place: nowhere for {:?} (target {:?}; LABs full {}, refused {:?})",
                        self.an.objects[o], target, self.full, self.refused
                    );
                }
                return legal;
            }
            let s = &*self.state;
            legal[o] = match self.an.objects[o] {
                Obj::Cluster(i) => unit(s.grid.xy[s.pos[self.an.clusters[i][0]].0]),
                Obj::Chain(c) => unit(s.grid.xy[s.pos[d.chains[c][0]].0]),
                Obj::Block(r) => unit(s.grid.site_xy[s.block_site[r]]),
            };
            let dist = (legal[o].0 - target.0).abs() + (legal[o].1 - target.1).abs();
            displaced = (
                displaced.0 + dist,
                displaced.1.max(dist),
                displaced.2 + (dist > 2.0) as usize,
            );
            if matches!(self.an.objects[o], Obj::Chain(_)) {
                chain_displaced += dist;
            }
        }
        self.state.recompute_costs();
        if self.trace {
            eprintln!(
                "place: displaced mean {:.2} max {:.1}, {} objects by more than 2; chains {:.0} in all; LABs full {}, refused {:?} (inputs, sync kind, clock, domains, control)",
                displaced.0 / targets.len() as f32,
                displaced.1,
                displaced.2,
                chain_displaced,
                self.full,
                self.refused
            );
        }
        if self.trace {
            self.round += 1;
            eprintln!(
                "place: round {} at {:.1} s: legalised in {:.2} s, wirelength {}",
                self.round,
                self.started.elapsed().as_secs_f64(),
                solved.elapsed().as_secs_f64(),
                self.state.wirelength()
            );
        }
        legal
    }

    fn reweight(&mut self, _legal: &[Pos], weights: &mut [f32], _areas: &mut [f32]) {
        if self.failed {
            return;
        }
        self.state.reweight();
        weights.copy_from_slice(&self.state.weight);
    }
}

/// How much more a fully critical net's wirelength counts than an uncritical
/// one's, and how sharply criticality is weighted (VPR's criticality_exp).
const TIMING_WEIGHT: f32 = 8.0;
const CRIT_EXP: f32 = 4.0;

/// How hard the annealer works: from scratch, or to refine a placement
/// the analytic placer made.
#[derive(Debug, Clone, Copy)]
struct Schedule {
    /// Initial move range, in LABs.
    range: f64,
    /// Initial temperature, as a multiple of the standard deviation of
    /// random moves' cost changes.
    heat: f64,
    /// Moves per temperature, as a multiple of N^(4/3) (VPR's inner_num).
    moves: f64,
}

/// After analytic placement: moves within a few LABs, starting warm
/// enough to reshape the critical paths the wirelength-driven analytic
/// placement left long, not to scramble it. Picked on routed critical
/// paths over every DE2 example, three placement seeds each, against the
/// annealer from scratch it replaced (docs/fast-pnr-status.md): colder
/// (heat 0.1, range 2) left picorv32 12% and m4k-tdp 14% slower than
/// that, hotter (heat 1, range 4, twice the moves) is as good for twice
/// the time.
const REFINE: Schedule = Schedule {
    range: 3.0,
    heat: 0.5,
    moves: 1.0,
};

fn anneal(s: &mut State, rng: &mut Rng, sched: Schedule) -> u64 {
    let d = s.design;
    let movable: Vec<usize> = (0..d.les.len())
        .filter(|&le| d.chain_of[le].is_none_or(|c| d.chains[c][0] == le))
        .collect();
    if movable.is_empty() && d.blocks.is_empty() {
        return 0;
    }
    let max_r = s
        .grid
        .xy
        .iter()
        .map(|&(x, y)| (x / 2).max(y))
        .max()
        .unwrap_or(1) as f64;
    let mut r = sched.range.min(max_r);
    let objects = movable.len() + d.blocks.len();
    let moves_per_t = (sched.moves * (objects as f64).powf(4.0 / 3.0)).max(200.0) as usize;
    let mut total_moves = 0u64;

    let attempt = |s: &mut State, rng: &mut Rng, r: f64, t: f64| -> Option<f64> {
        let pick = rng.below(objects);
        let ri = r.max(1.0) as i32;
        let undo = if let Some(&le) = movable.get(pick) {
            let (x, y) = s.grid.xy[s.pos[le].0];
            let tx = x + 2 * (rng.below(2 * ri as usize + 1) as i32 - ri);
            let ty = y + rng.below(2 * ri as usize + 1) as i32 - ri;
            let lab = s.grid.lab_at((tx, ty))?;
            let k = if d.chain_of[le].is_some() {
                0
            } else {
                rng.below(LES_PER_LAB)
            };
            s.try_move(le, lab, k)?
        } else {
            // Block columns are far apart: pick among the sites of the
            // block's kind in range rather than a point that is almost never
            // one.
            let block = pick - movable.len();
            let here = s.block_site[block];
            let (x, y) = s.grid.site_xy[here];
            let near: Vec<usize> = (0..s.grid.site_xy.len())
                .filter(|&i| {
                    let (sx, sy) = s.grid.site_xy[i];
                    s.grid.site_block[i] == s.grid.site_block[here]
                        && (sx - x).abs() / 2 <= ri
                        && (sy - y).abs() <= ri
                })
                .collect();
            let site = near[rng.below(near.len())];
            s.try_move_block(block, site)?
        };
        let delta = s.apply_cost(&undo);
        // An infinite temperature only probes: the move is measured and
        // taken back, so the placement being refined isn't scrambled.
        if t.is_infinite() {
            s.revert_cost();
            s.undo(&undo);
            return Some(delta);
        }
        if delta <= 0.0 || (t > 0.0 && rng.unit() < (-delta / t).exp()) {
            s.commit_cost();
            Some(delta)
        } else {
            s.revert_cost();
            s.undo(&undo);
            None
        }
    };

    // Starting temperature: so many standard deviations of random moves'
    // deltas.
    let mut deltas = Vec::new();
    for _ in 0..movable.len().max(50) {
        if let Some(dc) = attempt(s, rng, r, f64::INFINITY) {
            deltas.push(dc);
        }
    }
    let mean = deltas.iter().sum::<f64>() / deltas.len().max(1) as f64;
    let var = deltas.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / deltas.len().max(1) as f64;
    let mut t = sched.heat * var.sqrt();

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
        if std::env::var_os("CHIPSMITH_PLACE_TRACE").is_some() {
            eprintln!(
                "place: t {t:.3} r {r:.1} accepted {rate:.2} cost {cost:.0} wirelength {}",
                s.wirelength()
            );
        }
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

/// How many refining anneals run side by side, one thread each, from the
/// same legal start with different seeds. Fixed rather than the machine's
/// core count, so every machine places a design the same way.
const SEEDS: usize = 8;

/// Anneal copies of `start` in parallel; the one with the shortest
/// estimated critical path wins (then the lowest cost, then the first
/// seed), so the result doesn't depend on thread timing. A refining
/// anneal lands in one of many nearby optima whose critical paths differ
/// by several percent; taking the best of a few costs cores, not time.
fn best_of<'a>(start: &State<'a>, seeds: usize, sched: Schedule) -> (State<'a>, u64) {
    // Another set of seeds, for measuring how much a design's result
    // depends on them.
    let base: u64 = std::env::var("CHIPSMITH_PLACE_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let results: Vec<(State<'a>, u64, Ps, f64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..seeds)
            .map(|i| {
                let mut s = start.clone();
                scope.spawn(move || {
                    // Seed 0 is the one a single anneal always used.
                    let seed = 0x9e37_79b9_7f4a_7c15
                        ^ (i as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9)
                        ^ base.wrapping_mul(0x94d0_49bb_1331_11eb);
                    let moves = anneal(&mut s, &mut Rng(seed), sched);
                    let critical = s.reweight();
                    let cost = s.total();
                    (s, moves, critical, cost)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("anneal thread"))
            .collect()
    });
    let moves = results.iter().map(|r| r.1).sum();
    let best = results
        .into_iter()
        .min_by(|a, b| a.2.total_cmp(&b.2).then(a.3.total_cmp(&b.3)))
        .expect("at least one seed")
        .0;
    (best, moves)
}

/// Where a top-level port bit goes and how its pin is set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinPlan {
    /// `PIN_AE23`.
    pub pin: String,
    pub settings: PinSettings,
}

/// Place `packed`, write `placed`. `pins` maps top-level ports (`LEDR[0]`)
/// to pins; their settings go onto the pins' cells as parameters, and
/// `unused_pins` onto the top module as an attribute, for the bitstream
/// (`crate::pin`).
/// A PLL clock on an ordinary pin, which the packer put on the PLL's
/// external clock output (`pack_pll_outputs`): the pin goes on the clock's
/// global instead, as data (cyclone2-re README, "Global clocks as data").
/// The PLLOUT cell becomes the global's clock control, or, if the clock
/// has a global already, the pin moves onto it and the cell goes.
fn globals_for_pll_pins(json: &mut Value, design: &Design, cells: &[usize]) -> Result<()> {
    let top = json["modules"]["top"]["cells"]
        .as_object_mut()
        .context("no cells")?;
    let net = |c: &Value, p: &str| {
        c["connections"][p]
            .as_array()
            .and_then(|a| a.first()?.as_u64())
    };
    for &i in cells {
        let name = &design.cells[i].name;
        let po = &top[name.as_str()];
        let (inclk, outclk) = (net(po, "INCLK"), net(po, "OUTCLK"));
        let global = top
            .values()
            .find(|c| c["type"] == "CLKCTRL" && net(c, "INCLK") == inclk)
            .and_then(|c| net(c, "OUTCLK"));
        let Some(global) = global else {
            top.get_mut(name.as_str()).context("PLLOUT cell")?["type"] = Value::from("CLKCTRL");
            continue;
        };
        top.remove(name.as_str());
        for c in top.values_mut() {
            for bits in c["connections"]
                .as_object_mut()
                .into_iter()
                .flat_map(|m| m.values_mut())
            {
                if bits.as_array().and_then(|a| a.first()?.as_u64()) == outclk {
                    *bits = Value::from(vec![global]);
                }
            }
        }
    }
    Ok(())
}

pub fn place_file(
    db: &ChipDb,
    packed: &Path,
    pins: &std::collections::BTreeMap<String, PinPlan>,
    unused_pins: UnusedPins,
    placed: &Path,
) -> Result<Stats> {
    let text =
        std::fs::read_to_string(packed).with_context(|| format!("reading {}", packed.display()))?;
    let mut json: Value = serde_json::from_str(&text)?;
    let mut design = read_design(&json)?;
    let fixed = match fixed_cells(db, &design, pins)? {
        Fixed::Sites(f) => f,
        Fixed::OffPin(cells) => {
            globals_for_pll_pins(&mut json, &design, &cells)?;
            design = read_design(&json)?;
            match fixed_cells(db, &design, pins)? {
                Fixed::Sites(f) => f,
                Fixed::OffPin(_) => bail!("a PLL clock output is still off its pin"),
            }
        }
    };

    let grid = Grid::new(db);
    let mut fixed_xy = vec![None; design.cells.len()];
    for (&c, &(tile, _)) in &fixed {
        let (x, y) = db.tile_xy(tile);
        fixed_xy[c] = Some((x as i32, y as i32));
    }
    let mut state = State::new(&design, &grid, &fixed_xy);
    let trace = std::env::var_os("CHIPSMITH_PLACE_TRACE").is_some();
    if trace {
        let xs: std::collections::BTreeSet<i32> = grid.xy.iter().map(|p| p.0).collect();
        let ys: std::collections::BTreeSet<i32> = grid.xy.iter().map(|p| p.1).collect();
        eprintln!(
            "place: grid {}x{}, {} LABs at x {:?} y {:?}..{:?}; {} fixed cells at x {:?}..{:?}",
            grid.width,
            grid.height,
            grid.xy.len(),
            xs,
            ys.first(),
            ys.last(),
            fixed_xy.iter().flatten().count(),
            fixed_xy.iter().flatten().map(|p| p.0).min(),
            fixed_xy.iter().flatten().map(|p| p.0).max()
        );
    }
    let started = std::time::Instant::now();
    let clusters = cluster(&design, &grid, &fixed_xy);
    if trace {
        let mut sizes = [0usize; LES_PER_LAB + 1];
        for c in &clusters {
            sizes[c.len()] += 1;
        }
        let controls: HashSet<_> = design
            .sigs
            .iter()
            .filter_map(|s| s.ff.map(|f| (f.ena, f.sclr, f.aclr, f.sync_kind)))
            .collect();
        let distinct = |f: &dyn Fn(&FfCtl) -> Option<NetId>| {
            design
                .sigs
                .iter()
                .filter_map(|s| s.ff.and_then(|c| f(&c)))
                .collect::<HashSet<_>>()
                .len()
        };
        eprintln!(
            "place: {} clusters of {} LEs (sizes 1..16: {:?}), {} register control sets ({} enable, {} sclr, {} aclr nets), {} chains, in {:.1} s",
            clusters.len(),
            clusters.iter().map(Vec::len).sum::<usize>(),
            &sizes[1..],
            controls.len(),
            distinct(&|c| c.ena.0),
            distinct(&|c| c.sclr.0),
            distinct(&|c| c.aclr),
            design.chains.len(),
            started.elapsed().as_secs_f64()
        );
    }
    for b in Block::ALL {
        let used = (0..design.blocks.len())
            .filter(|&r| design.block_kind(r) == b)
            .count();
        let sites = grid.site_block.iter().filter(|&&s| s == b).count();
        ensure!(
            used <= sites,
            "the design has {used} {}, the chip {sites}",
            b.name()
        );
    }
    // More clusters than LABs can still fit: the legaliser puts small
    // clusters into the free slots of carry chains' last LABs and of
    // other small clusters when `lab_ok` accepts the union. Only more LEs
    // than slots is hopeless up front.
    ensure!(
        design.les.len() <= grid.tiles.len() * LES_PER_LAB,
        "the design needs {} LEs, the chip has {}",
        design.les.len(),
        grid.tiles.len() * LES_PER_LAB
    );
    let an = analytic_problem(&design, &grid, &fixed_xy, &clusters);
    let rounds = {
        let mut legalizer = LabLegalizer::new(&mut state, &an);
        legalizer.trace = trace;
        let env = |name: &str, default: f32| -> f32 {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        let defaults = Options::default();
        let options = Options {
            trace,
            // Objects are whole LABs' worth: no slack is needed for
            // legalisation.
            target_density: env("CHIPSMITH_PLACE_DENSITY", 1.0),
            anchor: env("CHIPSMITH_PLACE_ANCHOR", defaults.anchor),
            rounds: env("CHIPSMITH_PLACE_ROUNDS", defaults.rounds as f32) as usize,
            patience: env("CHIPSMITH_PLACE_PATIENCE", defaults.patience as f32) as usize,
        };
        let outcome = chipsmith_place::place(&an.problem, &mut legalizer, options);
        legalizer.legalize(&outcome.targets);
        ensure!(!legalizer.failed, "the design doesn't fit");
        outcome.rounds
    };
    let initial_cost = state.wirelength();
    if trace {
        eprintln!(
            "place: analytic {rounds} rounds, wirelength {initial_cost}, critical {:.2} ns, {:.1} s",
            state.reweight() / 1000.0,
            started.elapsed().as_secs_f64()
        );
    }
    let started = std::time::Instant::now();
    let env = |name: &str, default: f64| -> f64 {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let sched = Schedule {
        range: env("CHIPSMITH_PLACE_RANGE", REFINE.range),
        heat: env("CHIPSMITH_PLACE_HEAT", REFINE.heat),
        moves: env("CHIPSMITH_PLACE_MOVES", REFINE.moves),
    };
    let seeds = env("CHIPSMITH_PLACE_SEEDS", SEEDS as f64).max(1.0) as usize;
    let (mut state, moves) = best_of(&state, seeds, sched);
    let critical = state.reweight();
    if trace {
        eprintln!(
            "place: anneal {moves} moves, wirelength {}, critical {:.2} ns, {:.1} s",
            state.wirelength(),
            critical / 1000.0,
            started.elapsed().as_secs_f64()
        );
    }

    let top = &mut json["modules"]["top"];
    if unused_pins != UnusedPins::default() {
        top.as_object_mut()
            .context("no top module")?
            .entry("attributes")
            .or_insert_with(|| Value::Object(Default::default()))[assignment::UNUSED_PINS] =
            Value::String(unused_pins.quartus_name().into());
    }
    let cells = top["cells"].as_object_mut().context("no cells")?;
    for (i, c) in design.cells.iter().enumerate() {
        let (tile, bel) = match (design.le_of[i], design.block_of[i]) {
            (Some(le), _) => {
                let (lab, k) = state.pos[le];
                let half = if c.kind == Kind::Lut { "LUT" } else { "FF" };
                (grid.tiles[lab], format!("LE{k}.{half}"))
            }
            (_, Some(r)) => {
                let site = state.block_site[r];
                (
                    grid.site_tiles[site],
                    grid.site_block[site].bel().to_string(),
                )
            }
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
        if let Some(plan) = c.port.as_ref().and_then(|p| pins.get(p)) {
            let cell = cells.get_mut(&c.name).context("cell vanished")?;
            let params = cell
                .as_object_mut()
                .context("cell is not an object")?
                .entry("parameters")
                .or_insert_with(|| Value::Object(Default::default()));
            for (k, v) in plan.settings.params() {
                params[k] = Value::String(v);
            }
        }
    }
    std::fs::write(placed, serde_json::to_string(&json)?)
        .with_context(|| format!("writing {}", placed.display()))?;
    Ok(Stats {
        les: design.les.len(),
        chains: design.chains.len(),
        rounds,
        initial_cost,
        cost: state.wirelength(),
        moves,
        critical,
    })
}
