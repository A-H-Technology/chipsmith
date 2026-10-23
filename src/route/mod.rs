//! chipsmith's router: PathFinder (negotiated congestion) over the chip
//! database's routing graph.
//!
//! Input is a placed design in nextpnr's JSON (`nextpnr-himbaechel
//! --no-route --write`), output the same JSON with every net's `ROUTING`
//! filled in the way nextpnr writes it, so `cram pack` reads either.

pub mod chipdb;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

use chipdb::{ChipDb, Edge, NodeId};

/// One net to route: where it starts and every node it must reach.
#[derive(Debug, Clone)]
struct NetReq {
    /// The name that carries `ROUTING` in the output.
    name: String,
    source: NodeId,
    /// The source as a tile wire, for the first `ROUTING` entry.
    source_wire: (u32, u32),
    sinks: Vec<NodeId>,
}

/// A routed net: its source and, for every other node, the pip that drives it.
#[derive(Debug, Default, Clone)]
struct Tree {
    nodes: Vec<NodeId>,
    pips: Vec<Edge>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub nets: usize,
    pub pips: usize,
    pub iterations: usize,
}

/// Route `placed`, write `routed`.
pub fn route_file(db: &ChipDb, placed: &Path, routed: &Path) -> Result<Stats> {
    let text =
        std::fs::read_to_string(placed).with_context(|| format!("reading {}", placed.display()))?;
    let mut json: Value = serde_json::from_str(&text)?;
    let design = Design::read(db, &json)?;
    let mut router = Router::new(db, &design);
    let (trees, iterations) = router.route(&design.nets)?;

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
    })
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
    /// The driving tile wire and its node.
    source: Option<((u32, u32), NodeId)>,
    sinks: Vec<NodeId>,
}

/// What the router needs from the placed design.
struct Design {
    nets: Vec<NetReq>,
    /// LEs whose LUT inputs can't be permuted: (tile, LE index).
    fixed_inputs: HashSet<(u32, u8)>,
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
                let e = ends.entry(net).or_default();
                if output {
                    ensure!(
                        e.source.is_none(),
                        "net {net} has two drivers (one is {name}.{port})"
                    );
                    e.source = Some(((tile, wire), node));
                } else {
                    e.sinks.push(node);
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
            let Some((source_wire, source)) = source else {
                ensure!(
                    sinks.is_empty(),
                    "net {name} has loads but no driver in the fabric"
                );
                continue;
            };
            sinks.retain(|&s| s != source);
            sinks.sort();
            sinks.dedup();
            if sinks.is_empty() {
                continue;
            }
            nets.push(NetReq {
                name,
                source,
                source_wire,
                sinks,
            });
        }
        Ok(Design { nets, fixed_inputs })
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
const ASTAR: f32 = 0.6;

struct Router<'a> {
    db: &'a ChipDb,
    /// Nets using each node.
    occ: Vec<u16>,
    hist: Vec<f32>,
    present_factor: f32,
    // Search scratch, valid where `stamp == generation`.
    cost: Vec<f32>,
    via: Vec<Option<(NodeId, Edge)>>,
    stamp: Vec<u32>,
    generation: u32,
    in_tree: Vec<u32>,
    tree_generation: u32,
    /// Per tile type, per pip: the LE whose LUT input it permutes.
    permutes: Vec<Option<Vec<i8>>>,
    fixed_inputs: &'a HashSet<(u32, u8)>,
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
            cost: vec![0.0; n],
            via: vec![None; n],
            stamp: vec![0; n],
            generation: 0,
            in_tree: vec![0; n],
            tree_generation: 0,
            permutes: db.tile_types.iter().map(permuting_pips).collect(),
            fixed_inputs: &design.fixed_inputs,
        }
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
        let (ax, ay) = self.db.node_xy[a as usize];
        let (bx, by) = self.db.node_xy[b as usize];
        // Tile x counts routing and logic columns separately: halve it.
        (ax.abs_diff(bx) as f32) / 2.0 + ay.abs_diff(by) as f32
    }

    fn route(&mut self, nets: &[NetReq]) -> Result<(Vec<Tree>, usize)> {
        let mut trees: Vec<Tree> = vec![Tree::default(); nets.len()];
        for iteration in 1..=MAX_ITERATIONS {
            for (i, net) in nets.iter().enumerate() {
                let congested = trees[i].nodes.iter().any(|&n| self.occ[n as usize] > 1);
                if iteration > 1 && !congested {
                    continue;
                }
                for &n in &trees[i].nodes {
                    self.occ[n as usize] -= 1;
                }
                trees[i] = self.route_net(net)?;
                for &n in &trees[i].nodes {
                    self.occ[n as usize] += 1;
                }
            }
            let overused: Vec<usize> = (0..self.occ.len()).filter(|&n| self.occ[n] > 1).collect();
            if overused.is_empty() {
                return Ok((trees, iteration));
            }
            for n in overused {
                self.hist[n] += (self.occ[n] - 1) as f32;
            }
            self.present_factor *= 1.8;
        }
        bail!("routing did not converge in {MAX_ITERATIONS} iterations")
    }

    fn route_net(&mut self, net: &NetReq) -> Result<Tree> {
        self.tree_generation += 1;
        let mut tree = Tree {
            nodes: vec![net.source],
            pips: Vec::new(),
        };
        self.in_tree[net.source as usize] = self.tree_generation;

        let mut sinks = net.sinks.clone();
        sinks.sort_by(|&a, &b| {
            self.distance(net.source, a)
                .total_cmp(&self.distance(net.source, b))
        });
        for sink in sinks {
            if self.in_tree[sink as usize] == self.tree_generation {
                continue;
            }
            self.search(&tree.nodes, sink)
                .with_context(|| format!("net {}: no route to a sink", net.name))?;
            // Walk back from the sink to the tree, adding nodes and pips.
            let mut n = sink;
            while self.in_tree[n as usize] != self.tree_generation {
                self.in_tree[n as usize] = self.tree_generation;
                tree.nodes.push(n);
                let (from, edge) = self.via[n as usize].expect("searched nodes have a predecessor");
                tree.pips.push(edge);
                n = from;
            }
        }
        Ok(tree)
    }

    /// A* from every node of `tree` to `sink`; fills `via`.
    fn search(&mut self, tree: &[NodeId], sink: NodeId) -> Result<()> {
        self.generation += 1;
        let gen = self.generation;
        let mut heap = BinaryHeap::new();
        for &n in tree {
            self.stamp[n as usize] = gen;
            self.cost[n as usize] = 0.0;
            self.via[n as usize] = None;
            heap.push(Queued {
                f: ASTAR * self.distance(n, sink),
                g: 0.0,
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
                if !self.usable(e) {
                    continue;
                }
                let m = e.dst as usize;
                let g2 = g + self.node_cost(e.dst);
                if self.stamp[m] == gen && self.cost[m] <= g2 {
                    continue;
                }
                self.stamp[m] = gen;
                self.cost[m] = g2;
                self.via[m] = Some((node, *e));
                heap.push(Queued {
                    f: g2 + ASTAR * self.distance(e.dst, sink),
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
                },
                NetReq {
                    name: "b".into(),
                    source: node("S1"),
                    source_wire: (0, 1),
                    sinks: vec![node("T1")],
                },
            ],
            fixed_inputs: HashSet::new(),
        };
        let mut router = Router::new(&db, &design);
        let (trees, _) = router.route(&design.nets).unwrap();
        assert!(trees[0].nodes.contains(&node("N")));
        assert!(trees[1].nodes.contains(&node("M")));
        assert!(router.occ.iter().all(|&o| o <= 1));
    }
}
