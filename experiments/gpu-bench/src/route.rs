//! Is a GPU router worth it at chipsmith's sizes? The searches of a
//! first PathFinder iteration (every node costs 1, nothing is congested
//! yet), each net split into its source-to-sink connections and kept to
//! the net's region as the router keeps it, timed three ways:
//!
//! - A* on the CPU as `chipsmith-route` runs it (its distance estimate,
//!   weight 1.5), one thread and every rayon thread;
//! - on the GPU, the GPU-friendly form: a level-synchronous breadth-first
//!   search per connection, `SLOTS` connections side by side in one launch
//!   per level, the routing graph resident on the device. Integer atomics
//!   only, so the distances don't depend on thread timing.
//!
//! `gpu-bench route <dump> [cuda|hip]`, with the dump from a route run
//! with `CHIPSMITH_ROUTE_DUMP=<dump>` and the graph from
//! `CHIPSMITH_CYCLONE2_GRAPH` (the devshell sets it).

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::time::Instant;

use chipsmith_route::chipdb::{Box as Region, ChipDb};
use cubecl::prelude::*;
use rayon::prelude::*;

const UNITS: u32 = 256;
/// Connections searched side by side on the GPU: each has a distance per
/// node of the whole graph, so memory bounds it.
const SLOTS: usize = 32;
/// Frontier capacity per level, in (slot, node) pairs.
const FRONTIER: usize = 1 << 24;

struct Conn {
    src: u32,
    sink: u32,
    region: Region,
}

fn packed(b: Region) -> u32 {
    u32::from_le_bytes([b.x0, b.y0, b.x1, b.y1])
}

#[cube(launch_unchecked)]
fn fill_u32(a: &mut [u32], v: u32) {
    if ABSOLUTE_POS < a.len() {
        a[ABSOLUTE_POS] = v;
    }
}

/// Slot s starts at its source: distance 0, in the frontier.
/// Distances are stamped with the batch: `(batch << 16) | (0xffff -
/// depth)`, so a newer batch always wins an atomic max, a smaller depth
/// wins within a batch, and nothing is reset between batches.
#[cube(launch_unchecked)]
fn seed(srcs: &[u32], dist: &mut [u32], frontier: &mut [u32], stamp: u32, #[comptime] n: usize) {
    let s = ABSOLUTE_POS;
    if s < srcs.len() {
        let v = srcs[s] as usize;
        dist[s * n + v] = (stamp << 16) | 0xffffu32;
        frontier[2 * s] = s as u32;
        frontier[2 * s + 1] = v as u32;
    }
}

/// One level: every frontier entry of a slot not yet at its sink claims
/// its unvisited downhill neighbours inside the slot's region.
#[cube(launch_unchecked)]
fn level(
    edge_start: &[u32],
    edge_dst: &[u32],
    node_box: &[u32],
    regions: &[u32],
    sinks: &[u32],
    frontier: &[u32],
    count: u32,
    depth: u32,
    stamp: u32,
    dist: &[Atomic<u32>],
    next: &mut [u32],
    next_count: &[Atomic<u32>],
    found: &mut [u32],
    #[comptime] n: usize,
    #[comptime] cap: usize,
) {
    let t = ABSOLUTE_POS;
    if t < count as usize {
        let s = frontier[2 * t] as usize;
        let v = frontier[2 * t + 1] as usize;
        if found[s] == 0 {
            let r = regions[s];
            let rx0 = r & 255;
            let ry0 = (r >> 8) & 255;
            let rx1 = (r >> 16) & 255;
            let ry1 = r >> 24;
            for e in edge_start[v]..edge_start[v + 1] {
                let d = edge_dst[e as usize];
                let b = node_box[d as usize];
                let bx0 = b & 255;
                let by0 = (b >> 8) & 255;
                let bx1 = (b >> 16) & 255;
                let by1 = b >> 24;
                if bx0 <= rx1 && rx0 <= bx1 && by0 <= ry1 && ry0 <= by1 {
                    let old = dist[s * n + d as usize].fetch_max((stamp << 16) | (0xffffu32 - depth - 1));
                    if (old >> 16) < stamp {
                        let i = next_count[0].fetch_add(1u32) as usize;
                        if i < cap {
                            next[2 * i] = s as u32;
                            next[2 * i + 1] = d;
                        }
                        if d == sinks[s] {
                            found[s] = 1u32;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Queued {
    f: f32,
    g: u32,
    node: u32,
}
impl Eq for Queued {}
impl Ord for Queued {
    fn cmp(&self, o: &Self) -> Ordering {
        o.f.total_cmp(&self.f)
    }
}
impl PartialOrd for Queued {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// A* from `c.src` to `c.sink` inside the region; returns (hops, nodes
/// expanded). `seen` holds a generation stamp per node.
fn astar(db: &ChipDb, c: &Conn, seen: &mut [u32], best: &mut [u32], gen: u32) -> (u32, u64) {
    let [kx, ky] = db.tile_distance;
    let target = db.node_box[c.sink as usize];
    let est = |n: u32| {
        let (dx, dy) = db.node_box[n as usize].gap(target);
        1.5 * (dx as f32 * kx + dy as f32 * ky)
    };
    let mut heap = BinaryHeap::new();
    seen[c.src as usize] = gen;
    best[c.src as usize] = 0;
    heap.push(Queued { f: est(c.src), g: 0, node: c.src });
    let mut expanded = 0;
    while let Some(Queued { g, node, .. }) = heap.pop() {
        if node == c.sink {
            return (g, expanded);
        }
        if g > best[node as usize] {
            continue;
        }
        expanded += 1;
        for e in db.downhill(node) {
            if !db.node_box[e.dst as usize].overlaps(c.region) {
                continue;
            }
            let g2 = g + 1;
            let m = e.dst as usize;
            if seen[m] == gen && best[m] <= g2 {
                continue;
            }
            seen[m] = gen;
            best[m] = g2;
            heap.push(Queued { f: g2 as f32 + est(e.dst), g: g2, node: e.dst });
        }
    }
    (u32::MAX, expanded)
}

fn cpu(db: &ChipDb, conns: &[Conn], par: bool) -> (f64, u64, u64) {
    let n = db.nodes();
    let t = Instant::now();
    let run = |chunk: &[Conn]| -> (u64, u64) {
        let (mut seen, mut best) = (vec![0u32; n], vec![0u32; n]);
        let (mut hops, mut work) = (0u64, 0u64);
        for (i, c) in chunk.iter().enumerate() {
            let (h, w) = astar(db, c, &mut seen, &mut best, i as u32 + 1);
            hops += h.min(1000) as u64;
            work += w;
        }
        (hops, work)
    };
    let (hops, work) = if par {
        conns
            .par_chunks(conns.len().div_ceil(rayon::current_num_threads() * 8).max(1))
            .map(run)
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    } else {
        run(conns)
    };
    (t.elapsed().as_secs_f64(), hops, work)
}

fn count(n: usize) -> CubeCount {
    CubeCount::Static((n as u32).div_ceil(UNITS).max(1), 1, 1)
}

pub fn run(dump: &str, device: cubecl::Device) {
    let graph = std::env::var("CHIPSMITH_CYCLONE2_GRAPH").expect("CHIPSMITH_CYCLONE2_GRAPH (run in the devshell)");
    let db = ChipDb::read_cache(std::path::Path::new(&graph)).expect("the routing graph");
    let mut conns = Vec::new();
    for line in std::fs::read_to_string(dump).expect("the dump").lines() {
        let f: Vec<u32> = line.split_whitespace().map(|w| w.parse().unwrap()).collect();
        let region = Region { x0: f[0] as u8, y0: f[1] as u8, x1: f[2] as u8, y1: f[3] as u8 };
        for &sink in &f[5..] {
            conns.push(Conn { src: f[4], sink, region });
        }
    }
    let n = db.nodes();
    println!(
        "{} connections of {} nets; graph {} nodes, {} edges; CPU threads {}",
        conns.len(),
        std::fs::read_to_string(dump).unwrap().lines().count(),
        n,
        db.edge_count(),
        rayon::current_num_threads()
    );
    let (t1, hops1, work1) = cpu(&db, &conns, false);
    let (tp, hopsp, _) = cpu(&db, &conns, true);
    assert_eq!(hops1, hopsp);
    println!("CPU A*, 1 thread: {:.3} s, {} hops in all, {} nodes expanded ({:.0} a connection)", t1, hops1, work1, work1 as f64 / conns.len() as f64);
    println!("CPU A*, rayon:    {tp:.3} s");

    let client = device.client();
    let mut edge_start = vec![0u32];
    let mut edge_dst = Vec::with_capacity(db.edge_count());
    for v in 0..n as u32 {
        edge_dst.extend(db.downhill(v).iter().map(|e| e.dst));
        edge_start.push(edge_dst.len() as u32);
    }
    let node_box: Vec<u32> = db.node_box.iter().map(|&b| packed(b)).collect();
    let t_up = Instant::now();
    let up = |v: &[u32]| client.create_from_slice(u32::as_bytes(v));
    let (es, ed, nb) = (up(&edge_start), up(&edge_dst), up(&node_box));
    let dist = client.empty(SLOTS * n * 4);
    let (fa, fb) = (client.empty(FRONTIER * 8), client.empty(FRONTIER * 8));
    cubecl::future::block_on(client.sync()).unwrap();
    println!("GPU: graph uploaded in {:.3} s", t_up.elapsed().as_secs_f64());
    let arg = |h: &cubecl::server::Handle, len: usize| unsafe { BufferArg::from_raw_parts(h.clone(), len.max(1)) };
    let dim = CubeDim::new_1d(UNITS);
    let (mut levels, mut visited, mut hops_gpu, mut lost) = (0u64, 0u64, 0u64, 0u64);
    unsafe {
        fill_u32::launch_unchecked(&client, count(SLOTS * n), dim, arg(&dist, SLOTS * n), 0);
    }
    let mut stamp = 0u32;
    let mut run_gpu = |measure: bool| {
        let mut searching = 0.0;
        for batch in conns.chunks(SLOTS) {
            stamp += 1;
            let srcs: Vec<u32> = batch.iter().map(|c| c.src).collect();
            let sinks: Vec<u32> = batch.iter().map(|c| c.sink).collect();
            let regions: Vec<u32> = batch.iter().map(|c| packed(c.region)).collect();
            let (srcs_h, sinks_h, regions_h) = (up(&srcs), up(&sinks), up(&regions));
            let found = up(&vec![0u32; batch.len()]);
            let (mut cur, mut nxt) = (fa.clone(), fb.clone());
            cubecl::future::block_on(client.sync()).unwrap();
            let t = Instant::now();
            unsafe {
                seed::launch_unchecked(&client, count(batch.len()), dim, arg(&srcs_h, batch.len()), arg(&dist, SLOTS * n), arg(&cur, FRONTIER * 2), stamp, n);
            }
            let mut size = batch.len() as u32;
            let mut depth = 0u32;
            while size > 0 {
                let counter = up(&[0u32]);
                unsafe {
                    level::launch_unchecked(
                        &client,
                        count(size as usize),
                        dim,
                        arg(&es, n + 1),
                        arg(&ed, edge_dst.len()),
                        arg(&nb, n),
                        arg(&regions_h, batch.len()),
                        arg(&sinks_h, batch.len()),
                        arg(&cur, FRONTIER * 2),
                        size,
                        depth,
                        stamp,
                        arg(&dist, SLOTS * n),
                        arg(&nxt, FRONTIER * 2),
                        arg(&counter, 1),
                        arg(&found, batch.len()),
                        n,
                        FRONTIER,
                    );
                }
                let c = u32::from_bytes(&client.read_one(counter).unwrap())[0];
                if measure {
                    levels += 1;
                    visited += size as u64;
                    lost += c.saturating_sub(FRONTIER as u32) as u64;
                }
                size = c.min(FRONTIER as u32);
                depth += 1;
                std::mem::swap(&mut cur, &mut nxt);
            }
            searching += t.elapsed().as_secs_f64();
            if measure {
                // The sinks' distances: the hop counts to compare with A*'s.
                let d = u32::from_bytes(&client.read_one(dist.clone()).unwrap()).to_vec();
                for (s, c) in batch.iter().enumerate() {
                    let v = d[s * n + c.sink as usize];
                    hops_gpu += if v >> 16 == stamp { (0xffff - (v & 0xffff)).min(1000) as u64 } else { 1000 };
                }
            }
        }
        searching
    };
    run_gpu(false);
    let tg = run_gpu(true);
    println!(
        "GPU BFS, {SLOTS} at a time: {tg:.3} s searching, {levels} levels launched, {visited} nodes visited ({:.0} a connection), {hops_gpu} hops in all, {lost} frontier entries over capacity",
        visited as f64 / conns.len() as f64
    );
}
