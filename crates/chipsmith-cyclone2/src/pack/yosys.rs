//! yosys's JSON netlist (`write_json`) into a `Netlist`, the way nextpnr's
//! generic frontend imports it: cells, ports, netnames and their
//! attributes in sorted order (its JSON objects are `std::map`s), net names
//! picked by `prefer_netlabel`, a GND/VCC cell for every constant bit and a
//! port cell for every top-level port bit. Doing it the same way makes
//! names, net ids and iteration order the same as nextpnr's.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, ensure, Context, Result};
use serde_json::{Map, Value};

use super::netlist::{Dir, FfConfig, IoeIn, Kind, NetId, Netlist, Port, TopPort};
use crate::m4k;
use crate::pin::{register_param, PowerUp};

/// A bit of a yosys bit vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bit {
    Signal(u64),
    Const(bool),
    /// `z`: a net with no driver.
    Floating,
    /// `x`: left unconnected.
    Undef,
}

fn bits(v: &Value) -> Result<Vec<Bit>> {
    v.as_array()
        .context("bits are not an array")?
        .iter()
        .map(|b| match b {
            Value::Number(n) => n.as_u64().map(Bit::Signal).context("bad bit"),
            Value::String(s) => match s.as_str() {
                "0" => Ok(Bit::Const(false)),
                "1" => Ok(Bit::Const(true)),
                "z" => Ok(Bit::Floating),
                "x" => Ok(Bit::Undef),
                other => bail!("bad bit {other:?}"),
            },
            other => bail!("bad bit {other}"),
        })
        .collect()
}

/// A yosys parameter or attribute as a number: a binary string or a JSON
/// number.
fn int(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => u64::from_str_radix(s.trim_end(), 2).ok(),
        _ => None,
    }
}

fn flag(attrs: &Value, name: &str) -> bool {
    attrs.get(name).and_then(int).is_some_and(|v| v != 0)
}

fn object(v: &Value) -> BTreeMap<String, Value> {
    v.as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

/// nextpnr's `get_bit_name`.
pub fn bit_name(base: &str, i: usize, width: usize, offset: i64, upto: bool) -> String {
    if width == 1 && offset == 0 {
        return base.to_string();
    }
    let real = if upto {
        offset + (width - i - 1) as i64
    } else {
        offset + i as i64
    };
    format!("{base}[{real}]")
}

fn offset_upto(v: &Value) -> (i64, bool) {
    (
        v.get("offset").and_then(Value::as_i64).unwrap_or(0),
        v.get("upto").and_then(Value::as_i64).unwrap_or(0) != 0,
    )
}

struct Frontend<'a> {
    nl: Netlist,
    /// Top-level port names (nextpnr's `port_to_bus`), preferred net names.
    port_names: HashSet<&'a str>,
    /// Every name of each yosys bit, in netname order.
    bit_names: HashMap<u64, Vec<String>>,
    net_of_bit: HashMap<u64, NetId>,
    const_autoidx: usize,
}

impl Frontend<'_> {
    /// nextpnr's `prefer_netlabel`: is `a` a better net name than `b`?
    /// Its port lookups intern both names, which the ids depend on.
    fn prefer(&mut self, a: &str, b: &str) -> bool {
        self.nl.ids.intern(a);
        if self.port_names.contains(a) {
            return true;
        }
        self.nl.ids.intern(b);
        if self.port_names.contains(b) {
            return false;
        }
        if b.is_empty() {
            return true;
        }
        let count = |s: &str, c: char| s.chars().filter(|&x| x == c).count();
        count(a, '$')
            .cmp(&count(b, '$'))
            .then(count(a, '.').cmp(&count(b, '.')))
            .then(a.cmp(b))
            .is_lt()
    }

    fn unique_net_name(&mut self, base: &str) -> String {
        let mut incr = 0;
        loop {
            let name = if incr == 0 {
                base.to_string()
            } else {
                format!("{base}__unique__{incr}")
            };
            self.nl.ids.intern(&name);
            if !self.nl.has_net_named(&name) {
                return name;
            }
            incr += 1;
        }
    }

    fn unique_cell_name(&mut self, base: &str) -> String {
        let mut incr = 0;
        loop {
            let name = if incr == 0 {
                base.to_string()
            } else {
                format!("{base}__unique__{incr}")
            };
            self.nl.ids.intern(&name);
            if !self.nl.has_cell_named(&name) {
                return name;
            }
            incr += 1;
        }
    }

    fn net_for_bit(&mut self, bit: u64) -> NetId {
        if let Some(&n) = self.net_of_bit.get(&bit) {
            return n;
        }
        let names = self.bit_names.get(&bit).cloned().unwrap_or_default();
        let base = match names.split_first() {
            Some((first, rest)) => {
                let mut best = first.clone();
                for n in rest {
                    if self.prefer(n, &best) {
                        best = n.clone();
                    }
                }
                best
            }
            None => format!("$frontend${bit}"),
        };
        let name = self.unique_net_name(&base);
        let net = self.nl.create_net(name.clone());
        if names.is_empty() {
            self.nl.net_aliases.insert(name);
        }
        for alias in names {
            self.nl.ids.intern(&alias);
            self.nl.net_aliases.insert(alias);
        }
        self.net_of_bit.insert(bit, net);
        net
    }

    /// A fresh net for a constant bit, driven by a GND/VCC cell unless the
    /// bit floats.
    fn constant_net(&mut self, hint: &str, value: Option<bool>) -> NetId {
        let name = self.unique_net_name(hint);
        let net = self.nl.create_net(name.clone());
        if let Some(v) = value {
            let base = format!(
                "{name}{}{}",
                if v { "$VCC$" } else { "$GND$" },
                self.const_autoidx
            );
            self.const_autoidx += 1;
            let cell_name = self.unique_cell_name(&base);
            let c = self.nl.create_cell(cell_name, Kind::Const(v));
            self.nl.ids.intern("Y");
            self.nl.add_port(c, Port::Y);
            self.nl.connect(c, Port::Y, net);
        }
        net
    }

    fn bit_net(&mut self, bit: Bit, hint: impl FnOnce() -> String) -> Option<NetId> {
        match bit {
            Bit::Signal(b) => Some(self.net_for_bit(b)),
            Bit::Const(v) => Some(self.constant_net(&hint(), Some(v))),
            Bit::Floating => Some(self.constant_net(&hint(), None)),
            Bit::Undef => None,
        }
    }
}

fn kind_of(ty: &str, params: &BTreeMap<String, Value>, name: &str) -> Result<Kind> {
    // A pin buffer's register: absent or "none" is none (the cell
    // library's default), else its power-up value.
    let register = |p: &str| -> Result<Option<PowerUp>> {
        match params.get(p).and_then(Value::as_str).map(str::trim_end) {
            None | Some("none") => Ok(None),
            Some(v) => PowerUp::from_param(v)
                .map(Some)
                .with_context(|| format!("{name}: {p} = {v:?}")),
        }
    };
    Ok(match ty {
        "LCELL_COMB" => {
            let mask = params
                .get("LUT_MASK")
                .and_then(int)
                .with_context(|| format!("{name}: LCELL_COMB without a LUT_MASK"))?;
            ensure!(mask <= 0xFFFF, "{name}: LUT_MASK is wider than 16 bits");
            Kind::Lut { mask: mask as u16 }
        }
        "LCELL_FF" => Kind::Ff(FfConfig {
            clk_inv: params.get("CLK_INV").and_then(int).unwrap_or(0) != 0,
            ..FfConfig::default()
        }),
        "CY2_IBUF" => Kind::Ibuf {
            port: None,
            reg: register(register_param::INPUT)?,
        },
        "CY2_OBUF" => Kind::Obuf {
            port: None,
            data: IoeIn::signal(register(register_param::OUTPUT)?),
        },
        "CY2_IOBUF" => Kind::Iobuf {
            port: None,
            data: IoeIn::signal(register(register_param::OUTPUT)?),
            oe: IoeIn::signal(register(register_param::OE)?),
            in_reg: register(register_param::INPUT)?,
        },
        "CY2_M4K" => {
            // The packer needs no more than the shape's ports: a cell from
            // the yosys flow is taken as using every column; the bitstream
            // reads which it uses off its pins.
            let shape = m4k::Shape::parse(
                |k| params.get(k).and_then(Value::as_str).map(str::to_owned),
                |k| params.get(k).and_then(int),
                |w| w.bits() as u8,
            );
            Kind::M4k(shape.with_context(|| format!("M4K {name}"))?)
        }
        "CY2_MULT" => {
            let get = |k: &str| {
                params
                    .get(k)
                    .and_then(int)
                    .with_context(|| format!("multiplier {name}: {k} missing"))
            };
            let shape = crate::mult::Shape::new(
                get("A_WIDTH")? as usize,
                get("B_WIDTH")? as usize,
                [get("A_SIGNED")? != 0, get("B_SIGNED")? != 0],
            );
            // Registers are optional: an instantiated CY2_MULT may leave
            // them out.
            let mut flags = [false; 3];
            for (f, k) in flags.iter_mut().zip(crate::mult::Regs::PARAMS) {
                *f = params.get(k).and_then(int).unwrap_or(0) != 0;
            }
            Kind::Mult(
                shape.with_context(|| format!("multiplier {name}"))?,
                crate::mult::Regs::from_flags(flags),
            )
        }
        other => match crate::hard::Block::from_cell_type(other) {
            Some(b) => Kind::Hard(b),
            None => bail!("{name}: cell type {other} is not a Cyclone II cell the packer knows"),
        },
    })
}

/// The parameters `Kind` holds; the rest pass through.
const TYPED_PARAMS: [&str; 7] = [
    "CLK_INV",
    "LUT_MASK",
    "MODE",
    "WIDTH",
    register_param::INPUT,
    register_param::OUTPUT,
    register_param::OE,
];

/// A multiplier's parameters, all in its `Kind`. Only a multiplier's: an
/// M4K's `B_WIDTH` passes through (its `Kind` writes back only `MODE` and
/// `WIDTH`).
const MULT_PARAMS: [&str; 7] = [
    "A_WIDTH", "B_WIDTH", "A_SIGNED", "B_SIGNED", "A_REG", "B_REG", "P_REG",
];

pub fn import(json: &Value) -> Result<Netlist> {
    let modules = json["modules"].as_object().context("no modules")?;
    let mut nl = Netlist::default();

    // find_top_module: module names, then every cell type.
    let mut boxes = HashSet::new();
    let mut instantiated = HashSet::new();
    let mut by_attr = Vec::new();
    for (name, m) in modules {
        nl.ids.intern(name);
        let attrs = &m["attributes"];
        if flag(attrs, "blackbox") || flag(attrs, "whitebox") {
            boxes.insert(name.as_str());
        } else if flag(attrs, "top") {
            by_attr.push(name.as_str());
        }
        for c in m["cells"].as_object().into_iter().flat_map(Map::values) {
            let ty = c["type"].as_str().context("cell without a type")?;
            nl.ids.intern(ty);
            instantiated.insert(ty);
        }
    }
    let top_name = match by_attr.as_slice() {
        [one] => *one,
        [] => {
            let candidates: Vec<&str> = modules
                .keys()
                .map(String::as_str)
                .filter(|m| !boxes.contains(m) && !instantiated.contains(m))
                .collect();
            match candidates.as_slice() {
                [one] => *one,
                _ => bail!("no single top module (candidates: {candidates:?})"),
            }
        }
        many => bail!("several modules marked top: {many:?}"),
    };
    let top = &modules[top_name];
    let ports = top["ports"].as_object().cloned().unwrap_or_default();

    let mut fe = Frontend {
        nl,
        port_names: ports.keys().map(String::as_str).collect(),
        bit_names: HashMap::new(),
        net_of_bit: HashMap::new(),
        const_autoidx: 0,
    };
    for p in ports.keys() {
        fe.nl.ids.intern(p);
    }
    fe.nl.module_attrs = object(&top["attributes"]);
    for a in fe.nl.module_attrs.keys() {
        fe.nl.ids.intern(a);
    }

    let empty = Map::new();
    let netnames = top["netnames"].as_object().unwrap_or(&empty);
    for (base, nn) in netnames {
        let b = bits(&nn["bits"]).with_context(|| format!("netname {base}"))?;
        let (offset, upto) = offset_upto(nn);
        for (i, bit) in b.iter().enumerate() {
            if let Bit::Signal(s) = bit {
                fe.bit_names
                    .entry(*s)
                    .or_default()
                    .push(bit_name(base, i, b.len(), offset, upto));
            }
        }
    }

    for (cname, cd) in top["cells"].as_object().unwrap_or(&empty) {
        let ty = cd["type"].as_str().context("cell without a type")?;
        if matches!(ty, "$scopeinfo" | "$print" | "$check") {
            continue;
        }
        if modules.contains_key(ty) && !boxes.contains(ty) {
            bail!("{cname}: hierarchical designs are not supported (flatten in synthesis)");
        }
        let mut params = object(&cd["parameters"]);
        let kind = kind_of(ty, &params, cname)?;
        let inst = fe.unique_cell_name(cname);
        let c = fe.nl.create_cell(inst.clone(), kind.clone());
        let dirs = cd["port_directions"].as_object().unwrap_or(&empty);
        for p in dirs.keys() {
            fe.nl.ids.intern(p);
        }
        for (pname, conn) in cd["connections"].as_object().unwrap_or(&empty) {
            let dir = dirs
                .get(pname)
                .and_then(Value::as_str)
                .and_then(Dir::parse)
                .with_context(|| format!("{inst}: port {pname} has no direction"))?;
            let output = dir == Dir::Out;
            let b = bits(conn).with_context(|| format!("{inst}.{pname}"))?;
            for (i, &bit) in b.iter().enumerate() {
                let bname = bit_name(pname, i, b.len(), 0, false);
                let port =
                    Port::parse(&bname).with_context(|| format!("{inst}: unknown port {bname}"))?;
                ensure!(
                    dir == kind.dir(port),
                    "{inst}: {ty}.{bname} has the wrong direction"
                );
                fe.nl.ids.intern(&bname);
                fe.nl.add_port(c, port);
                let Some(net) = fe.bit_net(bit, || format!("{inst}.{bname}$const")) else {
                    continue;
                };
                if output && fe.nl.net(net).driver.is_some() {
                    bail!("net {} has two drivers", fe.nl.net(net).name);
                }
                fe.nl.connect(c, port, net);
            }
        }
        let attrs = object(&cd["attributes"]);
        for k in attrs.keys().chain(params.keys()) {
            fe.nl.ids.intern(k);
        }
        let mult = ty == "CY2_MULT";
        params.retain(|k, _| {
            !TYPED_PARAMS.contains(&k.as_str()) && !(mult && MULT_PARAMS.contains(&k.as_str()))
        });
        let cell = fe.nl.cell_mut(c);
        cell.attrs = attrs;
        cell.params = params;
    }

    // import_net_attrs: later netnames overwrite earlier ones' attributes.
    for nn in netnames.values() {
        let attrs = object(&nn["attributes"]);
        for bit in bits(&nn["bits"])? {
            let Bit::Signal(s) = bit else { continue };
            let Some(&net) = fe.net_of_bit.get(&s) else {
                continue;
            };
            for (k, v) in &attrs {
                fe.nl.ids.intern(k);
                fe.nl.net_mut(net).attrs.insert(k.clone(), v.clone());
            }
        }
    }

    // import_toplevel_ports: inouts last, so that they split the nets
    // the other ports have already claimed.
    let mut dirs = Vec::new();
    for (pname, pd) in &ports {
        let dir = pd["direction"]
            .as_str()
            .and_then(Dir::parse)
            .with_context(|| format!("port {pname} has no direction"))?;
        dirs.push((pname, pd, dir));
    }
    dirs.sort_by_key(|&(_, _, dir)| dir == Dir::InOut);
    for (pname, pd, dir) in dirs {
        let b = bits(&pd["bits"]).with_context(|| format!("port {pname}"))?;
        let (offset, upto) = offset_upto(pd);
        let mut port_bits = Vec::new();
        for (i, &bit) in b.iter().enumerate() {
            let bname = bit_name(pname, i, b.len(), offset, upto);
            // Unlike a cell's, a port's `x` bit gets a (driverless) net.
            let bit = if bit == Bit::Undef {
                Bit::Floating
            } else {
                bit
            };
            let net = fe.bit_net(bit, || format!("{bname}$const"));
            port_bits.push(net);
            // create_iobuf
            let Some(net) = net else { continue };
            ensure!(
                !fe.nl.has_cell_named(&bname),
                "cell {bname} has a top-level port's name"
            );
            match dir {
                Dir::Out => {
                    let c = fe.nl.create_cell(bname, Kind::TopOut);
                    fe.nl.add_port(c, Port::I);
                    fe.nl.connect(c, Port::I, net);
                }
                Dir::In => {
                    ensure!(
                        fe.nl.net(net).driver.is_none(),
                        "top-level input {bname} drives a net something else drives"
                    );
                    let c = fe.nl.create_cell(bname, Kind::TopIn);
                    fe.nl.add_port(c, Port::O);
                    fe.nl.connect(c, Port::O, net);
                }
                Dir::InOut => {
                    // split_io: the net's driver moves to a net of its own
                    // that feeds I; O drives the port's net.
                    let c = fe.nl.create_cell(bname.clone(), Kind::TopInOut);
                    fe.nl.add_port(c, Port::I);
                    fe.nl.add_port(c, Port::O);
                    let name = fe.unique_net_name(&format!("${bname}$iobuf_i"));
                    let split = fe.nl.create_net(name);
                    if let Some((d, p)) = fe.nl.net(net).driver {
                        fe.nl.disconnect(d, p);
                        fe.nl.connect(d, p, split);
                    }
                    fe.nl.connect(c, Port::I, split);
                    fe.nl.connect(c, Port::O, net);
                }
            }
        }
        fe.nl.top_ports.push(TopPort {
            name: pname.clone(),
            dir,
            bits: port_bits,
        });
    }
    Ok(fe.nl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_names_follow_offset_and_direction() {
        assert_eq!(bit_name("clk", 0, 1, 0, false), "clk");
        assert_eq!(bit_name("LEDR", 2, 4, 0, false), "LEDR[2]");
        assert_eq!(bit_name("x", 0, 4, 0, true), "x[3]");
        assert_eq!(bit_name("y", 0, 1, 3, false), "y[3]");
    }
}
