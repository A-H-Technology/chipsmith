//! A Zybo build as Vivado input, so Vivado's timer can time chipsmith's
//! own placement and routing (the 7-series counterpart of the DE2's
//! `cram quartus-export`):
//!
//! - `top.v`: the decoded bitstream (`crate::decode::decode_routed`) as a
//!   netlist of unisim primitives, each LUT reduced to the pins it reads
//!   and every cell nothing reads dropped;
//! - `place.xdc`: each cell's site and bel, each LUT's pins locked to the
//!   site pins it was routed to, the ports' package pins;
//! - `route.tcl`: each net's route as the wires it runs through, from its
//!   site output pin to every site input it reaches, a branch in `( )`,
//!   which the importer's `rt` turns into Vivado nodes and sets as the
//!   net's `FIXED_ROUTE`.
//!
//! Working from the bitstream, not from the placer's state, times what was
//! built: the routing as the pips are set, the cells as the bits configure
//! them. Not exported: BSCANs (the decoder cuts them into ports) and the
//! inverters the decoder adds in front of a DSP's or a tristate's inverted
//! inputs, which Vivado places and routes itself.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use anyhow::{Context, Result};
use chipsmith_netlist::{CellKind, Dir, Generic, Module, ParamValue, SigBit, State};
use chipsmith_route::chipdb::{ChipDb, NodeId};

use crate::decode::Routed;

/// What Vivado reads.
pub struct Export {
    pub verilog: String,
    pub xdc: String,
    pub route: String,
    /// Each placed cell's Vivado name, site and bel (`SITE` for a site's
    /// one bel), tab-separated.
    pub cells: String,
    /// Nets with a route to import, and nets left for Vivado's router
    /// (constants, and the ones whose site pins aren't the netlist's).
    pub routed: usize,
}

/// A LUT's table over its first `pins.len()` inputs; input `i` is on
/// physical pin `pins[i]` (1 to 6).
struct Lut {
    init: u64,
    pins: Vec<u8>,
}

/// `INIT` restricted to the inputs that aren't constant, which keep their
/// order.
fn reduce(init: u64, inputs: &[SigBit]) -> (Lut, Vec<SigBit>) {
    let n = inputs.len();
    let kept: Vec<usize> = (0..n)
        .filter(|&i| matches!(inputs[i], SigBit::Net(_)))
        .collect();
    let fixed: usize = (0..n)
        .filter(|&i| inputs[i] == SigBit::ONE)
        .map(|i| 1 << i)
        .sum();
    let mut out = 0u64;
    for a in 0..1usize << kept.len() {
        let full = kept
            .iter()
            .enumerate()
            .fold(fixed, |f, (j, &i)| f | ((a >> j) & 1) << i);
        out |= (init >> full & 1) << a;
    }
    let lut = Lut {
        init: out,
        pins: kept.iter().map(|&i| i as u8 + 1).collect(),
    };
    (lut, kept.iter().map(|&i| inputs[i]).collect())
}

fn param_u64(g: &Generic, name: &str) -> u64 {
    match g.params.get(name) {
        Some(ParamValue::Bits(c)) => {
            c.0.iter()
                .rev()
                .fold(0, |v, s| v << 1 | u64::from(*s == State::One))
        }
        _ => 0,
    }
}

fn is_output(g: &Generic, port: &str) -> bool {
    match g.ports.iter().find(|p| p.name == port).and_then(|p| p.dir) {
        Some(d) => d == Dir::Out,
        None => false,
    }
}

/// Where a decoded cell goes: its site and, inside a slice, its bel.
fn location(
    db: &ChipDb,
    slice_sites: &BTreeMap<String, String>,
    name: &str,
    ty: &str,
) -> Option<(String, Option<String>)> {
    if let Some(site) = name.strip_prefix("bufg_") {
        return Some((site.to_string(), None));
    }
    if let Some(site) = name
        .strip_suffix(".dsp")
        .or_else(|| name.strip_suffix(".cmt"))
    {
        return Some((site.to_string(), None));
    }
    if let Some((tile, h)) = name.split_once(".ramb18_y") {
        let tile = db.tile_names.iter().position(|t| t == tile)? as u32;
        let site = db
            .sites
            .iter()
            .find(|(_, s)| s.tile == tile && s.relative == format!("RAMB18_X0Y{h}"))?
            .0
            .clone();
        return Some((site, None));
    }
    let (key, what) = name.rsplit_once('.')?;
    let site = slice_sites.get(key)?.clone();
    let letter = what.chars().next()?;
    let bel = match &what[1..] {
        "6" => format!("{letter}6LUT"),
        "5" => format!("{letter}5LUT"),
        "ff" => format!("{letter}FF"),
        "5ff" => format!("{letter}5FF"),
        _ if what == "carry" => "CARRY4".into(),
        _ if what == "ram" && ty == "RAM32M" => return Some((site, None)),
        _ => return None,
    };
    Some((site, Some(bel)))
}

fn is_cmt(ty: &str) -> bool {
    crate::cmt::Prim::from_cell(ty).is_some()
}

/// A Vivado-safe cell name: the decoder's, dots made underscores.
fn cell_name(name: &str) -> String {
    name.replace(['.', '/', '[', ']'], "_")
}

/// `routed`'s netlist, placement and routing for Vivado; `period_ns` is
/// the clock constraint on every clock pin.
pub fn export(
    db: &ChipDb,
    routed: &Routed,
    pins: &BTreeMap<String, String>,
    period_ns: f64,
) -> Result<Export> {
    let m: &Module = routed.design.modules.first().context("an empty design")?;

    // Cells as (name, type, params, ports), LUTs reduced.
    struct C {
        name: String,
        g: Generic,
        lut: Option<Lut>,
    }
    let mut cells: Vec<C> = Vec::new();
    // Nets a reduced-away LUT made constant.
    let mut constant: HashMap<SigBit, SigBit> = HashMap::new();
    for c in &m.cells {
        let CellKind::Other(g) = &c.kind else {
            anyhow::bail!("{}: not a device cell", c.name);
        };
        let mut g = g.clone();
        let mut lut = None;
        if crate::synth::cells::LUTS.contains(&g.ty.as_str()) {
            let n = g.ports.iter().filter(|p| p.name.starts_with('I')).count();
            let inputs: Vec<SigBit> = (0..n)
                .map(|i| {
                    g.ports
                        .iter()
                        .find(|p| p.name == format!("I{i}"))
                        .map_or(SigBit::ONE, |p| p.sig[0])
                })
                .collect();
            let o = g
                .ports
                .iter()
                .find(|p| p.name == "O")
                .context("a LUT without O")?
                .sig[0];
            let (l, ins) = reduce(param_u64(&g, "INIT"), &inputs);
            if ins.is_empty() {
                constant.insert(o, SigBit::Const(State::from(l.init & 1 == 1)));
                continue;
            }
            let CellKind::Other(r) = crate::synth::cells::lut(&ins, l.init, o) else {
                unreachable!("a LUT is a device cell");
            };
            g = r;
            // The decoder's own inverters have no site: their inputs stay
            // where the decoder found them, not on a slice's pins.
            if location(db, &routed.slice_sites, &c.name, &g.ty).is_some() {
                lut = Some(l);
            }
        }
        cells.push(C {
            name: c.name.clone(),
            g,
            lut,
        });
    }
    for c in &mut cells {
        for p in &mut c.g.ports {
            for b in p.sig.iter_mut() {
                if let Some(&k) = constant.get(b) {
                    *b = k;
                }
            }
        }
    }
    // A DSP column's cascade wires are always connected, so the decoder
    // reads each block's PCIN, ACIN, ... as the PCOUT, ACOUT, ... below it,
    // and Vivado's timer times those arcs whatever OPMODE selects: what
    // OPMODE and A_/B_INPUT leave unread goes to 0. chipsmith builds no
    // carry or sign cascade.
    for c in cells.iter_mut().filter(|c| c.g.ty == "DSP48E1") {
        let opmode: Option<u64> = c.g.ports.iter().find(|p| p.name == "OPMODE").and_then(|p| {
            p.sig
                .iter()
                .enumerate()
                .try_fold(0u64, |v, (k, b)| match b {
                    SigBit::Const(s) => Some(v | u64::from(*s == State::One) << k),
                    _ => None,
                })
        });
        let reads_pcin = opmode.is_none_or(|o| matches!(o >> 4 & 7, 1 | 5));
        let cascade =
            |name: &str| matches!(c.g.params.get(name), Some(ParamValue::Str(v)) if v == "CASCADE");
        let (acin, bcin) = (cascade("A_INPUT"), cascade("B_INPUT"));
        for p in c.g.ports.iter_mut() {
            let unread = match p.name.as_str() {
                "PCIN" => !reads_pcin,
                "ACIN" => !acin,
                "BCIN" => !bcin,
                "CARRYCASCIN" | "MULTSIGNIN" => true,
                _ => false,
            };
            if unread {
                p.sig.iter_mut().for_each(|b| *b = SigBit::ZERO);
            }
        }
    }

    // Drop what nothing reads, until nothing changes.
    let port_bits: HashSet<SigBit> = m
        .wires
        .iter()
        .filter(|w| w.port == Some(Dir::Out))
        .flat_map(|w| w.bits.iter().copied())
        .collect();
    loop {
        let mut read: HashSet<SigBit> = port_bits.clone();
        for c in &cells {
            for p in &c.g.ports {
                if !is_output(&c.g, &p.name) {
                    read.extend(p.sig.iter().copied());
                }
            }
        }
        let before = cells.len();
        cells.retain(|c| {
            matches!(c.g.ty.as_str(), "OBUF" | "OBUFT")
                || c.g
                    .ports
                    .iter()
                    .filter(|p| is_output(&c.g, &p.name))
                    .any(|p| p.sig.iter().any(|b| read.contains(b)))
        });
        if cells.len() == before {
            break;
        }
    }

    // Net names: a port's bit where it is one, else `n<index>`.
    let mut net_name: HashMap<SigBit, String> = HashMap::new();
    for w in m.wires.iter().filter(|w| w.port.is_some()) {
        for (i, b) in w.bits.iter().enumerate() {
            let name = if w.bits.len() == 1 {
                w.name.clone()
            } else {
                format!("{}[{i}]", w.name)
            };
            net_name.entry(*b).or_insert(name);
        }
    }
    let name_of = |b: &SigBit| -> String {
        match b {
            SigBit::Const(State::One) => "1'b1".into(),
            SigBit::Const(_) => "1'b0".into(),
            SigBit::Net(n) => net_name
                .get(b)
                .cloned()
                .unwrap_or_else(|| format!("n{}", n.index())),
        }
    };

    let mut v = String::new();
    let ports: Vec<String> = m
        .wires
        .iter()
        .filter(|w| w.port.is_some())
        .map(|w| w.name.clone())
        .collect();
    writeln!(v, "module top({});", ports.join(", "))?;
    for w in m.wires.iter().filter(|w| w.port.is_some()) {
        let dir = if w.port == Some(Dir::In) {
            "input"
        } else {
            "output"
        };
        let range = if w.bits.len() > 1 {
            format!("[{}:0] ", w.bits.len() - 1)
        } else {
            String::new()
        };
        writeln!(v, "  {dir} wire {range}{};", w.name)?;
    }
    let mut declared: HashSet<String> = ports.iter().cloned().collect();
    for c in &cells {
        for p in &c.g.ports {
            for b in &p.sig {
                if let SigBit::Net(n) = b {
                    if !net_name.contains_key(b) && declared.insert(format!("n{}", n.index())) {
                        writeln!(v, "  wire n{};", n.index())?;
                    }
                }
            }
        }
    }
    for c in &cells {
        // A decoded block's input period isn't in its bits: the clock pins'.
        let period = is_cmt(&c.g.ty).then(|| format!(".CLKIN1_PERIOD({period_ns:.3})"));
        let params: Vec<String> =
            c.g.params
                .iter()
                .map(|(k, p)| match p {
                    ParamValue::Bits(b) => {
                        let bits: String =
                            b.0.iter()
                                .rev()
                                .map(|s| if *s == State::One { '1' } else { '0' })
                                .collect();
                        format!(".{k}({}'b{bits})", b.0.len())
                    }
                    // A PLL's or MMCM's numbers are reals to Vivado.
                    ParamValue::Str(s) if is_cmt(&c.g.ty) && s.parse::<f64>().is_ok() => {
                        format!(".{k}({s})")
                    }
                    ParamValue::Str(s) => format!(".{k}(\"{s}\")"),
                })
                .chain(period)
                .collect();
        let conns: Vec<String> =
            c.g.ports
                .iter()
                .map(|p| {
                    let bits: Vec<String> = p.sig.iter().rev().map(&name_of).collect();
                    if bits.len() == 1 {
                        format!(".{}({})", p.name, bits[0])
                    } else {
                        format!(".{}({{{}}})", p.name, bits.join(", "))
                    }
                })
                .collect();
        writeln!(
            v,
            "  (* DONT_TOUCH = \"yes\" *) {} #({}) {} ({});",
            c.g.ty,
            params.join(", "),
            cell_name(&c.name),
            conns.join(", ")
        )?;
    }
    writeln!(v, "endmodule")?;

    // Placement.
    let mut xdc = String::new();
    let mut listing = String::new();
    for (port, pin) in pins {
        writeln!(
            xdc,
            "set_property -dict {{PACKAGE_PIN {pin} IOSTANDARD LVCMOS33}} [get_ports {{{port}}}]"
        )?;
    }
    for c in &cells {
        let Some((site, bel)) = location(db, &routed.slice_sites, &c.name, &c.g.ty) else {
            continue;
        };
        let name = cell_name(&c.name);
        writeln!(
            listing,
            "{name}\t{site}\t{}",
            bel.as_deref().unwrap_or("SITE")
        )?;
        if let Some(l) = &c.lut {
            let locks: Vec<String> = l
                .pins
                .iter()
                .enumerate()
                .map(|(i, p)| format!("I{i}:A{p}"))
                .collect();
            writeln!(
                xdc,
                "set_property LOCK_PINS {{{}}} [get_cells {{{name}}}]",
                locks.join(" ")
            )?;
        }
        if let Some(bel) = bel {
            writeln!(xdc, "set_property BEL {bel} [get_cells {{{name}}}]")?;
        }
        writeln!(xdc, "set_property LOC {site} [get_cells {{{name}}}]")?;
    }
    // Every BUFG's input from a port is a clock.
    let driver_port: HashMap<SigBit, String> = cells
        .iter()
        .filter(|c| c.g.ty == "IBUF")
        .filter_map(|c| {
            let i = c.g.ports.iter().find(|p| p.name == "I")?.sig[0];
            let o = c.g.ports.iter().find(|p| p.name == "O")?.sig[0];
            Some((o, name_of(&i)))
        })
        .collect();
    let port_of = |c: &C, name: &str| c.g.ports.iter().find(|p| p.name == name).map(|p| p.sig[0]);
    // A PLL's or MMCM's CLKIN1 from a port: that port is a clock too, and
    // Vivado derives the outputs' clocks from it.
    let mut clocked = HashSet::new();
    for c in cells.iter().filter(|c| is_cmt(&c.g.ty)) {
        let i = port_of(c, "CLKIN1").context("CLKIN1")?;
        if let Some(port) = driver_port.get(&i) {
            if clocked.insert(port.clone()) {
                writeln!(
                    xdc,
                    "create_clock -period {period_ns:.3} -name {port} [get_ports {{{port}}}]"
                )?;
            }
        }
    }
    let mut generated = Vec::new();
    for c in cells.iter().filter(|c| c.g.ty == "BUFG") {
        let i = port_of(c, "I").context("a BUFG without I")?;
        if let Some(port) = driver_port.get(&i) {
            if clocked.insert(port.clone()) {
                writeln!(
                    xdc,
                    "create_clock -period {period_ns:.3} -name {port} [get_ports {{{port}}}]"
                )?;
            }
            continue;
        }
        // A register toggling (D its own Q through a LUT1 inverter) makes a
        // clock of half its clock's frequency.
        let Some(ff) = cells
            .iter()
            .find(|f| f.g.ty.starts_with("FD") && port_of(f, "Q") == Some(i))
        else {
            continue;
        };
        let toggles = cells.iter().any(|l| {
            l.g.ty == "LUT1"
                && port_of(l, "O") == port_of(ff, "D")
                && port_of(l, "I0") == Some(i)
                && param_u64(&l.g, "INIT") == 0b01
        });
        if toggles {
            generated.push((cell_name(&ff.name), cell_name(&c.name)));
        }
    }
    for (ff, bufg) in generated {
        // The master: the first port clock (its BUFG's), as the register's.
        writeln!(
            xdc,
            "create_generated_clock -name {bufg} -source [get_pins {{{ff}/C}}] -divide_by 2 [get_pins {{{ff}/Q}}]"
        )?;
    }

    // Routing: each site input the netlist reads, back to the site output
    // it is routed from.
    let driver_pin: HashMap<SigBit, String> = cells
        .iter()
        .flat_map(|c| {
            c.g.ports
                .iter()
                .filter(|p| is_output(&c.g, &p.name))
                .flat_map(move |p| {
                    p.sig.iter().enumerate().map(move |(i, b)| {
                        let pin = if p.sig.len() == 1 {
                            p.name.clone()
                        } else {
                            format!("{}[{i}]", p.name)
                        };
                        (*b, format!("{}/{pin}", cell_name(&c.name)))
                    })
                })
        })
        .collect();
    let mut children: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut read: Vec<NodeId> = routed.read.iter().copied().collect();
    read.sort_unstable();
    for &r in &read {
        let mut path = vec![r];
        let mut n = r;
        let reached = loop {
            if routed.source.contains_key(&n) && n != r {
                break true;
            }
            match routed.driver.get(&n) {
                Some(&d) if path.len() < 10_000 => {
                    path.push(d);
                    n = d;
                }
                _ => break false,
            }
        };
        if !reached {
            continue;
        }
        for w in path.windows(2) {
            if seen.insert(w[0]) {
                children.entry(w[1]).or_default().push(w[0]);
            }
        }
    }
    // A wire name for each node on a route.
    let mut on_route: HashSet<NodeId> = children.keys().copied().collect();
    on_route.extend(seen.iter().copied());
    let mut wire_of: HashMap<NodeId, String> = HashMap::new();
    for tile in 0..db.width * db.height {
        let tt = db.tile_type(tile);
        for w in 0..tt.wires.len() as u32 {
            let n = db.node(tile, w);
            if on_route.contains(&n) {
                wire_of.entry(n).or_insert_with(|| {
                    format!("{}/{}", db.tile_names[tile as usize], tt.wires.get(w))
                });
            }
        }
    }
    fn tree(
        n: NodeId,
        children: &HashMap<NodeId, Vec<NodeId>>,
        wire_of: &HashMap<NodeId, String>,
        out: &mut String,
    ) {
        out.push_str(&wire_of[&n]);
        let Some(cs) = children.get(&n) else {
            return;
        };
        let mut cs = cs.clone();
        cs.sort_unstable();
        let (last, rest) = cs.split_last().expect("not empty");
        for &c in rest {
            out.push_str(" ( ");
            tree(c, children, wire_of, out);
            out.push_str(" )");
        }
        out.push(' ');
        tree(*last, children, wire_of, out);
    }
    // chipsmith may route a RAM32M's write enable to the slice's CE pin
    // (WEMUX.CE); Vivado has the RAM32M's WE on the WE pin only, so a net
    // reaching a RAM slice's CE is left to Vivado's router.
    let mut refused: HashSet<NodeId> = HashSet::new();
    for c in cells.iter().filter(|c| c.g.ty == "RAM32M") {
        if let Some((site, _)) = location(db, &routed.slice_sites, &c.name, &c.g.ty) {
            refused.insert(crate::phys::pin_node(db, &site, "CE")?);
        }
    }
    fn reaches(n: NodeId, children: &HashMap<NodeId, Vec<NodeId>>, set: &HashSet<NodeId>) -> bool {
        set.contains(&n)
            || children
                .get(&n)
                .is_some_and(|cs| cs.iter().any(|&c| reaches(c, children, set)))
    }
    let mut route = String::new();
    let mut sources: Vec<(&NodeId, &SigBit)> = routed
        .source
        .iter()
        .filter(|(n, _)| children.contains_key(n))
        .collect();
    sources.sort_unstable_by_key(|(n, _)| **n);
    let mut nets = 0;
    for (&n, b) in sources {
        let Some(pin) = driver_pin.get(b) else {
            continue;
        };
        if reaches(n, &children, &refused) {
            continue;
        }
        let mut t = String::new();
        tree(n, &children, &wire_of, &mut t);
        writeln!(route, "rt {{{pin}}} {{{t}}}")?;
        nets += 1;
    }
    Ok(Export {
        verilog: v,
        xdc,
        route,
        cells: listing,
        routed: nets,
    })
}
