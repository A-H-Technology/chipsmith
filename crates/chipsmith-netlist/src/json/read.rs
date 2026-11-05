//! yosys' JSON netlist (`write_json`) into the IR.

use std::fmt;
use std::marker::PhantomData;

use rustc_hash::FxHashMap;
use serde::de::{self, Deserialize, Deserializer, MapAccess, Visitor};

use crate::cell::{Cell, CellKind};
use crate::module::{Design, Memory, Module, Wire};
use crate::param::{Attributes, ParamValue};
use crate::ports::{Dir, Generic, GenericPort};
use crate::sig::{Const, Net, SigBit, SigSpec, State};

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("not a yosys JSON netlist: {0}")]
    Json(#[from] serde_json::Error),
    #[error("module {module}: {message}")]
    Invalid { module: String, message: String },
}

/// Parses yosys' `write_json` output.
pub fn read_json(text: &[u8]) -> Result<Design, ReadError> {
    let j: JDesign = serde_json::from_slice(text)?;
    let modules = j
        .modules
        .0
        .into_iter()
        .map(|(name, m)| {
            module(&name, m).map_err(|message| ReadError::Invalid {
                module: name,
                message,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(Design { modules })
}

/// A JSON object, keeping its order (and, unlike a map, duplicate keys,
/// which [`Module::check`] then rejects).
struct Ordered<T>(Vec<(String, T)>);

impl<T> Default for Ordered<T> {
    fn default() -> Self {
        Ordered(Vec::new())
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Ordered<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Ordered<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut v = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some(e) = map.next_entry()? {
                    v.push(e);
                }
                Ok(Ordered(v))
            }
        }
        d.deserialize_map(V(PhantomData))
    }
}

/// A bit in a `bits` or `connections` list: a net id or a constant.
#[derive(Clone, Copy)]
enum JBit {
    Id(u64),
    Const(State),
}

impl<'de> Deserialize<'de> for JBit {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = JBit;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(r#"a bit id or "0", "1", "x", "z""#)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<JBit, E> {
                Ok(JBit::Id(v))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<JBit, E> {
                let mut c = v.chars();
                match (c.next().and_then(State::parse), c.next()) {
                    (Some(s), None) => Ok(JBit::Const(s)),
                    _ => Err(E::invalid_value(de::Unexpected::Str(v), &self)),
                }
            }
        }
        d.deserialize_any(V)
    }
}

/// yosys' parameter and attribute encoding: a string of only `01xz` is
/// bits, most significant first; any other string is a string, with one
/// trailing space added by the writer if it would otherwise look like bits
/// (`"1 "` is the string `"1"`). A number is a 32-bit integer.
struct JParam(ParamValue);

impl<'de> Deserialize<'de> for JParam {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = JParam;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or an integer")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<JParam, E> {
                Ok(JParam(ParamValue::Bits(Const::from_int(v as i64, 32))))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<JParam, E> {
                Ok(JParam(ParamValue::Bits(Const::from_int(v, 32))))
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<JParam, E> {
                Ok(JParam(parse_param(s)))
            }
        }
        d.deserialize_any(V)
    }
}

pub(super) fn parse_param(s: &str) -> ParamValue {
    match s.find(|c| !matches!(c, '0' | '1' | 'x' | 'z')) {
        None => ParamValue::Bits(Const::parse(s).expect("only 01xz")),
        Some(i) if s[i..].bytes().all(|b| b == b' ') => {
            ParamValue::Str(s[..s.len() - 1].to_owned())
        }
        Some(_) => ParamValue::Str(s.to_owned()),
    }
}

#[derive(serde::Deserialize)]
struct JDesign {
    #[serde(default)]
    modules: Ordered<JModule>,
}

#[derive(serde::Deserialize)]
struct JModule {
    #[serde(default)]
    attributes: Ordered<JParam>,
    #[serde(default)]
    parameter_default_values: Ordered<JParam>,
    #[serde(default)]
    ports: Ordered<JPort>,
    #[serde(default)]
    cells: Ordered<JCell>,
    #[serde(default)]
    memories: Ordered<JMemory>,
    #[serde(default)]
    netnames: Ordered<JNet>,
}

#[derive(serde::Deserialize)]
struct JPort {
    direction: String,
    bits: Vec<JBit>,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    upto: u8,
    #[serde(default)]
    signed: u8,
}

#[derive(serde::Deserialize)]
struct JCell {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    parameters: Ordered<JParam>,
    #[serde(default)]
    attributes: Ordered<JParam>,
    #[serde(default)]
    port_directions: Ordered<String>,
    #[serde(default)]
    connections: Ordered<Vec<JBit>>,
}

#[derive(serde::Deserialize)]
struct JNet {
    bits: Vec<JBit>,
    #[serde(default)]
    attributes: Ordered<JParam>,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    upto: u8,
    #[serde(default)]
    signed: u8,
}

#[derive(serde::Deserialize)]
struct JMemory {
    #[serde(default)]
    attributes: Ordered<JParam>,
    width: usize,
    #[serde(default)]
    start_offset: i64,
    size: usize,
}

fn attributes(a: Ordered<JParam>) -> Attributes {
    a.0.into_iter().map(|(k, v)| (k, v.0)).collect()
}

/// JSON bit ids to nets, numbered as first seen.
#[derive(Default)]
struct Nets(FxHashMap<u64, Net>);

impl Nets {
    fn sig(&mut self, bits: &[JBit]) -> SigSpec {
        bits.iter()
            .map(|b| match *b {
                JBit::Const(s) => SigBit::Const(s),
                JBit::Id(id) => {
                    let next = Net(self.0.len() as u32);
                    SigBit::Net(*self.0.entry(id).or_insert(next))
                }
            })
            .collect()
    }
}

fn module(name: &str, j: JModule) -> Result<Module, String> {
    let mut m = Module::new(name);
    m.attributes = attributes(j.attributes);
    m.parameter_default_values = attributes(j.parameter_default_values);
    let mut nets = Nets::default();

    // Ports first, so they're the first wires, in port order.
    let mut netnames: FxHashMap<String, JNet> = FxHashMap::default();
    let mut netname_order = Vec::with_capacity(j.netnames.0.len());
    for (n, net) in j.netnames.0 {
        netname_order.push(n.clone());
        if netnames.insert(n.clone(), net).is_some() {
            return Err(format!("two netnames {n}"));
        }
    }
    for (pname, p) in j.ports.0 {
        let dir = Dir::parse(&p.direction)
            .ok_or_else(|| format!("port {pname}: direction {:?}", p.direction))?;
        let bits = nets.sig(&p.bits);
        let mut wire = Wire::new(pname, bits);
        wire.port = Some(dir);
        match netnames.remove(&wire.name) {
            Some(n) => {
                if nets.sig(&n.bits) != wire.bits {
                    return Err(format!("port {} and its netname differ", wire.name));
                }
                wire.attributes = attributes(n.attributes);
                wire.offset = n.offset;
                wire.upto = n.upto != 0;
                wire.signed = n.signed != 0;
            }
            None => {
                wire.offset = p.offset;
                wire.upto = p.upto != 0;
                wire.signed = p.signed != 0;
            }
        }
        m.wires.push(wire);
    }

    for (cname, c) in j.cells.0 {
        let mut dirs: FxHashMap<String, Dir> = FxHashMap::default();
        for (port, d) in c.port_directions.0 {
            let d = Dir::parse(&d).ok_or_else(|| format!("cell {cname}: direction {d:?}"))?;
            dirs.insert(port, d);
        }
        let generic = Generic {
            ty: c.ty,
            params: attributes(c.parameters),
            ports: c
                .connections
                .0
                .into_iter()
                .map(|(port, bits)| GenericPort {
                    dir: dirs.get(&port).copied(),
                    name: port,
                    sig: nets.sig(&bits),
                })
                .collect(),
        };
        let kind = CellKind::from_generic(generic).map_err(|e| format!("cell {cname}: {e}"))?;
        m.cells.push(Cell {
            name: cname,
            kind,
            attributes: attributes(c.attributes),
        });
    }

    for (mname, mem) in j.memories.0 {
        m.memories.push(Memory {
            name: mname,
            width: mem.width,
            start_offset: mem.start_offset,
            size: mem.size,
            attributes: attributes(mem.attributes),
        });
    }

    for n in netname_order {
        let Some(net) = netnames.remove(&n) else {
            continue; // a port's, already a wire
        };
        let mut wire = Wire::new(n, nets.sig(&net.bits));
        wire.attributes = attributes(net.attributes);
        wire.offset = net.offset;
        wire.upto = net.upto != 0;
        wire.signed = net.signed != 0;
        m.wires.push(wire);
    }

    m.set_net_count(nets.0.len() as u32);
    renumber(&mut m);
    Ok(m)
}

/// Numbers nets in the order the IR visits them (port wires, cells' ports
/// in their typed order, other wires), which is also the order the writer
/// emits them in: so reading what was written gives back the same nets,
/// whatever order the original file listed connections in.
fn renumber(m: &mut Module) {
    const UNSEEN: u32 = u32::MAX;
    let mut map = vec![UNSEEN; m.net_count()];
    let mut next = 0u32;
    let mut see = |b: &SigBit| {
        if let SigBit::Net(n) = b {
            if map[n.index()] == UNSEEN {
                map[n.index()] = next;
                next += 1;
            }
        }
    };
    m.wires
        .iter()
        .filter(|w| w.port.is_some())
        .for_each(|w| w.bits.iter().for_each(&mut see));
    for c in &m.cells {
        c.kind.for_each_port(|p| p.bits.iter().for_each(&mut see));
    }
    m.wires
        .iter()
        .for_each(|w| w.bits.iter().for_each(&mut see));
    m.map_bits(|b| match b {
        SigBit::Net(n) => SigBit::Net(Net(map[n.index()])),
        c => c,
    });
    // Nets only an asynchronous memory port's dropped clock used are gone.
    m.set_net_count(next);
}
