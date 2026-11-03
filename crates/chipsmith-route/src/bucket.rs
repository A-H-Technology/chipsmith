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
/// Bucket width: a round relaxes nodes within this of the best estimate
/// (`CHIPSMITH_ROUTE_BUCKET`, in node costs, overrides it).
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
    pub bucket_q: u64,
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
            bucket_q: std::env::var("CHIPSMITH_ROUTE_BUCKET")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .map_or(BUCKET, |b| q(b).max(1)),
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
        let allowed = self.confined.get(&net);
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
            let bound = fmin + fx.bucket_q;
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
                    if self.reserved_for_other(dst, net)
                        || !self.node[m].bbox.overlaps(region)
                        || allowed.is_some_and(|a| {
                            dst != sink && !a[self.db.node_type[m] as usize]
                        })
                    {
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
        index: u32,
        net: &NetReq,
        crit: &[f32],
        keep: Option<&Tree>,
    ) -> Result<Tree> {
        let mut l = std::mem::take(&mut s.labels);
        let tree = self.route_net_bucketed_with(fx, s, &mut l, index, net, crit, keep);
        s.labels = l;
        tree
    }

    /// A net's starting tree for the integer search: the source and what
    /// survives of `keep`, marked in `s` under a new tree generation.
    pub(crate) fn start_tree(
        &self,
        fx: &Fixed,
        s: &mut Scratch,
        net: &NetReq,
        crit: &[f32],
        keep: Option<&Tree>,
    ) -> Start {
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
        let delays: Vec<Ps> = tree.nodes.iter().map(|&n| s.delay_to[n as usize]).collect();
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
        Start {
            tree,
            delays,
            dq,
            gen,
        }
    }

    /// The region a net's integer searches stay in first; a confined net
    /// searches the whole chip, as `Router::search` does.
    pub(crate) fn search_region(&self, index: u32, net: &NetReq) -> chipdb::Box {
        match self.region(net) {
            Some(r) if !self.confined.contains_key(&index) => r,
            _ => chipdb::Box::EVERYWHERE,
        }
    }

    /// The integer search in `region`, then over the whole chip.
    #[allow(clippy::too_many_arguments)]
    fn search_bucketed_anywhere(
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
        self.search_bucketed(fx, l, net, seeds, sink, crit_q, region, work)
            .or_else(|| {
                (region != chipdb::Box::EVERYWHERE)
                    .then(|| {
                        self.search_bucketed(fx, l, net, seeds, sink, crit_q, chipdb::Box::EVERYWHERE, work)
                    })
                    .flatten()
            })
    }

    /// Add a branch to a `Start`'s tree, keeping its `Fixed` delays.
    fn graft_q(&self, fx: &Fixed, s: &mut Scratch, st: &mut Start, branch: &[(NodeId, Edge)], seed: NodeId) {
        let before = st.tree.nodes.len();
        self.graft(s, &mut st.tree, &mut st.delays, branch, seed, st.gen);
        for j in before..st.tree.nodes.len() {
            let own = fx.delay_q[st.tree.nodes[j] as usize] as u64;
            st.dq.push(st.dq[st.tree.parent[j] as usize] + own);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn route_net_bucketed_with(
        &self,
        fx: &Fixed,
        s: &mut Scratch,
        l: &mut Labels,
        index: u32,
        net: &NetReq,
        crit: &[f32],
        keep: Option<&Tree>,
    ) -> Result<Tree> {
        let mut st = self.start_tree(fx, s, net, crit, keep);
        let region = self.search_region(index, net);
        for i in self.sink_order(net, crit) {
            let sink = net.sinks[i];
            if s.in_tree[sink as usize] == st.gen {
                continue;
            }
            let crit_q = Fixed::crit_q(crit[i]);
            let seeds: Vec<(NodeId, u64)> =
                st.tree.nodes.iter().copied().zip(st.dq.iter().copied()).collect();
            let found =
                self.search_bucketed_anywhere(fx, l, index, &seeds, sink, crit_q, region, &mut s.work);
            let (branch, seed) =
                found.with_context(|| format!("net {}: no route to a sink", net.name))?;
            self.graft_q(fx, s, &mut st, &branch, seed);
        }
        let mut tree = st.tree;
        tree.sink_delay = net
            .sinks
            .iter()
            .map(|&sink| s.delay_to[sink as usize])
            .collect();
        Ok(tree)
    }

    /// A net alone in its batch (a wide one, or one confined to some wire
    /// types), with the integer search: its sinks `group` at a time side
    /// by side, each from the tree as it was when the group began, the
    /// branches grafted in order. On the GPU (`gpu`) a group is one launch,
    /// a slot per sink; a search the GPU gives up on is the CPU's against
    /// the same tree, so the tree is the same either way.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn route_net_grouped(
        &self,
        fx: &Fixed,
        s: &mut Scratch,
        index: u32,
        net: &NetReq,
        crit: &[f32],
        keep: Option<&Tree>,
        group: usize,
        pool: &[Mutex<Option<Scratch>>],
        #[cfg(feature = "gpu")] mut gpu: Option<&mut crate::gpu::Searcher>,
    ) -> Result<Tree> {
        let mut st = self.start_tree(fx, s, net, crit, keep);
        let region = self.search_region(index, net);
        let order = self.sink_order(net, crit);
        let mut l = std::mem::take(&mut s.labels);
        for chunk in order.chunks(group.max(1)) {
            let todo: Vec<usize> = chunk
                .iter()
                .copied()
                .filter(|&i| s.in_tree[net.sinks[i] as usize] != st.gen)
                .collect();
            let seeds: Vec<(NodeId, u64)> =
                st.tree.nodes.iter().copied().zip(st.dq.iter().copied()).collect();
            let search = |l: &mut Labels, work: &mut [u64; 3], i: usize| {
                self.search_bucketed_anywhere(
                    fx,
                    l,
                    index,
                    &seeds,
                    net.sinks[i],
                    Fixed::crit_q(crit[i]),
                    region,
                    work,
                )
            };
            #[allow(unused_mut)]
            let mut on_device: Option<Vec<Option<Branch>>> = None;
            #[cfg(feature = "gpu")]
            if let Some(g) = gpu.as_deref_mut().filter(|_| todo.len() > 1) {
                on_device = Some(self.search_group_gpu(g, index, &st, &todo, net, crit, region));
            }
            let branches: Vec<Option<Branch>> = match on_device {
                // The GPU's, and the CPU's search where it gave up.
                Some(found) => found
                    .into_iter()
                    .zip(&todo)
                    .map(|(b, &i)| b.or_else(|| search(&mut l, &mut s.work, i)))
                    .collect(),
                None if todo.len() > 1 => todo
                    .par_iter()
                    .map(|&i| {
                        let slot = rayon::current_thread_index().unwrap_or(0) % pool.len();
                        let mut ps = pool[slot].lock().expect("a router thread panicked");
                        let ps = ps.get_or_insert_with(|| Scratch::new(self.db.nodes()));
                        search(&mut ps.labels, &mut ps.work, i)
                    })
                    .collect(),
                None => todo.iter().map(|&i| search(&mut l, &mut s.work, i)).collect(),
            };
            for b in branches {
                let (branch, seed) =
                    b.with_context(|| format!("net {}: no route to a sink", net.name))?;
                self.graft_q(fx, s, &mut st, &branch, seed);
            }
        }
        s.labels = l;
        let mut tree = st.tree;
        tree.sink_delay = net
            .sinks
            .iter()
            .map(|&sink| s.delay_to[sink as usize])
            .collect();
        Ok(tree)
    }
}

/// A net's tree as the integer search builds it: the tree, each node's
/// delay from the source in picoseconds and in `Fixed` units, and the tree
/// generation its nodes are marked with in the search scratch.
pub(crate) struct Start {
    pub tree: Tree,
    pub delays: Vec<Ps>,
    pub dq: Vec<u64>,
    pub gen: u32,
}
