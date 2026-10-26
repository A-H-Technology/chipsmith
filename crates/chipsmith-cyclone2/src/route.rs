//! The Cyclone II side of the router: a placed design in nextpnr's JSON
//! (`nextpnr-himbaechel --no-route --write`) in, the same JSON with every
//! net's `ROUTING` filled in the way nextpnr writes it out, so the bitstream
//! (`bitstream::nextpnr`, cyclone2-re's `cram pack`) reads either. The search itself is `chipsmith_route`'s.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

use chipsmith_route::chipdb::{ChipDb, NodeId};
use chipsmith_route::timing::{self, Ps};
use chipsmith_route::{Design, NetReq, Router, Tree};

use crate::m4k::{Pin, Ports, Sel, Shape, Side};
use crate::pin::register_param;

/// A prebuilt routing graph (`chipsmith routing-graph`).
pub const GRAPH_VAR: &str = "CHIPSMITH_CYCLONE2_GRAPH";
/// cyclone2-re's `db/` directory, holding `EP2C35F672C6/chipdb.txt.xz`.
pub const DB_VAR: &str = "CHIPSMITH_CYCLONE2_DB";

/// The EP2C35's graph: the prebuilt one at `GRAPH_VAR` (the nix package
/// ships one), or else parsed from the chip database under `DB_VAR`, which
/// takes several seconds longer. A missing or stale cache at `GRAPH_VAR`
/// is rewritten from the parse when the path is writable (the devshell
/// points it into `target/`).
pub fn load_graph() -> Result<ChipDb> {
    let cache = std::env::var_os(GRAPH_VAR).map(std::path::PathBuf::from);
    if let Some(cache) = cache.as_deref().filter(|c| c.exists()) {
        match ChipDb::read_cache(cache) {
            Ok(db) => return Ok(db),
            Err(e) => eprintln!("    {e:#}; parsing the chip database instead"),
        }
    }
    let db = std::env::var_os(DB_VAR).with_context(|| {
        format!("{DB_VAR} is not set; run chipsmith from its nix package or `nix develop`")
    })?;
    let graph = ChipDb::load(&Path::new(&db).join("EP2C35F672C6/chipdb.txt.xz"))?;
    if let Some(cache) = cache {
        if graph.write_cache(&cache).is_ok() {
            eprintln!("    cached the routing graph in {}", cache.display());
        }
    }
    Ok(graph)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub nets: usize,
    pub pips: usize,
    pub iterations: usize,
    /// Estimated minimum clock period (critical register-to-register path,
    /// any clock to any clock).
    pub critical: Ps,
    /// Every global clock, and the critical path of every pair of them
    /// that has a path.
    pub clocks: Vec<Clock>,
    pub pairs: Vec<ClockPair>,
}

/// One global clock: what feeds its clock control.
#[derive(Debug, Clone, PartialEq)]
pub struct Clock {
    /// The net into the clock control.
    pub name: String,
    pub source: ClockSource,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClockSource {
    /// A pin, by top-level port bit (`CLOCK_50`).
    Pin(String),
    /// Output `output` (c0-c2) of PLL cell `cell`, at the frequency its
    /// counters make of its input clock.
    Pll { cell: String, output: u8, hz: f64 },
    /// Logic in the core: a register, a LUT, the JTAG block's TCK. Its
    /// frequency is the design's business.
    Core,
}

/// The critical path from registers on `Stats::clocks[from]` to registers
/// on `Stats::clocks[to]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockPair {
    pub from: usize,
    pub to: usize,
    pub critical: Ps,
}

/// Route `placed`, write `routed`.
pub fn route_file(db: &ChipDb, placed: &Path, routed: &Path) -> Result<Stats> {
    let text =
        std::fs::read_to_string(placed).with_context(|| format!("reading {}", placed.display()))?;
    let mut json: Value = serde_json::from_str(&text)?;
    let design = read_design(db, &json)?;
    let mut router = Router::new(db, &design);
    let (trees, iterations, analysis) = router.route(&design.nets)?;
    if let Some(out) = std::env::var_os("CHIPSMITH_DUMP_CONNECTIONS") {
        dump_connections(db, &router, &design, &trees, Path::new(&out))?;
    }
    if std::env::var_os("CHIPSMITH_CRITICAL_PATH").is_some() {
        for &(n, t) in &analysis.path {
            eprintln!("      {t:8.0} ps  {}", design.timing.name(n));
        }
    }

    let netnames = json["modules"]["top"]["netnames"]
        .as_object_mut()
        .context("no netnames")?;
    let mut pips = 0;
    for (net, tree) in design.nets.iter().zip(&trees) {
        pips += tree.pips.len();
        let routing = routing_attribute(db, net, tree);
        netnames
            .get_mut(&net.name)
            .context("net vanished")?
            .as_object_mut()
            .context("netname is not an object")?
            .entry("attributes")
            .or_insert_with(|| Value::Object(Default::default()))["ROUTING"] =
            Value::String(routing);
    }
    std::fs::write(routed, serde_json::to_string(&json)?)
        .with_context(|| format!("writing {}", routed.display()))?;
    let clocks = clocks(&json)?;
    let index = |net: u32| clocks.iter().position(|(id, _)| *id == u64::from(net));
    let pairs = design
        .timing
        .clock_pairs(&Router::net_arcs(&design.nets, &trees))
        .into_iter()
        .filter_map(|p| {
            Some(ClockPair {
                from: index(p.from)?,
                to: index(p.to)?,
                critical: p.critical,
            })
        })
        .collect();
    Ok(Stats {
        nets: design.nets.len(),
        pips,
        iterations,
        critical: analysis.critical,
        clocks: clocks.into_iter().map(|(_, c)| c).collect(),
        pairs,
    })
}

/// For calibrating the delay model against quartus_sta: one line per
/// connection, `driver-port sink-port model-ps fanout wire-type...`.
fn dump_connections(
    db: &ChipDb,
    router: &Router,
    design: &Design,
    trees: &[Tree],
    out: &Path,
) -> Result<()> {
    let mut lines = String::new();
    for (net, tree) in design.nets.iter().zip(trees) {
        let in_tree: HashSet<NodeId> = tree.nodes.iter().copied().collect();
        let mut parent: HashMap<NodeId, NodeId> = HashMap::new();
        for &u in &tree.nodes {
            for e in db.downhill(u) {
                if in_tree.contains(&e.dst) && tree.pips.contains(e) {
                    parent.insert(e.dst, u);
                }
            }
        }
        for (i, &sink) in net.sinks.iter().enumerate() {
            let mut types = Vec::new();
            let mut n = sink;
            while n != net.source {
                types.push(db.wire_types[db.node_type[n as usize] as usize].as_str());
                n = parent[&n];
            }
            types.reverse();
            lines += &format!(
                "{} {} {} {} {}\n",
                design.timing.name(net.driver),
                design.timing.name(net.sink_ports[i]),
                tree.sink_delay[i],
                net.sinks.len(),
                types.join(" ")
            );
        }
    }
    let _ = router;
    std::fs::write(out, lines).with_context(|| format!("writing {}", out.display()))
}

/// nextpnr's `ROUTING`: `wire;pip;strength;` per wire, the source wire with
/// no pip. (The bitstream only reads the pips.)
fn routing_attribute(db: &ChipDb, net: &NetReq, tree: &Tree) -> String {
    let (tile, wire) = net.source_wire;
    let mut s = format!("{};;1;", db.wire_name(tile, wire));
    for e in &tree.pips {
        s += &format!(
            "{};{};1;",
            db.wire_name(e.tile, db.pip_dst_wire(e.tile, e.pip)),
            db.pip_name(e.tile, e.pip)
        );
    }
    s
}

/// A packed M4K's shape, from its parameters (yosys' JSON strings).
fn m4k_shape(cell: &Value) -> Option<Shape> {
    let p = &cell["parameters"];
    Shape::parse(
        |k| p[k].as_str().map(|v| v.trim_end().to_owned()),
        |k| {
            p[k].as_str()
                .and_then(|v| u64::from_str_radix(v.trim(), 2).ok())
        },
        |w| w.bits() as u8,
    )
    .ok()
}

/// The clock a pin of an M4K launches or captures on: a port's inputs on
/// its input clock, the read data on its output register's clock (else
/// the read port's), a clock enable or clear on its own clock.
fn m4k_pin_clock(s: Shape, pin: &str) -> Option<Sel> {
    let side_of_data = |k: u8| -> Side {
        let tdp = matches!(s.ports(), Ports::TrueDual { .. });
        if tdp && k >= 18 {
            Side::B
        } else {
            match s.ports() {
                Ports::SimpleDual { .. } => Side::B,
                _ => Side::A,
            }
        }
    };
    Some(match Pin::parse(pin)? {
        Pin::Clk0 | Pin::Clk1 => return None,
        Pin::Ena(k) | Pin::Clr(k) => k,
        Pin::We => s.input_clock(Side::A),
        Pin::Re | Pin::Ra(_) => s.input_clock(Side::B),
        Pin::Wa(_) => s.input_clock(Side::A),
        Pin::Be(k) => s.input_clock(if k >= 2 && matches!(s.ports(), Ports::TrueDual { .. }) {
            Side::B
        } else {
            Side::A
        }),
        Pin::Di(k) => {
            let b = matches!(s.ports(), Ports::TrueDual { .. }) && k >= 18
                || matches!(k, 26 | 35) && s.width(Side::B).is_some_and(|w| w.addr_bits() >= 11);
            s.input_clock(if b { Side::B } else { Side::A })
        }
        Pin::Do(k) => {
            let side = side_of_data(k);
            let r = s.read_ports().into_iter().find(|(x, _)| *x == side)?.1;
            r.out_reg.map_or(s.input_clock(side), |o| o.clock)
        }
    })
}

fn net_of(cell: &Value, port: &str) -> Option<u64> {
    cell["connections"][port].as_array()?.first()?.as_u64()
}

/// The global clocks, by the id of the clock control's output net.
fn clocks(json: &Value) -> Result<Vec<(u64, Clock)>> {
    let top = &json["modules"]["top"];
    let cells = top["cells"].as_object().context("no cells")?;
    let mut names: HashMap<u64, &str> = HashMap::new();
    for (name, n) in top["netnames"].as_object().context("no netnames")? {
        if let Some(id) = n["bits"].as_array().and_then(|a| a.first()?.as_u64()) {
            names.entry(id).or_insert(name);
        }
    }
    let mut out = Vec::new();
    for (name, c) in cells.iter().filter(|(_, c)| c["type"] == "CLKCTRL") {
        let (Some(o), Some(i)) = (net_of(c, "OUTCLK"), net_of(c, "INCLK")) else {
            bail!("{name}: clock control not connected");
        };
        // The driving cell and port.
        let driver = cells.iter().find_map(|(dn, d)| {
            let (p, _) = d["connections"].as_object()?.iter().find(|(p, b)| {
                d["port_directions"][*p] == "output"
                    && b.as_array().and_then(|a| a.first()?.as_u64()) == Some(i)
            })?;
            Some((dn, d, p))
        });
        let source = match driver {
            Some((_, d, _)) if d["type"] == "CY2_IBUF" || d["type"] == "CY2_IOBUF" => {
                ClockSource::Pin(
                    d["attributes"]["PORT"]
                        .as_str()
                        .context("pin buffer without PORT")?
                        .to_string(),
                )
            }
            Some((dn, d, p)) if d["type"] == "CY2_PLL" => {
                let params: HashMap<String, String> = d["parameters"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect();
                let used = [0, 1, 2].map(|k| net_of(d, &format!("CLK{k}")).is_some());
                let output: u8 = p
                    .strip_prefix("CLK")
                    .and_then(|k| k.parse().ok())
                    .with_context(|| format!("{dn}.{p} clocks {name}"))?;
                let solved = crate::pll::Request::from_params(&params, used)
                    .and_then(|r| r.solve())
                    .with_context(|| dn.clone())?;
                ClockSource::Pll {
                    cell: dn.clone(),
                    output,
                    hz: solved.hz[output as usize].context("PLL output without a counter")?,
                }
            }
            _ => ClockSource::Core,
        };
        out.push((
            o,
            Clock {
                // Synthesis' own net names (`$...`) say nothing; the
                // driver's port does (`jtag.TCKUTAP`).
                name: match (names.get(&i), driver) {
                    (Some(n), _) if !n.starts_with('$') => n.to_string(),
                    (_, Some((dn, _, p))) => format!("{dn}.{p}"),
                    (n, None) => n.map_or_else(|| format!("net {i}"), |n| n.to_string()),
                },
                source,
            },
        ));
    }
    out.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    Ok(out)
}

/// A net's endpoints in the graph, as cells name them.
#[derive(Default)]
struct Ends {
    /// The driving tile wire, its node and its timing node.
    source: Option<((u32, u32), NodeId, u32)>,
    /// (node, timing node)
    sinks: Vec<(NodeId, u32)>,
}

fn tile_xy(name: &str) -> Option<(u32, u32)> {
    let (x, y) = name.strip_prefix('X')?.split_once('Y')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

/// What the router needs from a placed design.
fn read_design(db: &ChipDb, json: &Value) -> Result<Design> {
    let top = &json["modules"]["top"];
    // net id -> (source, sinks)
    let mut ends: HashMap<u64, Ends> = HashMap::new();
    let mut fixed_inputs = HashSet::new();
    // net id -> number of cell inputs on it, for "is COUT used".
    let mut loads: HashMap<u64, usize> = HashMap::new();
    let mut carry_luts: Vec<(u32, u8, Option<u64>, bool)> = Vec::new();
    let mut timing = timing::Graph::default();

    for (name, cell) in top["cells"].as_object().context("no cells")? {
        let bel = cell["attributes"]["NEXTPNR_BEL"]
            .as_str()
            .with_context(|| format!("{name} is not placed"))?;
        let (tile_name, bel_name) = bel.split_once('/').context("bel name")?;
        let (x, y) = tile_xy(tile_name).with_context(|| format!("tile {tile_name}"))?;
        let tile = db
            .tile_index(x, y)
            .with_context(|| format!("{name}: tile {tile_name} off chip"))?;
        let pins = &db
            .tile_type(tile)
            .bels
            .get(bel_name)
            .with_context(|| format!("{name}: no bel {bel}"))?
            .pins;
        let dirs = &cell["port_directions"];
        let conns = cell["connections"].as_object().context("connections")?;
        let connected: Vec<&str> = conns
            .iter()
            .filter(|(_, b)| b.as_array().is_some_and(|a| !a.is_empty()))
            .map(|(p, _)| p.as_str())
            .collect();
        let le: u8 = bel_name
            .strip_prefix("LE")
            .and_then(|b| b.split_once('.'))
            .and_then(|(k, _)| k.parse().ok())
            .unwrap_or(0);
        let ty = cell["type"].as_str().unwrap_or("");
        timing.add_cell(name, ty, &connected, le);
        if matches!(ty, "CY2_IBUF" | "CY2_OBUF" | "CY2_IOBUF") {
            let reg = |p: &str| cell["parameters"][p].is_string();
            timing.add_ioe_registers(
                name,
                reg(register_param::INPUT),
                reg(register_param::OUTPUT),
                reg(register_param::OE),
            );
        }
        let clock = match ty {
            "LCELL_FF" => net_of(cell, "CLK"),
            "CY2_M4K" => net_of(cell, "CLK0"),
            _ => None,
        };
        if let Some(clock) = clock {
            // A block on two clocks: each pin on its own (`m4k_pin_clock`).
            let shape = (ty == "CY2_M4K").then(|| m4k_shape(cell)).flatten();
            for p in &connected {
                let n = timing.node(name, p);
                let c = shape
                    .and_then(|s| m4k_pin_clock(s, p))
                    .and_then(|sel| match sel {
                        Sel::Zero => net_of(cell, "CLK0"),
                        Sel::One => net_of(cell, "CLK1"),
                    })
                    .unwrap_or(clock);
                timing.set_clock(n, c as u32);
            }
        }
        for (port, bits) in conns {
            let Some(net) = bits
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_u64)
            else {
                continue;
            };
            let output = dirs[port] == "output";
            if !output {
                *loads.entry(net).or_default() += 1;
            }
            // The packer drops the pad side of every pin, so a connected
            // port without a wire is a cell on a bel that can't take it
            // (an output enable on a clock-only IOE), not one to skip.
            let &wire = pins
                .get(port)
                .with_context(|| format!("{name}.{port}: bel {bel} has no such pin"))?;
            let node = db.node(tile, wire);
            let tnode = timing.node(name, port);
            let e = ends.entry(net).or_default();
            if output {
                ensure!(
                    e.source.is_none(),
                    "net {net} has two drivers (one is {name}.{port})"
                );
                e.source = Some(((tile, wire), node, tnode));
            } else {
                e.sinks.push((node, tnode));
            }
        }
        if cell["type"] == "LCELL_COMB" {
            let k: u8 = bel_name
                .strip_prefix("LE")
                .and_then(|b| b.strip_suffix(".LUT"))
                .and_then(|k| k.parse().ok())
                .with_context(|| format!("{name}: LUT on bel {bel_name}"))?;
            let net_of = |p: &str| conns.get(p).and_then(|b| b.as_array()?.first()?.as_u64());
            carry_luts.push((tile, k, net_of("COUT"), net_of("CIN").is_some()));
        }
    }
    // Carry LEs can't permute their inputs: C is the carry in and D picks
    // the sum or carry half. Chain heads too (COUT used, no CIN).
    for (tile, k, cout, cin) in carry_luts {
        if cin || cout.is_some_and(|n| loads.get(&n).copied().unwrap_or(0) > 0) {
            fixed_inputs.insert((tile, k));
        }
    }

    // One name per net carries ROUTING; the bitstream adds up every name's
    // pips.
    let mut names: HashMap<u64, String> = HashMap::new();
    for (name, n) in top["netnames"].as_object().context("no netnames")? {
        if let Some(id) = n["bits"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_u64)
        {
            names.entry(id).or_insert_with(|| name.clone());
        }
    }

    let mut nets = Vec::new();
    let mut ids: Vec<u64> = ends.keys().copied().collect();
    ids.sort();
    for id in ids {
        let Ends { source, mut sinks } = ends.remove(&id).unwrap();
        let name = names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("net {id}"));
        let Some((source_wire, source, driver)) = source else {
            ensure!(
                sinks.is_empty(),
                "net {name} has loads but no driver in the fabric"
            );
            continue;
        };
        let direct_ports: Vec<u32> = sinks
            .iter()
            .filter(|&&(s, _)| s == source)
            .map(|&(_, t)| t)
            .collect();
        sinks.retain(|&(s, _)| s != source);
        sinks.sort();
        sinks.dedup();
        if sinks.is_empty() && direct_ports.is_empty() {
            continue;
        }
        nets.push(NetReq {
            name,
            source,
            source_wire,
            driver,
            sink_ports: sinks.iter().map(|&(_, t)| t).collect(),
            direct_ports,
            sinks: sinks.into_iter().map(|(n, _)| n).collect(),
        });
    }
    Ok(Design {
        nets,
        fixed_inputs,
        timing,
    })
}
