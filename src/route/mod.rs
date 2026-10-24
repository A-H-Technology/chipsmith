//! chipsmith's router: PathFinder (negotiated congestion) over the chip
//! database's routing graph.
//!
//! Input is a placed design in nextpnr's JSON (`nextpnr-himbaechel
//! --no-route --write`), output the same JSON with every net's `ROUTING`
//! filled in the way nextpnr writes it, so `cram pack` reads either.

pub mod chipdb;
pub mod xc7;
pub mod xray;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

use chipdb::{ChipDb, Edge, NodeId};

use crate::timing::{self, Arc, Ps};

/// One net to route: where it starts and every node it must reach.
#[derive(Debug, Clone)]
struct NetReq {
    /// The name that carries `ROUTING` in the output.
    name: String,
    source: NodeId,
    /// The source as a tile wire, for the first `ROUTING` entry.
    source_wire: (u32, u32),
    sinks: Vec<NodeId>,
    /// The driving cell port's timing node.
    driver: u32,
    /// Each sink's cell port, as a timing node.
    sink_ports: Vec<u32>,
    /// Cell ports on the source's own wire, reached without routing: a
    /// carry chain's COUT is the next LE's CIN.
    direct_ports: Vec<u32>,
}

/// A routed net: its source and, for every other node, the pip that drives it.
#[derive(Debug, Default, Clone)]
struct Tree {
    nodes: Vec<NodeId>,
    pips: Vec<Edge>,
    /// Routing delay from the source to each sink, in `NetReq::sinks` order.
    sink_delay: Vec<Ps>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub nets: usize,
    pub pips: usize,
    pub iterations: usize,
    /// Estimated minimum clock period (critical register-to-register path).
    pub critical: Ps,
}

/// Route `placed`, write `routed`.
pub fn route_file(db: &ChipDb, placed: &Path, routed: &Path) -> Result<Stats> {
    let text =
        std::fs::read_to_string(placed).with_context(|| format!("reading {}", placed.display()))?;
    let mut json: Value = serde_json::from_str(&text)?;
    let design = Design::read(db, &json)?;
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
    Ok(Stats {
        nets: design.nets.len(),
        pips,
        iterations,
        critical: analysis.critical,
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
/// no pip. (cram only reads the pips.)
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

/// A net's endpoints in the graph, as cells name them.
#[derive(Default)]
struct Ends {
    /// The driving tile wire, its node and its timing node.
    source: Option<((u32, u32), NodeId, u32)>,
    /// (node, timing node)
    sinks: Vec<(NodeId, u32)>,
}

/// What the router needs from the placed design.
struct Design {
    nets: Vec<NetReq>,
    /// LEs whose LUT inputs can't be permuted: (tile, LE index).
    fixed_inputs: HashSet<(u32, u8)>,
    timing: timing::Graph,
}

fn tile_xy(name: &str) -> Option<(u32, u32)> {
    let (x, y) = name.strip_prefix('X')?.split_once('Y')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

impl Design {
    fn read(db: &ChipDb, json: &Value) -> Result<Self> {
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
            timing.add_cell(name, cell["type"].as_str().unwrap_or(""), &connected, le);
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
                // Ports with no wire (the pad side of an I/O) aren't routed.
                let Some(&wire) = pins.get(port) else {
                    continue;
                };
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

        // One name per net carries ROUTING; cram adds up every name's pips.
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
}

#[derive(Clone, Copy, PartialEq)]
struct Queued {
    f: f32,
    g: f32,
    node: NodeId,
}

impl Eq for Queued {}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap on f.
        other.f.total_cmp(&self.f)
    }
}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

const MAX_ITERATIONS: usize = 60;
/// Weight of the distance estimate. Above what a single hop can cover, so
/// searches head for the sink instead of flooding the chip; the price is
/// that a route may be a little longer than the shortest.
const ASTAR: f32 = 1.5;
/// Delays are costed in units of an R4 wire.
const DELAY_UNIT: Ps = 235.0;
/// Delay per tile of distance, for the A* estimate: an R4 spans 4 LABs.
const HOP_DELAY_ESTIMATE: Ps = 60.0;
/// Criticality is sharpened (VPR's `criticality_exp`) and capped below 1
/// so even the most critical connection still sees congestion.
const CRIT_EXP: f32 = 4.0;
const MAX_CRITICALITY: f32 = 0.95;
/// Nets with a connection this critical are rerouted every iteration, not
/// only when congested, so they can follow the timing as it changes.
const REROUTE_CRITICALITY: f32 = 0.8;

struct Router<'a> {
    db: &'a ChipDb,
    /// Nets using each node.
    occ: Vec<u16>,
    hist: Vec<f32>,
    present_factor: f32,
    /// Routing delay of each node.
    delay: Vec<Ps>,
    // Search scratch, valid where `stamp == generation`.
    cost: Vec<f32>,
    via: Vec<Option<(NodeId, Edge)>>,
    stamp: Vec<u32>,
    generation: u32,
    in_tree: Vec<u32>,
    tree_generation: u32,
    /// Delay from the source, for nodes of the tree being built.
    delay_to: Vec<Ps>,
    /// Per tile type, per pip: the LE whose LUT input it permutes.
    permutes: Vec<Option<Vec<i8>>>,
    fixed_inputs: &'a HashSet<(u32, u8)>,
    timing: &'a timing::Graph,
    /// `ChipDb::tile_distance`, or `CHIPSMITH_TILE_DISTANCE=<x>,<y>` for
    /// tuning it.
    tile_distance: [f32; 2],
    /// Nodes only one net may use (by index), e.g. the ends of routes
    /// another tool has fixed. Empty when nothing is reserved.
    owner: Vec<u32>,
}

fn permuting_pips(tt: &chipdb::TileType) -> Option<Vec<i8>> {
    // LE<k>.<P> -> LE<k>.I<Q> with P != Q.
    let le_pin = |name: &str| -> Option<(i8, String)> {
        let (bel, pin) = name.strip_prefix("LE")?.split_once('.')?;
        Some((bel.parse().ok()?, pin.to_string()))
    };
    let mut out = vec![-1i8; tt.pips.len()];
    let mut any = false;
    for (i, &(s, d)) in tt.pips.iter().enumerate() {
        let (Some((ks, p)), Some((kd, q))) = (le_pin(tt.wires.get(s)), le_pin(tt.wires.get(d)))
        else {
            continue;
        };
        if ks == kd && p.len() == 1 && q.len() == 2 && q.starts_with('I') && q[1..] != p {
            out[i] = kd;
            any = true;
        }
    }
    any.then_some(out)
}

impl<'a> Router<'a> {
    fn new(db: &'a ChipDb, design: &'a Design) -> Self {
        let n = db.nodes();
        Router {
            db,
            occ: vec![0; n],
            hist: vec![0.0; n],
            present_factor: 0.5,
            delay: {
                let by_type: Vec<Ps> = db
                    .wire_types
                    .iter()
                    .map(|t| timing::wire_delay(t))
                    .collect();
                db.node_type.iter().map(|&t| by_type[t as usize]).collect()
            },
            cost: vec![0.0; n],
            via: vec![None; n],
            stamp: vec![0; n],
            generation: 0,
            in_tree: vec![0; n],
            tree_generation: 0,
            delay_to: vec![0.0; n],
            permutes: db.tile_types.iter().map(permuting_pips).collect(),
            fixed_inputs: &design.fixed_inputs,
            timing: &design.timing,
            owner: Vec::new(),
            tile_distance: std::env::var("CHIPSMITH_TILE_DISTANCE")
                .ok()
                .and_then(|v| {
                    let (x, y) = v.split_once(',')?;
                    Some([x.parse().ok()?, y.parse().ok()?])
                })
                .unwrap_or(db.tile_distance),
        }
    }

    /// Keep `node` for net `net` alone.
    fn reserve(&mut self, node: NodeId, net: u32) -> Result<()> {
        if self.owner.is_empty() {
            self.owner = vec![u32::MAX; self.db.nodes()];
        }
        let o = &mut self.owner[node as usize];
        ensure!(
            *o == u32::MAX || *o == net,
            "node {node} is an end of two nets' routes"
        );
        *o = net;
        Ok(())
    }

    fn reserved_for_other(&self, node: NodeId, net: u32) -> bool {
        self.owner
            .get(node as usize)
            .is_some_and(|&o| o != u32::MAX && o != net)
    }

    fn usable(&self, e: &Edge) -> bool {
        let Some(permutes) = &self.permutes[self.db.tile_type_index(e.tile) as usize] else {
            return true;
        };
        let k = permutes[e.pip as usize];
        k < 0 || !self.fixed_inputs.contains(&(e.tile, k as u8))
    }

    fn node_cost(&self, n: NodeId) -> f32 {
        let n = n as usize;
        (1.0 + self.hist[n]) * (1.0 + self.present_factor * self.occ[n] as f32)
    }

    fn distance(&self, a: NodeId, b: NodeId) -> f32 {
        let (dx, dy) = self.db.node_box[a as usize].gap(self.db.node_box[b as usize]);
        let [kx, ky] = self.tile_distance;
        dx as f32 * kx + dy as f32 * ky
    }

    /// Net arcs of the current routing, for timing analysis: every routed
    /// sink in net and sink order, then the unrouted direct ones.
    fn net_arcs(nets: &[NetReq], trees: &[Tree]) -> Vec<Arc> {
        let direct = nets.iter().flat_map(|net| {
            net.direct_ports.iter().map(|&to| Arc {
                from: net.driver,
                to,
                delay: 0.0,
            })
        });
        nets.iter()
            .zip(trees)
            .flat_map(|(net, tree)| {
                net.sink_ports
                    .iter()
                    .zip(&tree.sink_delay)
                    .map(|(&to, &delay)| Arc {
                        from: net.driver,
                        to,
                        delay,
                    })
            })
            .chain(direct)
            .collect()
    }

    fn route(&mut self, nets: &[NetReq]) -> Result<(Vec<Tree>, usize, timing::Analysis)> {
        let mut trees: Vec<Tree> = vec![Tree::default(); nets.len()];
        // Per net, per sink: how critical its connection is. Nothing is
        // known before the first routing.
        let mut crit: Vec<Vec<f32>> = nets.iter().map(|n| vec![0.0; n.sinks.len()]).collect();
        for iteration in 1..=MAX_ITERATIONS {
            for (i, net) in nets.iter().enumerate() {
                let congested = trees[i].nodes.iter().any(|&n| self.occ[n as usize] > 1);
                let critical = crit[i].iter().any(|&c| c > REROUTE_CRITICALITY);
                if iteration > 1 && !congested && !critical {
                    continue;
                }
                for &n in &trees[i].nodes {
                    self.occ[n as usize] -= 1;
                }
                trees[i] = self.route_net(i as u32, net, &crit[i])?;
                for &n in &trees[i].nodes {
                    self.occ[n as usize] += 1;
                }
            }

            let arcs = Self::net_arcs(nets, &trees);
            let analysis = self.timing.analyse(&arcs);
            let mut k = 0;
            for c in crit.iter_mut() {
                for c in c.iter_mut() {
                    *c = (analysis.criticality(&arcs[k]).powf(CRIT_EXP)).min(MAX_CRITICALITY);
                    k += 1;
                }
            }

            let overused: Vec<usize> = (0..self.occ.len()).filter(|&n| self.occ[n] > 1).collect();
            if overused.is_empty() {
                return Ok((trees, iteration, analysis));
            }
            for n in overused {
                self.hist[n] += (self.occ[n] - 1) as f32;
            }
            self.present_factor *= 1.8;
        }
        bail!("routing did not converge in {MAX_ITERATIONS} iterations")
    }

    fn route_net(&mut self, index: u32, net: &NetReq, crit: &[f32]) -> Result<Tree> {
        self.tree_generation += 1;
        let mut tree = Tree {
            nodes: vec![net.source],
            pips: Vec::new(),
            sink_delay: Vec::new(),
        };
        self.in_tree[net.source as usize] = self.tree_generation;
        self.delay_to[net.source as usize] = self.delay[net.source as usize];

        // Most critical first, so they get the direct wires; then nearest.
        let mut order: Vec<usize> = (0..net.sinks.len()).collect();
        order.sort_by(|&a, &b| {
            crit[b].total_cmp(&crit[a]).then(
                self.distance(net.source, net.sinks[a])
                    .total_cmp(&self.distance(net.source, net.sinks[b])),
            )
        });
        for i in order {
            let sink = net.sinks[i];
            if self.in_tree[sink as usize] == self.tree_generation {
                continue;
            }
            self.search(index, &tree.nodes, sink, crit[i])
                .with_context(|| format!("net {}: no route to a sink", net.name))?;
            // Walk back from the sink to the tree, then add the branch.
            let mut branch = Vec::new();
            let mut n = sink;
            while self.in_tree[n as usize] != self.tree_generation {
                let (from, edge) = self.via[n as usize].expect("searched nodes have a predecessor");
                branch.push((n, edge));
                n = from;
            }
            let mut d = self.delay_to[n as usize];
            for &(n, edge) in branch.iter().rev() {
                d += self.delay[n as usize];
                self.delay_to[n as usize] = d;
                self.in_tree[n as usize] = self.tree_generation;
                tree.nodes.push(n);
                tree.pips.push(edge);
            }
        }
        tree.sink_delay = net
            .sinks
            .iter()
            .map(|&s| self.delay_to[s as usize])
            .collect();
        Ok(tree)
    }

    /// A* from every node of `tree` to `sink`; fills `via`. Each node costs
    /// `(1 - crit)` times its congestion cost plus `crit` times its delay
    /// (in units of an R4 wire, so both terms are about 1 per hop).
    fn search(&mut self, net: u32, tree: &[NodeId], sink: NodeId, crit: f32) -> Result<()> {
        self.generation += 1;
        let gen = self.generation;
        let mut heap = BinaryHeap::new();
        let per_hop = 1.0 - crit + crit * HOP_DELAY_ESTIMATE / DELAY_UNIT;
        for &n in tree {
            let g = crit * self.delay_to[n as usize] / DELAY_UNIT;
            self.stamp[n as usize] = gen;
            self.cost[n as usize] = g;
            self.via[n as usize] = None;
            heap.push(Queued {
                f: g + ASTAR * per_hop * self.distance(n, sink),
                g,
                node: n,
            });
        }
        while let Some(Queued { g, node, .. }) = heap.pop() {
            if node == sink {
                return Ok(());
            }
            if g > self.cost[node as usize] {
                continue;
            }
            for e in self.db.downhill(node) {
                if !self.usable(e) || self.reserved_for_other(e.dst, net) {
                    continue;
                }
                let m = e.dst as usize;
                let g2 =
                    g + (1.0 - crit) * self.node_cost(e.dst) + crit * self.delay[m] / DELAY_UNIT;
                if self.stamp[m] == gen && self.cost[m] <= g2 {
                    continue;
                }
                self.stamp[m] = gen;
                self.cost[m] = g2;
                self.via[m] = Some((node, *e));
                heap.push(Queued {
                    f: g2 + ASTAR * per_hop * self.distance(e.dst, sink),
                    g: g2,
                    node: e.dst,
                });
            }
        }
        bail!("unreachable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permuting_pips_are_the_cross_input_ones() {
        let db = ChipDb::parse(
            "chip 1 1\ntt LAB\nw LE3.A W\nw LE3.B W\nw LE3.IA W\nw LE3.IB W\nw LE3.COMB W\n\
             p LE3.A LE3.IA\np LE3.A LE3.IB\np LE3.B LE3.IA\np LE3.COMB LE3.IA\nend\nt 0 0 LAB\n"
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(permuting_pips(&db.tile_types[0]), Some(vec![-1, 3, 3, -1]));
    }

    #[test]
    fn two_nets_negotiate_for_a_shared_wire() {
        // Two sources, two sinks; the direct path of both runs through M.
        // Net 1 can only use M, so net 0 must take the detour through N.
        let db = ChipDb::parse(
            "chip 1 1\ntt T\nw S0 W\nw S1 W\nw M W\nw N W\nw T0 W\nw T1 W\n\
             p S0 M\np S0 N\np S1 M\np M T0\np M T1\np N T0\nend\nt 0 0 T\n"
                .as_bytes(),
        )
        .unwrap();
        let node = |w: &str| db.node(0, db.tile_type(0).wire(w).unwrap());
        let design = Design {
            nets: vec![
                NetReq {
                    name: "a".into(),
                    source: node("S0"),
                    source_wire: (0, 0),
                    sinks: vec![node("T0")],
                    driver: 0,
                    sink_ports: vec![1],
                    direct_ports: vec![],
                },
                NetReq {
                    name: "b".into(),
                    source: node("S1"),
                    source_wire: (0, 1),
                    sinks: vec![node("T1")],
                    driver: 2,
                    sink_ports: vec![3],
                    direct_ports: vec![],
                },
            ],
            fixed_inputs: HashSet::new(),
            timing: {
                let mut g = timing::Graph::default();
                for (c, p) in [("a", "O"), ("a", "I"), ("b", "O"), ("b", "I")] {
                    g.node(c, p);
                }
                g
            },
        };
        let mut router = Router::new(&db, &design);
        let (trees, _, _) = router.route(&design.nets).unwrap();
        assert!(trees[0].nodes.contains(&node("N")));
        assert!(trees[1].nodes.contains(&node("M")));
        assert!(router.occ.iter().all(|&o| o <= 1));
    }

    #[test]
    fn a_node_reserved_for_another_net_is_avoided() {
        // S0 reaches T0 directly through M, or through N; M is kept for net 1.
        let db = ChipDb::parse(
            "chip 1 1\ntt T\nw S0 W\nw M W\nw N W\nw T0 W\n\
             p S0 M\np S0 N\np M T0\np N T0\nend\nt 0 0 T\n"
                .as_bytes(),
        )
        .unwrap();
        let node = |w: &str| db.node(0, db.tile_type(0).wire(w).unwrap());
        let mut timing = timing::Graph::default();
        let design = Design {
            nets: vec![NetReq {
                name: "a".into(),
                source: node("S0"),
                source_wire: (0, 0),
                sinks: vec![node("T0")],
                driver: timing.node("a", "O"),
                sink_ports: vec![timing.node("a", "I")],
                direct_ports: vec![],
            }],
            fixed_inputs: HashSet::new(),
            timing,
        };
        let mut router = Router::new(&db, &design);
        router.reserve(node("M"), 1).unwrap();
        let (trees, _, _) = router.route(&design.nets).unwrap();
        assert!(trees[0].nodes.contains(&node("N")));
        assert!(!trees[0].nodes.contains(&node("M")));
        assert!(router.reserve(node("M"), 0).is_err());
    }
}
