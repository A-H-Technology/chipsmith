//! `flatten`: every instance of one of the design's own modules replaced
//! by that module's contents, as yosys' `flatten` does. The DE2 flow
//! synthesizes one flat module.
//!
//! The instance's wires and cells come in under `instance.name` (yosys'
//! `$flatten\instance.` for internal `$` names) with their attributes and
//! an `hdlname` giving the path. A port's nets become the connected nets;
//! where the module joins two ports (`assign b = a`) or ties an output to
//! a constant, the parent's nets are joined the same way, which with the
//! IR's canonical nets means rewriting one into the other. Blackboxes and
//! device cells stay cells, and the modules no longer used are removed.

use chipsmith_netlist::{CellKind, Design, Module, Net, ParamValue, SigBit, SigSpec, State};
use rustc_hash::FxHashMap;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlattenError {
    #[error("no top module (none has the `top` attribute)")]
    NoTop,
    #[error("module {0} instantiates itself")]
    Recursive(String),
    #[error("instance {instance} of {module}: {message}")]
    Connection {
        instance: String,
        module: String,
        message: String,
    },
}

pub fn flatten(design: &mut Design) -> Result<(), FlattenError> {
    let top = design.top().ok_or(FlattenError::NoTop)?.name.clone();
    let mut done: FxHashMap<String, Module> = FxHashMap::default();
    let flat = flat_module(design, &top, &mut done, &mut Vec::new())?;
    design.modules.retain(|m| m.is_blackbox());
    design.modules.insert(0, flat);
    Ok(())
}

/// The module with every instance below it inlined (memoized).
fn flat_module(
    design: &Design,
    name: &str,
    done: &mut FxHashMap<String, Module>,
    stack: &mut Vec<String>,
) -> Result<Module, FlattenError> {
    if let Some(m) = done.get(name) {
        return Ok(m.clone());
    }
    if stack.iter().any(|s| s == name) {
        return Err(FlattenError::Recursive(name.to_owned()));
    }
    stack.push(name.to_owned());
    let mut m = design.module(name).expect("instances name modules").clone();
    let instances: Vec<usize> = m
        .cells
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match &c.kind {
            CellKind::Other(g) => design
                .module(&g.ty)
                .filter(|sub| !sub.is_blackbox())
                .map(|_| i),
            _ => None,
        })
        .collect();
    if !instances.is_empty() {
        let mut joins = Joins::new(&m);
        let mut new_cells = Vec::new();
        let mut new_wires = Vec::new();
        for &i in &instances {
            let cell = m.cells[i].clone();
            let CellKind::Other(g) = &cell.kind else {
                unreachable!("instances are generic cells")
            };
            let sub = flat_module(design, &g.ty, done, stack)?;
            let err = |message: String| FlattenError::Connection {
                instance: cell.name.clone(),
                module: g.ty.clone(),
                message,
            };
            // Each of the submodule's nets, as a bit of this module.
            let mut map: Vec<Option<SigBit>> = vec![None; sub.net_count()];
            for (_, w, dir) in sub.ports() {
                let conn = g.port(&w.name);
                if let Some(c) = conn {
                    if c.sig.len() != w.bits.len() {
                        return Err(err(format!(
                            "port {} is {} bits, connected to {}",
                            w.name,
                            w.bits.len(),
                            c.sig.len()
                        )));
                    }
                }
                for (k, &inner) in w.bits.iter().enumerate() {
                    let outer = conn.map(|c| c.sig[k]);
                    match inner {
                        SigBit::Net(n) => match (map[n.index()], outer) {
                            (None, Some(o)) => map[n.index()] = Some(o),
                            (None, None) => {}
                            (Some(prev), Some(o)) => joins.join(prev, o),
                            (Some(_), None) => {}
                        },
                        // An output tied to a constant inside drives the
                        // connected net with it.
                        SigBit::Const(s) => {
                            if let (Some(o), true) = (outer, dir.drives()) {
                                joins.join(o, SigBit::Const(s));
                            }
                        }
                    }
                }
            }
            let mut bit = |b: SigBit, m: &mut Module| -> SigBit {
                match b {
                    SigBit::Const(_) => b,
                    SigBit::Net(n) => *map[n.index()].get_or_insert_with(|| {
                        let net = m.new_net();
                        joins.grow(net);
                        SigBit::Net(net)
                    }),
                }
            };
            let path = |name: &str| -> String {
                match name.strip_prefix('$') {
                    Some(_) => format!("$flatten\\{}.{}", cell.name, name),
                    None => format!("{}.{}", cell.name, name),
                }
            };
            let hdlname = |name: &str, attrs: &chipsmith_netlist::Attributes| -> Option<ParamValue> {
                if name.starts_with('$') {
                    return None;
                }
                let inner = match attrs.get("hdlname") {
                    Some(ParamValue::Str(h)) => h.clone(),
                    _ => name.to_owned(),
                };
                Some(ParamValue::Str(format!("{} {inner}", cell.name)))
            };
            for w in &sub.wires {
                let bits: SigSpec = w.bits.iter().map(|&b| bit(b, &mut m)).collect();
                let mut nw = w.clone();
                nw.name = path(&w.name);
                nw.bits = bits;
                nw.port = None;
                if let Some(h) = hdlname(&w.name, &w.attributes) {
                    nw.attributes.insert("hdlname".into(), h);
                }
                new_wires.push(nw);
            }
            for c in &sub.cells {
                let mut nc = c.clone();
                nc.kind.for_each_bit_mut(|b| *b = bit(*b, &mut m));
                // Generic ports with only constants still get mapped
                // above; the typed visitor skips constant-only pieces,
                // which never hold nets.
                nc.name = path(&c.name);
                if let Some(h) = hdlname(&c.name, &c.attributes) {
                    nc.attributes.insert("hdlname".into(), h);
                }
                new_cells.push(nc);
            }
        }
        let mut keep = vec![true; m.cells.len()];
        for &i in &instances {
            keep[i] = false;
        }
        let mut k = 0;
        m.cells.retain(|_| {
            k += 1;
            keep[k - 1]
        });
        m.cells.extend(new_cells);
        m.wires.extend(new_wires);
        joins.apply(&mut m);
    }
    stack.pop();
    done.insert(name.to_owned(), m.clone());
    Ok(m)
}

/// Nets of the module being flattened that must become one (or a
/// constant): a union-find over net indices.
struct Joins {
    parent: Vec<u32>,
    konst: Vec<Option<State>>,
    nets: Vec<Option<Net>>,
    any: bool,
}

impl Joins {
    fn new(m: &Module) -> Joins {
        let n = m.net_count();
        let mut nets = vec![None; n];
        let mut note = |b: &SigBit| {
            if let SigBit::Net(net) = b {
                nets[net.index()] = Some(*net);
            }
        };
        m.wires.iter().flat_map(|w| &w.bits).for_each(&mut note);
        for c in &m.cells {
            c.kind.for_each_port(|p| p.bits.iter().for_each(&mut note));
        }
        Joins {
            parent: (0..n as u32).collect(),
            konst: vec![None; n],
            nets,
            any: false,
        }
    }

    fn grow(&mut self, net: Net) {
        let i = net.index();
        if self.parent.len() <= i {
            let old = self.parent.len();
            self.parent.extend(old as u32..=i as u32);
            self.konst.resize(i + 1, None);
            self.nets.resize(i + 1, None);
        }
        self.nets[i] = Some(net);
    }

    fn find(&mut self, mut i: u32) -> u32 {
        while self.parent[i as usize] != i {
            let p = self.parent[i as usize];
            self.parent[i as usize] = self.parent[p as usize];
            i = p;
        }
        i
    }

    fn join(&mut self, a: SigBit, b: SigBit) {
        self.any = true;
        for s in [a, b] {
            if let SigBit::Net(n) = s {
                if n.index() >= self.parent.len() || self.nets[n.index()].is_none() {
                    self.grow(n);
                }
            }
        }
        match (a, b) {
            (SigBit::Net(x), SigBit::Net(y)) => {
                let (rx, ry) = (self.find(x.index() as u32), self.find(y.index() as u32));
                if rx != ry {
                    self.parent[rx as usize] = ry;
                    if let Some(s) = self.konst[rx as usize] {
                        self.konst[ry as usize] = Some(s);
                    }
                }
            }
            (SigBit::Net(x), SigBit::Const(s)) | (SigBit::Const(s), SigBit::Net(x)) => {
                let r = self.find(x.index() as u32);
                self.konst[r as usize] = Some(s);
            }
            (SigBit::Const(_), SigBit::Const(_)) => {}
        }
    }

    fn apply(mut self, m: &mut Module) {
        if !self.any {
            return;
        }
        let n = self.parent.len();
        m.map_bits(|b| match b {
            SigBit::Net(net) if net.index() < n => {
                let r = self.find(net.index() as u32) as usize;
                match self.konst[r] {
                    Some(s) => SigBit::Const(s),
                    None => SigBit::Net(self.nets[r].unwrap_or(net)),
                }
            }
            other => other,
        });
    }
}
