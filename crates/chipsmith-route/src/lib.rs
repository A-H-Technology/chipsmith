//! chipsmith's router: PathFinder (negotiated congestion) over the chip
//! database's routing graph, shared by the Cyclone II and 7-series back
//! ends. Each device reads its placed design into a [`Design`] and writes
//! the routed trees back in its own format.

pub mod chipdb;
pub mod timing;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

use anyhow::{bail, ensure, Context, Result};

use chipdb::{ChipDb, Edge, NodeId};

use crate::timing::{Arc, Ps};

/// One net to route: where it starts and every node it must reach.
#[derive(Debug, Clone)]
pub struct NetReq {
    /// The name that carries `ROUTING` in the output.
    pub name: String,
    pub source: NodeId,
    /// The source as a tile wire, for the first `ROUTING` entry.
    pub source_wire: (u32, u32),
    pub sinks: Vec<NodeId>,
    /// The driving cell port's timing node.
    pub driver: u32,
    /// Each sink's cell port, as a timing node.
    pub sink_ports: Vec<u32>,
    /// Cell ports on the source's own wire, reached without routing: a
    /// carry chain's COUT is the next LE's CIN.
    pub direct_ports: Vec<u32>,
}

/// A routed net: its source and, for every other node, the pip that drives it.
#[derive(Debug, Default, Clone)]
pub struct Tree {
    pub nodes: Vec<NodeId>,
    pub pips: Vec<Edge>,
    /// Routing delay from the source to each sink, in `NetReq::sinks` order.
    pub sink_delay: Vec<Ps>,
    /// Each node's parent, as an index into `nodes` (the source's own).
    parent: Vec<u32>,
}

/// What the router needs from the placed design.
pub struct Design {
    pub nets: Vec<NetReq>,
    /// LEs whose LUT inputs can't be permuted: (tile, LE index).
    pub fixed_inputs: HashSet<(u32, u8)>,
    pub timing: timing::Graph,
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
/// `Router::bbox_margin`'s default, in tiles.
const BBOX_MARGIN: f32 = 3.0;
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

/// What the search reads of a node for every edge into it, side by side:
/// the search is bound by cache misses on random nodes, and one line per
/// node beats one per array.
#[derive(Clone, Copy)]
struct NodeInfo {
    /// Congestion history.
    hist: f32,
    /// Routing delay.
    delay: Ps,
    /// Nets using the node.
    occ: u16,
    bbox: chipdb::Box,
}

/// One node's search scratch, valid where `stamp` is the search's
/// generation.
#[derive(Clone, Copy, Default)]
struct Visit {
    cost: f32,
    stamp: u32,
    via: Option<(NodeId, Edge)>,
}

/// What one search writes, apart from the routing state every search
/// reads.
struct Scratch {
    visit: Vec<Visit>,
    generation: u32,
    in_tree: Vec<u32>,
    tree_generation: u32,
    /// Index into the tree being built, for nodes in it.
    tree_pos: Vec<u32>,
    /// Sinks whose branch survives a partial rip-up, where equal to
    /// `tree_generation`.
    kept_sink: Vec<u32>,
    /// Delay from the source, for nodes of the tree being built.
    delay_to: Vec<Ps>,
    /// Search work, for `CHIPSMITH_ROUTE_TRACE`: tree nodes seeded, nodes
    /// pushed while expanding, nodes expanded.
    work: [u64; 3],
}

impl Scratch {
    fn new(n: usize) -> Self {
        Scratch {
            visit: vec![Visit::default(); n],
            generation: 0,
            in_tree: vec![0; n],
            tree_generation: 0,
            tree_pos: vec![0; n],
            kept_sink: vec![0; n],
            delay_to: vec![0.0; n],
            work: [0; 3],
        }
    }
}

pub struct Router<'a> {
    db: &'a ChipDb,
    node: Vec<NodeInfo>,
    present_factor: f32,
    scratch: Scratch,
    /// Rip up only the congested and the critical branches of a net
    /// rather than all of it (`CHIPSMITH_ROUTE_INCREMENTAL=0` turns it off).
    incremental: bool,
    /// A net's searches stay within this many tiles of the box around its
    /// source and sinks (VPR's `bb_factor`), unless a sink can't be reached
    /// inside it (`CHIPSMITH_ROUTE_BBOX_MARGIN`; `inf`: the whole chip).
    bbox_margin: f32,
    /// `ASTAR`, or `CHIPSMITH_ROUTE_ASTAR` for tuning it.
    astar: f32,
    /// Per tile type, per pip: the LE whose LUT input it permutes.
    permutes: Vec<Option<Vec<i8>>>,
    /// Per tile, bit k: LE k's LUT inputs can't be permuted
    /// (`Design::fixed_inputs`).
    fixed_inputs: Vec<u64>,
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
    pub fn new(db: &'a ChipDb, design: &'a Design) -> Self {
        let n = db.nodes();
        Router {
            db,
            node: {
                let by_type: Vec<Ps> = db
                    .wire_types
                    .iter()
                    .map(|t| timing::wire_delay(t))
                    .collect();
                db.node_type
                    .iter()
                    .zip(&db.node_box)
                    .map(|(&t, &bbox)| NodeInfo {
                        hist: 0.0,
                        delay: by_type[t as usize],
                        occ: 0,
                        bbox,
                    })
                    .collect()
            },
            present_factor: 0.5,
            scratch: Scratch::new(n),
            incremental: std::env::var("CHIPSMITH_ROUTE_INCREMENTAL").map_or(true, |v| v != "0"),
            bbox_margin: std::env::var("CHIPSMITH_ROUTE_BBOX_MARGIN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(BBOX_MARGIN),
            astar: std::env::var("CHIPSMITH_ROUTE_ASTAR")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(ASTAR),
            permutes: db.tile_types.iter().map(permuting_pips).collect(),
            fixed_inputs: {
                let mut bits = vec![0u64; (db.width * db.height) as usize];
                for &(tile, k) in &design.fixed_inputs {
                    bits[tile as usize] |= 1 << k;
                }
                bits
            },
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
    pub fn reserve(&mut self, node: NodeId, net: u32) -> Result<()> {
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
        k < 0 || self.fixed_inputs[e.tile as usize] & (1 << k) == 0
    }

    fn node_cost(&self, n: NodeId) -> f32 {
        let n = &self.node[n as usize];
        (1.0 + n.hist) * (1.0 + self.present_factor * n.occ as f32)
    }

    fn distance(&self, a: NodeId, b: NodeId) -> f32 {
        let (dx, dy) = self.node[a as usize].bbox.gap(self.node[b as usize].bbox);
        let [kx, ky] = self.tile_distance;
        dx as f32 * kx + dy as f32 * ky
    }

    /// Net arcs of the current routing, for timing analysis: every routed
    /// sink in net and sink order, then the unrouted direct ones.
    pub fn net_arcs(nets: &[NetReq], trees: &[Tree]) -> Vec<Arc> {
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

    pub fn route(&mut self, nets: &[NetReq]) -> Result<(Vec<Tree>, usize, timing::Analysis)> {
        let mut trees: Vec<Tree> = vec![Tree::default(); nets.len()];
        // Per net, per sink: how critical its connection is. Nothing is
        // known before the first routing.
        let mut crit: Vec<Vec<f32>> = nets.iter().map(|n| vec![0.0; n.sinks.len()]).collect();
        let trace = std::env::var_os("CHIPSMITH_ROUTE_TRACE").is_some();
        let started = std::time::Instant::now();
        // Out of `self` while the searches borrow it; put back on the way
        // out (a failed search ends the routing).
        let mut scratch = std::mem::replace(&mut self.scratch, Scratch::new(0));
        for iteration in 1..=MAX_ITERATIONS {
            let t_iter = std::time::Instant::now();
            let mut rerouted = 0;
            let mut t_wide = 0.0;
            scratch.work = [0; 3];
            for (i, net) in nets.iter().enumerate() {
                let congested = trees[i].nodes.iter().any(|&n| self.node[n as usize].occ > 1);
                let critical = crit[i].iter().any(|&c| c > REROUTE_CRITICALITY);
                if iteration > 1 && !congested && !critical {
                    continue;
                }
                for &n in &trees[i].nodes {
                    self.node[n as usize].occ -= 1;
                }
                rerouted += 1;
                let t_net = std::time::Instant::now();
                let old = std::mem::take(&mut trees[i]);
                let keep = (self.incremental && iteration > 1).then_some(&old);
                trees[i] = self.route_net(&mut scratch, i as u32, net, &crit[i], keep)?;
                if net.sinks.len() >= 32 {
                    t_wide += t_net.elapsed().as_secs_f64();
                }
                for &n in &trees[i].nodes {
                    self.node[n as usize].occ += 1;
                }
            }

            let t_search = t_iter.elapsed().as_secs_f64();
            let arcs = Self::net_arcs(nets, &trees);
            let analysis = self.timing.analyse(&arcs);
            let mut k = 0;
            for c in crit.iter_mut() {
                for c in c.iter_mut() {
                    *c = (analysis.criticality(&arcs[k]).powf(CRIT_EXP)).min(MAX_CRITICALITY);
                    k += 1;
                }
            }

            let overused: Vec<usize> = (0..self.node.len()).filter(|&n| self.node[n].occ > 1).collect();
            if trace {
                eprintln!(
                    "route: iteration {iteration}: {rerouted} nets in {t_search:.2} s ({t_wide:.2} s on nets of 32 sinks or more; {:?} seeded, pushed, expanded), timing {:.2} s, {} overused, critical {:.2} ns, {:.1} s in all",
                    scratch.work,
                    t_iter.elapsed().as_secs_f64() - t_search,
                    overused.len(),
                    analysis.critical / 1000.0,
                    started.elapsed().as_secs_f64()
                );
            }
            if overused.is_empty() {
                self.scratch = scratch;
                return Ok((trees, iteration, analysis));
            }
            for n in overused {
                let n = &mut self.node[n];
                n.hist += (n.occ - 1) as f32;
            }
            self.present_factor *= 1.8;
        }
        self.scratch = scratch;
        bail!("routing did not converge in {MAX_ITERATIONS} iterations")
    }

    /// Route `net`, starting from what survives of `keep` (its previous
    /// tree, already ripped up from `occ`).
    fn route_net(
        &self,
        s: &mut Scratch,
        index: u32,
        net: &NetReq,
        crit: &[f32],
        keep: Option<&Tree>,
    ) -> Result<Tree> {
        s.tree_generation += 1;
        let mut tree = Tree {
            nodes: vec![net.source],
            pips: Vec::new(),
            sink_delay: Vec::new(),
            parent: vec![0],
        };
        s.in_tree[net.source as usize] = s.tree_generation;
        s.tree_pos[net.source as usize] = 0;
        s.delay_to[net.source as usize] = self.node[net.source as usize].delay;
        if let Some(old) = keep.filter(|t| !t.nodes.is_empty()) {
            self.keep_branches(s, net, crit, old, &mut tree);
        }

        let region = self.bbox_margin.is_finite().then(|| {
            let mut b = self.node[net.source as usize].bbox;
            for &n in &net.sinks {
                b = b.union(self.node[n as usize].bbox);
            }
            b.grow(self.bbox_margin.min(255.0) as u8)
        });
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
            if s.in_tree[sink as usize] == s.tree_generation {
                continue;
            }
            self.search(s, index, &tree.nodes, sink, crit[i], region)
                .with_context(|| format!("net {}: no route to a sink", net.name))?;
            // Walk back from the sink to the tree, then add the branch.
            let mut branch = Vec::new();
            let mut n = sink;
            while s.in_tree[n as usize] != s.tree_generation {
                let (from, edge) = s.visit[n as usize].via.expect("searched nodes have a predecessor");
                branch.push((n, edge));
                n = from;
            }
            let mut d = s.delay_to[n as usize];
            let mut parent = s.tree_pos[n as usize];
            for &(n, edge) in branch.iter().rev() {
                d += self.node[n as usize].delay;
                s.delay_to[n as usize] = d;
                s.in_tree[n as usize] = s.tree_generation;
                s.tree_pos[n as usize] = tree.nodes.len() as u32;
                parent = add_node(&mut tree, n, edge, parent);
            }
        }
        tree.sink_delay = net
            .sinks
            .iter()
            .map(|&sink| s.delay_to[sink as usize])
            .collect();
        Ok(tree)
    }

    /// Copy into `tree` (holding only the source) the branches of `old`
    /// that stay: those on no node another net uses that lead to a sink
    /// whose connection isn't critical enough to reroute. On a congested
    /// net with hundreds of sinks, one shared wire otherwise reroutes all
    /// of them.
    fn keep_branches(&self, s: &mut Scratch, net: &NetReq, crit: &[f32], old: &Tree, tree: &mut Tree) {
        let gen = s.tree_generation;
        for (&sink, &c) in net.sinks.iter().zip(crit) {
            if c <= REROUTE_CRITICALITY {
                s.kept_sink[sink as usize] = gen;
            }
        }
        let len = old.nodes.len();
        // Parents come before their children, so one pass down marks
        // what is uncongested all the way from the source, one pass up
        // what of that leads to a kept sink.
        let mut clear = vec![false; len];
        clear[0] = true;
        for i in 1..len {
            clear[i] = clear[old.parent[i] as usize] && self.node[old.nodes[i] as usize].occ == 0;
        }
        let mut needed = vec![false; len];
        for i in (1..len).rev() {
            if clear[i] && (needed[i] || s.kept_sink[old.nodes[i] as usize] == gen) {
                needed[i] = true;
                needed[old.parent[i] as usize] = true;
            }
        }
        let mut pos = vec![0u32; len];
        for i in 1..len {
            if !needed[i] {
                continue;
            }
            let n = old.nodes[i];
            let parent = pos[old.parent[i] as usize];
            pos[i] = add_node(tree, n, old.pips[i - 1], parent);
            s.in_tree[n as usize] = gen;
            s.tree_pos[n as usize] = pos[i];
            s.delay_to[n as usize] =
                s.delay_to[tree.nodes[parent as usize] as usize] + self.node[n as usize].delay;
        }
    }

    /// A* from every node of `tree` to `sink`; fills `via`. Each node costs
    /// `(1 - crit)` times its congestion cost plus `crit` times its delay
    /// (in units of an R4 wire, so both terms are about 1 per hop).
    /// Only nodes that reach into `region` are searched; if the sink
    /// can't be reached inside it, the whole chip is.
    fn search(
        &self,
        s: &mut Scratch,
        net: u32,
        tree: &[NodeId],
        sink: NodeId,
        crit: f32,
        region: Option<chipdb::Box>,
    ) -> Result<()> {
        if let Some(r) = region {
            if self.search_in(s, net, tree, sink, crit, r).is_ok() {
                return Ok(());
            }
        }
        self.search_in(s, net, tree, sink, crit, chipdb::Box::EVERYWHERE)
    }

    fn search_in(
        &self,
        s: &mut Scratch,
        net: u32,
        tree: &[NodeId],
        sink: NodeId,
        crit: f32,
        region: chipdb::Box,
    ) -> Result<()> {
        s.generation += 1;
        let gen = s.generation;
        let mut heap = BinaryHeap::new();
        let per_hop = 1.0 - crit + crit * HOP_DELAY_ESTIMATE / DELAY_UNIT;
        for &n in tree {
            s.work[0] += 1;
            let g = crit * s.delay_to[n as usize] / DELAY_UNIT;
            s.visit[n as usize] = Visit {
                cost: g,
                stamp: gen,
                via: None,
            };
            heap.push(Queued {
                f: g + self.astar * per_hop * self.distance(n, sink),
                g,
                node: n,
            });
        }
        while let Some(Queued { g, node, .. }) = heap.pop() {
            if node == sink {
                return Ok(());
            }
            if g > s.visit[node as usize].cost {
                continue;
            }
            s.work[2] += 1;
            for e in self.db.downhill(node) {
                if !self.usable(e)
                    || self.reserved_for_other(e.dst, net)
                    || !self.node[e.dst as usize].bbox.overlaps(region)
                {
                    continue;
                }
                let m = e.dst as usize;
                let g2 = g
                    + (1.0 - crit) * self.node_cost(e.dst)
                    + crit * self.node[m].delay / DELAY_UNIT;
                let v = &mut s.visit[m];
                if v.stamp == gen && v.cost <= g2 {
                    continue;
                }
                *v = Visit {
                    cost: g2,
                    stamp: gen,
                    via: Some((node, *e)),
                };
                s.work[1] += 1;
                heap.push(Queued {
                    f: g2 + self.astar * per_hop * self.distance(e.dst, sink),
                    g: g2,
                    node: e.dst,
                });
            }
        }
        bail!("unreachable")
    }
}

/// Append `n`, driven by `edge` from the node at index `parent`; returns
/// its own index.
fn add_node(tree: &mut Tree, n: NodeId, edge: Edge, parent: u32) -> u32 {
    tree.nodes.push(n);
    tree.pips.push(edge);
    tree.parent.push(parent);
    tree.nodes.len() as u32 - 1
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
        assert!(router.node.iter().all(|n| n.occ <= 1));
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
