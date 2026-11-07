//! chipsmith's 7-series placer: simulated annealing over slices.
//!
//! Input is the packer's design (`crate::pack`): LUTs, registers, CARRY4s
//! with their feed-through LUTs, RAM32Ms as eight RAMD32 LUTs, pins, BUFGs,
//! BSCANs and the PS7. Which LUTs and registers share a slice, and where
//! every slice, carry chain, RAM and clock buffer goes, is decided here; the
//! result is every cell's site and bel ([`Placement`]).
//!
//! Slices are filled in shapes that are legal by construction, so that
//! legality is a property of the shapes rather than something to check
//! after each move:
//!
//! - a BLE (a LUT in an `x6LUT` and the register in `xFF` beside it, or
//!   either alone) goes in any slice; the register takes its D from the LUT's
//!   O6 or through the X bypass, which nothing else in a logic slice uses.
//!   The only per-slice rule left is that every register in a slice shares
//!   one control set (clock, CE, SR and their modes);
//! - a carry chain owns whole slices: CARRY4 k sits in the slice above
//!   CARRY4 k-1 (the next one on the COUT -> CIN wire), its S LUTs in the
//!   `x6LUT`s, and registers fed straight from a sum output in the `xFF` of
//!   that bit;
//! - a RAM32M (eight RAMD32 LUTs) owns a whole SLICEM;
//! - a block RAM goes in a BRAM tile: a RAMB18E1 in either half, a
//!   RAMB36E1 in the whole tile; a DSP48E1 in either site of a DSP tile.
//!   These hard blocks move by swapping places with their own kind.
//!
//! Carry chains and RAMs go in the even slice of a tile (X0, the SLICEM of a
//! CLBLM). Chains in either slice were tried (branch xc7-qor, README):
//! they cost zybo-jtag-counter and picorv32 more than they gave sha256.
//! Pins are where the manifest put them; the BSCAN for USER<n> is
//! BSCAN_X0Y<n-1>; the PS7 has the one PS7 site; each BUFGCTRL takes the
//! free one nearest its source.
//!
//! The cost is half-perimeter wirelength, x counted at half weight: grid x
//! advances by two or more per interconnect column (site columns are
//! columns of their own), y by one per interconnect row. Clock nets (driven
//! by a BUFGCTRL) and the constant nets are left out; they don't use the
//! general interconnect.

use std::time::Instant;

use anyhow::{ensure, Context, Result};
use rustc_hash::FxHashMap as HashMap;

use crate::delay::Delays;
use chipsmith_route::chipdb::ChipDb;
use chipsmith_route::timing::{self, Arc, Ps};

pub(crate) type NetId = u32;
pub(crate) type CellId = u32;
/// A position on the grid: tile x, tile y.
type Xy = (i32, i32);

/// The registers' shared configuration within a slice: the slice has one
/// clock, CE and SR, one clock inversion and one SR mode for all eight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ControlSet {
    pub clk: Option<NetId>,
    pub ce: Option<NetId>,
    pub sr: Option<NetId>,
    pub clk_inv: bool,
    /// SR acts on the clock edge (FDRE, FDSE) rather than at once.
    pub sync: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    Lut {
        dram: bool,
    },
    Ff(ControlSet),
    Carry,
    /// A package pin's site (`IOB_X0Y78`) and its buffers, for the port
    /// bit `port` (`led[2]`).
    Pad {
        site: String,
        port: String,
    },
    Bufg,
    /// A PLL or MMCM, in the clock management tile nearest its input.
    Cmt {
        prim: crate::cmt::Prim,
    },
    Bscan {
        chain: u32,
    },
    /// The Zynq's processor system, as seen from the fabric.
    Ps7,
    /// A block RAM; `reg`: its output register is on, per port (SDP: port
    /// A's for the whole read); `sdp`: port A reads all, port B writes all.
    Bram {
        size: crate::bram::Size,
        reg: [bool; 2],
        sdp: bool,
    },
    /// A DSP48E1 multiplying: which stages are registered, A's and B's
    /// (AREG, BREG), M and P.
    Dsp {
        inreg: [bool; 2],
        mreg: bool,
        preg: bool,
    },
    Gnd,
    Vcc,
}

#[derive(Debug)]
pub(crate) struct Cell {
    pub name: String,
    pub kind: Kind,
    pub pins: Vec<(String, NetId)>,
}

impl Cell {
    pub fn net(&self, port: &str) -> Option<NetId> {
        self.pins.iter().find(|(p, _)| p == port).map(|&(_, n)| n)
    }
}

#[derive(Debug, Default)]
pub(crate) struct Net {
    pub driver: Option<(CellId, String)>,
    pub users: Vec<(CellId, String)>,
}

/// One LUT and the register beside it; with `lut5`, a second LUT on the
/// same five inputs out of the same LUT6's O5 (A6 tied high), and its
/// register in the letter's 5FF (`crate::pack`'s pairs).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Ble {
    pub lut: Option<CellId>,
    pub ff: Option<CellId>,
    pub lut5: Option<CellId>,
    pub ff5: Option<CellId>,
}

impl Ble {
    pub fn cells(&self) -> impl Iterator<Item = CellId> {
        self.lut
            .into_iter()
            .chain(self.ff)
            .chain(self.lut5)
            .chain(self.ff5)
    }
}

/// One slice of a carry chain: the CARRY4 and, per bit, the cells the
/// packer tied to it.
#[derive(Debug, Clone, Default)]
pub(crate) struct CarrySlice {
    pub carry: CellId,
    pub lut6: [Option<CellId>; 4],
    pub lut5: [Option<CellId>; 4],
    pub ff: [Option<CellId>; 4],
}

/// A RAM32M: per bit, the RAMD32 in the 6LUT and the one in the 5LUT.
#[derive(Debug, Clone)]
pub(crate) struct Dram {
    pub lut6: [CellId; 4],
    pub lut5: [CellId; 4],
}

pub(crate) struct Design {
    pub cells: Vec<Cell>,
    pub nets: Vec<Net>,
    /// Distinct cells of each net that counts for wirelength (empty for
    /// clock and constant nets and nets within one cell).
    pub net_cells: Vec<Vec<CellId>>,
    /// Nets that count, per cell.
    pub cell_nets: Vec<Vec<NetId>>,
    pub bles: Vec<Ble>,
    pub carries: Vec<Vec<CarrySlice>>,
    pub drams: Vec<Dram>,
    /// Hard blocks, by [`Hard`] kind: block RAM, then DSP cells.
    pub hard: [Vec<CellId>; 2],
    /// Per DSP in `hard`, the DSP it cascades into (PCOUT -> PCIN), which
    /// moves with it in the same tile's upper site and isn't in `hard`.
    pub cascaded: Vec<Option<CellId>>,
    /// Each BLE's register's control set, numbered densely.
    pub ble_cs: Vec<Option<u32>>,
}

/// Groups the packer's cells into what the placer moves: carry chains
/// (with the LUTs and registers the packer tied to each bit), RAM32Ms and
/// BLEs.
pub(crate) fn build(
    cells: Vec<Cell>,
    nets: Vec<Net>,
    pairs: &[(CellId, CellId)],
) -> Result<Design> {
    let mut d = Design {
        cells,
        nets,
        net_cells: Vec::new(),
        cell_nets: Vec::new(),
        bles: Vec::new(),
        carries: Vec::new(),
        drams: Vec::new(),
        hard: [Vec::new(), Vec::new()],
        cascaded: Vec::new(),
        ble_cs: Vec::new(),
    };
    // A DSP whose PCIN its cascade partner's PCOUT drives, by lower DSP.
    let mut upper: HashMap<CellId, CellId> = HashMap::default();
    for (c, cell) in d.cells.iter().enumerate() {
        let Some(lower) = cell
            .net("PCIN0")
            .and_then(|n| d.nets[n as usize].driver.as_ref())
        else {
            continue;
        };
        if Hard::of(&d.cells[lower.0 as usize].kind) == Some(Hard::Dsp) && lower.1 == "PCOUT0" {
            upper.insert(lower.0, c as CellId);
        }
    }
    let uppers: std::collections::HashSet<CellId> = upper.values().copied().collect();
    for c in 0..d.cells.len() as CellId {
        if let Some(h) = Hard::of(&d.cells[c as usize].kind) {
            if uppers.contains(&c) {
                continue;
            }
            d.hard[h as usize].push(c);
            if h == Hard::Dsp {
                d.cascaded.push(upper.get(&c).copied());
            }
        }
    }
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
            // Each S has a LUT of its own (the packer adds feed-throughs);
            // DI comes in on the bit's X bypass.
            for k in 0..4 {
                if let Some(l) = lut_driver(&d, cell.net(&format!("S{k}"))) {
                    ensure!(
                        !taken[l as usize],
                        "LUT {} feeds two carry bits",
                        d.cells[l as usize].name
                    );
                    taken[l as usize] = true;
                    s.lut6[k] = Some(l);
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

    // RAM32Ms: the packer names the eight RAMD32s `<ram>/DPR<k>_<h>`, bit
    // k, h = 1 in the 6LUT and 0 in the 5LUT.
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
        let (ram, k, h) = parsed.with_context(|| format!("distributed RAM {}", c.name))?;
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
                _ => anyhow::bail!("distributed RAM {ram} isn't a RAM32M"),
            }
        }
        for c in dram.lut5.iter().chain(&dram.lut6) {
            taken[*c as usize] = true;
        }
        d.drams.push(dram);
    }

    // Paired LUTs (`crate::pack`): one BLE each pair, each LUT's register
    // (the first register its output drives) beside it, if they share a
    // control set.
    let mut ble_of_lut: Vec<bool> = taken.clone();
    let first_ff = |d: &Design, taken: &[bool], l: CellId| -> Option<CellId> {
        let o = d.cells[l as usize].net("O6")?;
        d.nets[o as usize]
            .users
            .iter()
            .filter(|(u, p)| p == "D" && cs_of(d, *u).is_some() && !taken[*u as usize])
            .map(|&(u, _)| u)
            .min()
    };
    for &(l6, l5) in pairs {
        let ff = first_ff(&d, &taken, l6);
        let ff5 =
            first_ff(&d, &taken, l5).filter(|&f5| ff.is_none_or(|f| cs_of(&d, f) == cs_of(&d, f5)));
        for c in [Some(l6), Some(l5), ff, ff5].into_iter().flatten() {
            taken[c as usize] = true;
            ble_of_lut[c as usize] = true;
        }
        d.bles.push(Ble {
            lut: Some(l6),
            ff,
            lut5: Some(l5),
            ff5,
        });
    }
    // BLEs: each register with the LUT driving its D, if that LUT is free.
    for c in 0..d.cells.len() as CellId {
        if taken[c as usize] || cs_of(&d, c).is_none() {
            continue;
        }
        let lut = lut_driver(&d, d.cells[c as usize].net("D")).filter(|&l| !ble_of_lut[l as usize]);
        if let Some(l) = lut {
            ble_of_lut[l as usize] = true;
        }
        taken[c as usize] = true;
        d.bles.push(Ble {
            lut,
            ff: Some(c),
            ..Ble::default()
        });
    }
    for c in 0..d.cells.len() as CellId {
        if !ble_of_lut[c as usize] && matches!(d.cells[c as usize].kind, Kind::Lut { .. }) {
            d.bles.push(Ble {
                lut: Some(c),
                ..Ble::default()
            });
        }
    }
    let mut cs_ids: HashMap<ControlSet, u32> = HashMap::default();
    let ble_cs: Vec<Option<u32>> = d
        .bles
        .iter()
        .map(|b| {
            let cs = cs_of(&d, b.ff.or(b.ff5)?)?;
            let next = cs_ids.len() as u32;
            Some(*cs_ids.entry(cs).or_insert(next))
        })
        .collect();
    d.ble_cs = ble_cs;
    d.index_nets();
    Ok(d)
}

impl Design {
    /// The DSP hard block `b` cascades into, in its tile's upper site.
    fn upper(&self, h: Hard, b: u32) -> Option<CellId> {
        match h {
            Hard::Dsp => self.cascaded[b as usize],
            Hard::Bram => None,
        }
    }

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
                Kind::Bufg | Kind::Cmt { .. } | Kind::Gnd | Kind::Vcc
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
pub(crate) struct Conn {
    pub from_cell: CellId,
    pub to_cell: CellId,
    /// Timing nodes of the two ports.
    pub from: u32,
    pub to: u32,
    pub net: NetId,
    /// Stays inside a slice whatever the placement: a LUT into its BLE's
    /// register, a carry chain's own LUTs and registers, CO3 into the next
    /// CIN.
    pub internal: bool,
}

impl Timing {
    /// The estimated delay of a connection between cells at `a` and `b`.
    fn estimate(&self, a: Xy, b: Xy) -> Ps {
        self.delays.route(a.0 - b.0, a.1 - b.1)
    }
}

/// The design's timing graph (cell arcs, path starts and ends) and every
/// connection the placement decides the delay of.
pub(crate) struct Timing {
    pub graph: timing::Graph,
    pub conns: Vec<Conn>,
    delays: Delays,
}

/// Criticality weighting, as the Cyclone II placer does it (VPR's): a net
/// counts `1 + TIMING_WEIGHT * criticality^CRIT_EXP` times its wirelength.
/// The Cyclone II's 8 is too weak here: swept on picorv32 and
/// zybo-jtag-counter, nextpnr's Fmax on chipsmith's routing (README), 20
/// came out best; 40 and 80 no better, a lower exponent worse.
/// Rechecked on chipsmith's calibrated timing (bench/zybo-timing.sh, five
/// seeds): 10 with an exponent of 2 placed riscv-bram 3% and sha256 2%
/// worse, picorv32 no better.
const TIMING_WEIGHT: f32 = 20.0;
const CRIT_EXP: f32 = 4.0;

impl Timing {
    pub(crate) fn new(d: &Design, delays: &Delays) -> Self {
        let mut g = timing::Graph::default();
        g.cut_loops();
        // Which object (and which slice of it) each cell is in: 0 BLE, 1
        // carry chain, 2 RAM.
        let mut obj: Vec<Option<(u8, u32, u32)>> = vec![None; d.cells.len()];
        for (b, ble) in d.bles.iter().enumerate() {
            for c in ble.cells() {
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
                            g.set_setup(n, delays.setup_ce + delays.clock_margin);
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
                        g.set_setup(n, setup + delays.clock_margin);
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
                Kind::Bram { reg, sdp, .. } => {
                    for &p in &ports {
                        let n = node(&mut g, p);
                        if p.starts_with("DO") {
                            let port = bram_port(p, *sdp);
                            g.set_launch(
                                n,
                                if reg[port] {
                                    delays.bram.clock_to_out_reg
                                } else {
                                    delays.bram.clock_to_out
                                },
                            );
                        } else if let Some(setup) = delays.bram.setup(p) {
                            g.set_setup(n, setup + delays.clock_margin);
                        }
                    }
                }
                Kind::Dsp { inreg, mreg, preg } => {
                    let dl = &delays.dsp;
                    let is_p =
                        |p: &str| p.starts_with('P') && p[1..].chars().all(|c| c.is_ascii_digit());
                    let operand = |p: &str| -> Option<usize> {
                        let (name, idx) = p.split_at(1);
                        (idx.chars().all(|c| c.is_ascii_digit()) && !idx.is_empty())
                            .then(|| ["A", "B"].iter().position(|x| *x == name))
                            .flatten()
                    };
                    let is_pcout = |p: &str| {
                        p.strip_prefix("PCOUT")
                            .is_some_and(|i| i.chars().all(|c| c.is_ascii_digit()))
                    };
                    let outs: Vec<&str> = ports.iter().copied().filter(|p| is_p(p)).collect();
                    let pcouts: Vec<&str> = ports.iter().copied().filter(|p| is_pcout(p)).collect();
                    for &p in &ports {
                        let n = node(&mut g, p);
                        if is_p(p) {
                            let launch = if *preg {
                                Some(dl.clock_preg_to_p)
                            } else if *mreg {
                                Some(dl.clock_mreg_to_p)
                            } else if inreg[0] || inreg[1] {
                                Some(dl.clock_areg_to_p)
                            } else {
                                None
                            };
                            if let Some(l) = launch {
                                g.set_launch(n, l);
                            }
                        } else if let Some(k) = operand(p) {
                            if inreg[k] {
                                g.set_setup(n, dl.setup_areg + delays.clock_margin);
                            } else if *mreg {
                                g.set_setup(n, dl.setup_mreg + delays.clock_margin);
                            } else if *preg {
                                g.set_setup(n, dl.setup_preg + delays.clock_margin);
                            } else {
                                for &o in &outs {
                                    let to = node(&mut g, o);
                                    g.add_arc(n, to, dl.a_to_p);
                                }
                                for &o in &pcouts {
                                    let to = node(&mut g, o);
                                    g.add_arc(n, to, dl.a_to_pcout);
                                }
                            }
                        } else if p.starts_with("PCIN") && !(inreg[0] || inreg[1]) {
                            // A cascade's upper DSP. With registered
                            // operands its P launches from the clock (the
                            // lower's PCOUT as late as its own product).
                            for &o in &outs {
                                let to = node(&mut g, o);
                                g.add_arc(n, to, dl.pcin_to_p);
                            }
                        } else if p.starts_with("CE") || p.starts_with("RST") {
                            g.set_setup(n, dl.setup_control + delays.clock_margin);
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

/// Which port (0 A, 1 B) of a block RAM a site pin belongs to, for its
/// clock: SDP writes `{B, A}` on port B's clock and reads it on A's.
pub(crate) fn bram_port(pin: &str, sdp: bool) -> usize {
    if sdp && pin.starts_with("DI") {
        return 1;
    }
    if sdp && pin.starts_with("DO") {
        return 0;
    }
    const A: [&str; 11] = [
        "CLKARDCLK",
        "REGCLKARDRCLK",
        "ENARDEN",
        "REGCEAREGCE",
        "RSTRAMARSTRAM",
        "RSTREGARSTREG",
        "ADDRARDADDR",
        "ADDRATIEHIGH",
        "WEA",
        "DIADI",
        "DIPADIP",
    ];
    let a = A.iter().any(|p| pin.starts_with(p))
        || pin.starts_with("DOADO")
        || pin.starts_with("DOPADOP");
    usize::from(!a)
}

/// The kinds of hard block, each with tiles of two sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hard {
    Bram = 0,
    Dsp = 1,
}

impl Hard {
    const ALL: [Hard; 2] = [Hard::Bram, Hard::Dsp];

    fn of(kind: &Kind) -> Option<Hard> {
        match kind {
            Kind::Bram { .. } => Some(Hard::Bram),
            Kind::Dsp { .. } => Some(Hard::Dsp),
            _ => None,
        }
    }
}

/// One BRAM or DSP tile: its two sites (lower, upper) and, a BRAM tile,
/// its RAMB36.
#[derive(Debug, Clone)]
struct HardTile {
    xy: Xy,
    half: [String; 2],
    full: Option<String>,
}

/// One slice site.
#[derive(Debug, Clone)]
struct Slice {
    /// Absolute site name, `SLICE_X30Y39`.
    site: String,
    xy: Xy,
    /// The tile's X0 slice (the SLICEM of a CLBLM): where carry chains and
    /// distributed RAM go.
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
    /// PLLE2_ADV and MMCME2_ADV sites: (site, kind, position).
    cmts: Vec<(String, crate::cmt::Prim, Xy)>,
    /// BRAM tiles, DSP tiles.
    hard: [Vec<HardTile>; 2],
}

impl Grid {
    fn new(db: &ChipDb) -> Result<Self> {
        let mut slices: Vec<Slice> = Vec::new();
        let mut bufgs = Vec::new();
        let mut cmts = Vec::new();
        let mut bram_sites: HashMap<u32, (Option<String>, Option<String>, Option<String>)> =
            HashMap::default();
        let mut dsp_sites: HashMap<u32, [Option<String>; 2]> = HashMap::default();
        let xy_of = |tile: u32| {
            let (x, y) = db.tile_xy(tile);
            (x as i32, y as i32)
        };
        let mut sites: Vec<(&String, &chipsmith_route::chipdb::Site)> = db.sites.iter().collect();
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
                ty if crate::cmt::Prim::from_cell(ty).is_some() => cmts.push((
                    name.clone(),
                    crate::cmt::Prim::from_cell(ty).expect("matched"),
                    xy_of(s.tile),
                )),
                // Block RAM sites by relative name: RAMB18_X0Y0 (the FIFO18E1
                // site, lower), RAMB18_X0Y1, RAMB36_X0Y0.
                "FIFO18E1" | "RAMB18E1" | "RAMBFIFO36E1" => {
                    let e = bram_sites.entry(s.tile).or_default();
                    let slot = if s.ty == "RAMBFIFO36E1" {
                        &mut e.2
                    } else if s.relative.ends_with('0') {
                        &mut e.0
                    } else {
                        &mut e.1
                    };
                    *slot = Some(name.clone());
                }
                "DSP48E1" => {
                    let e = dsp_sites.entry(s.tile).or_default();
                    e[usize::from(!s.relative.ends_with('0'))] = Some(name.clone());
                }
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
        let mut brams: Vec<HardTile> = bram_sites
            .into_iter()
            .filter_map(|(tile, (lo, hi, full))| {
                Some(HardTile {
                    xy: xy_of(tile),
                    half: [lo?, hi?],
                    full: Some(full?),
                })
            })
            .collect();
        brams.sort_by_key(|b| (b.xy.0, b.xy.1));
        let mut dsps: Vec<HardTile> = dsp_sites
            .into_iter()
            .filter_map(|(tile, [lo, hi])| {
                Some(HardTile {
                    xy: xy_of(tile),
                    half: [lo?, hi?],
                    full: None,
                })
            })
            .collect();
        dsps.sort_by_key(|b| (b.xy.0, b.xy.1));
        Ok(Grid {
            cols: xs.len() as u32,
            y_max,
            slices,
            at,
            bufgs,
            cmts,
            hard: [brams, dsps],
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
    /// Hard block k of `Design::hard[kind]`; its position a slot, tile `t`
    /// site `h` as `2t + h` (a RAMB36E1 at site 0).
    Hard(Hard, u32),
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
    hard_slot: [Vec<u32>; 2],
    /// Per hard block slot, the block in it (a RAMB36E1 in both halves).
    hard_occ: [Vec<Option<u32>>; 2],
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
        t.estimate(a, b)
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

    /// Each LUT input's criticality (`Placement::lut_criticality`).
    fn lut_criticality(&self) -> HashMap<(CellId, String), f32> {
        let mut out = HashMap::default();
        let Some(t) = self.timing else {
            return out;
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
        for (c, a) in t.conns.iter().zip(&arcs) {
            if !matches!(
                self.d.cells[c.to_cell as usize].kind,
                Kind::Lut { dram: false }
            ) {
                continue;
            }
            let Some((_, port)) = t.graph.name(c.to).rsplit_once('.') else {
                continue;
            };
            let crit = analysis.criticality(a);
            let e = out.entry((c.to_cell, port.to_string())).or_insert(0.0f32);
            *e = e.max(crit);
        }
        out
    }

    fn pos(&self, o: Obj) -> u32 {
        match o {
            Obj::Ble(b) => self.ble_slot[b as usize],
            Obj::Carry(c) => self.carry_root[c as usize],
            Obj::Dram(r) => self.dram_slice[r as usize],
            Obj::Hard(h, b) => self.hard_slot[h as usize][b as usize],
        }
    }

    /// A RAMB36E1, or two cascaded DSPs: the whole tile.
    fn hard_full(&self, h: Hard, b: u32) -> bool {
        matches!(
            self.d.cells[self.d.hard[h as usize][b as usize] as usize].kind,
            Kind::Bram {
                size: crate::bram::Size::Full,
                ..
            }
        ) || (h == Hard::Dsp && self.d.cascaded[b as usize].is_some())
    }

    /// The slots hard block `b` takes at `slot`; None where it can't go.
    fn hard_slots(&self, h: Hard, b: u32, slot: u32) -> Option<Vec<u32>> {
        if slot as usize >= self.hard_occ[h as usize].len() {
            return None;
        }
        if self.hard_full(h, b) {
            (slot % 2 == 0).then(|| vec![slot, slot + 1])
        } else {
            Some(vec![slot])
        }
    }

    fn slices_of(&self, o: Obj, pos: u32) -> Option<Vec<u32>> {
        match o {
            Obj::Ble(_) => Some(vec![pos / SLOTS as u32]),
            Obj::Carry(c) => self.g.chain(pos, self.d.carries[c as usize].len()),
            Obj::Dram(_) => Some(vec![pos]),
            Obj::Hard(..) => Some(Vec::new()),
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
            Obj::Hard(h, b) => {
                let slots = self
                    .hard_slots(h, b, self.hard_slot[h as usize][b as usize])
                    .expect("placed legally");
                for k in slots {
                    self.hard_occ[h as usize][k as usize] = None;
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
                for c in ble.cells() {
                    self.cell_xy[c as usize] = xy;
                }
                true
            }
            Obj::Hard(h, b) => {
                let Some(slots) = self.hard_slots(h, b, pos) else {
                    return false;
                };
                let occ = &mut self.hard_occ[h as usize];
                if slots.iter().any(|&k| occ[k as usize].is_some()) {
                    return false;
                }
                for &k in &slots {
                    occ[k as usize] = Some(b);
                }
                self.hard_slot[h as usize][b as usize] = pos;
                let xy = self.g.hard[h as usize][pos as usize / 2].xy;
                self.cell_xy[d.hard[h as usize][b as usize] as usize] = xy;
                if let Some(u) = d.upper(h, b) {
                    self.cell_xy[u as usize] = xy;
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
                        Obj::Ble(_) | Obj::Hard(..) => unreachable!(),
                    };
                    for c in cells {
                        self.cell_xy[c as usize] = xy;
                    }
                }
                match o {
                    Obj::Carry(c) => self.carry_root[c as usize] = pos,
                    Obj::Dram(r) => self.dram_slice[r as usize] = pos,
                    Obj::Ble(_) | Obj::Hard(..) => unreachable!(),
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
            Obj::Ble(b) => out.extend(d.bles[b as usize].cells()),
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
            Obj::Hard(h, b) => {
                out.push(d.hard[h as usize][b as usize]);
                out.extend(d.upper(h, b));
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
            // Swapped with what is there: a block RAM of the same size, or
            // a RAMB18E1 with the other half of a free tile.
            Obj::Hard(h, b) => {
                let mine = self.hard_slots(h, b, pos)?;
                let occ = &self.hard_occ[h as usize];
                let mut others: Vec<u32> = mine.iter().filter_map(|&k| occ[k as usize]).collect();
                others.sort_unstable();
                others.dedup();
                others.retain(|&x| x != b);
                let mut moves = vec![(o, pos)];
                match others[..] {
                    [] => {}
                    [x] if self.hard_full(h, x) == self.hard_full(h, b)
                        && self.hard_slot[h as usize][x as usize] == pos =>
                    {
                        moves.push((Obj::Hard(h, x), from))
                    }
                    _ => return None,
                }
                Some(moves)
            }
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
/// A state with nothing placed.
fn initial_empty<'a>(d: &'a Design, g: &'a Grid, timing: Option<&'a Timing>) -> State<'a> {
    let n_slices = g.slices.len();
    State {
        d,
        g,
        timing,
        ble_slot: vec![u32::MAX; d.bles.len()],
        carry_root: vec![u32::MAX; d.carries.len()],
        dram_slice: vec![u32::MAX; d.drams.len()],
        hard_slot: [
            vec![u32::MAX; d.hard[0].len()],
            vec![u32::MAX; d.hard[1].len()],
        ],
        hard_occ: [
            vec![None; 2 * g.hard[0].len()],
            vec![None; 2 * g.hard[1].len()],
        ],
        occ: vec![Occ::Free; n_slices * SLOTS],
        slice_macro: vec![None; n_slices],
        slice_cs: vec![None; n_slices],
        cell_xy: vec![(0, 0); d.cells.len()],
        net_cost: vec![0.0; d.nets.len()],
        weight: vec![1.0; d.nets.len()],
        stamp: vec![0; d.nets.len()],
        epoch: 0,
    }
}

/// The greedy start: chains and RAMs nearest the middle, then registers by
/// control set and lone LUTs into what is left.
fn initial<'a>(d: &'a Design, g: &'a Grid, timing: Option<&'a Timing>) -> Result<State<'a>> {
    let n_slices = g.slices.len();
    let mut s = initial_empty(d, g, timing);
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
    place_hard(&mut s, |_, _| (cx, cy))?;

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
    /// Started from the electrostatic global placement.
    pub electro: bool,
}

/// Where an anneal starts: hot from the greedy start, or cold from a
/// legalised global placement, which only needs local refinement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    Hot,
    Cold,
}

/// One anneal from `start`; returns the moves tried.
fn anneal(s: &mut State, rng: &mut Rng, start: Start) -> u64 {
    let d = s.d;
    let g = s.g;
    let objects: Vec<Obj> = (0..d.bles.len() as u32)
        .map(Obj::Ble)
        .chain((0..d.carries.len() as u32).map(Obj::Carry))
        .chain((0..d.drams.len() as u32).map(Obj::Dram))
        .chain(
            Hard::ALL
                .into_iter()
                .flat_map(|h| (0..d.hard[h as usize].len() as u32).map(move |b| Obj::Hard(h, b))),
        )
        .collect();
    if objects.is_empty() {
        return 0;
    }
    let max_r = (g.cols as i32).max(g.y_max).max(1) as f64;
    // Cold: moves within a few slices of the global placement's.
    let mut r = match start {
        Start::Hot => max_r,
        Start::Cold => COLD_RANGE.min(max_r),
    };
    let moves = match start {
        Start::Hot => 2.0,
        Start::Cold => COLD_MOVES,
    };
    let moves_per_t = (moves * (objects.len() as f64).powf(4.0 / 3.0)).max(200.0) as usize;
    let mut total_moves = 0u64;
    let (mut cells, mut nets) = (Vec::new(), Vec::new());

    let mut attempt = |s: &mut State, rng: &mut Rng, r: f64, t: f64| -> Option<f64> {
        let o = objects[rng.below(objects.len())];
        let ri = r.max(1.0) as i32;
        let pos = if let Obj::Hard(h, b) = o {
            // A slot of its kind within range of where it is, columns
            // counted as the slices' are (in grid x, two per interconnect
            // column).
            let tiles = &s.g.hard[h as usize];
            let (x, y) = tiles[s.hard_slot[h as usize][b as usize] as usize / 2].xy;
            let t = rng.below(tiles.len());
            let (tx, ty) = tiles[t].xy;
            if (tx - x).abs() / 2 > ri || (ty - y).abs() > ri {
                return None;
            }
            (2 * t + rng.below(2)) as u32
        } else {
            u32::MAX
        };
        let from = s.g.slices[(match o {
            Obj::Hard(..) => 0,
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
        let pos = match o {
            Obj::Hard(..) => pos,
            Obj::Ble(_) => {
                *s.g.at.get(&(u32::try_from(col).ok()?, y, even))? * SLOTS as u32
                    + rng.below(SLOTS) as u32
            }
            _ => *s.g.at.get(&(u32::try_from(col).ok()?, y, even))?,
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
    let mut t = match start {
        Start::Hot => 20.0,
        Start::Cold => COLD_TEMPERATURE,
    } * var.sqrt();
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

/// Where a cell sits in its site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bel {
    /// A slice's `<letter>6LUT`, `<letter>5LUT` or `<letter>FF`, letter 0
    /// to 3 for A to D.
    Lut6(usize),
    Lut5(usize),
    Ff(usize),
    /// The letter's second register, `<letter>5FF`.
    Ff5(usize),
    Carry4,
    /// The site's one bel: a pin's IOB, a BUFGCTRL, a BSCAN, the PS7.
    Site,
}

impl Bel {
    /// Vivado's name of the bel; a site's one bel goes by the site's type,
    /// so by `SITE` here.
    pub(crate) fn vivado_name(self) -> String {
        let letter = |k: usize| ['A', 'B', 'C', 'D'][k];
        match self {
            Bel::Lut6(k) => format!("{}6LUT", letter(k)),
            Bel::Lut5(k) => format!("{}5LUT", letter(k)),
            Bel::Ff(k) => format!("{}FF", letter(k)),
            Bel::Ff5(k) => format!("{}5FF", letter(k)),
            Bel::Carry4 => "CARRY4".into(),
            Bel::Site => "SITE".into(),
        }
    }
}

/// The placement: each cell's site (`SLICE_X30Y39`) and bel; constants have
/// none.
pub(crate) struct Placement {
    pub sites: Vec<Option<(String, Bel)>>,
    /// Each LUT input's criticality on the final placement's timing, by
    /// (cell, port), for choosing its site pin; inputs on no timed path
    /// aren't in it.
    pub lut_criticality: HashMap<(CellId, String), f32>,
    pub stats: Stats,
}

fn sites(
    d: &Design,
    g: &Grid,
    s: &State,
    fixed: &[(CellId, String, Xy)],
) -> Vec<Option<(String, Bel)>> {
    let mut out: Vec<Option<(String, Bel)>> = vec![None; d.cells.len()];
    let site = |slice: u32| g.slices[slice as usize].site.clone();
    for (b, ble) in d.bles.iter().enumerate() {
        let slot = s.ble_slot[b];
        let (slice, k) = (slot / SLOTS as u32, slot as usize % SLOTS);
        if let Some(l) = ble.lut {
            out[l as usize] = Some((site(slice), Bel::Lut6(k)));
        }
        if let Some(f) = ble.ff {
            out[f as usize] = Some((site(slice), Bel::Ff(k)));
        }
        if let Some(l) = ble.lut5 {
            out[l as usize] = Some((site(slice), Bel::Lut5(k)));
        }
        if let Some(f) = ble.ff5 {
            out[f as usize] = Some((site(slice), Bel::Ff5(k)));
        }
    }
    for (c, chain) in d.carries.iter().enumerate() {
        let slices = g
            .chain(s.carry_root[c], chain.len())
            .expect("placed legally");
        for (cs, &slice) in chain.iter().zip(&slices) {
            out[cs.carry as usize] = Some((site(slice), Bel::Carry4));
            for k in 0..SLOTS {
                for (cell, bel) in [
                    (cs.lut6[k], Bel::Lut6(k)),
                    (cs.lut5[k], Bel::Lut5(k)),
                    (cs.ff[k], Bel::Ff(k)),
                ] {
                    if let Some(cell) = cell {
                        out[cell as usize] = Some((site(slice), bel));
                    }
                }
            }
        }
    }
    for (r, ram) in d.drams.iter().enumerate() {
        let slice = s.dram_slice[r];
        for k in 0..SLOTS {
            out[ram.lut6[k] as usize] = Some((site(slice), Bel::Lut6(k)));
            out[ram.lut5[k] as usize] = Some((site(slice), Bel::Lut5(k)));
        }
    }
    for h in Hard::ALL {
        for (b, &c) in d.hard[h as usize].iter().enumerate() {
            let slot = s.hard_slot[h as usize][b] as usize;
            let tile = &g.hard[h as usize][slot / 2];
            let site = match (&d.cells[c as usize].kind, &tile.full) {
                (
                    Kind::Bram {
                        size: crate::bram::Size::Full,
                        ..
                    },
                    Some(full),
                ) => full.clone(),
                _ => tile.half[slot % 2].clone(),
            };
            out[c as usize] = Some((site, Bel::Site));
            if let Some(u) = d.upper(h, b as u32) {
                out[u as usize] = Some((tile.half[1].clone(), Bel::Site));
            }
        }
    }
    for (c, site, _) in fixed {
        out[*c as usize] = Some((site.clone(), Bel::Site));
    }
    out
}

/// Each hard block in the free slot of its kind nearest where `target`
/// wants it, RAMB36E1s first.
fn place_hard(s: &mut State, target: impl Fn(Hard, u32) -> Xy) -> Result<()> {
    let g = s.g;
    for h in Hard::ALL {
        let mut order: Vec<u32> = (0..s.d.hard[h as usize].len() as u32).collect();
        order.sort_by_key(|&b| (!s.hard_full(h, b), b));
        let tiles = &g.hard[h as usize];
        for b in order {
            let (x, y) = target(h, b);
            let mut slots: Vec<u32> = (0..2 * tiles.len() as u32).collect();
            slots.sort_by_key(|&k| {
                let (tx, ty) = tiles[k as usize / 2].xy;
                ((tx - x).abs() / 2 + (ty - y).abs(), k)
            });
            let ok = slots.into_iter().any(|k| s.place(Obj::Hard(h, b), k));
            ensure!(
                ok,
                "no {h:?} site left: the design has {}",
                s.d.hard[h as usize].len()
            );
        }
    }
    Ok(())
}

/// The cells that don't move: pins, BSCANs, the PS7, and BUFGCTRLs (each
/// the free one nearest its source). Returns (cell, site, position).
fn fixed_sites(db: &ChipDb, d: &Design, g: &Grid) -> Result<Vec<(CellId, String, Xy)>> {
    let site_xy = |site: &str| -> Result<Xy> {
        let s = db
            .sites
            .get(site)
            .with_context(|| format!("no site {site}"))?;
        let (x, y) = db.tile_xy(s.tile);
        Ok((x as i32, y as i32))
    };
    let mut out = Vec::new();
    for (i, c) in d.cells.iter().enumerate() {
        let site = match &c.kind {
            Kind::Pad { site, .. } => site.clone(),
            Kind::Bscan { chain } => format!("BSCAN_X0Y{}", chain - 1),
            Kind::Ps7 => "PS7_X0Y0".to_string(),
            _ => continue,
        };
        let xy = site_xy(&site).with_context(|| format!("placing {}", c.name))?;
        out.push((i as CellId, site, xy));
    }
    // PLLs and MMCMs next: each in the free site of its kind nearest the
    // cell that drives CLKIN1 (a clock pin reaches the tile of its own
    // clock region without a BUFG).
    let pinned: HashMap<CellId, Xy> = out.iter().map(|(c, _, xy)| (*c, *xy)).collect();
    let mut cmt_free = vec![true; g.cmts.len()];
    for (i, c) in d.cells.iter().enumerate() {
        let Kind::Cmt { prim } = c.kind else { continue };
        let source = c
            .net("CLKIN1")
            .and_then(|n| d.nets[n as usize].driver.as_ref())
            .and_then(|(drv, _)| pinned.get(drv).copied())
            .unwrap_or((0, g.y_max / 2));
        let site = (0..g.cmts.len())
            .filter(|&s| cmt_free[s] && g.cmts[s].1 == prim)
            .min_by_key(|&s| {
                let (x, y) = g.cmts[s].2;
                ((y - source.1).abs() + (x - source.0).abs() / 2, s)
            })
            .with_context(|| format!("{}: no {} site left", c.name, prim.cell()))?;
        cmt_free[site] = false;
        let (name, _, xy) = &g.cmts[site];
        out.push((i as CellId, name.clone(), *xy));
    }
    // BUFGCTRLs last: their sources may be the pins, BSCANs and clock
    // blocks above. One driven from the fabric has no fixed source; the
    // middle of the device stands in.
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
        out.push((i as CellId, name.clone(), *xy));
    }
    Ok(out)
}

/// A cold anneal's range limit, in slices, starting temperature, in
/// standard deviations of its moves' cost changes, and moves per
/// temperature, times N^(4/3) (a hot one: the whole device, 20 and 2):
/// the Cyclone II's `REFINE`, its polish after a cluster-level analytic
/// placement. Its `REFINE_LE` (1, 0.05, 0.1) placed sha256 in 16 s but
/// left it slower (estimated 54.7 against 59.6 MHz) and picorv32 slower
/// still (165 against 186 MHz); (2, 0.2, 0.3) was worse on both (53.4,
/// 158 MHz). The global placement is wirelength-only and its BLEs are
/// legalised greedily, so the anneal has timing left to fix.
const COLD_RANGE: f64 = 3.0;
const COLD_TEMPERATURE: f64 = 0.5;
const COLD_MOVES: f64 = 1.0;

/// Anneals run in parallel from the same start with different seeds; the
/// lowest-cost one wins, so the result doesn't depend on thread timing.
fn best_of<'a>(start: &State<'a>, seeds: usize, from: Start) -> (State<'a>, u64) {
    // CHIPSMITH_XC7_SEED=<n>: another set of seeds, to tell a change from
    // the anneal's luck (bench/zybo-timing.sh's ZYBO_SEEDS); 0 is the default.
    let base: u64 = std::env::var("CHIPSMITH_XC7_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let results: Vec<(State, u64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..seeds)
            .map(|i| {
                let mut s = start.clone();
                scope.spawn(move || {
                    let mut rng = Rng(0x9e37_79b9_7f4a_7c15
                        ^ (i as u64 + 1).wrapping_mul(0xbf58_476d_1ce4_e5b9)
                        ^ base.wrapping_mul(0x94d0_49bb_1331_11eb));
                    let moves = anneal(&mut s, &mut rng, from);
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

/// From this many BLEs, chains and RAMs on, the anneal starts cold from an
/// electrostatic global placement (chipsmith-place, ADR-0012) instead of
/// hot from the greedy start; `CHIPSMITH_XC7_PLACER=anneal` or `electro`
/// forces either.
///
/// Five seeds each on chipsmith's calibrated timing, the electrostatic
/// start (timing-driven, below) against the greedy one: sha256 +5%,
/// picorv32 +4%, riscv-bram +2%, bram-selftest +4%, mult-selftest's
/// 62.5 MHz logic -6%; placement 4-5 times faster on the middle three.
/// By Vivado's timer on picorv32's five seeds, even (145.4 against 145.1
/// MHz): the estimate's bias moves with the placement's style.
const ELECTRO_FROM: usize = 200;

/// The global placement reweights its nets by their criticality (as the
/// anneal does) every this many iterations once it has spread: sha256 at
/// 52.7 MHz against 50.1 without (five seeds).
const GLOBAL_TIMING_EVERY: usize = 25;

/// The global placement's nets weighted by their most critical connection,
/// as the anneal weights them, on the objects' current centres.
struct GlobalTiming<'a> {
    timing: Option<&'a Timing>,
    obj_of: &'a [Option<u32>],
    fixed_xy: &'a HashMap<CellId, Xy>,
    problem_net: &'a [Option<usize>],
}

impl chipsmith_place::electro::Timing for GlobalTiming<'_> {
    fn reweight(&mut self, centres: &[chipsmith_place::Pos], weights: &mut [f32]) {
        let Some(t) = self.timing else {
            return;
        };
        let xy = |c: CellId| -> Option<Xy> {
            match self.obj_of[c as usize] {
                Some(o) => {
                    let (cx, cy) = centres[o as usize];
                    Some(((cx * 2.0) as i32, cy as i32))
                }
                None => self.fixed_xy.get(&c).copied(),
            }
        };
        let arcs: Vec<Arc> = t
            .conns
            .iter()
            .map(|c| {
                let same = self.obj_of[c.from_cell as usize].is_some()
                    && self.obj_of[c.from_cell as usize] == self.obj_of[c.to_cell as usize];
                let delay = match (xy(c.from_cell), xy(c.to_cell)) {
                    _ if c.internal && same => 0.0,
                    (Some(a), Some(b)) => t.estimate(a, b),
                    _ => 0.0,
                };
                Arc {
                    from: c.from,
                    to: c.to,
                    delay,
                }
            })
            .collect();
        let analysis = t.graph.analyse(&arcs);
        weights.iter_mut().for_each(|w| *w = 1.0);
        for (c, a) in t.conns.iter().zip(&arcs) {
            if let Some(n) = self.problem_net[c.net as usize] {
                let w = 1.0 + TIMING_WEIGHT * analysis.criticality(a).powf(CRIT_EXP);
                weights[n] = weights[n].max(w);
            }
        }
    }
}

/// The objects' global placement (chipsmith-place's electrostatic placer,
/// timing-driven by [`GlobalTiming`]), each then put in the free place nearest its target:
/// chains and RAMs first, then BLEs.
fn global<'a>(
    d: &'a Design,
    g: &'a Grid,
    timing: Option<&'a Timing>,
    fixed: &[(CellId, String, Xy)],
) -> Result<State<'a>> {
    use chipsmith_place::electro;
    use chipsmith_place::{Pin, PinOf};
    // Bins of two grid columns by one row, in the cost's units (x halved);
    // capacity in BLE slots, as a share of the fullest bin's.
    let bin = |xy: Xy| ((xy.0 / 2) as usize, xy.1 as usize);
    let width = g.slices.iter().map(|s| bin(s.xy).0).max().unwrap_or(0) + 1;
    let height = g.y_max as usize + 1;
    let mut slots = vec![0.0f32; width * height];
    for s in &g.slices {
        let (x, y) = bin(s.xy);
        slots[y * width + x] += SLOTS as f32;
    }
    let full = slots.iter().copied().fold(1.0f32, f32::max);
    let capacity = vec![slots.iter().map(|v| v / full).collect()];
    let mut p = electro::Problem::new(width, height, capacity);
    let mut obj_of: Vec<Option<u32>> = vec![None; d.cells.len()];
    let mut objects: Vec<Obj> = Vec::new();
    for (b, ble) in d.bles.iter().enumerate() {
        let o = p.add_object(1, &[(0, 1.0 / full)]);
        for c in ble.cells() {
            obj_of[c as usize] = Some(o);
        }
        objects.push(Obj::Ble(b as u32));
    }
    for (k, chain) in d.carries.iter().enumerate() {
        let rows = chain.len().min(height) as u32;
        let o = p.add_object(rows, &[(0, (SLOTS * chain.len()) as f32 / full)]);
        for cs in chain {
            let cells = std::iter::once(cs.carry).chain(
                cs.lut6
                    .iter()
                    .chain(&cs.lut5)
                    .chain(&cs.ff)
                    .flatten()
                    .copied(),
            );
            for c in cells {
                obj_of[c as usize] = Some(o);
            }
        }
        objects.push(Obj::Carry(k as u32));
    }
    for (r, ram) in d.drams.iter().enumerate() {
        let o = p.add_object(1, &[(0, SLOTS as f32 / full)]);
        for &c in ram.lut6.iter().chain(&ram.lut5) {
            obj_of[c as usize] = Some(o);
        }
        objects.push(Obj::Dram(r as u32));
    }
    // Block RAMs: an object each, of a BRAM tile's five rows and a slice
    // column's area, taking none of the BLEs' room (they have sites of
    // their own).
    let mut hard_obj: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
    for h in Hard::ALL {
        for (b, &c) in d.hard[h as usize].iter().enumerate() {
            let o = p.add_object(5, &[(0, 0.0)]);
            obj_of[c as usize] = Some(o);
            if let Some(u) = d.upper(h, b as u32) {
                obj_of[u as usize] = Some(o);
            }
            hard_obj[h as usize].push(o);
        }
    }
    let mut fixed_of: HashMap<CellId, u32> = HashMap::default();
    for (c, _, xy) in fixed {
        fixed_of.insert(
            *c,
            p.add_fixed(((xy.0 / 2) as f32 + 0.5, xy.1 as f32 + 0.5)),
        );
    }
    // Each design net's net in the problem, for reweighting.
    let mut problem_net: Vec<Option<usize>> = vec![None; d.net_cells.len()];
    let mut problem_nets = 0;
    for (n, cells) in d.net_cells.iter().enumerate() {
        let mut pins: Vec<Pin> = Vec::new();
        for &c in cells {
            let of = match (obj_of[c as usize], fixed_of.get(&c)) {
                (Some(o), _) => PinOf::Object(o),
                (None, Some(&f)) => PinOf::Fixed(f),
                _ => continue,
            };
            if !pins.iter().any(|q| q.of == of) {
                pins.push(Pin {
                    of,
                    dx: 0.0,
                    dy: 0.0,
                });
            }
        }
        if pins.len() >= 2 {
            p.add_net(pins, 1.0);
            problem_net[n] = Some(problem_nets);
            problem_nets += 1;
        }
    }
    let opts = electro::Options {
        accel: electro::Accel::from_env().map_err(anyhow::Error::msg)?,
        timing_every: if timing.is_some() {
            GLOBAL_TIMING_EVERY
        } else {
            0
        },
        trace: std::env::var_os("CHIPSMITH_XC7_TRACE").is_some(),
        ..Default::default()
    };
    let fixed_xy: HashMap<CellId, Xy> = fixed.iter().map(|(c, _, xy)| (*c, *xy)).collect();
    let mut hook = GlobalTiming {
        timing,
        obj_of: &obj_of,
        fixed_xy: &fixed_xy,
        problem_net: &problem_net,
    };
    let out = electro::place(&p, &mut hook, None, &opts).map_err(anyhow::Error::msg)?;
    let target = |o: usize| {
        let (cx, cy) = out.centres[o];
        ((cx * 2.0) as i32, cy as i32)
    };

    let mut s = initial_empty(d, g, timing);
    let near = |xy: Xy| {
        let mut order: Vec<u32> = (0..g.slices.len() as u32).collect();
        order.sort_by_key(|&i| {
            let (x, y) = g.slices[i as usize].xy;
            ((x - xy.0).abs() / 2 + (y - xy.1).abs(), i)
        });
        order
    };
    // Macros, longest first.
    let mut macros: Vec<usize> = (d.bles.len()..objects.len()).collect();
    macros.sort_by_key(|&o| match objects[o] {
        Obj::Carry(k) => std::cmp::Reverse(d.carries[k as usize].len()),
        _ => std::cmp::Reverse(1),
    });
    for o in macros {
        let ok = near(target(o))
            .into_iter()
            .any(|sl| s.place(objects[o], sl));
        ensure!(ok, "no room for {:?}", objects[o]);
    }
    place_hard(&mut s, |h, b| {
        target(hard_obj[h as usize][b as usize] as usize)
    })?;
    // BLEs: a ring search outward from the target, slot by slot.
    let col_of_x = |x: i32| {
        g.slices
            .iter()
            .min_by_key(|sl| (sl.xy.0 - x).abs())
            .map_or(0, |sl| sl.col as i32)
    };
    for o in 0..d.bles.len() {
        let (tx, ty) = target(o);
        let tcol = col_of_x(tx);
        let mut placed = false;
        'rings: for r in 0..(g.cols as i32).max(g.y_max + 1) {
            for dc in -r..=r {
                for dy in -r..=r {
                    if dc.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let Ok(col) = u32::try_from(tcol + dc) else {
                        continue;
                    };
                    for even in [true, false] {
                        let Some(&sl) = g.at.get(&(col, ty + dy, even)) else {
                            continue;
                        };
                        for k in 0..SLOTS as u32 {
                            let slot = sl * SLOTS as u32 + k;
                            if s.occ[slot as usize] == Occ::Free && s.place(objects[o], slot) {
                                placed = true;
                                break 'rings;
                            }
                        }
                    }
                }
            }
        }
        ensure!(placed, "the design doesn't fit the device");
    }
    Ok(s)
}

/// Places the packer's design. With `delays` the placement is
/// timing-driven; without, wirelength alone.
pub(crate) fn place(db: &ChipDb, delays: Option<&Delays>, d: &Design) -> Result<Placement> {
    let started = Instant::now();
    let g = Grid::new(db)?;
    let timing = delays.map(|delays| Timing::new(d, delays));
    let fixed = fixed_sites(db, d, &g)?;
    let objects =
        d.bles.len() + d.carries.len() + d.drams.len() + d.hard.iter().map(Vec::len).sum::<usize>();
    let electro = match std::env::var("CHIPSMITH_XC7_PLACER").as_deref() {
        Ok("anneal") => false,
        Ok("electro") => true,
        _ => objects >= ELECTRO_FROM,
    };
    let (mut start, from) = if electro {
        (global(d, &g, timing.as_ref(), &fixed)?, Start::Cold)
    } else {
        (initial(d, &g, timing.as_ref())?, Start::Hot)
    };
    for (c, _, xy) in &fixed {
        start.cell_xy[*c as usize] = *xy;
    }
    for n in 0..d.nets.len() {
        start.net_cost[n] = start.cost(n);
    }
    let initial_wirelength = start.wirelength();
    let threads = SEEDS;
    let (mut best, moves) = best_of(&start, threads, from);
    let critical = best.analyse(false);
    Ok(Placement {
        sites: sites(d, &g, &best, &fixed),
        lut_criticality: best.lut_criticality(),
        stats: Stats {
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
            electro,
        },
    })
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
            cmts: Vec::new(),
            hard: [Vec::new(), Vec::new()],
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
            hard: [Vec::new(), Vec::new()],
            cascaded: Vec::new(),
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
                ff: Some(i),
                ..Ble::default()
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
            ..Ble::default()
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
