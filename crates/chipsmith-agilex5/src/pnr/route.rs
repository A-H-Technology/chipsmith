//! PathFinder routing over the graph (README (w), (z)): every net is a tree
//! from its source node to its sinks, found by A* from the tree built so
//! far; nodes carry one net, and overused nodes get dearer each iteration
//! until no node is shared. A node's delay is the delay model's mean for
//! its type (tools/timing/fit.py, README (u)); a sink's search weighs delay
//! against congestion by the connection's criticality, as VPR does.

use super::det::HashMap;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

use anyhow::{bail, Result};

use super::graph::{Graph, CLOCK, MUX, PASS};

#[derive(Default)]
pub struct Net {
    pub name: String,
    pub source: u32,
    pub sinks: Vec<u32>,
    /// Per sink, other nodes that serve as well: the pins a LUT input may
    /// swap to (pack::swap_group, README (z8)); the router takes whichever
    /// is cheapest, and PathFinder settles which net gets which pin.
    pub alts: Vec<Vec<u32>>,
    pub clock: bool,
}

pub struct Routed {
    /// Per net: the tree's edges as (from, to).
    pub trees: Vec<Vec<(u32, u32)>>,
    /// Per net and sink, the node the route ends at (the sink or one of
    /// its alternatives).
    pub reached: Vec<Vec<u32>>,
    pub iterations: usize,
}

/// Per node type, a base cost: the mean delay into that node type (ns).
pub fn base_costs(g: &Graph, delays: &HashMap<String, f32>) -> Vec<f32> {
    g.types
        .iter()
        .map(|t| {
            delays
                .get(&format!("re:{t}"))
                .copied()
                .unwrap_or(0.05)
                .max(0.005)
        })
        .collect()
}

/// The routing iteration from which criticality decays.
const CRIT_DECAY_FROM: usize = 20;

pub fn route(
    g: &Graph,
    nets: &[Net],
    base_by_type: &[f32],
    crit: &[Vec<f32>],
    max_iter: usize,
) -> Result<Routed> {
    let n = g.len();
    let mut occ = vec![0u16; n];
    let mut hist = vec![0f32; n];
    let mut cost = vec![f32::INFINITY; n];
    let mut prev = vec![u32::MAX; n];
    let mut stamp = vec![0u32; n];
    let mut stamp_now = 0u32;
    // Per node in the net being routed, the delay from its source along the
    // tree (by base cost).
    let mut tdel = vec![0f32; n];
    let lab_re = g.type_id("LAB_RE");
    let mut trees: Vec<Vec<(u32, u32)>> = vec![Vec::new(); nets.len()];
    let mut reached: Vec<Vec<u32>> = nets.iter().map(|n| n.sinks.clone()).collect();
    let mut pres = 0.5f32;
    // A* lower bound per column and row: the fastest wire per unit length
    // (H12, V8 in the delay model), times 2 (VPR's astar_fac, a little greedier).
    let per = |pre: char| -> f32 {
        g.types
            .iter()
            .enumerate()
            .filter_map(|(k, t)| {
                t.strip_prefix(pre)
                    .and_then(|l| l.parse::<f32>().ok())
                    .filter(|&l| l > 0.0)
                    .map(|l| base_by_type[k] / l)
            })
            .fold(f32::INFINITY, f32::min)
    };
    let (hx, hy) = (2.0 * per('H').min(0.05), 2.0 * per('V').min(0.05));
    let mut reroute = vec![true; nets.len()];
    // Criticality squared: only the most critical connections ignore
    // congestion (README (z8): the CPU 136 → 141 MHz register to register,
    // fr 52 → 7 routing iterations; the 4th power and a second pass with
    // criticalities from the routes did worse). From CRIT_DECAY_FROM on,
    // criticality decays 10 % an iteration: two critical connections paying
    // 2 % of the congestion cost never parted on one LAB line, and sha256's
    // first placement failed in 80 iterations (README (z15)).
    for iter in 1..=max_iter {
        let t_iter = std::time::Instant::now();
        let mut routed_n = 0;
        for (ni, net) in nets.iter().enumerate() {
            if !reroute[ni] {
                continue;
            }
            routed_n += 1;
            // Rip up.
            for &(_, d) in &trees[ni] {
                occ[d as usize] -= 1;
            }
            if !trees[ni].is_empty() {
                occ[net.source as usize] -= 1;
            }
            trees[ni].clear();
            occ[net.source as usize] += 1;
            let mut in_tree: Vec<u32> = vec![net.source];
            tdel[net.source as usize] = 0.0;
            const MARGIN: i32 = 3;
            let pts = std::iter::once(net.source).chain(net.sinks.iter().copied());
            let bb = pts.fold((i32::MAX, i32::MIN, i32::MAX, i32::MIN), |b, m| {
                let (x, y) = (g.x[m as usize] as i32, g.y[m as usize] as i32);
                (
                    b.0.min(x - MARGIN),
                    b.1.max(x + MARGIN),
                    b.2.min(y - MARGIN),
                    b.3.max(y + MARGIN),
                )
            });
            // Most critical sinks first, then the nearest.
            let mut sinks: Vec<(usize, u32, f32)> = net
                .sinks
                .iter()
                .copied()
                .zip(crit[ni].iter().copied())
                .enumerate()
                .map(|(i, (s, c))| (i, s, c))
                .collect();
            let (sx, sy) = (
                g.x[net.source as usize] as i32,
                g.y[net.source as usize] as i32,
            );
            sinks.sort_by_key(|&(_, s, c)| {
                (
                    std::cmp::Reverse((c * 10.0) as i32),
                    (g.x[s as usize] as i32 - sx).abs() + (g.y[s as usize] as i32 - sy).abs(),
                )
            });
            for &(si, t, cr) in &sinks {
                let cr = (cr * cr).min(0.98)
                    * if iter >= CRIT_DECAY_FROM {
                        0.9f32.powi((iter - CRIT_DECAY_FROM) as i32 + 1)
                    } else {
                        1.0
                    };
                let alts = net.alts.get(si).map(Vec::as_slice).unwrap_or(&[]);
                let target = |m: u32| m == t || alts.contains(&m);
                if let Some(&m) = in_tree.iter().find(|&&m| target(m)) {
                    reached[ni][si] = m;
                    continue;
                }
                let (tx, ty) = (g.x[t as usize] as i32, g.y[t as usize] as i32);
                let h = |m: u32| {
                    hx * (g.x[m as usize] as i32 - tx).abs() as f32
                        + hy * (g.y[m as usize] as i32 - ty).abs() as f32
                };
                // Search inside the net's bounding box plus a margin first
                // (VPR's bb_factor); clock nets and misses search everywhere.
                let mut found = false;
                let mut expanded = 0usize;
                for bounded in [!net.clock, false] {
                    if found || (bounded && net.clock) {
                        continue;
                    }
                    stamp_now += 1;
                    let mut heap = BinaryHeap::new();
                    // Seed with the tree's nodes near the sink only: big
                    // nets' trees made every sink's search start from
                    // thousands of nodes.
                    let dist = |m: u32| {
                        (g.x[m as usize] as i32 - tx).abs() + (g.y[m as usize] as i32 - ty).abs()
                    };
                    let dmin = in_tree.iter().map(|&m| dist(m)).min().unwrap_or(0);
                    // A tree node starts at its delay from the source, weighed
                    // by criticality (VPR's re-expansion): from 0 a critical
                    // sink branched off wherever the tree came nearest,
                    // however late that was.
                    for &m in in_tree.iter().filter(|&&m| !bounded || dist(m) <= dmin + 6) {
                        stamp[m as usize] = stamp_now;
                        cost[m as usize] = cr * tdel[m as usize];
                        prev[m as usize] = u32::MAX;
                        heap.push((Reverse((cost[m as usize] + h(m)).to_bits()), m));
                    }
                    expanded = 0;
                    while let Some((Reverse(fb), m)) = heap.pop() {
                        if target(m) {
                            found = true;
                            reached[ni][si] = m;
                            break;
                        }
                        let gm = cost[m as usize];
                        if f32::from_bits(fb) > gm + h(m) + 1e-6 {
                            continue;
                        }
                        expanded += 1;
                        if expanded > 4_000_000 {
                            break;
                        }
                        // LAB_RE pins are cell ports: never pass through one.
                        if Some(g.ty[m as usize]) == lab_re && m != net.source {
                            continue;
                        }
                        for e in g.fanout(m) {
                            let c = g.class[e];
                            let d = g.dst[e];
                            let ok = if net.clock {
                                c == CLOCK || ((c == PASS || c == MUX) && !g.is_wire(d))
                            } else {
                                c == MUX || c == PASS
                            };
                            let ok = ok && !(c == PASS && g.ambiguous[d as usize]);
                            if !ok {
                                continue;
                            }
                            let du = d as usize;
                            if bounded {
                                let (x, y) = (g.x[du] as i32, g.y[du] as i32);
                                if x < bb.0 || x > bb.1 || y < bb.2 || y > bb.3 {
                                    continue;
                                }
                            }
                            let over = occ[du] as f32;
                            let b = base_by_type[g.ty[du] as usize];
                            let nc =
                                gm + cr * b + (1.0 - cr) * (b + hist[du]) * (1.0 + pres * over);
                            if stamp[du] != stamp_now || nc < cost[du] {
                                stamp[du] = stamp_now;
                                cost[du] = nc;
                                prev[du] = m;
                                heap.push((Reverse((nc + h(d)).to_bits()), d));
                            }
                        }
                    }
                }
                if !found {
                    bail!(
                        "net {}: no path to {} (iteration {iter}, {expanded} nodes expanded)",
                        net.name,
                        g.name(t)
                    );
                }
                let mut m = reached[ni][si];
                let mut path = Vec::new();
                while prev[m as usize] != u32::MAX {
                    let p = prev[m as usize];
                    trees[ni].push((p, m));
                    occ[m as usize] += 1;
                    in_tree.push(m);
                    path.push((p, m));
                    m = p;
                }
                for &(p, m) in path.iter().rev() {
                    tdel[m as usize] = tdel[p as usize] + base_by_type[g.ty[m as usize] as usize];
                }
            }
        }
        let over: Vec<usize> = (0..n).filter(|&k| occ[k] > 1).collect();
        eprintln!(
            "route: iteration {iter}: {routed_n} nets routed, {} overused nodes, {:.1} s",
            over.len(),
            t_iter.elapsed().as_secs_f64()
        );
        if over.is_empty() {
            return Ok(Routed {
                trees,
                reached,
                iterations: iter,
            });
        }
        if iter == max_iter {
            for &k in &over {
                let users: Vec<&str> = nets
                    .iter()
                    .enumerate()
                    .filter(|(ni, net)| {
                        net.source == k as u32 || trees[*ni].iter().any(|&(_, d)| d == k as u32)
                    })
                    .map(|(_, net)| net.name.as_str())
                    .collect();
                let sink = nets
                    .iter()
                    .filter(|net| net.sinks.contains(&(k as u32)))
                    .count();
                eprintln!(
                    "route: overused {} ({sink} nets end there): {}",
                    g.name(k as u32),
                    users.join(", ")
                );
                for (ni, net) in nets.iter().enumerate() {
                    for &(p, d) in trees[ni].iter().filter(|&&(_, d)| d == k as u32) {
                        let next: Vec<String> = trees[ni]
                            .iter()
                            .filter(|&&(q, _)| q == d)
                            .map(|&(_, x)| g.name(x))
                            .collect();
                        eprintln!(
                            "route:   {}: from {} (source {}) to {}",
                            net.name,
                            g.name(p),
                            g.name(net.source),
                            next.join(" ")
                        );
                    }
                }
            }
        }
        for &k in &over {
            hist[k] += 0.05 * (occ[k] - 1) as f32;
        }
        // Capped (as VPR caps pres_fac): grown 1.8x every iteration it
        // reached 1e20 by iteration 80, every path cost the same and the
        // CPU's last two nets on one node never parted (specimens/p8/cpu/r2).
        pres = (pres * 1.8).min(1000.0);
        // Next time only the nets on an overused node (VPR's incremental
        // rerouting).
        let hot: super::det::HashSet<u32> = over.iter().map(|&k| k as u32).collect();
        for (ni, net) in nets.iter().enumerate() {
            reroute[ni] =
                hot.contains(&net.source) || trees[ni].iter().any(|&(_, d)| hot.contains(&d));
        }
    }
    bail!("routing did not converge in {max_iter} iterations")
}

/// Connection delay estimates for placement (README (z)): the routed delay
/// from a LAB's outputs to the inputs of a LAB dx columns and dy rows away,
/// found by Dijkstra over the router's edges from a LAB near the middle.
pub struct DelayTable {
    /// [|dx|][|dy|], ns; NaN where nothing was reached.
    t: Vec<Vec<f32>>,
}

impl DelayTable {
    pub fn lab(&self, dx: i64, dy: i64) -> f32 {
        let (x, y) = (
            (dx.unsigned_abs() as usize).min(self.t.len() - 1),
            (dy.unsigned_abs() as usize).min(self.t[0].len() - 1),
        );
        self.t[x][y]
    }

    /// A connection's estimate from its ends' placed positions.
    pub fn conn(
        &self,
        p: &super::pack::Packed,
        pl: &super::place::Placement,
        labs: &[super::place::Lab],
        port_xy: &HashMap<usize, (u32, u32)>,
        d: super::pack::End,
        s: super::pack::End,
    ) -> f32 {
        use super::pack::End;
        let xy = |e: End| -> Option<(i64, i64)> {
            match e {
                End::Out { alm, .. } | End::In { alm, .. } | End::Ctl { alm, .. } => {
                    let l = labs[pl.at[alm].0];
                    Some((l.x as i64, l.y as i64))
                }
                End::Port(k) | End::Io { port: k, .. } => {
                    port_xy.get(&k).map(|&(x, y)| (x as i64, y as i64))
                }
                End::M20k { m, .. } => Some((pl.m20k_at[m].0 as i64, pl.m20k_at[m].1 as i64)),
                End::Dsp { d, .. } => Some((pl.dsp_at[d].0 as i64, pl.dsp_at[d].1 as i64)),
                End::Pll(_) => None,
                End::PllRst(_) => Some((
                    super::place::PLL_RST_XY.0 as i64,
                    super::place::PLL_RST_XY.1 as i64,
                )),
            }
        };
        let _ = p;
        match (xy(d), xy(s)) {
            (Some(a), Some(b)) => self.lab(a.0 - b.0, a.1 - b.1),
            _ => 0.0,
        }
    }
}

pub fn delay_table(
    g: &Graph,
    base_by_type: &[f32],
    labs: &[super::place::Lab],
) -> Result<DelayTable> {
    const R: usize = 48;
    let t0 = std::time::Instant::now();
    let lab_re = g.type_id("LAB_RE");
    // A LAB near the device's middle, away from the I/O columns.
    let (cx, cy) = (
        labs.iter().map(|l| l.x).max().unwrap() / 2,
        labs.iter().map(|l| l.y).max().unwrap() / 2,
    );
    let src_lab = labs
        .iter()
        .min_by_key(|l| (l.x as i64 - cx as i64).abs() + (l.y as i64 - cy as i64).abs())
        .unwrap();
    let mut dist: HashMap<u32, f32> = HashMap::default();
    let mut heap = BinaryHeap::new();
    for pin in 89..129 {
        if let Some(n) = g.node(&format!("LAB_RE:X{}Y{}Z0I{pin}", src_lab.x, src_lab.y)) {
            dist.insert(n, 0.0);
            heap.push((Reverse(0f32.to_bits()), n));
        }
    }
    // Per destination LAB and input pin, the best delay.
    let mut best: HashMap<(u8, u8, u32), f32> = HashMap::default();
    while let Some((Reverse(db), m)) = heap.pop() {
        let dm = f32::from_bits(db);
        if dist.get(&m).is_some_and(|&d| d < dm) {
            continue;
        }
        if dm > 6.0 {
            break;
        }
        if Some(g.ty[m as usize]) == lab_re && dm > 0.0 {
            if g.i[m as usize] < 80 {
                best.insert((g.x[m as usize], g.y[m as usize], g.i[m as usize]), dm);
            }
            continue;
        }
        for e in g.fanout(m) {
            if g.class[e] != MUX && g.class[e] != PASS {
                continue;
            }
            let d = g.dst[e];
            if (g.x[d as usize] as i64 - src_lab.x as i64).unsigned_abs() as usize > R
                || (g.y[d as usize] as i64 - src_lab.y as i64).unsigned_abs() as usize > R
            {
                continue;
            }
            let nd = dm + base_by_type[g.ty[d as usize] as usize];
            if dist.get(&d).is_none_or(|&o| nd < o) {
                dist.insert(d, nd);
                heap.push((Reverse(nd.to_bits()), d));
            }
        }
    }
    // Mean over a LAB's input pins, then symmetric in dx and dy.
    let mut sum = vec![vec![(0f32, 0u32); R + 1]; R + 1];
    for (&(x, y, _), &d) in &best {
        let (dx, dy) = (
            (x as i64 - src_lab.x as i64).unsigned_abs() as usize,
            (y as i64 - src_lab.y as i64).unsigned_abs() as usize,
        );
        if dx <= R && dy <= R {
            sum[dx][dy].0 += d;
            sum[dx][dy].1 += 1;
        }
    }
    let mut t = vec![vec![f32::NAN; R + 1]; R + 1];
    for dx in 0..=R {
        for dy in 0..=R {
            if sum[dx][dy].1 > 0 {
                t[dx][dy] = sum[dx][dy].0 / sum[dx][dy].1 as f32;
            }
        }
    }
    // Fill gaps (columns without LABs) from the nearest known entry, and
    // keep the table non-decreasing with distance.
    for dx in 0..=R {
        for dy in 0..=R {
            if t[dx][dy].is_nan() {
                let mut v = f32::NAN;
                'search: for r in 1..=R {
                    for (ax, ay) in [
                        (dx.saturating_sub(r), dy),
                        (dx, dy.saturating_sub(r)),
                        ((dx + r).min(R), dy),
                        (dx, (dy + r).min(R)),
                    ] {
                        if !sum[ax][ay].1 == 0 || !t[ax][ay].is_nan() {
                            v = t[ax][ay];
                            if !v.is_nan() {
                                break 'search;
                            }
                        }
                    }
                }
                t[dx][dy] = v;
            }
        }
    }
    for dx in 0..=R {
        for dy in 0..=R {
            let m = [
                if dx > 0 { t[dx - 1][dy] } else { 0.0 },
                if dy > 0 { t[dx][dy - 1] } else { 0.0 },
            ]
            .into_iter()
            .fold(t[dx][dy], f32::max);
            t[dx][dy] = m;
        }
    }
    eprintln!(
        "delay table: from LAB X{}Y{}, {} input pins reached, {:.1} s; same LAB {:.3} ns, 1 column {:.3}, 10 columns {:.3}, 10 rows {:.3}, (20, 20) {:.3}",
        src_lab.x,
        src_lab.y,
        best.len(),
        t0.elapsed().as_secs_f64(),
        t[0][0],
        t[1][0],
        t[10][0],
        t[0][10],
        t[20][20]
    );
    Ok(DelayTable { t })
}
