//! The router's search in a form a GPU can run and the CPU can reproduce
//! exactly (docs/gpu-pnr.md, "Routing"): label-correcting A* in rounds.
//! Each round relaxes every frontier node whose estimate is within a
//! bucket of the frontier's best, from the cost the node had when the
//! round began; a node's label is the least (cost, edge) any relaxation
//! offered it, so the order relaxations happen in doesn't matter. Costs,
//! delays and the distance estimate are integers (1/1024 of an R4 wire),
//! so the arithmetic is the same everywhere.
//!
//! The search stops when the sink's cost is no more than the frontier's
//! best estimate, as A* stops when it pops the sink. With the router's
//! estimate (weighted 1.5, like the A*'s) the path need not be the
//! shortest, but it is the same on any machine and any thread count.

use super::*;

/// Fixed point: one unit of cost (an R4 wire's delay, or an uncongested
/// node) is this much.
pub const SCALE: u64 = 1024;
/// A node costs at most this, so a path's cost fits in 28 bits (the GPU
/// packs it with a batch stamp and an edge).
pub const COST_CAP: u64 = 1 << 20;
/// A path's cost saturates here (28 bits, as the GPU packs it).
pub const G_MAX: u64 = (1 << 28) - 1;
/// `via` of a node the search started from.
pub const SEED: u32 = (1 << 24) - 1;
/// Bucket width: a round relaxes nodes within this of the best estimate.
pub const BUCKET: u64 = SCALE;

/// What the integer search needs of the graph, flattened once.
pub struct Fixed {
    /// Per node: its delay in units of `DELAY_UNIT / SCALE`.
    pub delay_q: Vec<u32>,
    /// Per edge (`ChipDb::edge` order): source node, and whether the
    /// router may use it (`Router::usable`).
    pub edge_src: Vec<u32>,
    pub edge_ok: Vec<bool>,
    pub kx_q: u64,
    pub ky_q: u64,
    pub astar_q: u64,
    pub hop_q: u64,
}

pub fn q(v: f32) -> u64 {
    (v as f64 * SCALE as f64).round().max(0.0) as u64
}

impl Fixed {
    pub fn new(r: &Router) -> Fixed {
        let db = r.db;
        let n = db.nodes();
        let mut edge_src = Vec::with_capacity(db.edge_count());
        let mut edge_ok = Vec::with_capacity(db.edge_count());
        for v in 0..n as NodeId {
            for e in db.downhill(v) {
                edge_src.push(v);
                edge_ok.push(r.usable(e));
            }
        }
        Fixed {
            delay_q: r
                .node
                .iter()
                .map(|ni| q(ni.delay / DELAY_UNIT) as u32)
                .collect(),
            edge_src,
            edge_ok,
            kx_q: q(r.tile_distance[0]),
            ky_q: q(r.tile_distance[1]),
            astar_q: q(r.astar),
            hop_q: q(HOP_DELAY_ESTIMATE / DELAY_UNIT),
        }
    }

    /// A criticality in fixed point.
    pub fn crit_q(crit: f32) -> u64 {
        q(crit).min(SCALE)
    }

    /// The router's node cost, `(1 - crit) (1 + hist) (1 + pf occ) + crit
    /// delay`, in fixed point; `hist` is a whole number.
    pub fn cost(&self, node: usize, hist: u64, occ: u64, pf_q: u64, crit_q: u64) -> u64 {
        let cong = (1 + hist) * (SCALE + pf_q * occ);
        (((SCALE - crit_q) * cong + crit_q * self.delay_q[node] as u64) / SCALE).min(COST_CAP)
    }

    /// The A* estimate from a node `dx`, `dy` tiles from the sink.
    pub fn estimate(&self, dx: u64, dy: u64, crit_q: u64) -> u64 {
        let per_hop = (SCALE - crit_q) + crit_q * self.hop_q / SCALE;
        per_hop * (dx * self.kx_q + dy * self.ky_q) / SCALE * self.astar_q / SCALE
    }

    /// A seed's starting cost from its delay from the source.
    pub fn seed_cost(delay_q: u64, crit_q: u64) -> u64 {
        crit_q * delay_q / SCALE
    }
}

fn pack(g: u64, via: u32) -> u64 {
    (g << 24) | via as u64
}

fn unpack(l: u64) -> (u64, u32) {
    (l >> 24, (l & SEED as u64) as u32)
}

/// One search's labels, valid where `stamp` is the search's generation.
#[derive(Default)]
pub struct Labels {
    label: Vec<u64>,
    stamp: Vec<u32>,
    gen: u32,
}

impl Labels {
    fn get(&self, n: usize) -> u64 {
        if self.stamp[n] == self.gen {
            self.label[n]
        } else {
            u64::MAX
        }
    }

    fn offer(&mut self, n: usize, l: u64) -> bool {
        if l < self.get(n) {
            self.label[n] = l;
            self.stamp[n] = self.gen;
            true
        } else {
            false
        }
    }
}

impl Router<'_> {
    /// The integer search from `seeds` (tree nodes with their delay from
    /// the source, in `Fixed` units) to `sink` within `region`: the branch
    /// back from the sink, sink first, with the tree node it starts from.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn search_bucketed(
        &self,
        fx: &Fixed,
        l: &mut Labels,
        net: u32,
        seeds: &[(NodeId, u64)],
        sink: NodeId,
        crit_q: u64,
        region: chipdb::Box,
        work: &mut [u64; 3],
    ) -> Option<Branch> {
        if l.label.len() != self.node.len() {
            *l = Labels {
                label: vec![0; self.node.len()],
                stamp: vec![0; self.node.len()],
                gen: 0,
            };
        }
        l.gen += 1;
        let pf_q = q(self.present_factor);
        let target = self.node[sink as usize].bbox;
        let f_of = |n: NodeId, g: u64| {
            let (dx, dy) = self.node[n as usize].bbox.gap(target);
            g + fx.estimate(dx as u64, dy as u64, crit_q)
        };
        let mut frontier: Vec<(NodeId, u64)> = Vec::with_capacity(seeds.len());
        for &(n, dq) in seeds {
            let g = Fixed::seed_cost(dq, crit_q).min(G_MAX);
            if l.offer(n as usize, pack(g, SEED)) {
                frontier.push((n, g));
            }
            work[0] += 1;
        }
        let mut improved: Vec<NodeId> = Vec::new();
        loop {
            let sink_g = match l.get(sink as usize) {
                u64::MAX => None,
                lab => Some(unpack(lab).0),
            };
            // Done when nothing left could beat the sink's cost; failed
            // when nothing is left and the sink wasn't reached.
            let Some(fmin) = frontier.iter().map(|&(n, g)| f_of(n, g)).min() else {
                if sink_g.is_some() {
                    break;
                }
                return None;
            };
            if sink_g.is_some_and(|g| g <= fmin) {
                break;
            }
            let bound = fmin + BUCKET;
            improved.clear();
            let mut deferred: Vec<(NodeId, u64)> = Vec::new();
            for &(v, gv) in &frontier {
                if f_of(v, gv) > bound {
                    deferred.push((v, gv));
                    continue;
                }
                work[2] += 1;
                for e in self.db.downhill_range(v) {
                    if !fx.edge_ok[e] {
                        continue;
                    }
                    let dst = self.db.edge(e).dst;
                    let m = dst as usize;
                    if self.reserved_for_other(dst, net) || !self.node[m].bbox.overlaps(region) {
                        continue;
                    }
                    let ni = &self.node[m];
                    let g2 =
                        (gv + fx.cost(m, ni.hist as u64, ni.occ as u64, pf_q, crit_q)).min(G_MAX);
                    if sink_g.is_some_and(|sg| g2 >= sg) {
                        continue;
                    }
                    if l.offer(m, pack(g2, e as u32)) {
                        improved.push(dst);
                        work[1] += 1;
                    }
                }
            }
            // The next frontier: the deferred and the improved, each at
            // its label now, once; in node order.
            frontier.clear();
            for n in deferred
                .iter()
                .map(|&(n, _)| n)
                .chain(improved.iter().copied())
            {
                frontier.push((n, unpack(l.get(n as usize)).0));
            }
            frontier.sort_unstable();
            frontier.dedup_by_key(|e| e.0);
        }
        let mut branch = Vec::new();
        let mut n = sink;
        loop {
            let (_, via) = unpack(l.get(n as usize));
            if via == SEED {
                return Some((branch, n));
            }
            let e = via as usize;
            branch.push((n, *self.db.edge(e)));
            n = fx.edge_src[e];
        }
    }

    /// `route_net` with the integer search.
    pub(crate) fn route_net_bucketed(
        &self,
        fx: &Fixed,
        s: &mut Scratch,
        l: &mut Labels,
        index: u32,
        net: &NetReq,
        crit: &[f32],
        keep: Option<&Tree>,
    ) -> Result<Tree> {
        s.tree_generation += 1;
        let gen = s.tree_generation;
        let mut tree = Tree {
            nodes: vec![net.source],
            pips: Vec::new(),
            sink_delay: Vec::new(),
            parent: vec![0],
        };
        s.in_tree[net.source as usize] = gen;
        s.tree_pos[net.source as usize] = 0;
        s.delay_to[net.source as usize] = self.node[net.source as usize].delay;
        if let Some(old) = keep.filter(|t| !t.nodes.is_empty()) {
            self.keep_branches(s, net, crit, old, &mut tree);
        }
        let mut delays: Vec<Ps> = tree.nodes.iter().map(|&n| s.delay_to[n as usize]).collect();
        // Each tree node's delay in `Fixed` units, along its parents.
        let mut dq: Vec<u64> = Vec::with_capacity(tree.nodes.len());
        for i in 0..tree.nodes.len() {
            let own = fx.delay_q[tree.nodes[i] as usize] as u64;
            let up = if i == 0 {
                0
            } else {
                dq[tree.parent[i] as usize]
            };
            dq.push(up + own);
        }
        let region = self.region(net).unwrap_or(chipdb::Box::EVERYWHERE);
        let mut order: Vec<usize> = (0..net.sinks.len()).collect();
        order.sort_by(|&a, &b| {
            crit[b].total_cmp(&crit[a]).then(
                self.distance(net.source, net.sinks[a])
                    .total_cmp(&self.distance(net.source, net.sinks[b])),
            )
        });
        for i in order {
            let sink = net.sinks[i];
            if s.in_tree[sink as usize] == gen {
                continue;
            }
            let crit_q = Fixed::crit_q(crit[i]);
            let seeds: Vec<(NodeId, u64)> =
                tree.nodes.iter().copied().zip(dq.iter().copied()).collect();
            let found = self
                .search_bucketed(fx, l, index, &seeds, sink, crit_q, region, &mut s.work)
                .or_else(|| {
                    self.search_bucketed(
                        fx,
                        l,
                        index,
                        &seeds,
                        sink,
                        crit_q,
                        chipdb::Box::EVERYWHERE,
                        &mut s.work,
                    )
                });
            let (branch, seed) =
                found.with_context(|| format!("net {}: no route to a sink", net.name))?;
            let before = tree.nodes.len();
            self.graft(s, &mut tree, &mut delays, &branch, seed, gen);
            for j in before..tree.nodes.len() {
                let own = fx.delay_q[tree.nodes[j] as usize] as u64;
                dq.push(dq[tree.parent[j] as usize] + own);
            }
        }
        tree.sink_delay = net
            .sinks
            .iter()
            .map(|&sink| s.delay_to[sink as usize])
            .collect();
        Ok(tree)
    }
}
