//! Routing a 7-series design placed by openXC7's nextpnr with chipsmith's
//! router.
//!
//! nextpnr's xilinx graph has sites inside it: site wires, the pips of the
//! site muxes, LUT input permutation and route-through pseudo pips. The
//! prjxray graph (`route::xray`) has only the interconnect. So nextpnr's
//! routed design (`--write`) is taken apart per net:
//!
//! - every pip that is an interconnect pip of the prjxray graph is
//!   dropped: that part is chipsmith's to route;
//! - everything else (site routing, the LUT pin nextpnr permuted a signal
//!   to, route-throughs, pips with no FASM feature) is kept as it is.
//!
//! What remains splits each net into segments: a segment starts at a graph
//! node the net enters from outside the interconnect (a site output pin
//! wire, or the far side of a kept pip) and must reach every graph node the
//! net leaves the interconnect from (a site input pin wire). Segments are
//! the router's nets; their endpoints are reserved for them.
//!
//! The result is nextpnr's `ROUTING` spelling per net (kept entries plus
//! chipsmith's pips), as nextpnr's `-o prerouted=<file>` reads it, so
//! nextpnr's own FASM writer turns it into features.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

use super::chipdb::{ChipDb, Edge, NodeId};
use super::{Design, NetReq, Router};
use crate::timing;

/// A wire of nextpnr's route: an interconnect node of the graph, or
/// something only nextpnr models (a site wire), by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Node(NodeId),
    Other(String),
}

/// How a wire of nextpnr's route is driven.
#[derive(Debug, Clone)]
enum Via {
    /// A source wire (no pip).
    Source { wire: String },
    /// An interconnect pip: chipsmith's to route.
    Graph,
    /// Anything else, kept as nextpnr chose it.
    Kept { wire: String, pip: String },
}

/// One net of nextpnr's routed design.
struct Net {
    name: String,
    /// (wire, how it is driven, its driver)
    entries: Vec<(Key, Via, Option<Key>)>,
}

/// One segment: a net's stretch through the interconnect.
#[derive(Debug, Clone)]
struct Segment {
    net: usize,
    source: NodeId,
    sinks: Vec<NodeId>,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub nets: usize,
    pub segments: usize,
    /// Interconnect pips in nextpnr's routing, and in chipsmith's.
    pub nextpnr_pips: usize,
    pub pips: usize,
    /// Entries of nextpnr's routing kept as they were.
    pub kept: usize,
    pub iterations: usize,
    pub route_seconds: f64,
    /// Interconnect pips by the wire class they drive, chipsmith's and
    /// nextpnr's.
    pub by_class: Vec<(String, usize, usize)>,
}

/// Wire and pip names to the graph's indices, per tile type.
struct Names<'a> {
    db: &'a ChipDb,
    wires: Vec<HashMap<&'a str, u32>>,
    pips: Vec<HashMap<(u32, u32), u32>>,
}

impl<'a> Names<'a> {
    fn new(db: &'a ChipDb) -> Self {
        let wires = db
            .tile_types
            .iter()
            .map(|tt| {
                (0..tt.wires.len() as u32)
                    .map(|w| (tt.wires.get(w), w))
                    .collect()
            })
            .collect();
        let pips = db
            .tile_types
            .iter()
            .map(|tt| {
                tt.pips
                    .iter()
                    .enumerate()
                    .map(|(i, &(s, d))| ((s, d), i as u32))
                    .collect()
            })
            .collect();
        Names { db, wires, pips }
    }

    fn tile_wire(&self, tile: u32, name: &str) -> Option<u32> {
        self.wires[self.db.tile_type_index(tile) as usize]
            .get(name)
            .copied()
    }

    /// `X<x>Y<y>/<name>`; nextpnr's site wires are `SITEWIRE/<site>/<name>`.
    fn wire(&self, name: &str) -> Key {
        let node = name.split_once('/').and_then(|(tile, wire)| {
            let tile = self.db.tile_by_name(tile)?;
            Some(self.db.node(tile, self.tile_wire(tile, wire)?))
        });
        match node {
            Some(n) => Key::Node(n),
            None => Key::Other(name.to_string()),
        }
    }

    /// `X<x>Y<y>/<dst>/<src>`: (src, dst, the graph's pip if it is one).
    fn pip(&self, name: &str) -> Result<(Key, Key, Option<Edge>)> {
        let mut parts = name.splitn(3, '/');
        let (Some(tile_name), Some(dst), Some(src)) = (parts.next(), parts.next(), parts.next())
        else {
            bail!("pip name {name}");
        };
        let tile = self
            .db
            .tile_by_name(tile_name)
            .with_context(|| format!("pip {name}: no tile"))?;
        let key = |w: &str| match self.tile_wire(tile, w) {
            Some(i) => Key::Node(self.db.node(tile, i)),
            None => Key::Other(format!("{tile_name}/{w}")),
        };
        let edge = match (self.tile_wire(tile, src), self.tile_wire(tile, dst)) {
            (Some(s), Some(d)) => self.pips[self.db.tile_type_index(tile) as usize]
                .get(&(s, d))
                .map(|&pip| Edge {
                    dst: self.db.node(tile, d),
                    tile,
                    pip,
                }),
            _ => None,
        };
        Ok((key(src), key(dst), edge))
    }
}

/// The top module of nextpnr's JSON: the one marked `top`, else the only one.
fn top_module(json: &mut Value) -> Result<&mut Value> {
    let modules = json["modules"].as_object_mut().context("no modules")?;
    let name = modules
        .iter()
        .find(|(_, m)| {
            m["attributes"]["top"]
                .as_str()
                .is_some_and(|t| t.trim_start_matches('0') == "1")
        })
        .or_else(|| (modules.len() == 1).then(|| modules.iter().next().unwrap()))
        .map(|(n, _)| n.clone())
        .context("no top module")?;
    Ok(modules.get_mut(&name).unwrap())
}

fn read_nets(names: &Names, top: &Value) -> Result<Vec<Net>> {
    let mut nets = Vec::new();
    for (name, net) in top["netnames"].as_object().context("no netnames")? {
        // nextpnr writes " " for a net it didn't route (no sinks).
        let Some(routing) = net["attributes"]["ROUTING"]
            .as_str()
            .filter(|r| !r.trim().is_empty())
        else {
            continue;
        };
        let mut f: Vec<&str> = routing.split(';').collect();
        // nextpnr writes no `;` after the last entry; chipsmith's does.
        if f.len() % 3 == 1 && f.last() == Some(&"") {
            f.pop();
        }
        ensure!(f.len().is_multiple_of(3), "net {name}: malformed ROUTING");
        let mut entries = Vec::new();
        for e in f.as_chunks::<3>().0 {
            let (wire, pip) = (e[0], e[1]);
            if pip.is_empty() {
                entries.push((
                    names.wire(wire),
                    Via::Source {
                        wire: wire.to_string(),
                    },
                    None,
                ));
                continue;
            }
            let (src, dst, edge) = names.pip(pip)?;
            let via = match edge {
                Some(_) => Via::Graph,
                None => Via::Kept {
                    wire: wire.to_string(),
                    pip: pip.to_string(),
                },
            };
            entries.push((dst, via, Some(src)));
        }
        nets.push(Net {
            name: name.clone(),
            entries,
        });
    }
    Ok(nets)
}

/// The segments of one net: walk its tree from each source, starting a
/// segment wherever the route enters the interconnect from outside it.
fn segments(net_index: usize, net: &Net) -> Result<Vec<Segment>> {
    let mut children: HashMap<&Key, Vec<(&Key, bool)>> = HashMap::new();
    let mut roots = Vec::new();
    for (key, via, from) in &net.entries {
        match (via, from) {
            (Via::Source { .. }, _) => roots.push(key),
            (via, Some(from)) => children
                .entry(from)
                .or_default()
                .push((key, matches!(via, Via::Graph))),
            (_, None) => unreachable!("pips have a source"),
        }
    }
    // nextpnr names a site wire one way as a source (`SITEWIRE/<site>/<wire>`)
    // and another in pips (`X<x>Y<y>/<site in tile>.<wire>`), so a pip source
    // that no pip drives is a root too.
    let driven: std::collections::HashSet<&Key> = net
        .entries
        .iter()
        .filter(|(_, via, _)| !matches!(via, Via::Source { .. }))
        .map(|(k, _, _)| k)
        .collect();
    for from in children.keys() {
        if !driven.contains(from) && !roots.contains(from) {
            roots.push(from);
        }
    }
    roots.sort_by_key(|k| format!("{k:?}"));
    roots.dedup();
    let mut out: Vec<Segment> = Vec::new();
    // (wire, segment it is in, entered through the graph)
    let mut stack: Vec<(&Key, Option<usize>, bool)> =
        roots.iter().map(|&k| (k, None, false)).collect();
    while let Some((key, segment, through_graph)) = stack.pop() {
        let segment = match key {
            Key::Node(n) => {
                let s = if through_graph {
                    segment.context("graph pip from outside the graph")?
                } else {
                    out.push(Segment {
                        net: net_index,
                        source: *n,
                        sinks: Vec::new(),
                    });
                    out.len() - 1
                };
                let kids = children.get(key).map(Vec::as_slice).unwrap_or(&[]);
                // Leaving the interconnect here, or ending here: a sink.
                if kids.is_empty() || kids.iter().any(|&(_, graph)| !graph) {
                    out[s].sinks.push(*n);
                }
                Some(s)
            }
            Key::Other(_) => None,
        };
        for &(child, graph) in children.get(key).map(Vec::as_slice).unwrap_or(&[]) {
            stack.push((child, segment, graph));
        }
    }
    for s in &mut out {
        let source = s.source;
        s.sinks.retain(|&n| n != source);
        s.sinks.sort();
        s.sinks.dedup();
    }
    out.retain(|s| !s.sinks.is_empty());
    Ok(out)
}

/// Every route is a tree of the graph's pips from its source to all its
/// sinks, and no node carries two routes.
fn check(db: &ChipDb, nets: &[NetReq], trees: &[super::Tree]) -> Result<()> {
    let mut used: HashMap<NodeId, usize> = HashMap::new();
    for (i, (net, tree)) in nets.iter().zip(trees).enumerate() {
        let mut reached = std::collections::HashSet::from([net.source]);
        // `pips` is in the order the tree grew, so each pip's source is
        // already in the tree when it is added.
        for (n, e) in tree.nodes[1..].iter().zip(&tree.pips) {
            ensure!(e.dst == *n, "net {}: pip out of order", net.name);
            let src = db.node(e.tile, db.tile_type(e.tile).pips[e.pip as usize].0);
            ensure!(
                reached.contains(&src) && db.downhill(src).contains(e),
                "net {}: {} is not driven from the route",
                net.name,
                db.pip_name(e.tile, e.pip)
            );
            reached.insert(*n);
        }
        for s in &net.sinks {
            ensure!(
                reached.contains(s),
                "net {}: a sink is not reached",
                net.name
            );
        }
        for &n in &tree.nodes {
            if let Some(other) = used.insert(n, i) {
                bail!("nets {} and {} share node {n}", nets[other].name, net.name);
            }
        }
    }
    Ok(())
}

/// nextpnr writes a site wire as `SITEWIRE/<site>/<wire>` but reads it
/// back only as `X<x>Y<y>/<relative site>.<wire>`; other names are left as
/// they are.
fn readable_wire(db: &ChipDb, name: &str) -> Result<String> {
    let Some(rest) = name.strip_prefix("SITEWIRE/") else {
        return Ok(name.to_string());
    };
    let (site, wire) = rest.split_once('/').context("site wire name")?;
    let s = db
        .sites
        .get(site)
        .with_context(|| format!("site wire {name}: no site {site}"))?;
    let (x, y) = db.tile_xy(s.tile);
    Ok(format!("X{x}Y{y}/{}.{wire}", s.relative))
}

/// nextpnr writes a cell's bel as `<site>/<bel>` (`SLICE_X30Y39/D6LUT`) but
/// finds it only as `X<x>Y<y>/<relative site>.<bel>`, with `~<variant>`
/// after a bel of a site variant other than the site's primary type
/// (`IOB_X0Y1.IOB33.OUTBUF~IOB33` in an IOB33M site). Bels of tiles without
/// sites (`X0Y0/PSEUDO_GND`) are named that way already.
pub(crate) fn bel_name(db: &ChipDb, bel: &str) -> Result<String> {
    let (site, name) = bel.split_once('/').context("bel name")?;
    if db.tile_by_name(site).is_some() {
        return Ok(bel.to_string());
    }
    let s = db
        .sites
        .get(site)
        .with_context(|| format!("bel {bel}: no site {site}"))?;
    let (x, y) = db.tile_xy(s.tile);
    let mut out = format!("X{x}Y{y}/{}.{name}", s.relative);
    if let Some((variant, _)) = name.split_once('.') {
        if variant != s.ty {
            out += &format!("~{variant}");
        }
    }
    Ok(out)
}

/// Check that nextpnr's routed design `routed` has every cell on the bel
/// chipsmith's placer gave it (`placement`, `-o preplaced` lines): nextpnr
/// binds them without complaint even where it would have to move one, so a
/// placement it didn't take shows up only here. Returns how many cells
/// were checked; 0 for an empty placement (nextpnr placed the design).
pub fn check_placement(db: &ChipDb, routed: &Path, placement: &Path) -> Result<usize> {
    let ours = std::fs::read_to_string(placement)
        .with_context(|| format!("reading {}", placement.display()))?;
    let ours: HashMap<&str, &str> = ours.lines().filter_map(|l| l.split_once('\t')).collect();
    if ours.is_empty() {
        return Ok(0);
    }
    let text =
        std::fs::read_to_string(routed).with_context(|| format!("reading {}", routed.display()))?;
    let mut json: Value = serde_json::from_str(&text)?;
    let cells = top_module(&mut json)?["cells"]
        .as_object()
        .context("no cells")?;
    let mut moved = Vec::new();
    for (cell, bel) in &ours {
        let theirs = cells
            .get(*cell)
            .with_context(|| format!("nextpnr's design has no cell {cell}"))?["attributes"]
            ["NEXTPNR_BEL"]
            .as_str()
            .with_context(|| format!("nextpnr didn't place {cell}"))?;
        let theirs = bel_name(db, theirs)?;
        if theirs != *bel {
            moved.push(format!("{cell}: {bel} -> {theirs}"));
        }
    }
    let unplaced = cells.len() - ours.len();
    if !moved.is_empty() || unplaced != 0 {
        moved.sort();
        moved.truncate(10);
        bail!(
            "nextpnr moved {} of chipsmith's cells and placed {unplaced} of its own: {}",
            moved.len(),
            moved.join("; ")
        );
    }
    Ok(ours.len())
}

/// Route nextpnr's routed design `routed` again: write the routes as
/// `-o prerouted` lines to `prerouted`, and every cell's bel as
/// `-o preplaced` lines to `preplaced`, so nextpnr can rebuild the same
/// design from the netlist with chipsmith's routing.
pub fn route_file(db: &ChipDb, routed: &Path, prerouted: &Path, preplaced: &Path) -> Result<Stats> {
    let text =
        std::fs::read_to_string(routed).with_context(|| format!("reading {}", routed.display()))?;
    let mut json: Value = serde_json::from_str(&text)?;
    let names = Names::new(db);
    let nets = read_nets(&names, top_module(&mut json)?)?;

    let mut stats = Stats {
        nets: nets.len(),
        ..Default::default()
    };
    let mut segs: Vec<Segment> = Vec::new();
    for (i, net) in nets.iter().enumerate() {
        segs.extend(segments(i, net).with_context(|| format!("net {}", net.name))?);
        for (_, via, _) in &net.entries {
            match via {
                Via::Graph => stats.nextpnr_pips += 1,
                Via::Kept { .. } => stats.kept += 1,
                Via::Source { .. } => {}
            }
        }
    }
    stats.segments = segs.len();

    // Each segment is a net to the router; timing nodes are only names
    // here (no cell delays yet), so routing is congestion-driven.
    let mut graph = timing::Graph::default();
    let reqs: Vec<NetReq> = segs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let tag = format!("{}#{i}", nets[s.net].name);
            NetReq {
                name: tag.clone(),
                source: s.source,
                source_wire: (0, 0),
                driver: graph.node(&tag, "source"),
                sink_ports: (0..s.sinks.len())
                    .map(|k| graph.node(&tag, &format!("sink{k}")))
                    .collect(),
                direct_ports: Vec::new(),
                sinks: s.sinks.clone(),
            }
        })
        .collect();
    let design = Design {
        nets: reqs,
        fixed_inputs: Default::default(),
        timing: graph,
    };
    let started = Instant::now();
    let mut router = Router::new(db, &design);
    for (i, s) in segs.iter().enumerate() {
        for &n in std::iter::once(&s.source).chain(&s.sinks) {
            router.reserve(n, i as u32)?;
        }
    }
    let (trees, iterations, _) = router.route(&design.nets)?;
    stats.route_seconds = started.elapsed().as_secs_f64();
    check(db, &design.nets, &trees)?;
    stats.iterations = iterations;
    stats.pips = trees.iter().map(|t| t.pips.len()).sum();

    // Pips by the class of the wire they drive, ours and nextpnr's.
    let mut by_class: HashMap<&str, (usize, usize)> = HashMap::new();
    let class = |n: NodeId| db.wire_types[db.node_type[n as usize] as usize].as_str();
    for t in &trees {
        for e in &t.pips {
            by_class.entry(class(e.dst)).or_default().0 += 1;
        }
    }
    for net in &nets {
        for (key, via, _) in &net.entries {
            if let (Key::Node(n), Via::Graph) = (key, via) {
                by_class.entry(class(*n)).or_default().1 += 1;
            }
        }
    }
    stats.by_class = by_class
        .into_iter()
        .map(|(c, (a, b))| (c.to_string(), a, b))
        .collect();
    stats
        .by_class
        .sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));

    // One prerouted line per net: nextpnr's sources and kept pips, then
    // the segments' pips.
    let mut seg_trees: Vec<Vec<usize>> = vec![Vec::new(); nets.len()];
    for (i, s) in segs.iter().enumerate() {
        seg_trees[s.net].push(i);
    }
    let mut lines = String::new();
    for (i, net) in nets.iter().enumerate() {
        let mut route = String::new();
        for (_, via, _) in &net.entries {
            match via {
                Via::Source { wire } => route += &format!("{};;1;", readable_wire(db, wire)?),
                Via::Kept { wire, pip } => {
                    route += &format!("{};{pip};1;", readable_wire(db, wire)?)
                }
                Via::Graph => {}
            }
        }
        for &s in &seg_trees[i] {
            for e in &trees[s].pips {
                route += &format!(
                    "{};{};1;",
                    db.wire_name(e.tile, db.pip_dst_wire(e.tile, e.pip)),
                    db.pip_name(e.tile, e.pip)
                );
            }
        }
        lines += &format!("{}\t{route}\n", net.name);
    }
    std::fs::write(prerouted, lines).with_context(|| format!("writing {}", prerouted.display()))?;

    // Every cell on the bel nextpnr gave it, for `-o preplaced`.
    let mut cells = String::new();
    for (name, cell) in top_module(&mut json)?["cells"]
        .as_object()
        .context("no cells")?
    {
        if let Some(bel) = cell["attributes"]["NEXTPNR_BEL"].as_str() {
            cells += &format!("{name}\t{}\n", bel_name(db, bel)?);
        }
    }
    std::fs::write(preplaced, cells).with_context(|| format!("writing {}", preplaced.display()))?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: Key, via: Via, from: Option<Key>) -> (Key, Via, Option<Key>) {
        (key, via, from)
    }

    #[test]
    fn site_names_are_spelled_the_way_nextpnr_reads_them() {
        let mut db = ChipDb::parse(super::super::chipdb::tests::TINY.as_bytes()).unwrap();
        let site = |relative: &str, ty: &str| super::super::chipdb::Site {
            tile: 1,
            relative: relative.into(),
            ty: ty.into(),
        };
        db.sites
            .insert("SLICE_X30Y39".into(), site("SLICE_X0Y0", "SLICEL"));
        db.sites
            .insert("IOB_X0Y54".into(), site("IOB_X0Y1", "IOB33M"));
        assert_eq!(
            bel_name(&db, "SLICE_X30Y39/D6LUT").unwrap(),
            "X1Y0/SLICE_X0Y0.D6LUT"
        );
        assert_eq!(
            bel_name(&db, "IOB_X0Y54/IOB33M.PAD").unwrap(),
            "X1Y0/IOB_X0Y1.IOB33M.PAD"
        );
        assert_eq!(
            bel_name(&db, "IOB_X0Y54/IOB33.OUTBUF").unwrap(),
            "X1Y0/IOB_X0Y1.IOB33.OUTBUF~IOB33"
        );
        assert_eq!(bel_name(&db, "X0Y0/PSEUDO_GND").unwrap(), "X0Y0/PSEUDO_GND");
        assert_eq!(
            readable_wire(&db, "SITEWIRE/SLICE_X30Y39/DQ").unwrap(),
            "X1Y0/SLICE_X0Y0.DQ"
        );
        assert_eq!(readable_wire(&db, "X1Y0/A").unwrap(), "X1Y0/A");
    }

    #[test]
    fn a_route_through_a_site_splits_into_two_segments() {
        // site out -> [exit] -> n1 -> n2 -> [into a site] ; n2 -> n3 -> [kept
        // route-through] -> n4 -> n5 -> [into a site]
        let o = |s: &str| Key::Other(s.into());
        let kept = || Via::Kept {
            wire: String::new(),
            pip: String::new(),
        };
        let net = Net {
            name: "n".into(),
            entries: vec![
                entry(o("out"), Via::Source { wire: "out".into() }, None),
                entry(Key::Node(1), kept(), Some(o("out"))),
                entry(Key::Node(2), Via::Graph, Some(Key::Node(1))),
                entry(o("in2"), kept(), Some(Key::Node(2))),
                entry(Key::Node(3), Via::Graph, Some(Key::Node(2))),
                entry(Key::Node(4), kept(), Some(Key::Node(3))),
                entry(Key::Node(5), Via::Graph, Some(Key::Node(4))),
                entry(o("in5"), kept(), Some(Key::Node(5))),
            ],
        };
        let mut segs = segments(0, &net).unwrap();
        segs.sort_by_key(|s| s.source);
        assert_eq!(segs.len(), 2);
        assert_eq!((segs[0].source, segs[0].sinks.clone()), (1, vec![2, 3]));
        assert_eq!((segs[1].source, segs[1].sinks.clone()), (4, vec![5]));
    }
}
