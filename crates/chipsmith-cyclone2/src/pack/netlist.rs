//! The netlist the packer rewrites: cells with typed settings, nets with a
//! driver and users.
//!
//! Some packing rules depend on the order cells and users are visited in
//! (folding one buffer can make another LUT a one-input LUT; the first
//! register to claim a LUT gets it). The packer was ported from nextpnr's
//! cyclone2 uarch, so to pack the same way the containers here iterate the
//! way nextpnr's do: cells and nets like its `hashlib::dict` (newest first,
//! an erased entry's slot taken by the last one), users like its
//! `indexed_store` (slots in index order, freed slots reused last-freed
//! first). Net ids are what nextpnr writes, the order names were first
//! interned (`Interner`); the router routes nets in id order.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use crate::hard;
use crate::m4k::{self, AddrPort};

pub type CellId = usize;
pub type NetId = usize;

/// Every port a cell can have, from the yosys cells (synth/cyclone2.ys),
/// the clock control the packer adds and nextpnr's frontend cells. A bus
/// port is one port per bit, as nextpnr's frontend splits it (`A_ADDR[3]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Port {
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
    Dataout,
    /// A tri-state pin's output enable.
    Oe,
    Pad,
    Inclk,
    Outclk,
    /// Constant drivers' output.
    Y,
    /// Top-level port cells: input port's output, output port's input.
    O,
    I,
    /// CY2_M4K's logical ports (synth/m4k_map.v), until `pack_m4k` moves
    /// them onto the block's pins. Its clock is `Clk`.
    RamAddr(AddrPort, u8),
    RamDataIn(u8),
    RamWe,
    RamDataOut(u8),
    /// A pin of the M4K bel, from `pack_m4k` on.
    M4k(m4k::Pin),
    /// A pin of the JTAG or serial-flash block.
    Hard(hard::Pin),
}

pub const LUT_INPUTS: [Port; 4] = [Port::DataA, Port::DataB, Port::DataC, Port::DataD];

impl Port {
    const FIXED: [(Port, &'static str); 22] = [
        (Port::DataA, "DATAA"),
        (Port::DataB, "DATAB"),
        (Port::DataC, "DATAC"),
        (Port::DataD, "DATAD"),
        (Port::Cin, "CIN"),
        (Port::Cout, "COUT"),
        (Port::Combout, "COMBOUT"),
        (Port::Datain, "DATAIN"),
        (Port::Sdata, "SDATA"),
        (Port::Clk, "CLK"),
        (Port::Ena, "ENA"),
        (Port::Aclr, "ACLR"),
        (Port::Sclr, "SCLR"),
        (Port::Regout, "REGOUT"),
        (Port::Dataout, "DATAOUT"),
        (Port::Oe, "OE"),
        (Port::Pad, "PAD"),
        (Port::Inclk, "INCLK"),
        (Port::Outclk, "OUTCLK"),
        (Port::Y, "Y"),
        (Port::O, "O"),
        (Port::I, "I"),
    ];

    /// A yosys cell port (bit) name. M4K pins aren't: only the packer
    /// creates those.
    pub fn parse(name: &str) -> Option<Port> {
        if let Some(&(p, _)) = Self::FIXED.iter().find(|(_, n)| *n == name) {
            return Some(p);
        }
        if name == "A_WE" {
            return Some(Port::RamWe);
        }
        if let Some(p) = hard::Pin::parse(name) {
            return Some(Port::Hard(p));
        }
        let (base, index) = name.strip_suffix(']')?.split_once('[')?;
        let i: u8 = index.parse().ok()?;
        match base {
            "A_ADDR" if i < 12 => Some(Port::RamAddr(AddrPort::A, i)),
            "B_ADDR" if i < 12 => Some(Port::RamAddr(AddrPort::B, i)),
            "A_DATAIN" if i < 36 => Some(Port::RamDataIn(i)),
            "DATAOUT" if i < 36 => Some(Port::RamDataOut(i)),
            _ => None,
        }
    }

    pub fn name(self) -> Cow<'static, str> {
        match self {
            Port::RamAddr(AddrPort::A, i) => format!("A_ADDR[{i}]").into(),
            Port::RamAddr(AddrPort::B, i) => format!("B_ADDR[{i}]").into(),
            Port::RamDataIn(i) => format!("A_DATAIN[{i}]").into(),
            Port::RamWe => "A_WE".into(),
            Port::RamDataOut(i) => format!("DATAOUT[{i}]").into(),
            Port::M4k(pin) => pin.to_string().into(),
            Port::Hard(pin) => pin.name().into(),
            fixed => Self::FIXED
                .iter()
                .find(|(p, _)| *p == fixed)
                .expect("every other port is in FIXED")
                .1
                .into(),
        }
    }

    /// Which LUT input (A = 0 .. D = 3) this is.
    pub fn lut_index(self) -> Option<usize> {
        LUT_INPUTS.iter().position(|&p| p == self)
    }
}

/// A register's LAB-wide control inversions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FfConfig {
    pub ena_inv: bool,
    pub sclr_inv: bool,
}

/// What one of an IOE's inputs from the core (output data, output enable)
/// carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoeIn {
    /// The port's net, inverted in the IOE or not.
    Signal { invert: bool },
    /// A constant (the port unconnected).
    Const(bool),
}

/// Which way a cell port or a top-level port points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    In,
    Out,
    InOut,
}

impl Dir {
    pub fn parse(s: &str) -> Option<Dir> {
        match s {
            "input" => Some(Dir::In),
            "output" => Some(Dir::Out),
            "inout" => Some(Dir::InOut),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dir::In => "input",
            Dir::Out => "output",
            Dir::InOut => "inout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Lut {
        mask: u16,
    },
    Ff(FfConfig),
    /// Pin buffers, with the top-level port bit they serve (`LEDR[0]`)
    /// once pack_io has found it.
    Ibuf {
        port: Option<String>,
    },
    Obuf {
        port: Option<String>,
        data: IoeIn,
    },
    /// A tri-state output or a bidir (CY2_IOBUF): the pad is driven with
    /// `data` while `oe` is high; COMBOUT, if connected, reads the pad.
    Iobuf {
        port: Option<String>,
        data: IoeIn,
        oe: IoeIn,
    },
    ClkCtrl,
    /// An M4K RAM block; its contents (`INIT`) pass through as a parameter.
    M4k(m4k::Shape),
    /// The JTAG or serial-flash block.
    Hard(hard::Block),
    /// GND / VCC: the frontend's driver of a constant bit.
    Const(bool),
    /// nextpnr's top-level port cells, removed by pack_io. An inout's
    /// (`$nextpnr_iobuf`) takes the port net's driver on I and drives the
    /// port net from O.
    TopIn,
    TopOut,
    TopInOut,
}

impl Kind {
    pub fn is_lut(&self) -> bool {
        matches!(self, Kind::Lut { .. })
    }
    pub fn is_ff(&self) -> bool {
        matches!(self, Kind::Ff(_))
    }

    /// The top-level port bit a pin buffer serves.
    pub fn pin_port(&mut self) -> Option<&mut Option<String>> {
        match self {
            Kind::Ibuf { port } | Kind::Obuf { port, .. } | Kind::Iobuf { port, .. } => Some(port),
            _ => None,
        }
    }

    /// The IOE input a pin buffer port is, for folding constants and
    /// inverters into the IOE (OUT_CONST / DATA_INVERT, OE_CONST /
    /// OE_INVERT).
    pub fn ioe_in(&mut self, port: Port) -> Option<&mut IoeIn> {
        if !self.is_ioe_in(port) {
            return None;
        }
        match (self, port) {
            (Kind::Obuf { data, .. } | Kind::Iobuf { data, .. }, Port::Dataout) => Some(data),
            (Kind::Iobuf { oe, .. }, Port::Oe) => Some(oe),
            _ => None,
        }
    }

    pub fn is_ioe_in(&self, port: Port) -> bool {
        matches!(
            (self, port),
            (Kind::Obuf { .. } | Kind::Iobuf { .. }, Port::Dataout)
                | (Kind::Iobuf { .. }, Port::Oe)
        )
    }

    pub fn dir(&self, port: Port) -> Dir {
        let out = match self {
            Kind::Lut { .. } => matches!(port, Port::Combout | Port::Cout),
            Kind::Ff(_) => port == Port::Regout,
            Kind::Ibuf { .. } => port == Port::Combout,
            Kind::Obuf { .. } => port == Port::Pad,
            Kind::Iobuf { .. } if port == Port::Pad => return Dir::InOut,
            Kind::Iobuf { .. } => port == Port::Combout,
            Kind::ClkCtrl => port == Port::Outclk,
            Kind::M4k(_) => matches!(port, Port::RamDataOut(_) | Port::M4k(m4k::Pin::Do(_))),
            Kind::Hard(_) => matches!(port, Port::Hard(p) if p.is_output()),
            Kind::Const(_) => port == Port::Y,
            Kind::TopIn | Kind::TopInOut => port == Port::O,
            Kind::TopOut => false,
        };
        if out {
            Dir::Out
        } else {
            Dir::In
        }
    }

    /// Ports this kind drives. Every other port is a load, an inout one
    /// too (nextpnr's `connectPort`).
    pub fn is_output(&self, port: Port) -> bool {
        self.dir(port) == Dir::Out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Conn {
    net: NetId,
    /// Slot in the net's users; None for the driver.
    user: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Cell {
    pub name: String,
    pub kind: Kind,
    /// Ports in creation order (nextpnr's `CellInfo::ports`), connected or not.
    ports: Vec<(Port, Option<Conn>)>,
    /// yosys attributes and parameters other than the ones `kind` types,
    /// written back as they came.
    pub attrs: BTreeMap<String, Value>,
    pub params: BTreeMap<String, Value>,
}

impl Cell {
    pub fn has_port(&self, port: Port) -> bool {
        self.ports.iter().any(|(p, _)| *p == port)
    }

    pub fn net(&self, port: Port) -> Option<NetId> {
        self.ports
            .iter()
            .find(|(p, _)| *p == port)
            .and_then(|(_, c)| c.map(|c| c.net))
    }

    pub fn ports(&self) -> impl Iterator<Item = (Port, Option<NetId>)> + '_ {
        self.ports.iter().map(|&(p, c)| (p, c.map(|c| c.net)))
    }

    pub fn mask(&self) -> u16 {
        match self.kind {
            Kind::Lut { mask } => mask,
            _ => panic!("{} is not a LUT", self.name),
        }
    }

    pub fn set_mask(&mut self, m: u16) {
        match &mut self.kind {
            Kind::Lut { mask } => *mask = m,
            _ => panic!("{} is not a LUT", self.name),
        }
    }
}

/// A load: cell and port.
pub type PortRef = (CellId, Port);

/// nextpnr's `indexed_store<PortRef>`.
#[derive(Debug, Clone, Default)]
struct Users {
    slots: Vec<Slot>,
    first_free: usize,
    count: usize,
}

#[derive(Debug, Clone, Copy)]
enum Slot {
    Used(PortRef),
    Free { next: usize },
}

impl Users {
    fn add(&mut self, r: PortRef) -> usize {
        self.count += 1;
        if self.first_free == self.slots.len() {
            self.slots.push(Slot::Used(r));
            self.first_free += 1;
            self.slots.len() - 1
        } else {
            let i = self.first_free;
            let Slot::Free { next } = self.slots[i] else {
                unreachable!("free list points at a used slot")
            };
            self.first_free = next;
            self.slots[i] = Slot::Used(r);
            i
        }
    }

    fn remove(&mut self, i: usize) {
        self.count -= 1;
        self.slots[i] = Slot::Free {
            next: self.first_free,
        };
        self.first_free = i;
    }

    fn iter(&self) -> impl Iterator<Item = PortRef> + '_ {
        self.slots.iter().filter_map(|s| match s {
            Slot::Used(r) => Some(*r),
            Slot::Free { .. } => None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Net {
    pub name: String,
    pub driver: Option<PortRef>,
    users: Users,
    /// yosys netname attributes (`src`), written back as they came.
    pub attrs: BTreeMap<String, Value>,
}

/// nextpnr's `hashlib::dict` entry order.
#[derive(Debug, Default, Clone)]
struct Order {
    entries: Vec<usize>,
    pos: HashMap<usize, usize>,
}

impl Order {
    fn insert(&mut self, k: usize) {
        self.pos.insert(k, self.entries.len());
        self.entries.push(k);
    }

    fn erase(&mut self, k: usize) {
        let i = self.pos.remove(&k).expect("erasing an absent entry");
        let last = self.entries.pop().unwrap();
        if last != k {
            self.entries[i] = last;
            self.pos.insert(last, i);
        }
    }

    fn contains(&self, k: usize) -> bool {
        self.pos.contains_key(&k)
    }

    /// Iteration order: newest entry first.
    fn snapshot(&self) -> Vec<usize> {
        self.entries.iter().rev().copied().collect()
    }
}

/// nextpnr's IdString pool: ids in the order strings were first seen.
#[derive(Debug, Default, Clone)]
pub struct Interner(HashMap<String, u32>);

impl Interner {
    pub fn intern(&mut self, s: &str) -> u32 {
        let next = self.0.len() as u32;
        *self.0.entry(s.to_string()).or_insert(next)
    }

    pub fn get(&self, s: &str) -> u32 {
        self.0[s]
    }
}

/// A top-level port as the output lists it: each bit's net.
#[derive(Debug, Clone)]
pub struct TopPort {
    pub name: String,
    pub dir: Dir,
    pub bits: Vec<Option<NetId>>,
}

#[derive(Debug, Default, Clone)]
pub struct Netlist {
    cells: Vec<Cell>,
    nets: Vec<Net>,
    cell_order: Order,
    net_order: Order,
    cell_names: HashSet<String>,
    net_names: HashSet<String>,
    /// Every yosys name a net goes by; new net names avoid them.
    pub net_aliases: HashSet<String>,
    pub ids: Interner,
    pub top_ports: Vec<TopPort>,
    pub module_attrs: BTreeMap<String, Value>,
}

impl Netlist {
    pub fn cell(&self, c: CellId) -> &Cell {
        &self.cells[c]
    }
    pub fn cell_mut(&mut self, c: CellId) -> &mut Cell {
        &mut self.cells[c]
    }
    pub fn net(&self, n: NetId) -> &Net {
        &self.nets[n]
    }
    pub fn net_mut(&mut self, n: NetId) -> &mut Net {
        &mut self.nets[n]
    }

    pub fn net_exists(&self, n: NetId) -> bool {
        self.net_order.contains(n)
    }
    pub fn has_cell_named(&self, name: &str) -> bool {
        self.cell_names.contains(name)
    }
    pub fn has_net_named(&self, name: &str) -> bool {
        self.net_names.contains(name) || self.net_aliases.contains(name)
    }

    /// Live cells, in nextpnr's iteration order.
    pub fn cells(&self) -> Vec<CellId> {
        self.cell_order.snapshot()
    }
    /// Live nets, in nextpnr's iteration order.
    pub fn nets(&self) -> Vec<NetId> {
        self.net_order.snapshot()
    }

    pub fn create_cell(&mut self, name: String, kind: Kind) -> CellId {
        self.ids.intern(&name);
        assert!(self.cell_names.insert(name.clone()), "cell {name} twice");
        let id = self.cells.len();
        self.cells.push(Cell {
            name,
            kind,
            ports: Vec::new(),
            attrs: BTreeMap::new(),
            params: BTreeMap::new(),
        });
        self.cell_order.insert(id);
        id
    }

    pub fn create_net(&mut self, name: String) -> NetId {
        self.ids.intern(&name);
        assert!(self.net_names.insert(name.clone()), "net {name} twice");
        let id = self.nets.len();
        self.nets.push(Net {
            name,
            driver: None,
            users: Users::default(),
            attrs: BTreeMap::new(),
        });
        self.net_order.insert(id);
        id
    }

    pub fn add_port(&mut self, c: CellId, port: Port) {
        if !self.cells[c].has_port(port) {
            self.cells[c].ports.push((port, None));
        }
    }

    /// Drops an unconnected port (nextpnr's `ports.erase`).
    pub fn remove_port(&mut self, c: CellId, port: Port) {
        let ports = &mut self.cells[c].ports;
        if let Some(i) = ports.iter().position(|(p, _)| *p == port) {
            assert!(ports[i].1.is_none(), "removing a connected port");
            // hashlib: the last entry takes the erased one's slot.
            ports.swap_remove(i);
        }
    }

    /// Connects an existing, unconnected port.
    pub fn connect(&mut self, c: CellId, port: Port, net: NetId) {
        let output = self.cells[c].kind.is_output(port);
        let user = if output {
            let n = &mut self.nets[net];
            assert!(
                n.driver.is_none(),
                "{}: second driver {}",
                n.name,
                self.cells[c].name
            );
            n.driver = Some((c, port));
            None
        } else {
            Some(self.nets[net].users.add((c, port)))
        };
        let cell = &mut self.cells[c];
        let slot = cell
            .ports
            .iter_mut()
            .find(|(p, _)| *p == port)
            .unwrap_or_else(|| panic!("{} has no port {}", cell.name, port.name()));
        assert!(
            slot.1.is_none(),
            "{}.{} already connected",
            cell.name,
            port.name()
        );
        slot.1 = Some(Conn { net, user });
    }

    pub fn disconnect(&mut self, c: CellId, port: Port) {
        let Some(slot) = self.cells[c].ports.iter_mut().find(|(p, _)| *p == port) else {
            return;
        };
        let Some(conn) = slot.1.take() else { return };
        let net = &mut self.nets[conn.net];
        if let Some(u) = conn.user {
            net.users.remove(u);
        }
        if net.driver == Some((c, port)) {
            net.driver = None;
        }
    }

    /// Moves `port` of `c` from whatever it is on to `net`.
    pub fn reconnect(&mut self, c: CellId, port: Port, net: NetId) {
        self.disconnect(c, port);
        self.connect(c, port, net);
    }

    /// Disconnects every port, newest first (nextpnr iterating `ports`).
    pub fn disconnect_all(&mut self, c: CellId) {
        let ports: Vec<Port> = self.cells[c].ports.iter().rev().map(|(p, _)| *p).collect();
        for p in ports {
            self.disconnect(c, p);
        }
    }

    /// Removes a cell. Ports still connected leave their nets without that
    /// driver or user (nextpnr leaves a dangling pointer; nothing reads it).
    pub fn erase_cell(&mut self, c: CellId) {
        self.disconnect_all(c);
        self.cell_order.erase(c);
        self.cell_names.remove(&self.cells[c].name);
    }

    pub fn erase_net(&mut self, n: NetId) {
        self.net_order.erase(n);
        self.net_names.remove(&self.nets[n].name);
    }

    pub fn users(&self, n: NetId) -> Vec<PortRef> {
        self.nets[n].users.iter().collect()
    }

    pub fn user_count(&self, n: NetId) -> usize {
        self.nets[n].users.count
    }

    /// Whether a net has loads (nextpnr: `net && !net->users.empty()`).
    pub fn has_users(&self, n: Option<NetId>) -> bool {
        n.is_some_and(|n| self.nets[n].users.count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn users_reuse_the_last_freed_slot_first() {
        let mut u = Users::default();
        let a = u.add((0, Port::DataA));
        let b = u.add((1, Port::DataA));
        let c = u.add((2, Port::DataA));
        u.remove(a);
        u.remove(c);
        assert_eq!(u.add((3, Port::DataA)), c);
        assert_eq!(u.add((4, Port::DataA)), a);
        let order: Vec<CellId> = u.iter().map(|(c, _)| c).collect();
        assert_eq!(order, [4, 1, 3]);
        assert_eq!(b, 1);
    }

    #[test]
    fn dict_order_is_newest_first_and_erase_moves_the_last_entry() {
        let mut o = Order::default();
        for k in 0..4 {
            o.insert(k);
        }
        o.erase(1);
        assert_eq!(o.snapshot(), [2, 3, 0]);
    }
}
