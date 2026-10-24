//! chipsmith's 7-series placer: simulated annealing over slices.
//!
//! Input is nextpnr's packed netlist (`nextpnr-himbaechel --pack-only
//! --write`): its packer has already turned yosys' cells into bel types
//! (SLICE_LUTX, SLICE_FFX, CARRY4, PAD, IOB33_*, BUFGCTRL, BSCAN, PS7), split and
//! rechained the carries, added the feed-through LUTs and placed the pins
//! from the XDC. Which LUTs and registers share a slice, and where every
//! slice, carry chain, RAM and clock buffer goes, is decided here. The output
//! is one `-o preplaced` line per cell (`<cell>\t<bel>`, the bel spelled the
//! way nextpnr reads it), so nextpnr binds the whole design and has nothing
//! left to place.
//!
//! Slices are filled in a subset of what nextpnr's `xc7_logic_tile_valid`
//! accepts, chosen so that legality is a property of the shapes rather than
//! something to check after each move:
//!
//! - a BLE (a LUT in an `x6LUT` and the register in `xFF` beside it, or
//!   either alone) goes in any slice; the register takes its D from the LUT's
//!   O6 or through the X bypass, which nothing else in a logic slice uses.
//!   The only per-slice rule left is that every register in a slice shares
//!   one control set (clock, CE, SR and their modes);
//! - a carry chain owns whole slices: CARRY4 k sits in the slice above
//!   CARRY4 k-1 (the next one on the COUT -> CIN wire), its S LUTs in the
//!   `x6LUT`s, its DI LUTs in the `x5LUT`s (where nextpnr's packer put them
//!   in a cluster), and registers fed straight from a sum output in the
//!   `xFF` of that bit;
//! - a RAM32M (eight RAMD32 LUTs) owns a whole SLICEM.
//!
//! Carry chains and RAMs go in the even slice of a tile (X0, the SLICEM of a
//! CLBLM), as nextpnr's packer constrains them. Pins are where the XDC put
//! them; the BSCAN for USER<n> is BSCAN_X0Y<n-1>; the PS7 has the one PS7
//! site; each BUFGCTRL takes the free one nearest its source.
//!
//! The cost is half-perimeter wirelength, x counted at half weight: grid x
//! advances by two or more per interconnect column (site columns are
//! columns of their own), y by one per interconnect row. Clock nets (driven
//! by a BUFGCTRL) and the constant nets are left out; they don't use the
//! general interconnect.
//!
//! A design with anything else (block RAM, DSPs, other distributed RAM
//! shapes, SRLs, wide muxes, I/O logic) comes back `Unsupported`, and the
//! flow lets nextpnr place it as before.

use std::path::Path;
use std::time::Instant;

use anyhow::{bail, ensure, Context, Result};
use rustc_hash::FxHashMap as HashMap;
use serde_json::Value;

use super::xc7_delay::Delays;
use crate::route::chipdb::ChipDb;
use crate::route::xc7::bel_name;
use crate::timing::{self, Arc, Ps};

type NetId = u32;
type CellId = u32;
/// A position on the grid: tile x, tile y.
type Xy = (i32, i32);

/// The registers' shared configuration within a slice: nextpnr's half-tile
/// check compares exactly these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ControlSet {
    clk: Option<NetId>,
    ce: Option<NetId>,
    sr: Option<NetId>,
    clk_inv: bool,
    sr_inv: bool,
    latch: bool,
    sync: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Lut {
        dram: bool,
    },
    Ff(ControlSet),
    Carry,
    /// A package pin, placed by the XDC: its bel as nextpnr reads it.
    Pad {
        bel: String,
    },
    InBuf,
    OutBuf,
    Bufg,
    Bscan {
        chain: u32,
    },
    /// The Zynq's processor system, as seen from the fabric.
    Ps7,
    Gnd,
    Vcc,
}

#[derive(Debug)]
struct Cell {
    name: String,
    kind: Kind,
    pins: Vec<(String, NetId)>,
}

impl Cell {
    fn net(&self, port: &str) -> Option<NetId> {
        self.pins.iter().find(|(p, _)| p == port).map(|&(_, n)| n)
    }
}

#[derive(Debug, Default)]
struct Net {
    driver: Option<(CellId, String)>,
    users: Vec<(CellId, String)>,
}

/// A design chipsmith doesn't place yet, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported(pub String);

/// One LUT and the register beside it.
#[derive(Debug, Clone, Copy)]
struct Ble {
    lut: Option<CellId>,
    ff: Option<CellId>,
}

/// One slice of a carry chain: the CARRY4 and, per bit, the cells nextpnr's
/// packer tied to it.
#[derive(Debug, Clone, Default)]
struct CarrySlice {
    carry: CellId,
    lut6: [Option<CellId>; 4],
    lut5: [Option<CellId>; 4],
    ff: [Option<CellId>; 4],
}

/// A RAM32M: per bit, the RAMD32 in the 6LUT and the one in the 5LUT.
#[derive(Debug, Clone)]
struct Dram {
    lut6: [CellId; 4],
    lut5: [CellId; 4],
}

struct Design {
    cells: Vec<Cell>,
    nets: Vec<Net>,
    /// Distinct cells of each net that counts for wirelength (empty for
    /// clock and constant nets and nets within one cell).
    net_cells: Vec<Vec<CellId>>,
    /// Nets that count, per cell.
    cell_nets: Vec<Vec<NetId>>,
    bles: Vec<Ble>,
    carries: Vec<Vec<CarrySlice>>,
    drams: Vec<Dram>,
    /// Each BLE's register's control set, numbered densely.
    ble_cs: Vec<Option<u32>>,
}

const LETTERS: [char; 4] = ['A', 'B', 'C', 'D'];

/// nextpnr's attributes are present or absent (`"1 "` when present).
fn has_attr(cell: &Value, name: &str) -> bool {
    cell["attributes"].get(name).is_some()
}

/// A parameter as nextpnr writes it: a binary string, or a number.
fn param(cell: &Value, name: &str) -> u64 {
    match &cell["parameters"][name] {
        Value::String(s) if s.chars().all(|c| c == '0' || c == '1') && !s.is_empty() => {
            u64::from_str_radix(s, 2).unwrap_or(0)
        }
        Value::String(s) => s.trim().parse().unwrap_or(0),
        Value::Number(n) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

fn top_module(json: &Value) -> Result<&Value> {
    let modules = json["modules"].as_object().context("no modules")?;
    modules
        .values()
        .find(|m| {
            m["attributes"]["top"]
                .as_str()
                .is_some_and(|t| t.trim_start_matches('0') == "1")
        })
        .or_else(|| (modules.len() == 1).then(|| modules.values().next().unwrap()))
        .context("no top module")
}

/// The packed netlist, or what in it chipsmith can't place yet.
fn read_design(json: &Value) -> Result<std::result::Result<Design, Unsupported>> {
    let top = top_module(json)?;
    let mut net_ids: HashMap<u64, NetId> = HashMap::default();
    let mut cells = Vec::new();
    for (name, c) in top["cells"].as_object().context("no cells")? {
        let ty = c["type"].as_str().context("cell type")?;
        let mut pins = Vec::new();
        for (port, bits) in c["connections"].as_object().context("connections")? {
            let bits = bits.as_array().context("connection bits")?;
            ensure!(
                bits.len() <= 1,
                "cell {name} port {port}: {} bits",
                bits.len()
            );
            // Strings are constant bits ("0", "1", "x"); nextpnr's packer has
            // already tied constants to its GND and VCC nets.
            if let Some(bit) = bits.first().and_then(Value::as_u64) {
                let next = net_ids.len() as NetId;
                pins.push((port.clone(), *net_ids.entry(bit).or_insert(next)));
            }
        }
        let net_of = |port: &str| pins.iter().find(|(p, _)| p == port).map(|&(_, n)| n);
        let kind = match ty {
            "SLICE_LUTX" => {
                if has_attr(c, "X_LUT_AS_SRL") {
                    return Ok(Err(Unsupported(format!("shift register {name}"))));
                }
                let dram = has_attr(c, "X_LUT_AS_DRAM");
                // Before nextpnr's post-placement fixup every LUT drives O6
                // (a DI feed-through moves to O5 once placed); RAMD32s in a
                // 5LUT drive O5 already.
                if !dram && net_of("O5").is_some() {
                    return Ok(Err(Unsupported(format!("dual-output LUT {name}"))));
                }
                Kind::Lut { dram }
            }
            "SLICE_FFX" => Kind::Ff(ControlSet {
                clk: net_of("CK"),
                ce: net_of("CE"),
                sr: net_of("SR"),
                clk_inv: param(c, "IS_C_INVERTED") == 1,
                sr_inv: [
                    "IS_R_INVERTED",
                    "IS_S_INVERTED",
                    "IS_CLR_INVERTED",
                    "IS_PRE_INVERTED",
                ]
                .iter()
                .any(|p| param(c, p) == 1),
                latch: has_attr(c, "X_FF_AS_LATCH"),
                sync: has_attr(c, "X_FFSYNC"),
            }),
            "CARRY4" => Kind::Carry,
            "PAD" => Kind::Pad {
                bel: c["attributes"]["BEL"]
                    .as_str()
                    .with_context(|| format!("pad {name} has no pin"))?
                    .to_string(),
            },
            "IOB33_INBUF_EN" => Kind::InBuf,
            "IOB33_OUTBUF" => Kind::OutBuf,
            "BUFGCTRL" => Kind::Bufg,
            "BSCAN" => Kind::Bscan {
                chain: match param(c, "JTAG_CHAIN") {
                    0 => 1,
                    n => n as u32,
                },
            },
            "PS7_PS7" => Kind::Ps7,
            "PSEUDO_GND" => Kind::Gnd,
            "PSEUDO_VCC" => Kind::Vcc,
            other => return Ok(Err(Unsupported(format!("cell {name} of type {other}")))),
        };
        cells.push(Cell {
            name: name.clone(),
            kind,
            pins,
        });
    }

    let mut nets: Vec<Net> = (0..net_ids.len()).map(|_| Net::default()).collect();
    for (i, (_, c)) in top["cells"]
        .as_object()
        .expect("read above")
        .iter()
        .enumerate()
    {
        let dirs = &c["port_directions"];
        for (port, net) in &cells[i].pins {
            let n = &mut nets[*net as usize];
            if dirs[port.as_str()].as_str() == Some("output") {
                ensure!(n.driver.is_none(), "net {net} has two drivers");
                n.driver = Some((i as CellId, port.clone()));
            } else {
                n.users.push((i as CellId, port.clone()));
            }
        }
    }
    build(cells, nets).map(|d| {
        d.map(|mut d| {
            d.index_nets();
            d
        })
    })
}

fn build(cells: Vec<Cell>, nets: Vec<Net>) -> Result<std::result::Result<Design, Unsupported>> {
    let mut d = Design {
        cells,
        nets,
        net_cells: Vec::new(),
        cell_nets: Vec::new(),
        bles: Vec::new(),
        carries: Vec::new(),
        drams: Vec::new(),
        ble_cs: Vec::new(),
    };
    // Which object each LUT and register went into.
    let mut taken = vec![false; d.cells.len()];
    let lut_driver = |d: &Design, net: Option<NetId>| -> Option<CellId> {
        let (c, port) = d.nets[net? as usize].driver.as_ref()?;
        (matches!(d.cells[*c as usize].kind, Kind::Lut { dram: false }) && port == "O6")
            .then_some(*c)
    };
    let cs_of = |d: &Design, c: CellId| match d.cells[c as usize].kind {
        Kind::Ff(cs) => Some(cs),
        _ => None,
    };

    // Carry chains: CO3 -> CIN links, from the CARRY4 whose CIN no CARRY4
    // drives.
    let is_carry = |d: &Design, c: CellId| d.cells[c as usize].kind == Kind::Carry;
    let next_carry = |d: &Design, c: CellId| -> Option<CellId> {
        let co = d.cells[c as usize].net("CO3")?;
        d.nets[co as usize]
            .users
            .iter()
            .find(|(u, p)| p == "CIN" && is_carry(d, *u))
            .map(|&(u, _)| u)
    };
    let roots: Vec<CellId> = (0..d.cells.len() as CellId)
        .filter(|&c| is_carry(&d, c))
        .filter(|&c| {
            let cin = d.cells[c as usize].net("CIN");
            !cin.and_then(|n| d.nets[n as usize].driver.as_ref())
                .is_some_and(|(drv, _)| is_carry(&d, *drv))
        })
        .collect();
    let mut chained = 0;
    for root in roots {
        let mut chain = Vec::new();
        let mut at = Some(root);
        while let Some(c) = at {
            ensure!(
                !taken[c as usize],
                "carry chain loops at {}",
                d.cells[c as usize].name
            );
            taken[c as usize] = true;
            let cell = &d.cells[c as usize];
            let mut s = CarrySlice {
                carry: c,
                ..Default::default()
            };
            for k in 0..4 {
                for (port, slot) in [("S", &mut s.lut6[k]), ("DI", &mut s.lut5[k])] {
                    if let Some(l) = lut_driver(&d, cell.net(&format!("{port}{k}"))) {
                        if taken[l as usize] {
                            return Ok(Err(Unsupported(format!(
                                "LUT {} feeds two carry bits",
                                d.cells[l as usize].name
                            ))));
                        }
                        taken[l as usize] = true;
                        *slot = Some(l);
                    }
                }
            }
            // A register on a sum output goes in that bit's FF: the xFFMUX
            // takes O<k> without leaving the slice. All of them share the
            // slice's control set.
            let mut slice_cs = None;
            for k in 0..4 {
                let Some(o) = cell.net(&format!("O{k}")) else {
                    continue;
                };
                for (u, p) in &d.nets[o as usize].users {
                    let Some(cs) = cs_of(&d, *u).filter(|_| p == "D" && !taken[*u as usize]) else {
                        continue;
                    };
                    if slice_cs.is_none_or(|s| s == cs) {
                        slice_cs = Some(cs);
                        taken[*u as usize] = true;
                        s.ff[k] = Some(*u);
                        break;
                    }
                }
            }
            chain.push(s);
            at = next_carry(&d, c);
        }
        chained += chain.len();
        d.carries.push(chain);
    }
    let carries = d.cells.iter().filter(|c| c.kind == Kind::Carry).count();
    ensure!(
        chained == carries,
        "{} CARRY4s in no chain",
        carries - chained
    );

    // RAM32Ms: nextpnr's packer names the eight RAMD32s `<ram>/DPR<k>_<h>`,
    // bit k, h = 1 in the 6LUT and 0 in the 5LUT.
    let mut rams: HashMap<&str, [[Option<CellId>; 2]; 4]> = HashMap::default();
    for (i, c) in d.cells.iter().enumerate() {
        if c.kind != (Kind::Lut { dram: true }) {
            continue;
        }
        let parsed = c.name.rsplit_once("/DPR").and_then(|(ram, bit)| {
            let (k, h) = bit.split_once('_')?;
            let (k, h): (usize, usize) = (k.parse().ok()?, h.parse().ok()?);
            (k < 4 && h < 2).then_some((ram, k, h))
        });
        let Some((ram, k, h)) = parsed else {
            return Ok(Err(Unsupported(format!("distributed RAM {}", c.name))));
        };
        rams.entry(ram).or_default()[k][h] = Some(i as CellId);
    }
    let mut rams: Vec<_> = rams.into_iter().collect();
    rams.sort_by(|a, b| a.0.cmp(b.0));
    for (ram, bits) in rams {
        let mut dram = Dram {
            lut6: [0; 4],
            lut5: [0; 4],
        };
        for (k, bit) in bits.iter().enumerate() {
            match *bit {
                [Some(l5), Some(l6)] => {
                    dram.lut5[k] = l5;
                    dram.lut6[k] = l6;
                }
                _ => {
                    return Ok(Err(Unsupported(format!(
                        "distributed RAM {ram} isn't a RAM32M"
                    ))))
                }
            }
        }
        for c in dram.lut5.iter().chain(&dram.lut6) {
            taken[*c as usize] = true;
        }
        d.drams.push(dram);
    }

    // BLEs: each register with the LUT driving its D, if that LUT is free.
    let mut ble_of_lut: Vec<bool> = taken.clone();
    for c in 0..d.cells.len() as CellId {
        if taken[c as usize] || cs_of(&d, c).is_none() {
            continue;
        }
        let lut = lut_driver(&d, d.cells[c as usize].net("D")).filter(|&l| !ble_of_lut[l as usize]);
        if let Some(l) = lut {
            ble_of_lut[l as usize] = true;
        }
        taken[c as usize] = true;
        d.bles.push(Ble { lut, ff: Some(c) });
    }
    for c in 0..d.cells.len() as CellId {
        if !ble_of_lut[c as usize] && matches!(d.cells[c as usize].kind, Kind::Lut { .. }) {
            d.bles.push(Ble {
                lut: Some(c),
                ff: None,
            });
        }
    }
    let mut cs_ids: HashMap<ControlSet, u32> = HashMap::default();
    let ble_cs: Vec<Option<u32>> = d
        .bles
        .iter()
        .map(|b| {
            let cs = cs_of(&d, b.ff?)?;
            let next = cs_ids.len() as u32;
            Some(*cs_ids.entry(cs).or_insert(next))
        })
        .collect();
    d.ble_cs = ble_cs;
    Ok(Ok(d))
}

impl Design {
    /// Which nets count for wirelength, and their cells.
    fn index_nets(&mut self) {
        self.net_cells = vec![Vec::new(); self.nets.len()];
        self.cell_nets = vec![Vec::new(); self.cells.len()];
        for (n, net) in self.nets.iter().enumerate() {
            let Some((drv, _)) = &net.driver else {
                continue;
            };
            if matches!(
                self.cells[*drv as usize].kind,
                Kind::Bufg | Kind::Gnd | Kind::Vcc
            ) {
                continue;
            }
            let mut cells: Vec<CellId> = std::iter::once(*drv)
                .chain(net.users.iter().map(|(c, _)| *c))
                .collect();
            cells.sort_unstable();
            cells.dedup();
            if cells.len() < 2 {
                continue;
            }
            for &c in &cells {
                self.cell_nets[c as usize].push(n as NetId);
            }
            self.net_cells[n] = cells;
        }
    }
}

/// A connection from a cell output to a cell input, for timing.
#[derive(Debug, Clone, Copy)]
struct Conn {
    from_cell: CellId,
    to_cell: CellId,
    /// Timing nodes of the two ports.
    from: u32,
    to: u32,
    net: NetId,
    /// Stays inside a slice whatever the placement: a LUT into its BLE's
    /// register, a carry chain's own LUTs and registers, CO3 into the next
    /// CIN.
    internal: bool,
}

/// The design's timing graph (cell arcs, path starts and ends) and every
/// connection the placement decides the delay of.
struct Timing {
    graph: timing::Graph,
    conns: Vec<Conn>,
    delays: Delays,
}

/// Criticality weighting, as the Cyclone II placer does it (VPR's): a net
/// counts `1 + TIMING_WEIGHT * criticality^CRIT_EXP` times its wirelength.
/// The Cyclone II's 8 is too weak here: swept on picorv32 and
/// zybo-jtag-counter, nextpnr's Fmax on chipsmith's routing (README), 20
/// came out best; 40 and 80 no better, a lower exponent worse.
const TIMING_WEIGHT: f32 = 20.0;
const CRIT_EXP: f32 = 4.0;

impl Timing {
    fn new(d: &Design, delays: &Delays) -> Self {
        let mut g = timing::Graph::default();
        // Which object (and which slice of it) each cell is in: 0 BLE, 1
        // carry chain, 2 RAM.
        let mut obj: Vec<Option<(u8, u32, u32)>> = vec![None; d.cells.len()];
        for (b, ble) in d.bles.iter().enumerate() {
            for c in ble.lut.into_iter().chain(ble.ff) {
                obj[c as usize] = Some((0, b as u32, 0));
            }
        }
        for (k, chain) in d.carries.iter().enumerate() {
            for (i, cs) in chain.iter().enumerate() {
                let cells = std::iter::once(cs.carry).chain(
                    cs.lut6
                        .iter()
                        .chain(&cs.lut5)
                        .chain(&cs.ff)
                        .flatten()
                        .copied(),
                );
                for c in cells {
                    obj[c as usize] = Some((1, k as u32, i as u32));
                }
            }
        }
        for (r, ram) in d.drams.iter().enumerate() {
            for &c in ram.lut6.iter().chain(&ram.lut5) {
                obj[c as usize] = Some((2, r as u32, 0));
            }
        }

        for c in &d.cells {
            let ports: Vec<&str> = c.pins.iter().map(|(p, _)| p.as_str()).collect();
            let node = |g: &mut timing::Graph, p: &str| g.node(&c.name, p);
            match &c.kind {
                Kind::Lut { .. } => {
                    let outs: Vec<&str> = ports
                        .iter()
                        .copied()
                        .filter(|p| *p == "O6" || *p == "O5")
                        .collect();
                    for &i in &ports {
                        if i.len() == 2 && i.starts_with('A') {
                            for &o in &outs {
                                let (from, to) = (node(&mut g, i), node(&mut g, o));
                                g.add_arc(from, to, delays.lut);
                            }
                        } else if i.starts_with("DI") || i.starts_with("WA") || i == "WE" {
                            // A RAM write: its data, address and enable are
                            // sampled on the clock like a register's D.
                            let n = node(&mut g, i);
                            g.set_setup(n, delays.setup_ce);
                        }
                    }
                }
                Kind::Ff(_) => {
                    for &p in &ports {
                        let setup = match p {
                            "Q" => {
                                let n = node(&mut g, p);
                                g.set_launch(n, delays.clock_to_q);
                                continue;
                            }
                            "D" => delays.setup_d,
                            "CE" => delays.setup_ce,
                            "SR" => delays.recovery_sr,
                            _ => continue,
                        };
                        let n = node(&mut g, p);
                        g.set_setup(n, setup);
                    }
                }
                Kind::Carry => {
                    for &i in &ports {
                        for &o in &ports {
                            if let Some(&delay) = delays.carry.get(&(i.to_string(), o.to_string()))
                            {
                                let (from, to) = (node(&mut g, i), node(&mut g, o));
                                g.add_arc(from, to, delay);
                            }
                        }
                    }
                }
                // Pins, the BSCAN, the PS7 and clock buffers neither launch nor
                // capture: like Quartus' and nextpnr's reports with only a
                // clock constraint, paths run register to register.
                _ => {}
            }
        }

        let mut conns = Vec::new();
        for (n, net) in d.nets.iter().enumerate() {
            if d.net_cells[n].is_empty() {
                continue;
            }
            let Some((drv, dport)) = &net.driver else {
                continue;
            };
            for (user, uport) in &net.users {
                let internal = match (obj[*drv as usize], obj[*user as usize]) {
                    (Some((0, a, _)), Some((0, b, _))) => a == b && uport == "D",
                    (Some((1, a, i)), Some((1, b, j))) => {
                        a == b && (i == j || (dport == "CO3" && uport == "CIN"))
                    }
                    _ => false,
                };
                conns.push(Conn {
                    from_cell: *drv,
                    to_cell: *user,
                    from: g.node(&d.cells[*drv as usize].name, dport),
                    to: g.node(&d.cells[*user as usize].name, uport),
                    net: n as NetId,
                    internal,
                });
            }
        }
        Timing {
            graph: g,
            conns,
            delays: delays.clone(),
        }
    }
}

/// One slice site.
#[derive(Debug, Clone)]
struct Slice {
    /// Absolute site name, `SLICE_X30Y39`.
    site: String,
    xy: Xy,
    /// The tile's X0 slice: where nextpnr's packer constrains carry chains
    /// and distributed RAM.
    even: bool,
    slicem: bool,
    /// The slice whose CIN this slice's COUT drives.
    next: Option<u32>,
    /// Index among the device's slice columns, for picking nearby slices.
    col: u32,
}

/// The sites the placer uses.
struct Grid {
    slices: Vec<Slice>,
    /// (column, y, even) -> slice.
    at: HashMap<(u32, i32, bool), u32>,
    cols: u32,
    y_max: i32,
    /// BUFGCTRL sites: (site, position).
    bufgs: Vec<(String, Xy)>,
}

impl Grid {
    fn new(db: &ChipDb) -> Result<Self> {
        let mut slices: Vec<Slice> = Vec::new();
        let mut bufgs = Vec::new();
        let xy_of = |tile: u32| {
            let (x, y) = db.tile_xy(tile);
            (x as i32, y as i32)
        };
        let mut sites: Vec<(&String, &crate::route::chipdb::Site)> = db.sites.iter().collect();
        sites.sort_by(|a, b| a.0.cmp(b.0));
        for (name, s) in sites {
            match s.ty.as_str() {
                "SLICEL" | "SLICEM" => slices.push(Slice {
                    site: name.clone(),
                    xy: xy_of(s.tile),
                    even: s.relative.starts_with("SLICE_X0"),
                    slicem: s.ty == "SLICEM",
                    next: None,
                    col: 0,
                }),
                "BUFGCTRL" => bufgs.push((name.clone(), xy_of(s.tile))),
                _ => {}
            }
        }
        ensure!(!slices.is_empty(), "the device has no slices");
        let mut xs: Vec<i32> = slices.iter().map(|s| s.xy.0).collect();
        xs.sort_unstable();
        xs.dedup();
        for s in &mut slices {
            s.col = xs.binary_search(&s.xy.0).expect("collected above") as u32;
        }

        // Carry successors from the routing graph: COUT drives COUT_N
        // (an always-on pip), which is the CIN of the slice above, across
        // the HCLK row too.
        let pin_node = |s: &Slice, pin: &str| -> Option<u32> {
            let site = &db.sites[&s.site];
            let tt = db.tile_type(site.tile);
            let wire = *tt.bels.get(&site.relative)?.pins.get(pin)?;
            Some(db.node(site.tile, wire))
        };
        let cin: HashMap<u32, u32> = slices
            .iter()
            .enumerate()
            .filter_map(|(i, s)| Some((pin_node(s, "CIN")?, i as u32)))
            .collect();
        for slice in &mut slices {
            let Some(cout) = pin_node(slice, "COUT") else {
                continue;
            };
            slice.next = std::iter::once(cout)
                .chain(db.downhill(cout).iter().map(|e| e.dst))
                .find_map(|n| cin.get(&n).copied());
        }
        let at = slices
            .iter()
            .enumerate()
            .map(|(i, s)| ((s.col, s.xy.1, s.even), i as u32))
            .collect();
        let y_max = slices.iter().map(|s| s.xy.1).max().unwrap_or(0);
        Ok(Grid {
            cols: xs.len() as u32,
            y_max,
            slices,
            at,
            bufgs,
        })
    }

    /// The slices a carry chain of `len` starting at `root` occupies.
    fn chain(&self, root: u32, len: usize) -> Option<Vec<u32>> {
        let mut out = vec![root];
        while out.len() < len {
            out.push(self.slices[*out.last().unwrap() as usize].next?);
        }
        Some(out)
    }
}

/// What a slot (one LUT/FF position of a slice) holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Occ {
    Free,
    Ble(u32),
    /// Part of a carry chain's or RAM's slice.
    Macro,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Obj {
    Ble(u32),
    Carry(u32),
    Dram(u32),
}

const SLOTS: usize = 4;

#[derive(Clone)]
struct State<'a> {
    d: &'a Design,
    g: &'a Grid,
    /// None: wirelength alone.
    timing: Option<&'a Timing>,
    /// BLE -> slot (slice * 4 + letter).
    ble_slot: Vec<u32>,
    /// Carry chain -> its first slice.
    carry_root: Vec<u32>,
    dram_slice: Vec<u32>,
    occ: Vec<Occ>,
    /// Which macro owns a slice.
    slice_macro: Vec<Option<Obj>>,
    /// Per slice: the registers' control set and how many share it.
    slice_cs: Vec<Option<(u32, u8)>>,
    cell_xy: Vec<Xy>,
    net_cost: Vec<f32>,
    weight: Vec<f32>,
    /// Per net, the move it was last counted in (dedup without a set).
    stamp: Vec<u32>,
    epoch: u32,
}

/// Each object's position: a slot for a BLE, a slice for a macro.
type Moves = Vec<(Obj, u32)>;

impl<'a> State<'a> {
    fn cost(&self, net: usize) -> f32 {
        self.weight[net] * self.hpwl(net)
    }

    /// Half-perimeter, x at half weight (see the module comment).
    fn hpwl(&self, net: usize) -> f32 {
        let cells = &self.d.net_cells[net];
        let Some(&first) = cells.first() else {
            return 0.0;
        };
        let (mut x0, mut y0) = self.cell_xy[first as usize];
        let (mut x1, mut y1) = (x0, y0);
        for &c in &cells[1..] {
            let (x, y) = self.cell_xy[c as usize];
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        0.5 * (x1 - x0) as f32 + (y1 - y0) as f32
    }

    fn total(&self) -> f64 {
        self.net_cost.iter().map(|&c| c as f64).sum()
    }

    fn conn_delay(&self, t: &Timing, c: &Conn) -> Ps {
        let (a, b) = (
            self.cell_xy[c.from_cell as usize],
            self.cell_xy[c.to_cell as usize],
        );
        // Another placer's placement (`measure`) may split what chipsmith
        // keeps together.
        if c.internal && a == b {
            return 0.0;
        }
        t.delays.route(a.0 - b.0, a.1 - b.1)
    }

    /// Static timing of the current placement; with `reweight`, every net
    /// weighted by its most critical connection. The estimated critical
    /// path (ps), or 0 without a timing graph.
    fn analyse(&mut self, reweight: bool) -> Ps {
        let Some(t) = self.timing else {
            return 0.0;
        };
        let arcs: Vec<Arc> = t
            .conns
            .iter()
            .map(|c| Arc {
                from: c.from,
                to: c.to,
                delay: self.conn_delay(t, c),
            })
            .collect();
        let analysis = t.graph.analyse(&arcs);
        if reweight {
            self.weight.iter_mut().for_each(|w| *w = 1.0);
            for (c, a) in t.conns.iter().zip(&arcs) {
                let w = 1.0 + TIMING_WEIGHT * analysis.criticality(a).powf(CRIT_EXP);
                let n = c.net as usize;
                self.weight[n] = self.weight[n].max(w);
            }
            for n in 0..self.d.nets.len() {
                self.net_cost[n] = self.cost(n);
            }
        }
        analysis.critical
    }

    fn wirelength(&self) -> f64 {
        (0..self.d.nets.len()).map(|n| self.hpwl(n) as f64).sum()
    }

    fn pos(&self, o: Obj) -> u32 {
        match o {
            Obj::Ble(b) => self.ble_slot[b as usize],
            Obj::Carry(c) => self.carry_root[c as usize],
            Obj::Dram(r) => self.dram_slice[r as usize],
        }
    }

    fn slices_of(&self, o: Obj, pos: u32) -> Option<Vec<u32>> {
        match o {
            Obj::Ble(_) => Some(vec![pos / SLOTS as u32]),
            Obj::Carry(c) => self.g.chain(pos, self.d.carries[c as usize].len()),
            Obj::Dram(_) => Some(vec![pos]),
        }
    }

    fn unplace(&mut self, o: Obj) {
        match o {
            Obj::Ble(b) => {
                let slot = self.ble_slot[b as usize] as usize;
                debug_assert_eq!(self.occ[slot], Occ::Ble(b));
                self.occ[slot] = Occ::Free;
                if self.d.ble_cs[b as usize].is_some() {
                    let s = &mut self.slice_cs[slot / SLOTS];
                    let (cs, n) = s.expect("a register's slice has a control set");
                    *s = (n > 1).then_some((cs, n - 1));
                }
            }
            Obj::Carry(_) | Obj::Dram(_) => {
                let slices = self.slices_of(o, self.pos(o)).expect("placed legally");
                for s in slices {
                    self.slice_macro[s as usize] = None;
                    for k in 0..SLOTS {
                        self.occ[s as usize * SLOTS + k] = Occ::Free;
                    }
                }
            }
        }
    }

    /// Place `o` at `pos` if that is legal there; false (and nothing
    /// changed) if not.
    fn place(&mut self, o: Obj, pos: u32) -> bool {
        let d = self.d;
        match o {
            Obj::Ble(b) => {
                let slot = pos as usize;
                if self.occ[slot] != Occ::Free {
                    return false;
                }
                if let Some(cs) = d.ble_cs[b as usize] {
                    match &mut self.slice_cs[slot / SLOTS] {
                        s @ None => *s = Some((cs, 1)),
                        Some((have, n)) if *have == cs => *n += 1,
                        Some(_) => return false,
                    }
                }
                self.occ[slot] = Occ::Ble(b);
                self.ble_slot[b as usize] = pos;
                let xy = self.g.slices[slot / SLOTS].xy;
                let ble = d.bles[b as usize];
                for c in ble.lut.into_iter().chain(ble.ff) {
                    self.cell_xy[c as usize] = xy;
                }
                true
            }
            Obj::Carry(_) | Obj::Dram(_) => {
                let Some(slices) = self.slices_of(o, pos) else {
                    return false;
                };
                let fits = slices.iter().all(|&s| {
                    let sl = &self.g.slices[s as usize];
                    sl.even
                        && (sl.slicem || !matches!(o, Obj::Dram(_)))
                        && self.slice_macro[s as usize].is_none()
                        && (0..SLOTS).all(|k| self.occ[s as usize * SLOTS + k] == Occ::Free)
                });
                if !fits {
                    return false;
                }
                for (i, &s) in slices.iter().enumerate() {
                    self.slice_macro[s as usize] = Some(o);
                    for k in 0..SLOTS {
                        self.occ[s as usize * SLOTS + k] = Occ::Macro;
                    }
                    let xy = self.g.slices[s as usize].xy;
                    let cells: Vec<CellId> = match o {
                        Obj::Carry(c) => {
                            let cs = &d.carries[c as usize][i];
                            std::iter::once(cs.carry)
                                .chain(cs.lut6.iter().flatten().copied())
                                .chain(cs.lut5.iter().flatten().copied())
                                .chain(cs.ff.iter().flatten().copied())
                                .collect()
                        }
                        Obj::Dram(r) => {
                            let r = &d.drams[r as usize];
                            r.lut6.iter().chain(&r.lut5).copied().collect()
                        }
                        Obj::Ble(_) => unreachable!(),
                    };
                    for c in cells {
                        self.cell_xy[c as usize] = xy;
                    }
                }
                match o {
                    Obj::Carry(c) => self.carry_root[c as usize] = pos,
                    Obj::Dram(r) => self.dram_slice[r as usize] = pos,
                    Obj::Ble(_) => unreachable!(),
                }
                true
            }
        }
    }

    /// Move every object in `moves` at once; the old positions, or None
    /// (and nothing changed) if any new one is illegal.
    fn apply(&mut self, moves: &[(Obj, u32)]) -> Option<Moves> {
        let old: Moves = moves.iter().map(|&(o, _)| (o, self.pos(o))).collect();
        for &(o, _) in moves {
            self.unplace(o);
        }
        for (i, &(o, p)) in moves.iter().enumerate() {
            if !self.place(o, p) {
                for &(o, _) in &moves[..i] {
                    self.unplace(o);
                }
                for &(o, p) in &old {
                    assert!(self.place(o, p), "restoring a legal placement");
                }
                return None;
            }
        }
        Some(old)
    }

    fn undo(&mut self, old: &Moves) {
        for &(o, _) in old {
            self.unplace(o);
        }
        for &(o, p) in old {
            assert!(self.place(o, p), "restoring a legal placement");
        }
    }

    fn cells_of(&self, o: Obj, out: &mut Vec<CellId>) {
        let d = self.d;
        match o {
            Obj::Ble(b) => {
                let ble = d.bles[b as usize];
                out.extend(ble.lut.into_iter().chain(ble.ff));
            }
            Obj::Carry(c) => {
                for cs in &d.carries[c as usize] {
                    out.push(cs.carry);
                    out.extend(cs.lut6.iter().chain(&cs.lut5).chain(&cs.ff).flatten());
                }
            }
            Obj::Dram(r) => {
                let r = &d.drams[r as usize];
                out.extend(r.lut6.iter().chain(&r.lut5));
            }
        }
    }

    fn nets_of(&mut self, moves: &[(Obj, u32)], cells: &mut Vec<CellId>, nets: &mut Vec<u32>) {
        cells.clear();
        nets.clear();
        for &(o, _) in moves {
            self.cells_of(o, cells);
        }
        self.epoch += 1;
        for &c in cells.iter() {
            for &n in &self.d.cell_nets[c as usize] {
                if self.stamp[n as usize] != self.epoch {
                    self.stamp[n as usize] = self.epoch;
                    nets.push(n);
                }
            }
        }
    }

    /// The moves that take `o` to `pos`, displacing what is there: a BLE
    /// swaps with a BLE; a macro's new slices hand their BLEs (or a RAM) to
    /// the slices it leaves, slice for slice, so their control sets stay
    /// together.
    fn moves_to(&self, o: Obj, pos: u32) -> Option<Moves> {
        let from = self.pos(o);
        if from == pos {
            return None;
        }
        match o {
            Obj::Ble(_) => match self.occ[pos as usize] {
                Occ::Free => Some(vec![(o, pos)]),
                Occ::Ble(other) => Some(vec![(o, pos), (Obj::Ble(other), from)]),
                Occ::Macro => None,
            },
            Obj::Carry(_) | Obj::Dram(_) => {
                let old = self.slices_of(o, from)?;
                let new = self.slices_of(o, pos)?;
                let gained: Vec<u32> = new.iter().copied().filter(|s| !old.contains(s)).collect();
                let freed: Vec<u32> = old.iter().copied().filter(|s| !new.contains(s)).collect();
                let mut moves = vec![(o, pos)];
                for (&g, &f) in gained.iter().zip(&freed) {
                    match self.slice_macro[g as usize] {
                        None => {}
                        // Two RAMs trade slices.
                        Some(other @ Obj::Dram(_)) if matches!(o, Obj::Dram(_)) => {
                            moves.push((other, f));
                            continue;
                        }
                        Some(_) => return None,
                    }
                    for k in 0..SLOTS {
                        if let Occ::Ble(b) = self.occ[g as usize * SLOTS + k] {
                            moves.push((Obj::Ble(b), f * SLOTS as u32 + k as u32));
                        }
                    }
                }
                Some(moves)
            }
        }
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
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
    fn offset(&mut self, r: i32) -> i32 {
        self.below(2 * r as usize + 1) as i32 - r
    }
}

/// Start compact: macros first, nearest the middle of the device, then BLEs
/// filling slices outwards, one control set at a time.
fn initial<'a>(d: &'a Design, g: &'a Grid, timing: Option<&'a Timing>) -> Result<State<'a>> {
    let n_slices = g.slices.len();
    let mut s = State {
        d,
        g,
        timing,
        ble_slot: vec![u32::MAX; d.bles.len()],
        carry_root: vec![u32::MAX; d.carries.len()],
        dram_slice: vec![u32::MAX; d.drams.len()],
        occ: vec![Occ::Free; n_slices * SLOTS],
        slice_macro: vec![None; n_slices],
        slice_cs: vec![None; n_slices],
        cell_xy: vec![(0, 0); d.cells.len()],
        net_cost: vec![0.0; d.nets.len()],
        weight: vec![1.0; d.nets.len()],
        stamp: vec![0; d.nets.len()],
        epoch: 0,
    };
    let (cx, cy) = {
        let (x0, x1) = g.slices.iter().fold((i32::MAX, i32::MIN), |(a, b), s| {
            (a.min(s.xy.0), b.max(s.xy.0))
        });
        ((x0 + x1) / 2, g.y_max / 2)
    };
    let mut by_distance: Vec<u32> = (0..n_slices as u32).collect();
    by_distance.sort_by_key(|&i| {
        let (x, y) = g.slices[i as usize].xy;
        ((x - cx).abs() / 2 + (y - cy).abs(), i)
    });

    let mut chains: Vec<u32> = (0..d.carries.len() as u32).collect();
    chains.sort_by_key(|&c| std::cmp::Reverse(d.carries[c as usize].len()));
    for c in chains {
        let ok = by_distance.iter().any(|&root| s.place(Obj::Carry(c), root));
        ensure!(
            ok,
            "no room for a carry chain of {}",
            d.carries[c as usize].len()
        );
    }
    for r in 0..d.drams.len() as u32 {
        let ok = by_distance.iter().any(|&sl| s.place(Obj::Dram(r), sl));
        ensure!(ok, "no SLICEM left for a RAM32M");
    }

    // Registers grouped by control set, then lone LUTs into what is left.
    let mut order: Vec<u32> = (0..d.bles.len() as u32).collect();
    order.sort_by_key(|&b| (d.ble_cs[b as usize].is_none(), d.ble_cs[b as usize], b));
    let mut slots = by_distance
        .iter()
        .flat_map(|&sl| (0..SLOTS as u32).map(move |k| sl * SLOTS as u32 + k));
    let mut skipped: Vec<u32> = Vec::new();
    for b in order {
        // A slot passed over for one control set stays available to lone
        // LUTs and to later sets only from the top: retry the skipped ones.
        if let Some(i) = skipped.iter().position(|&slot| s.place(Obj::Ble(b), slot)) {
            skipped.remove(i);
            continue;
        }
        loop {
            let slot = slots.next().context("the design doesn't fit the device")?;
            if s.occ[slot as usize] != Occ::Free {
                continue;
            }
            if s.place(Obj::Ble(b), slot) {
                break;
            }
            skipped.push(slot);
        }
    }
    for n in 0..d.nets.len() {
        s.net_cost[n] = s.cost(n);
    }
    Ok(s)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub cells: usize,
    pub bles: usize,
    pub carry_chains: usize,
    pub rams: usize,
    pub initial_wirelength: f64,
    pub wirelength: f64,
    /// The critical path by chipsmith's estimate (ps), 0 if wirelength
    /// alone drove the placement.
    pub critical: Ps,
    pub moves: u64,
    pub seconds: f64,
    pub threads: usize,
}

/// One anneal from `start`; returns the moves tried.
fn anneal(s: &mut State, rng: &mut Rng) -> u64 {
    let d = s.d;
    let g = s.g;
    let objects: Vec<Obj> = (0..d.bles.len() as u32)
        .map(Obj::Ble)
        .chain((0..d.carries.len() as u32).map(Obj::Carry))
        .chain((0..d.drams.len() as u32).map(Obj::Dram))
        .collect();
    if objects.is_empty() {
        return 0;
    }
    let max_r = (g.cols as i32).max(g.y_max).max(1) as f64;
    let mut r = max_r;
    let moves_per_t = (2.0 * (objects.len() as f64).powf(4.0 / 3.0)).max(200.0) as usize;
    let mut total_moves = 0u64;
    let (mut cells, mut nets) = (Vec::new(), Vec::new());

    let mut attempt = |s: &mut State, rng: &mut Rng, r: f64, t: f64| -> Option<f64> {
        let o = objects[rng.below(objects.len())];
        let ri = r.max(1.0) as i32;
        let from = s.g.slices[(match o {
            Obj::Ble(b) => s.ble_slot[b as usize] / SLOTS as u32,
            _ => s.pos(o),
        }) as usize]
            .clone();
        let col = from.col as i32 + rng.offset(ri);
        let y = from.xy.1 + rng.offset(ri);
        let even = match o {
            Obj::Ble(_) => rng.below(2) == 0,
            _ => true,
        };
        let &slice = s.g.at.get(&(u32::try_from(col).ok()?, y, even))?;
        let pos = match o {
            Obj::Ble(_) => slice * SLOTS as u32 + rng.below(SLOTS) as u32,
            _ => slice,
        };
        let moves = s.moves_to(o, pos)?;
        s.nets_of(&moves, &mut cells, &mut nets);
        let before: f64 = nets.iter().map(|&n| s.net_cost[n as usize] as f64).sum();
        let old = s.apply(&moves)?;
        let mut after = 0.0;
        for &n in &nets {
            after += s.cost(n as usize) as f64;
        }
        let delta = after - before;
        if delta <= 0.0 || (t > 0.0 && rng.unit() < (-delta / t).exp()) {
            for &n in &nets {
                s.net_cost[n as usize] = s.cost(n as usize);
            }
            Some(delta)
        } else {
            s.undo(&old);
            None
        }
    };

    // Starting temperature: 20 standard deviations of random moves' deltas.
    let mut deltas = Vec::new();
    for _ in 0..objects.len().max(50) {
        if let Some(dc) = attempt(s, rng, r, f64::INFINITY) {
            deltas.push(dc);
        }
    }
    let mean = deltas.iter().sum::<f64>() / deltas.len().max(1) as f64;
    let var = deltas.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / deltas.len().max(1) as f64;
    let mut t = 20.0 * var.sqrt();
    let live_nets = d.net_cells.iter().filter(|c| !c.is_empty()).count().max(1);

    loop {
        s.analyse(true);
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
        if t < 0.005 * s.total() / live_nets as f64 {
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

/// Every cell's bel, as `-o preplaced` spells it.
fn bels(db: &ChipDb, d: &Design, g: &Grid, s: &State) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(CellId, String)> = Vec::new();
    let slice_bel = |slice: u32, bel: String| format!("{}/{bel}", g.slices[slice as usize].site);
    for (b, ble) in d.bles.iter().enumerate() {
        let slot = s.ble_slot[b];
        let (slice, k) = (slot / SLOTS as u32, LETTERS[slot as usize % SLOTS]);
        if let Some(l) = ble.lut {
            out.push((l, slice_bel(slice, format!("{k}6LUT"))));
        }
        if let Some(f) = ble.ff {
            out.push((f, slice_bel(slice, format!("{k}FF"))));
        }
    }
    for (c, chain) in d.carries.iter().enumerate() {
        let slices = g
            .chain(s.carry_root[c], chain.len())
            .expect("placed legally");
        for (cs, &slice) in chain.iter().zip(&slices) {
            out.push((cs.carry, slice_bel(slice, "CARRY4".into())));
            for (k, l) in LETTERS.iter().enumerate() {
                for (cell, bel) in [
                    (cs.lut6[k], format!("{l}6LUT")),
                    (cs.lut5[k], format!("{l}5LUT")),
                    (cs.ff[k], format!("{l}FF")),
                ] {
                    if let Some(cell) = cell {
                        out.push((cell, slice_bel(slice, bel)));
                    }
                }
            }
        }
    }
    for (r, ram) in d.drams.iter().enumerate() {
        let slice = s.dram_slice[r];
        for (k, l) in LETTERS.iter().enumerate() {
            out.push((ram.lut6[k], slice_bel(slice, format!("{l}6LUT"))));
            out.push((ram.lut5[k], slice_bel(slice, format!("{l}5LUT"))));
        }
    }
    let mut lines: Vec<(String, String)> = out
        .into_iter()
        .map(|(c, bel)| Ok((d.cells[c as usize].name.clone(), bel_name(db, &bel)?)))
        .collect::<Result<_>>()?;
    lines.extend(
        fixed_bels(db, d, g)?
            .into_iter()
            .map(|(c, bel, _)| (d.cells[c as usize].name.clone(), bel)),
    );
    lines.sort();
    Ok(lines)
}

/// The cells that don't move: pins and their buffers, BSCANs, the PS7, the constant
/// drivers, and BUFGCTRLs (each the free one nearest its source). Returns
/// (cell, bel as nextpnr reads it, position).
fn fixed_bels(db: &ChipDb, d: &Design, g: &Grid) -> Result<Vec<(CellId, String, Xy)>> {
    let tile_xy = |bel: &str| -> Result<Xy> {
        let tile = bel.split('/').next().unwrap_or_default();
        let t = db
            .tile_by_name(tile)
            .with_context(|| format!("bel {bel}: no tile {tile}"))?;
        let (x, y) = db.tile_xy(t);
        Ok((x as i32, y as i32))
    };
    let mut out = Vec::new();
    // A pin's buffer sits in the pin's site: `X<x>Y<y>/<site>.<type>.PAD`
    // has its buffers at `X<x>Y<y>/<site>.IOB33.<bel>`, with nextpnr's
    // `~IOB33` when the site's own type is IOB33M or IOB33S.
    let mut pad_of_net: HashMap<NetId, &str> = HashMap::default();
    for (i, c) in d.cells.iter().enumerate() {
        if let Kind::Pad { bel } = &c.kind {
            out.push((i as CellId, bel.clone(), tile_xy(bel)?));
            if let Some(n) = c.net("PAD") {
                pad_of_net.insert(n, bel);
            }
        }
    }
    for (i, c) in d.cells.iter().enumerate() {
        let (port, bel) = match c.kind {
            Kind::InBuf => ("PAD", "INBUF_EN"),
            Kind::OutBuf => ("OUT", "OUTBUF"),
            _ => continue,
        };
        let pad = c
            .net(port)
            .and_then(|n| pad_of_net.get(&n))
            .with_context(|| format!("I/O buffer {} reaches no pin", c.name))?;
        let (site, ty) = pad
            .strip_suffix(".PAD")
            .and_then(|p| p.rsplit_once('.'))
            .with_context(|| format!("pin bel {pad}"))?;
        let variant = if ty == "IOB33" { "" } else { "~IOB33" };
        let name = format!("{site}.IOB33.{bel}{variant}");
        let xy = tile_xy(&name)?;
        out.push((i as CellId, name, xy));
    }
    for (i, c) in d.cells.iter().enumerate() {
        match c.kind {
            Kind::Gnd => out.push((i as CellId, "X0Y0/PSEUDO_GND".into(), (0, 0))),
            Kind::Vcc => out.push((i as CellId, "X0Y0/PSEUDO_VCC".into(), (0, 0))),
            Kind::Bscan { chain } => {
                let site = format!("BSCAN_X0Y{}", chain - 1);
                let s = db
                    .sites
                    .get(&site)
                    .with_context(|| format!("BSCAN {}: no site {site}", c.name))?;
                let (x, y) = db.tile_xy(s.tile);
                out.push((
                    i as CellId,
                    bel_name(db, &format!("{site}/BSCAN"))?,
                    (x as i32, y as i32),
                ));
            }
            Kind::Ps7 => {
                let site = "PS7_X0Y0";
                let s = db
                    .sites
                    .get(site)
                    .with_context(|| format!("PS7 {}: no site {site}", c.name))?;
                let (x, y) = db.tile_xy(s.tile);
                out.push((
                    i as CellId,
                    bel_name(db, &format!("{site}/PS7"))?,
                    (x as i32, y as i32),
                ));
            }
            _ => {}
        }
    }
    // BUFGCTRLs last: their sources may be the pins and BSCANs above. One
    // driven from the fabric has no fixed source; the middle of the device
    // stands in.
    let fixed: HashMap<CellId, Xy> = out.iter().map(|(c, _, xy)| (*c, *xy)).collect();
    let middle = (
        g.slices.iter().map(|s| s.xy.0).max().unwrap_or(0) / 2,
        g.y_max / 2,
    );
    let mut free: Vec<bool> = vec![true; g.bufgs.len()];
    for (i, c) in d.cells.iter().enumerate() {
        if c.kind != Kind::Bufg {
            continue;
        }
        let source = c
            .net("I0")
            .and_then(|n| d.nets[n as usize].driver.as_ref())
            .and_then(|(drv, _)| fixed.get(drv).copied())
            .unwrap_or(middle);
        let site = (0..g.bufgs.len())
            .filter(|&b| free[b])
            .min_by_key(|&b| {
                let (x, y) = g.bufgs[b].1;
                ((x - source.0).abs() / 2 + (y - source.1).abs(), b)
            })
            .context("more BUFGCTRLs than the device has")?;
        free[site] = false;
        let (name, xy) = &g.bufgs[site];
        out.push((i as CellId, bel_name(db, &format!("{name}/BUFGCTRL"))?, *xy));
    }
    Ok(out)
}

/// What `place_file` did.
#[derive(Debug)]
pub enum Outcome {
    Placed(Stats),
    /// Nothing written to the placement; nextpnr places the design.
    Unsupported(Unsupported),
}

/// Anneals run in parallel from the same start with different seeds; the
/// lowest-cost one wins, so the result doesn't depend on thread timing.
fn best_of<'a>(start: &State<'a>, seeds: usize) -> (State<'a>, u64) {
    let results: Vec<(State, u64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..seeds)
            .map(|i| {
                let mut s = start.clone();
                scope.spawn(move || {
                    let mut rng =
                        Rng(0x9e37_79b9_7f4a_7c15
                            ^ (i as u64 + 1).wrapping_mul(0xbf58_476d_1ce4_e5b9));
                    let moves = anneal(&mut s, &mut rng);
                    (s, moves)
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
        .min_by(|a, b| a.0.total().total_cmp(&b.0.total()))
        .expect("at least one seed")
        .0;
    (best, moves)
}

/// How many anneals run side by side, one thread each. Fixed rather than
/// the machine's core count, so every machine places a design the same way.
const SEEDS: usize = 8;

/// Place nextpnr's packed netlist `packed`; write `-o preplaced` lines to
/// `placement` (empty if chipsmith can't place the design, so nextpnr
/// does).
///
/// With `delays` the placement is timing-driven; without, wirelength alone.
pub fn place_file(
    db: &ChipDb,
    delays: Option<&Delays>,
    packed: &Path,
    placement: &Path,
) -> Result<Outcome> {
    let text =
        std::fs::read_to_string(packed).with_context(|| format!("reading {}", packed.display()))?;
    let json: Value = serde_json::from_str(&text)?;
    let write = |text: &str| {
        std::fs::write(placement, text).with_context(|| format!("writing {}", placement.display()))
    };
    let d = match read_design(&json)? {
        Ok(d) => d,
        Err(u) => {
            write("")?;
            return Ok(Outcome::Unsupported(u));
        }
    };
    let started = Instant::now();
    let g = Grid::new(db)?;
    let timing = delays.map(|delays| Timing::new(&d, delays));
    let mut start = initial(&d, &g, timing.as_ref())?;
    // Fixed cells' positions, for the nets they're on.
    for (c, _, xy) in fixed_bels(db, &d, &g)? {
        start.cell_xy[c as usize] = xy;
    }
    for n in 0..d.nets.len() {
        start.net_cost[n] = start.cost(n);
    }
    let initial_wirelength = start.wirelength();
    let threads = SEEDS;
    let (mut best, moves) = best_of(&start, threads);
    let critical = best.analyse(false);
    let lines = bels(db, &d, &g, &best)?;
    let mut text = String::new();
    for (cell, bel) in &lines {
        text += &format!("{cell}\t{bel}\n");
    }
    write(&text)?;
    Ok(Outcome::Placed(Stats {
        cells: d.cells.len(),
        bles: d.bles.len(),
        carry_chains: d.carries.len(),
        rams: d.drams.len(),
        initial_wirelength,
        wirelength: best.wirelength(),
        critical,
        moves,
        seconds: started.elapsed().as_secs_f64(),
        threads,
    }))
}

/// The wirelength and estimated critical path (ps) of another placement of
/// the same packed netlist (cell -> bel as nextpnr writes it in `--write`,
/// `SLICE_X30Y39/A6LUT`), measured the way the placer measures its own.
pub fn measure(db: &ChipDb, delays: &Delays, packed: &Path, placed: &Path) -> Result<(f64, Ps)> {
    let read = |p: &Path| -> Result<Value> {
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        Ok(serde_json::from_str(&text)?)
    };
    let d = match read_design(&read(packed)?)? {
        Ok(d) => d,
        Err(Unsupported(why)) => bail!("can't measure: {why}"),
    };
    let placed = read(placed)?;
    let cells = top_module(&placed)?["cells"]
        .as_object()
        .context("no cells")?;
    let g = Grid::new(db)?;
    let timing = Timing::new(&d, delays);
    let mut s = initial(&d, &g, Some(&timing))?;
    for (i, c) in d.cells.iter().enumerate() {
        let bel = cells
            .get(&c.name)
            .and_then(|c| c["attributes"]["NEXTPNR_BEL"].as_str())
            .with_context(|| format!("{}: not placed", c.name))?;
        let readable = bel_name(db, bel)?;
        let tile = readable.split('/').next().unwrap_or_default();
        let t = db
            .tile_by_name(tile)
            .with_context(|| format!("bel {bel}"))?;
        let (x, y) = db.tile_xy(t);
        s.cell_xy[i] = (x as i32, y as i32);
    }
    Ok((s.wirelength(), s.analyse(false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(clk: u32) -> ControlSet {
        ControlSet {
            clk: Some(clk),
            ce: None,
            sr: None,
            clk_inv: false,
            sr_inv: false,
            latch: false,
            sync: true,
        }
    }

    fn grid(n: u32) -> Grid {
        // One column of n tiles, two slices each; carries chain upwards.
        let mut slices = Vec::new();
        for y in 0..n as i32 {
            for even in [true, false] {
                slices.push(Slice {
                    site: format!("SLICE_X{}Y{y}", !even as u8),
                    xy: (0, y),
                    even,
                    slicem: even,
                    next: None,
                    col: 0,
                });
            }
        }
        for i in 0..slices.len() {
            if i + 2 < slices.len() {
                slices[i].next = Some(i as u32 + 2);
            }
        }
        let at = slices
            .iter()
            .enumerate()
            .map(|(i, s)| ((s.col, s.xy.1, s.even), i as u32))
            .collect();
        Grid {
            slices,
            at,
            cols: 1,
            y_max: n as i32 - 1,
            bufgs: Vec::new(),
        }
    }

    fn design(bles: Vec<Ble>, cells: Vec<Kind>, carries: Vec<Vec<CarrySlice>>) -> Design {
        let mut d = Design {
            cells: cells
                .into_iter()
                .enumerate()
                .map(|(i, kind)| Cell {
                    name: format!("c{i}"),
                    kind,
                    pins: Vec::new(),
                })
                .collect(),
            nets: Vec::new(),
            net_cells: Vec::new(),
            cell_nets: Vec::new(),
            bles,
            carries,
            drams: Vec::new(),
            ble_cs: Vec::new(),
        };
        d.ble_cs = d
            .bles
            .iter()
            .map(|b| match b.ff.map(|f| &d.cells[f as usize].kind) {
                Some(Kind::Ff(c)) => Some(c.clk.unwrap()),
                _ => None,
            })
            .collect();
        d.index_nets();
        d
    }

    #[test]
    fn registers_of_two_control_sets_never_share_a_slice() {
        let cells = vec![Kind::Ff(cs(0)), Kind::Ff(cs(1)), Kind::Ff(cs(0))];
        let bles = (0..3)
            .map(|i| Ble {
                lut: None,
                ff: Some(i),
            })
            .collect();
        let d = design(bles, cells, Vec::new());
        let g = grid(2);
        let mut s = initial(&d, &g, None).unwrap();
        let slice = |s: &State, b: usize| s.ble_slot[b] / SLOTS as u32;
        assert_ne!(slice(&s, 0), slice(&s, 1));
        // Moving register 1 next to register 0 is refused, and changes
        // nothing.
        let target = s.ble_slot[0] + 1;
        let before = s.ble_slot.clone();
        let moves = s.moves_to(Obj::Ble(1), target).unwrap();
        assert!(s.apply(&moves).is_none());
        assert_eq!(s.ble_slot, before);
        // Register 2 (same control set as 0) may join it.
        let free = (0..SLOTS as u32)
            .map(|k| slice(&s, 0) * SLOTS as u32 + k)
            .find(|&slot| s.occ[slot as usize] == Occ::Free)
            .unwrap();
        let moves = s.moves_to(Obj::Ble(2), free).unwrap();
        assert!(s.apply(&moves).is_some());
    }

    #[test]
    fn a_carry_chain_takes_the_slices_up_its_column_and_hands_their_bles_back() {
        let cells = vec![Kind::Carry, Kind::Carry, Kind::Lut { dram: false }];
        let chain = vec![
            CarrySlice {
                carry: 0,
                ..Default::default()
            },
            CarrySlice {
                carry: 1,
                ..Default::default()
            },
        ];
        let bles = vec![Ble {
            lut: Some(2),
            ff: None,
        }];
        let d = design(bles, cells, vec![chain]);
        let g = grid(4);
        let mut s = initial(&d, &g, None).unwrap();
        let root = s.carry_root[0];
        assert!(g.slices[root as usize].even);
        assert_eq!(g.chain(root, 2).unwrap(), vec![root, root + 2]);
        // Put the LUT in a slice the chain will move into, then move it.
        let target = if root == 0 { 4 } else { 0 };
        let lut_slot = target * SLOTS as u32 + 2;
        if let Some(m) = s.moves_to(Obj::Ble(0), lut_slot) {
            s.apply(&m).unwrap();
        }
        let m = s.moves_to(Obj::Carry(0), target).unwrap();
        assert!(s.apply(&m).is_some());
        assert_eq!(s.carry_root[0], target);
        let lut_slice = s.ble_slot[0] / SLOTS as u32;
        assert!(!g.chain(target, 2).unwrap().contains(&lut_slice));
        // The odd slices never host a chain.
        let m = s.moves_to(Obj::Carry(0), 1).unwrap();
        assert!(s.apply(&m).is_none());
    }
}
