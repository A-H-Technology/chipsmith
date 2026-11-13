//! The 7-series back end in process: synthesis' netlist to FASM through
//! chipsmith's packer (`crate::pack`), placer (`crate::place`), site
//! routing (`crate::phys`), router (`chipsmith_route`) and FASM writer
//! (`crate::fasm`), with a timing report of the routed design.
//!
//! Timing is Vivado's slow corner as `crate::delay` models it: cell delays
//! from prjxray-db's SDF (the register's as Vivado's timer has them), each
//! routing node's delay by its class as fitted to Vivado's net delays, a
//! site input's own delay into its site, and a margin for the clock tree,
//! whose clocks are otherwise taken to arrive everywhere at once.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use chipsmith_route::chipdb::ChipDb;
use chipsmith_route::timing::{Arc, Ps};
use chipsmith_route::{Design, NetReq, Router, Tree};

use crate::delay::Delays;
use crate::pack::{self, Conf};
use crate::place::{self, CellId, Kind, NetId};
use crate::{fasm, phys};

/// One clock's critical path.
#[derive(Debug, Clone, PartialEq)]
pub struct ClockTiming {
    /// The clock's source: a port (`clk`) or a cell output (`bscan.TCK`).
    pub name: String,
    /// The longest path from a register on the clock to one on the same
    /// clock, setup included.
    pub critical: Ps,
    /// A PLL's or MMCM's output: its frequency, which follows from the
    /// block's counters and its CLKIN1_PERIOD.
    pub hz: Option<f64>,
}

/// A PLL or MMCM, and the clock on its CLKIN1, for checking the period it
/// was solved for against the clock's frequency.
#[derive(Debug, Clone, PartialEq)]
pub struct CmtInput {
    pub cell: String,
    /// The clock's name, as [`ClockTiming::name`] has it.
    pub clock: String,
    pub period_ps: u32,
}

/// What the back end made.
#[derive(Debug, Clone)]
pub struct Report {
    pub luts: usize,
    pub registers: usize,
    pub carry4s: usize,
    pub ram32ms: usize,
    /// LUT6 sites in use (Vivado's "Slice LUTs"): a pair of LUTs on O6
    /// and O5 is one, as is a RAM32M's.
    pub lut_sites: usize,
    pub ramb18s: usize,
    pub ramb36s: usize,
    pub dsps: usize,
    /// Slices with anything in them.
    pub slices: usize,
    pub place: place::Stats,
    pub place_seconds: f64,
    pub nets: usize,
    pub pips: usize,
    pub iterations: usize,
    pub route_seconds: f64,
    pub clocks: Vec<ClockTiming>,
    pub cmt_inputs: Vec<CmtInput>,
}

/// Places, routes and writes the FASM of `netlist` (synthesis' yosys JSON);
/// `pins` maps each port bit to its package pin; `family` is prjxray-db's
/// directory of the part's family.
pub fn run(
    db: &ChipDb,
    delays: &Delays,
    family: &Path,
    netlist: &Path,
    pins: &BTreeMap<String, String>,
    fasm_out: &Path,
) -> Result<Report> {
    let text = std::fs::read(netlist).with_context(|| format!("reading {}", netlist.display()))?;
    let netlist = chipsmith_netlist::read_json(&text)
        .with_context(|| format!("parsing {}", netlist.display()))?;
    let packed = pack::pack(&netlist, pins, db)?;
    let d = &packed.design;

    let started = Instant::now();
    let placement = place::place(db, Some(delays), d)?;
    let place_seconds = started.elapsed().as_secs_f64();

    // Each cell's site and bel, beside the FASM.
    let mut text = String::new();
    for (c, at) in d.cells.iter().zip(&placement.sites) {
        if let Some((site, bel)) = at {
            text += &format!("{}\t{site}\t{bel:?}\n", c.name);
        }
    }
    let listing = fasm_out.with_file_name("placement.txt");
    std::fs::write(&listing, text).with_context(|| format!("writing {}", listing.display()))?;

    // The timing graph: the placer's (cell arcs, path ends), each register
    // and RAM on its clock net.
    let mut timing = place::Timing::new(d, delays);
    for c in &d.cells {
        let clock = match &c.kind {
            Kind::Ff(cs) => cs.clk,
            Kind::Lut { dram: true } => c.net("CLK"),
            // A block RAM's pins are each on their port's clock.
            Kind::Bram { sdp, size, .. } => {
                let pin_clock = |port: usize| -> Option<NetId> {
                    let base = ["CLKARDCLK", "CLKBWRCLK"][port];
                    match size {
                        crate::bram::Size::Full => c.net(&format!("{base}L")),
                        crate::bram::Size::Half => c.net(base),
                    }
                };
                for (port, _) in &c.pins {
                    if let Some(clk) = pin_clock(place::bram_port(port, *sdp)) {
                        let n = timing.graph.node(&c.name, port);
                        timing.graph.set_clock(n, clk);
                    }
                }
                None
            }
            // The PS7's clocked pins, each on its clock pin's net.
            Kind::Ps7 => {
                for (port, _) in &c.pins {
                    if let Some((clk_pin, _)) = crate::ps7_timing::of(port) {
                        if let Some(clk) = c.net(clk_pin) {
                            let n = timing.graph.node(&c.name, port);
                            timing.graph.set_clock(n, clk);
                        }
                    }
                }
                None
            }
            _ => None,
        };
        if let Some(clk) = clock {
            for (port, _) in &c.pins {
                let n = timing.graph.node(&c.name, port);
                timing.graph.set_clock(n, clk);
            }
        }
    }
    let node_delays = crate::delay::node_delays(db, family)?;
    let ph = phys::build(db, &packed, &placement, family, &node_delays)?;
    let reqs: Vec<NetReq> = ph
        .nets
        .iter()
        .map(|r| {
            let drv = d.nets[r.net as usize].driver.as_ref();
            let driver = match drv {
                Some((c, port)) => timing.graph.node(&d.cells[*c as usize].name, port),
                None => timing.graph.node(&r.name, "source"),
            };
            let sink_ports = r
                .sink_ports
                .iter()
                .enumerate()
                .map(|(k, ports)| match ports.first() {
                    Some((c, port)) => timing.graph.node(&d.cells[*c as usize].name, port),
                    None => timing.graph.node(&r.name, &format!("sink{k}")),
                })
                .collect();
            NetReq {
                name: r.name.clone(),
                source: r.source,
                source_wire: (0, 0),
                sinks: r.sinks.clone(),
                driver,
                sink_ports,
                direct_ports: Vec::new(),
            }
        })
        .collect();
    let design = Design {
        nets: reqs,
        fixed_inputs: Default::default(),
        timing: router_graph(d, &timing, &ph.nets),
    };
    let started = Instant::now();
    let mut router = Router::new(db, &design);
    router.set_node_delays(&node_delays);
    // Measured over three placer seeds of zybo-picorv32 and zybo-sha256
    // (docs/fast-pnr-status.md, "7-series routing"): routing 38-43% faster,
    // Fmax the same within the seeds' spread. The repair pass moved neither
    // design's critical path by more than 0.01 ns.
    router.batch_first(8, 4);
    router.without_repair();
    for (i, r) in ph.nets.iter().enumerate() {
        // A BUFG's net stays on the clock tree: its bounding box rarely
        // holds the clock rows it passes through, and inside the box the
        // router would find a way through the general interconnect.
        let from_bufg = d.nets[r.net as usize]
            .driver
            .as_ref()
            .is_some_and(|(c, _)| d.cells[*c as usize].kind == Kind::Bufg);
        if from_bufg {
            router.confine(i as u32, &["CLOCK"]);
        }
        for &n in std::iter::once(&r.source).chain(&r.sinks) {
            router
                .reserve(n, i as u32)
                .with_context(|| format!("net {}", r.name))?;
        }
    }
    if std::env::var_os("CHIPSMITH_XC7_REACH").is_some() {
        reach(db, &ph.nets, &packed.design)?;
    }
    let (trees, iterations, _) = router.route(&design.nets)?;
    let route_seconds = started.elapsed().as_secs_f64();
    check(db, &design.nets, &trees)?;

    let pips: Vec<_> = trees.iter().flat_map(|t| t.pips.iter().copied()).collect();
    let text = fasm::write(db, &ph, &pips)?;
    std::fs::write(fasm_out, text).with_context(|| format!("writing {}", fasm_out.display()))?;

    if let Some(path) = std::env::var_os("CHIPSMITH_XC7_CONNS") {
        conns_dump(
            db,
            &timing,
            delays,
            &ph.nets,
            &trees,
            &placement.sites,
            &path,
        )?;
    }
    let clocks = clock_timing(d, &packed.conf, &timing, &ph.nets, &trees, &placement.sites)?;
    let count = |f: &dyn Fn(&Kind, &Conf) -> bool| {
        d.cells
            .iter()
            .zip(&packed.conf)
            .filter(|(c, conf)| f(&c.kind, conf))
            .count()
    };
    Ok(Report {
        luts: count(&|k, _| *k == Kind::Lut { dram: false }),
        registers: count(&|k, _| matches!(k, Kind::Ff(_))),
        carry4s: count(&|k, _| *k == Kind::Carry),
        ram32ms: d.drams.len(),
        lut_sites: ph
            .slices
            .values()
            .map(|s| {
                (0..4)
                    .filter(|&k| s.lut_init[k].is_some() || s.lut5_init[k].is_some())
                    .count()
            })
            .sum(),
        ramb18s: count(&|k, _| {
            matches!(
                k,
                Kind::Bram {
                    size: crate::bram::Size::Half,
                    ..
                }
            )
        }),
        ramb36s: count(&|k, _| {
            matches!(
                k,
                Kind::Bram {
                    size: crate::bram::Size::Full,
                    ..
                }
            )
        }),
        dsps: count(&|k, _| matches!(k, Kind::Dsp { .. })),
        slices: ph.slices.len(),
        place: placement.stats,
        place_seconds,
        nets: ph.nets.len(),
        pips: pips.len(),
        iterations,
        route_seconds,
        clocks,
        cmt_inputs: d
            .cells
            .iter()
            .zip(&packed.conf)
            .filter_map(|(c, conf)| match conf {
                Conf::Cmt(cmt) => Some(CmtInput {
                    cell: c.name.clone(),
                    clock: c
                        .net("CLKIN1")
                        .map_or_else(String::new, |n| clock_name(d, n)),
                    period_ps: cmt.clkin_period_ps,
                }),
                _ => None,
            })
            .collect(),
    })
}

/// The timing graph the router times its routing by: the placer's, plus
/// what the routed nets' arcs (driver to each sink's first port) leave out,
/// at 0 ps as [`clock_timing`] takes them. Without these the router's paths
/// broke at every connection inside a slice (a LUT into its CARRY4's S, a
/// sum into its register) and at the second LUT of a pair on a shared
/// input pin: sha256's LUTs in front of its adders looked uncritical and
/// were routed for congestion, the router timing 8.0 ns of a 13.8 ns path.
fn router_graph(
    d: &place::Design,
    timing: &place::Timing,
    nets: &[phys::RouteNet],
) -> chipsmith_route::timing::Graph {
    let mut g = timing.graph.clone();
    let mut routed: std::collections::HashSet<(NetId, CellId, &str)> =
        std::collections::HashSet::new();
    for r in nets {
        for ports in &r.sink_ports {
            for (c, p) in ports {
                routed.insert((r.net, *c, p.as_str()));
            }
        }
    }
    for c in &timing.conns {
        let port = timing.graph.name(c.to);
        let port = port.rsplit_once('.').map_or(port, |p| p.1);
        if !routed.contains(&(c.net, c.to_cell, port)) {
            g.add_arc(c.from, c.to, 0.0);
        }
    }
    let d_name = |c: CellId| d.cells[c as usize].name.as_str();
    for r in nets {
        for ports in &r.sink_ports {
            let Some(((c0, p0), rest)) = ports.split_first() else {
                continue;
            };
            let first = g.node(d_name(*c0), p0);
            for (c, p) in rest {
                let other = g.node(d_name(*c), p);
                g.add_arc(first, other, 0.0);
            }
        }
    }
    g
}

/// A clock's name: a port's (`clk`) or a cell output's (`pll.CLKOUT0`),
/// through a BUFG to what feeds it.
fn clock_name(d: &place::Design, clk: NetId) -> String {
    let mut net = clk;
    if let Some((c, _)) = &d.nets[net as usize].driver {
        if d.cells[*c as usize].kind == Kind::Bufg {
            if let Some(i) = d.cells[*c as usize].net("I0") {
                net = i;
            }
        }
    }
    match &d.nets[net as usize].driver {
        Some((c, port)) => match &d.cells[*c as usize].kind {
            Kind::Pad { port, .. } => port.clone(),
            _ => format!("{}.{port}", d.cells[*c as usize].name),
        },
        None => format!("net {net}"),
    }
}

/// Debugging (`CHIPSMITH_XC7_REACH`): every sink reachable from its source
/// without passing another net's ends, by breadth-first search.
fn reach(db: &ChipDb, nets: &[phys::RouteNet], d: &place::Design) -> Result<()> {
    let mut owner = vec![u32::MAX; db.nodes()];
    for (i, r) in nets.iter().enumerate() {
        for &n in std::iter::once(&r.source).chain(&r.sinks) {
            owner[n as usize] = i as u32;
        }
    }
    for (i, r) in nets.iter().enumerate() {
        let mut seen = std::collections::HashSet::from([r.source]);
        let mut q = std::collections::VecDeque::from([r.source]);
        while let Some(n) = q.pop_front() {
            for e in db.downhill(n) {
                let o = owner[e.dst as usize];
                if (o == u32::MAX || o == i as u32) && seen.insert(e.dst) {
                    q.push_back(e.dst);
                }
            }
        }
        for (k, s) in r.sinks.iter().enumerate() {
            if !seen.contains(s) {
                let ports: Vec<String> = r.sink_ports[k]
                    .iter()
                    .map(|(c, p)| format!("{}.{p}", d.cells[*c as usize].name))
                    .collect();
                eprintln!("    {}: sink {s} ({ports:?}) unreachable", r.name);
                // The shortest path ignoring other nets' ends, and whose
                // ends it crosses.
                let mut from = std::collections::HashMap::from([(r.source, r.source)]);
                let mut q = std::collections::VecDeque::from([r.source]);
                while let Some(n) = q.pop_front() {
                    for e in db.downhill(n) {
                        if let std::collections::hash_map::Entry::Vacant(v) = from.entry(e.dst) {
                            v.insert(n);
                            q.push_back(e.dst);
                        }
                    }
                }
                let mut at = *s;
                while at != r.source {
                    let o = owner[at as usize];
                    if o != u32::MAX && o != i as u32 {
                        eprintln!(
                            "      crosses node {at}, an end of {}",
                            nets[o as usize].name
                        );
                    }
                    let Some(&prev) = from.get(&at) else { break };
                    at = prev;
                }
            }
        }
    }
    Ok(())
}

/// Every route is a tree of the graph's pips from its source to all its
/// sinks, and no node carries two routes.
fn check(db: &ChipDb, nets: &[NetReq], trees: &[Tree]) -> Result<()> {
    let mut used: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for (i, (net, tree)) in nets.iter().zip(trees).enumerate() {
        let mut reached = std::collections::HashSet::from([net.source]);
        for (n, e) in tree.nodes[1..].iter().zip(&tree.pips) {
            anyhow::ensure!(e.dst == *n, "net {}: pip out of order", net.name);
            let src = db.node(e.tile, db.tile_type(e.tile).pips[e.pip as usize].0);
            anyhow::ensure!(
                reached.contains(&src) && db.downhill(src).contains(e),
                "net {}: {} is not driven from the route",
                net.name,
                db.pip_name(e.tile, e.pip)
            );
            reached.insert(*n);
        }
        for s in &net.sinks {
            anyhow::ensure!(
                reached.contains(s),
                "net {}: a sink is not reached",
                net.name
            );
        }
        for &n in &tree.nodes {
            if let Some(other) = used.insert(n, i) {
                anyhow::bail!("nets {} and {} share node {n}", nets[other].name, net.name);
            }
        }
    }
    Ok(())
}

/// Each clock's own critical path over the routed design: the placer's
/// connections with their routed delays (0 inside a slice).
/// CHIPSMITH_XC7_CONNS=<file>: every routed connection's tile distance
/// (dx, dy), the placer's estimate, the routed delay (ps) and the sink
/// port, to fit the placer's estimate to the router's.
#[allow(clippy::too_many_arguments)]
fn conns_dump(
    db: &ChipDb,
    timing: &place::Timing,
    delays: &Delays,
    nets: &[phys::RouteNet],
    trees: &[Tree],
    sites: &[Option<(String, place::Bel)>],
    path: &std::ffi::OsStr,
) -> Result<()> {
    let mut delay: std::collections::HashMap<(NetId, CellId, &str), Ps> =
        std::collections::HashMap::new();
    for (r, t) in nets.iter().zip(trees) {
        for (ports, &dl) in r.sink_ports.iter().zip(&t.sink_delay) {
            for (c, p) in ports {
                delay.insert((r.net, *c, p.as_str()), dl);
            }
        }
    }
    let xy = |c: CellId| -> Option<(i32, i32)> {
        let (site, _) = sites[c as usize].as_ref()?;
        let (x, y) = db.tile_xy(db.sites.get(site)?.tile);
        Some((x as i32, y as i32))
    };
    let mut text = String::new();
    for c in &timing.conns {
        let port = timing.graph.name(c.to);
        let port = port.rsplit_once('.').map_or(port, |p| p.1);
        let (Some(dl), Some(a), Some(b)) = (
            delay.get(&(c.net, c.to_cell, port)),
            xy(c.from_cell),
            xy(c.to_cell),
        ) else {
            continue;
        };
        let (dx, dy) = (a.0 - b.0, a.1 - b.1);
        text += &format!("{dx}\t{dy}\t{:.0}\t{dl:.0}\t{port}\n", delays.route(dx, dy));
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.to_string_lossy()))
}

fn clock_timing(
    d: &place::Design,
    conf: &[Conf],
    timing: &place::Timing,
    nets: &[phys::RouteNet],
    trees: &[Tree],
    sites: &[Option<(String, place::Bel)>],
) -> Result<Vec<ClockTiming>> {
    let mut delay: std::collections::HashMap<(NetId, CellId, &str), Ps> =
        std::collections::HashMap::new();
    for (r, t) in nets.iter().zip(trees) {
        for (ports, &dl) in r.sink_ports.iter().zip(&t.sink_delay) {
            for (c, p) in ports {
                delay.insert((r.net, *c, p.as_str()), dl);
            }
        }
    }
    let arcs: Vec<Arc> = timing
        .conns
        .iter()
        .map(|c| {
            let port = timing.graph.name(c.to);
            let port = port.rsplit_once('.').map_or(port, |p| p.1);
            Arc {
                from: c.from,
                to: c.to,
                delay: delay.get(&(c.net, c.to_cell, port)).copied().unwrap_or(0.0),
            }
        })
        .collect();
    let _ = conf;
    // CHIPSMITH_XC7_ENDPOINTS=<file>: every path end's site, bel, port,
    // arrival (`-` if no timed path reaches it) and setup (ps), to hold
    // against another timer's
    // (`bench/zybo-timing.sh`).
    if let Some(path) = std::env::var_os("CHIPSMITH_XC7_ENDPOINTS") {
        let a = timing.graph.analyse(&arcs);
        let at: std::collections::HashMap<&str, &(String, place::Bel)> = d
            .cells
            .iter()
            .zip(sites)
            .filter_map(|(c, s)| Some((c.name.as_str(), s.as_ref()?)))
            .collect();
        let mut text = String::new();
        for n in 0..timing.graph.len() as u32 {
            let Some(setup) = timing.graph.setup(n) else {
                continue;
            };
            // `-`: no timed path reaches it.
            let arrival = a.arrival(n).map_or("-".to_string(), |t| format!("{t:.0}"));
            let name = timing.graph.name(n);
            let Some((cell, port)) = name.rsplit_once('.') else {
                continue;
            };
            let Some((site, bel)) = at.get(cell) else {
                continue;
            };
            text += &format!(
                "{site}\t{}\t{port}\t{arrival}\t{setup:.0}\n",
                bel.vivado_name()
            );
        }
        std::fs::write(&path, text)
            .with_context(|| format!("writing {}", path.to_string_lossy()))?;
    }
    // CHIPSMITH_XC7_PATH=1: the critical path, node by node, on stderr.
    if std::env::var("CHIPSMITH_XC7_PATH").is_ok_and(|v| v == "1") {
        for (node, at) in timing.graph.analyse(&arcs).path {
            eprintln!("  {at:8.0} ps  {}", timing.graph.name(node));
        }
    }
    let name_of = |clk: NetId| clock_name(d, clk);
    let hz_of = |clk: NetId| -> Option<f64> {
        let mut net = clk;
        if let Some((c, _)) = &d.nets[net as usize].driver {
            if d.cells[*c as usize].kind == Kind::Bufg {
                net = d.cells[*c as usize].net("I0")?;
            }
        }
        let (c, port) = d.nets[net as usize].driver.as_ref()?;
        let Conf::Cmt(cmt) = &conf[*c as usize] else {
            return None;
        };
        let k: usize = port.strip_prefix("CLKOUT")?.parse().ok()?;
        Some(cmt.config.output_ratio(k)? * 1e12 / f64::from(cmt.clkin_period_ps))
    };
    let mut out: Vec<ClockTiming> = timing
        .graph
        .clock_pairs(&arcs)
        .into_iter()
        .filter(|p| p.from == p.to)
        .map(|p| ClockTiming {
            name: name_of(p.from),
            critical: p.critical,
            hz: hz_of(p.from),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}
