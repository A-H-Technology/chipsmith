//! A* cost-to-go measured on the routing graph itself (VPR's map
//! lookahead): per class of node, the fewest nodes and the least delay
//! from a node of that class to a sink so many tiles away. A tile-distance
//! estimate can't tell a long wire from a local one, so on a fabric whose
//! wires span 1 to 18 tiles it is either far too low (each search floods)
//! or too high (it misses the long wires).
//!
//! The graph's wire types are too coarse for a class (the 7-series has 14,
//! one of them `OTHER` for every single, double, hex and long wire), so a
//! class is the type, the node's span in tiles and where in that span its
//! pips leave from: an east-going hex and a west-going one differ by the
//! end they drive from.

use std::collections::{HashMap, VecDeque};

use rayon::prelude::*;

use crate::chipdb::{Box, ChipDb, NodeId};
use crate::timing::Ps;

/// Tiles each way the table covers; beyond it the caller extrapolates.
pub const RADIUS: i32 = 16;
const SIDE: usize = (2 * RADIUS + 1) as usize;
/// Sample nodes per class, spread over the chip: one location's edge
/// effects (a column of block RAM, the chip's border) shouldn't be every
/// location's estimate.
const SAMPLES: usize = 4;
/// What a cell no sample reached costs: a dead end (a pin's feed reaches
/// its own pin only) shouldn't look free, but a sample's bad luck
/// shouldn't make a node unusable either.
const UNREACHED: f32 = 64.0;

pub struct Lookahead {
    /// Each node's class (an index into the table), `u16::MAX` for a node
    /// with no pips out.
    class: Vec<u16>,
    /// The tile each node's pips leave from, most of them.
    exit: Vec<(u8, u8)>,
    /// Per (class, dy, dx): fewest nodes entered to reach a sink there.
    hops: Vec<f32>,
    /// Per (class, dy, dx): least delay to reach a sink there.
    delay: Vec<Ps>,
    pub classes: usize,
}

/// Signed tiles from box `a` to box `b` per axis, 0 where they overlap.
pub fn signed_gap(a: Box, b: Box) -> (i32, i32) {
    let g = |a0: u8, a1: u8, b0: u8, b1: u8| {
        let (a0, a1, b0, b1) = (a0 as i32, a1 as i32, b0 as i32, b1 as i32);
        if b0 > a1 {
            b0 - a1
        } else if a0 > b1 {
            b1 - a0
        } else {
            0
        }
    };
    (g(a.x0, a.x1, b.x0, b.x1), g(a.y0, a.y1, b.y0, b.y1))
}

fn cell(c: usize, dx: i32, dy: i32) -> usize {
    (c * SIDE + (dy + RADIUS) as usize) * SIDE + (dx + RADIUS) as usize
}

impl Lookahead {
    /// Measure the table: breadth-first (hops) and Dijkstra (delay) from
    /// each sample node, over nodes within `RADIUS` tiles of its exit. A
    /// sink is a node with no pip out of it (a site's input pin). Each
    /// cell is the least over the samples, so the estimate errs low.
    pub fn build(db: &ChipDb, delay: &[Ps]) -> Self {
        let n = db.nodes();
        let mut exit = vec![(0u8, 0u8); n];
        let mut class = vec![u16::MAX; n];
        let mut ids: HashMap<(u8, u8, u8, u8, u8), u16> = HashMap::new();
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for v in 0..n as NodeId {
            let out = db.downhill(v);
            if out.is_empty() {
                continue;
            }
            counts.clear();
            for e in out {
                *counts.entry(e.tile).or_default() += 1;
            }
            let (&tile, _) = counts.iter().max_by_key(|&(&t, &k)| (k, std::cmp::Reverse(t))).expect("has pips");
            let (x, y) = db.tile_xy(tile);
            let (x, y) = (x as u8, y as u8);
            exit[v as usize] = (x, y);
            let b = db.node_box[v as usize];
            let key = (
                db.node_type[v as usize],
                b.x1 - b.x0,
                b.y1 - b.y0,
                x.saturating_sub(b.x0),
                y.saturating_sub(b.y0),
            );
            let next = ids.len() as u16;
            class[v as usize] = *ids.entry(key).or_insert(next);
        }
        let classes = ids.len();
        let (w, h) = (db.width as i32, db.height as i32);
        let targets: Vec<(i32, i32)> = (0..SAMPLES)
            .map(|k| {
                let (i, j) = ((k % 2) as i32, (k / 2) as i32);
                (w * (1 + 2 * i) / 4, h * (1 + 2 * j) / 4)
            })
            .collect();
        // Per class, per target: the nearest node of the class.
        let mut best: Vec<[(i32, NodeId); SAMPLES]> = vec![[(i32::MAX, 0); SAMPLES]; classes];
        for v in 0..n {
            if class[v] == u16::MAX {
                continue;
            }
            let (x, y) = exit[v];
            for (k, &(tx, ty)) in targets.iter().enumerate() {
                let d = (x as i32 - tx).abs() + (y as i32 - ty).abs();
                let b = &mut best[class[v] as usize][k];
                if d < b.0 {
                    *b = (d, v as NodeId);
                }
            }
        }
        let mut jobs: Vec<(usize, NodeId)> = Vec::new();
        for (c, b) in best.iter().enumerate() {
            let mut seen: Vec<NodeId> = Vec::new();
            for &(d, v) in b {
                if d != i32::MAX && !seen.contains(&v) {
                    seen.push(v);
                    jobs.push((c, v));
                }
            }
        }
        let per_job: Vec<(usize, Vec<f32>, Vec<Ps>)> = jobs
            .par_iter()
            .map_init(
                || (vec![u32::MAX; n], vec![Ps::INFINITY; n]),
                |(hop, dl), &(c, src)| {
                    let (hops, delays) = sample(db, delay, src, exit[src as usize], hop, dl);
                    (c, hops, delays)
                },
            )
            .collect();
        let mut hops = vec![f32::INFINITY; classes * SIDE * SIDE];
        let mut dl = vec![Ps::INFINITY; classes * SIDE * SIDE];
        for (c, hs, ds) in per_job {
            let base = cell(c, -RADIUS, -RADIUS);
            for i in 0..SIDE * SIDE {
                hops[base + i] = hops[base + i].min(hs[i]);
                dl[base + i] = dl[base + i].min(ds[i]);
            }
        }
        for v in &mut hops {
            if !v.is_finite() {
                *v = UNREACHED;
            }
        }
        for v in &mut dl {
            if !v.is_finite() {
                *v = 0.0;
            }
        }
        Lookahead {
            class,
            exit,
            hops,
            delay: dl,
            classes,
        }
    }

    /// (hops, delay) from node `v` to a sink spanning `sink`, and the tiles
    /// beyond `RADIUS` per axis; `None` for a node with no pips out.
    pub fn get(&self, v: NodeId, sink: Box) -> Option<(f32, Ps, i32, i32)> {
        let c = self.class[v as usize];
        if c == u16::MAX {
            return None;
        }
        let (x, y) = self.exit[v as usize];
        let (dx, dy) = signed_gap(Box::tile(x, y), sink);
        let r = RADIUS;
        let i = cell(c as usize, dx.clamp(-r, r), dy.clamp(-r, r));
        Some((self.hops[i], self.delay[i], (dx.abs() - r).max(0), (dy.abs() - r).max(0)))
    }
}

/// One sample's two searches; returns its rows of the table.
fn sample(
    db: &ChipDb,
    delay: &[Ps],
    src: NodeId,
    (ex, ey): (u8, u8),
    hop: &mut [u32],
    dl: &mut [Ps],
) -> (Vec<f32>, Vec<Ps>) {
    let origin = Box::tile(ex, ey);
    let near = |v: NodeId| {
        let (dx, dy) = signed_gap(origin, db.node_box[v as usize]);
        dx.abs() <= RADIUS && dy.abs() <= RADIUS
    };
    let at = |v: NodeId| {
        let (dx, dy) = signed_gap(origin, db.node_box[v as usize]);
        ((dy + RADIUS) as usize) * SIDE + (dx + RADIUS) as usize
    };
    let mut hops = vec![f32::INFINITY; SIDE * SIDE];
    let mut delays = vec![Ps::INFINITY; SIDE * SIDE];
    let mut touched: Vec<NodeId> = vec![src];

    hop[src as usize] = 0;
    let mut queue = VecDeque::from([src]);
    while let Some(v) = queue.pop_front() {
        let hv = hop[v as usize];
        let out = db.downhill(v);
        if out.is_empty() {
            let i = at(v);
            hops[i] = hops[i].min(hv as f32);
            continue;
        }
        for e in out {
            let m = e.dst as usize;
            if hop[m] == u32::MAX && near(e.dst) {
                hop[m] = hv + 1;
                touched.push(e.dst);
                queue.push_back(e.dst);
            }
        }
    }

    #[derive(PartialEq)]
    struct Q(Ps, NodeId);
    impl Eq for Q {}
    impl Ord for Q {
        fn cmp(&self, o: &Self) -> std::cmp::Ordering {
            o.0.total_cmp(&self.0)
        }
    }
    impl PartialOrd for Q {
        fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(o))
        }
    }
    dl[src as usize] = 0.0;
    let mut heap = std::collections::BinaryHeap::from([Q(0.0, src)]);
    while let Some(Q(d, v)) = heap.pop() {
        if d > dl[v as usize] {
            continue;
        }
        let out = db.downhill(v);
        if out.is_empty() {
            let i = at(v);
            delays[i] = delays[i].min(d);
            continue;
        }
        for e in out {
            let m = e.dst as usize;
            let d2 = d + delay[m];
            if d2 < dl[m] && near(e.dst) {
                dl[m] = d2;
                heap.push(Q(d2, e.dst));
            }
        }
    }
    // Dijkstra reaches exactly what the breadth-first search did: the same
    // edges inside the same window.
    for &v in &touched {
        hop[v as usize] = u32::MAX;
        dl[v as usize] = Ps::INFINITY;
    }
    (hops, delays)
}
