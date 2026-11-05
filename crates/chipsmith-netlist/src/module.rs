//! Designs, modules and wires.

use std::collections::{BTreeMap, HashSet};

use crate::cell::{Cell, CellKind};
use crate::param::{attr, is_set, Attributes, ParamValue};
use crate::ports::Dir;
use crate::sig::{Net, SigBit, SigSpec};

/// Index of a cell in [`Module::cells`].
pub type CellId = usize;
/// Index of a wire in [`Module::wires`].
pub type WireId = usize;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Design {
    pub modules: Vec<Module>,
}

impl Design {
    pub fn module(&self, name: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.name == name)
    }

    pub fn module_mut(&mut self, name: &str) -> Option<&mut Module> {
        self.modules.iter_mut().find(|m| m.name == name)
    }

    /// The module marked `top` (yosys' `hierarchy -top` marks it).
    pub fn top(&self) -> Option<&Module> {
        self.modules
            .iter()
            .find(|m| is_set(&m.attributes, attr::TOP))
    }

    /// Every invariant the types don't enforce; see [`Module::check`].
    pub fn check(&self) -> Result<(), String> {
        let mut names = HashSet::new();
        for m in &self.modules {
            if !names.insert(&m.name) {
                return Err(format!("two modules named {}", m.name));
            }
            m.check().map_err(|e| format!("module {}: {e}", m.name))?;
        }
        Ok(())
    }
}

/// A module: wires naming its nets, cells connecting them.
///
/// Connectivity is by [`Net`]: two ports on the same net are connected.
/// Wires are names for nets (and constants), not the nets themselves: a
/// wire bit can be a constant, and several wires can name one net, which
/// is how yosys' JSON has it after `opt_clean`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub attributes: Attributes,
    /// A parametric module's parameters and their defaults.
    pub parameter_default_values: BTreeMap<String, ParamValue>,
    /// The ports are the wires with a direction, in port order.
    pub wires: Vec<Wire>,
    pub cells: Vec<Cell>,
    /// RTLIL memories not yet collected into a `$mem_v2`, which the
    /// `$memrd`/`$memwr`/`$meminit` cells refer to by name.
    pub memories: Vec<Memory>,
    /// Nets allocated: every `Net` in the module is below this.
    nets: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wire {
    pub name: String,
    pub bits: SigSpec,
    /// Set if the wire is a port of the module.
    pub port: Option<Dir>,
    pub signed: bool,
    /// The index of bit 0 (`[7:4]` has offset 4).
    pub offset: i64,
    /// Declared `[lo:hi]` rather than `[hi:lo]`.
    pub upto: bool,
    pub attributes: Attributes,
}

impl Wire {
    pub fn new(name: impl Into<String>, bits: SigSpec) -> Wire {
        Wire {
            name: name.into(),
            bits,
            port: None,
            signed: false,
            offset: 0,
            upto: false,
            attributes: Attributes::new(),
        }
    }
}

/// An RTLIL memory object (`reg [w-1:0] m [size]` before
/// `memory_collect`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub name: String,
    pub width: usize,
    pub start_offset: i64,
    pub size: usize,
    pub attributes: Attributes,
}

impl Module {
    pub fn new(name: impl Into<String>) -> Module {
        Module {
            name: name.into(),
            attributes: Attributes::new(),
            parameter_default_values: BTreeMap::new(),
            wires: Vec::new(),
            cells: Vec::new(),
            memories: Vec::new(),
            nets: 0,
        }
    }

    pub fn new_net(&mut self) -> Net {
        let n = Net(self.nets);
        self.nets += 1;
        n
    }

    /// `width` fresh nets.
    pub fn new_sig(&mut self, width: usize) -> SigSpec {
        (0..width).map(|_| SigBit::Net(self.new_net())).collect()
    }

    /// One more than the highest net: the size of a per-net table.
    pub fn net_count(&self) -> usize {
        self.nets as usize
    }

    pub fn is_blackbox(&self) -> bool {
        is_set(&self.attributes, attr::BLACKBOX)
    }

    pub fn ports(&self) -> impl Iterator<Item = (WireId, &Wire, Dir)> {
        self.wires
            .iter()
            .enumerate()
            .filter_map(|(i, w)| w.port.map(|d| (i, w, d)))
    }

    pub fn wire(&self, name: &str) -> Option<&Wire> {
        self.wires.iter().find(|w| w.name == name)
    }

    pub fn cell(&self, name: &str) -> Option<&Cell> {
        self.cells.iter().find(|c| c.name == name)
    }

    /// A cell name not in use, yosys-style (`$auto$<prefix>$<n>`), for
    /// cells a pass creates.
    pub fn fresh_name(&self, prefix: &str) -> String {
        let used: HashSet<&str> = self.cells.iter().map(|c| c.name.as_str()).collect();
        (self.cells.len()..)
            .map(|i| format!("$auto${prefix}${i}"))
            .find(|n| !used.contains(n.as_str()))
            .expect("some name is free")
    }

    pub fn add_cell(&mut self, name: impl Into<String>, kind: CellKind) -> CellId {
        self.cells.push(Cell {
            name: name.into(),
            kind,
            attributes: Attributes::new(),
        });
        self.cells.len() - 1
    }

    /// Rewrites every bit of every wire and cell, to merge or replace nets.
    pub fn map_bits(&mut self, mut f: impl FnMut(SigBit) -> SigBit) {
        for w in &mut self.wires {
            w.bits.iter_mut().for_each(|b| *b = f(*b));
        }
        for c in &mut self.cells {
            c.kind.for_each_bit_mut(|b| *b = f(*b));
        }
    }

    /// Wire and cell names unique, every net allocated, cells' port widths
    /// consistent.
    pub fn check(&self) -> Result<(), String> {
        let net_ok = |b: &SigBit| b.net().is_none_or(|n| n.0 < self.nets);
        let mut names = HashSet::new();
        for w in &self.wires {
            if !names.insert(&w.name) {
                return Err(format!("two wires named {}", w.name));
            }
            if !w.bits.iter().all(net_ok) {
                return Err(format!("wire {} uses an unallocated net", w.name));
            }
        }
        names.clear();
        for c in &self.cells {
            if !names.insert(&c.name) {
                return Err(format!("two cells named {}", c.name));
            }
            let mut ok = true;
            c.kind.for_each_port(|p| ok &= p.bits.iter().all(net_ok));
            if !ok {
                return Err(format!("cell {} uses an unallocated net", c.name));
            }
            c.kind
                .check()
                .map_err(|e| format!("cell {} ({}): {e}", c.name, c.kind.type_name()))?;
        }
        Ok(())
    }

    /// For the JSON reader, which numbers nets itself.
    pub(crate) fn set_net_count(&mut self, n: u32) {
        self.nets = n;
    }
}
