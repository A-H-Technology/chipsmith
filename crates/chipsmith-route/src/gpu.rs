//! The integer search (`bucket`) on a GPU, for a batch of nets whose
//! regions don't overlap (docs/gpu-pnr.md, "Routing"). The routing graph,
//! the usable-edge mask, occupancy and history stay on the device for the
//! whole routing; a batch uploads its nets (source, sinks in order,
//! criticalities, region) and gets back each net's tree. Committed trees
//! go back up as occupancy deltas.
//!
//! Every net of the batch is a slot that runs its sinks one after another,
//! the slots in lockstep rounds: the frontier's best estimate per slot,
//! then a control step per slot (finish a sink: walk its branch back,
//! graft it onto the tree, seed the next sink), then relaxation of the
//! frontier within each slot's bucket, then compaction into the next
//! frontier. A label is `(4095 - generation) << 52 | cost << 24 | edge`
//! and is only ever lowered by an atomic minimum, so the labels after a
//! round don't depend on the order of its relaxations; every decision
//! reads values from the round's start. The trees are the CPU search's
//! (`Router::route_net_bucketed`), bit for bit.

use cubecl::prelude::*;
use cubecl::server::Handle;

use crate::bucket::{Fixed, SEED};
use crate::chipdb::{self, ChipDb, NodeId};

pub use chipsmith_place_runtime::Runtime;

/// The GPU runtimes (`CHIPSMITH_ACCEL`): the placer's names.
mod chipsmith_place_runtime {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Runtime {
        Cuda,
        Hip,
    }
}

const UNITS: u32 = 256;
/// Nets a batch runs at once (each has a label per node on the device).
pub const SLOTS: usize = 64;
/// A slot's tree holds at most this many nodes; a bigger net is routed on
/// the CPU.
pub const TREE_CAP: usize = 1024;
/// Frontier entries a slot's round may hold; more fails the slot (the
/// CPU routes its net).
const FCAP: usize = 1 << 16;

/// A slot's phase (0: searching).
const DONE: u32 = 2;

#[cube]
fn gap(a: u32, b: u32) -> u32 {
    // Boxes packed x0, y0, x1, y1 a byte each: tiles between them, x and
    // y added with their weights by the caller.
    let ax0 = a & 255;
    let ay0 = (a >> 8) & 255;
    let ax1 = (a >> 16) & 255;
    let ay1 = a >> 24;
    let bx0 = b & 255;
    let by0 = (b >> 8) & 255;
    let bx1 = (b >> 16) & 255;
    let by1 = b >> 24;
    let mut dx = 0u32;
    if bx0 > ax1 {
        dx = bx0 - ax1;
    }
    if ax0 > bx1 {
        dx = ax0 - bx1;
    }
    let mut dy = 0u32;
    if by0 > ay1 {
        dy = by0 - ay1;
    }
    if ay0 > by1 {
        dy = ay0 - by1;
    }
    (dx << 16) | dy
}

#[cube]
fn estimate(gapxy: u32, crit_q: u64, kx_q: u64, ky_q: u64, astar_q: u64, hop_q: u64) -> u64 {
    let dx = (gapxy >> 16) as u64;
    let dy = (gapxy & 65535) as u64;
    let per_hop = (1024u64 - crit_q) + crit_q * hop_q / 1024u64;
    per_hop * (dx * kx_q + dy * ky_q) / 1024u64 * astar_q / 1024u64
}

#[cube]
fn overlaps(a: u32, b: u32) -> bool {
    let ax0 = a & 255;
    let ay0 = (a >> 8) & 255;
    let ax1 = (a >> 16) & 255;
    let ay1 = a >> 24;
    let bx0 = b & 255;
    let by0 = (b >> 8) & 255;
    let bx1 = (b >> 16) & 255;
    let by1 = b >> 24;
    ax0 <= bx1 && bx0 <= ax1 && ay0 <= by1 && by0 <= ay1
}

/// One batch, one cube per slot: the slot's searches from first sink to
/// last, round after round, its threads sharing each round's frontier.
#[cube(launch_unchecked)]
fn slot_kernel(
    edge_start: &[u32],
    edge_dst: &[u32],
    edge_ok: &[u32],
    edge_src: &[u32],
    node_box: &[u32],
    delay_q: &[u32],
    occ: &[u32],
    hist: &[u32],
    owner: &[u32],
    inp: &[u32],
    out: &mut [u32],
    gen: &mut [u32],
    slot_round: &mut [u32],
    phase: &mut [u32],
    tree_len: &mut [u32],
    scal: &mut [u64],
    fmin: &[Atomic<u64>],
    misc: &mut [u32],
    next_count: &[Atomic<u32>],
    labels: &[Atomic<u64>],
    mark: &[Atomic<u32>],
    in_tree: &mut [u32],
    tree_node: &mut [u32],
    tree_parent: &mut [u32],
    tree_via: &mut [u32],
    tree_dq: &mut [u64],
    branch: &mut [u32],
    f_node: &mut [u32],
    f_g: &mut [u64],
    n_node: &mut [u32],
    pf_q: u64,
    kx_q: u64,
    ky_q: u64,
    astar_q: u64,
    hop_q: u64,
    has_owner: u32,
    batch: u32,
    slots: u32,
    o_sinks: u32,
    #[comptime] n: usize,
    #[comptime] tree_cap: usize,
    #[comptime] fcap: usize,
) {
    // inp: per slot source, net, region, first sink; then one more first
    // sink; then (sink, criticality) pairs.
    let s = CUBE_POS as usize;
    let k = slots as usize;
    let t = UNIT_POS as usize;
    let nt = CUBE_DIM as usize;
    let base = s * n;
    let fb = s * fcap;
    let tb = s * tree_cap;
    // scal: bound, sink_g, crit; misc: cur_sink, sink_box, next_sink, frontier size.
    if t == 0 {
        let src = inp[s];
        tree_node[tb] = src;
        tree_dq[tb] = delay_q[src as usize] as u64;
        tree_len[s] = 1u32;
        in_tree[base + src as usize] = batch;
        phase[s] = 4u32;
        misc[s * 4 + 2] = inp[3 * k + s];
        misc[s * 4 + 3] = 0u32;
        next_count[s].store(0u32);
    }
    sync_cube();
    let mut running = true;
    while running {
        let round = slot_round[s];
        if t == 0 {
            fmin[s].store(u64::MAX);
        }
        sync_cube();
        let fsize = misc[s * 4 + 3] as usize;
        if phase[s] == 0u32 {
            let sb = misc[s * 4 + 1];
            let cq = scal[s * 3 + 2];
            let mut i = t;
            while i < fsize {
                let v = f_node[fb + i] as usize;
                let f =
                    f_g[fb + i] + estimate(gap(node_box[v], sb), cq, kx_q, ky_q, astar_q, hop_q);
                fmin[s].fetch_min(f);
                i += nt;
            }
        }
        sync_cube();
        if t == 0 {
            let mut ph = phase[s];
            if ph == 0u32 {
                let sl = labels[base + misc[s * 4] as usize].load();
                let reached = (sl >> 52) == (4095u64 - gen[s] as u64);
                let sg = (sl >> 24) & 268435455u64;
                let fm = fmin[s].load();
                let empty = fsize == 0;
                if reached && (empty || sg <= fm) {
                    ph = 1u32;
                } else if empty {
                    ph = 3u32;
                } else {
                    scal[s * 3] = fm + 1024u64;
                    if reached {
                        scal[s * 3 + 1] = sg;
                    } else {
                        scal[s * 3 + 1] = 268435456u64;
                    }
                }
            }
            if ph == 1u32 {
                let mut len = 0u32;
                let mut v = misc[s * 4];
                let mut walking = true;
                let mut whole = true;
                while walking {
                    let l = labels[base + v as usize].load();
                    let via = (l & 16777215u64) as u32;
                    if via == 16777215u32 {
                        walking = false;
                    } else {
                        branch[tb * 2 + len as usize * 2] = v;
                        branch[tb * 2 + len as usize * 2 + 1] = via;
                        len += 1u32;
                        v = edge_src[via as usize];
                        if len as usize >= tree_cap {
                            walking = false;
                            whole = false;
                        }
                    }
                }
                let mut cut = len;
                let mut found = false;
                for i in 0..len {
                    if !found && in_tree[base + branch[tb * 2 + i as usize * 2] as usize] == batch {
                        cut = i;
                        found = true;
                    }
                }
                let mut attach = v;
                if cut < len {
                    attach = branch[tb * 2 + cut as usize * 2];
                }
                let tl = tree_len[s];
                let mut parent = 0u32;
                for i in 0..tl {
                    if tree_node[tb + i as usize] == attach {
                        parent = i;
                    }
                }
                let fits = whole && (tl + cut) as usize <= tree_cap;
                let mut j = cut;
                while j > 0u32 && fits {
                    j -= 1u32;
                    let node = branch[tb * 2 + j as usize * 2];
                    let idx = tree_len[s];
                    tree_node[tb + idx as usize] = node;
                    tree_parent[tb + idx as usize] = parent;
                    tree_via[tb + idx as usize] = branch[tb * 2 + j as usize * 2 + 1];
                    tree_dq[tb + idx as usize] =
                        tree_dq[tb + parent as usize] + delay_q[node as usize] as u64;
                    in_tree[base + node as usize] = batch;
                    parent = idx;
                    tree_len[s] = idx + 1u32;
                }
                if fits {
                    ph = 4u32;
                } else {
                    ph = 3u32;
                }
            }
            if ph == 4u32 {
                let mut kk = misc[s * 4 + 2];
                let end = inp[3 * k + s + 1];
                let mut picked = false;
                while kk < end && !picked {
                    if in_tree[base + inp[o_sinks as usize + 2 * kk as usize] as usize] != batch {
                        picked = true;
                    } else {
                        kk += 1u32;
                    }
                }
                if !picked {
                    ph = 2u32;
                } else {
                    misc[s * 4 + 2] = kk + 1u32;
                    misc[s * 4] = inp[o_sinks as usize + 2 * kk as usize];
                    misc[s * 4 + 1] = node_box[inp[o_sinks as usize + 2 * kk as usize] as usize];
                    scal[s * 3 + 2] = inp[o_sinks as usize + 2 * kk as usize + 1] as u64;
                    gen[s] += 1u32;
                    next_count[s].store(tree_len[s]);
                    ph = 6u32;
                }
            }
            phase[s] = ph;
        }
        sync_cube();
        let ph = phase[s];
        if ph == 2u32 || ph == 3u32 {
            running = false;
        } else {
            let tag = (4095u64 - gen[s] as u64) << 52;
            let cq = scal[s * 3 + 2];
            if ph == 6u32 {
                // The new search's seeds: every tree node.
                let tl = tree_len[s] as usize;
                let mut i = t;
                while i < tl {
                    let node = tree_node[tb + i];
                    let mut g = cq * tree_dq[tb + i] / 1024u64;
                    if g > 268435455u64 {
                        g = 268435455u64;
                    }
                    labels[base + node as usize].fetch_min(tag | (g << 24) | 16777215u64);
                    mark[base + node as usize].store(round);
                    n_node[fb + i] = node;
                    i += nt;
                }
            } else {
                let sb = misc[s * 4 + 1];
                let bound = scal[s * 3];
                let sg = scal[s * 3 + 1];
                let reg = inp[2 * k + s];
                let mut i = t;
                while i < fsize {
                    let v = f_node[fb + i] as usize;
                    let gv = f_g[fb + i];
                    let f = gv + estimate(gap(node_box[v], sb), cq, kx_q, ky_q, astar_q, hop_q);
                    if f > bound {
                        if mark[base + v].exchange(round) != round {
                            let at = next_count[s].fetch_add(1u32) as usize;
                            if at < fcap {
                                n_node[fb + at] = v as u32;
                            }
                        }
                    } else {
                        for e in edge_start[v]..edge_start[v + 1] {
                            let ei = e as usize;
                            if edge_ok[ei] != 0u32 {
                                let d = edge_dst[ei] as usize;
                                let mut allowed = overlaps(node_box[d], reg);
                                if has_owner != 0u32 {
                                    let o = owner[d];
                                    if o != 4294967295u32 && o != inp[k + s] {
                                        allowed = false;
                                    }
                                }
                                if allowed {
                                    let cong =
                                        (1u64 + hist[d] as u64) * (1024u64 + pf_q * occ[d] as u64);
                                    let mut cost =
                                        ((1024u64 - cq) * cong + cq * delay_q[d] as u64) / 1024u64;
                                    if cost > 1048576u64 {
                                        cost = 1048576u64;
                                    }
                                    let mut g2 = gv + cost;
                                    if g2 > 268435455u64 {
                                        g2 = 268435455u64;
                                    }
                                    if g2 < sg {
                                        let new = tag | (g2 << 24) | e as u64;
                                        let old = labels[base + d].fetch_min(new);
                                        if new < old && mark[base + d].exchange(round) != round {
                                            let at = next_count[s].fetch_add(1u32) as usize;
                                            if at < fcap {
                                                n_node[fb + at] = d as u32;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    i += nt;
                }
            }
            sync_cube();
            // The next frontier, each node at its label now.
            let nc = next_count[s].load() as usize;
            let mut i = t;
            while i < nc && i < fcap {
                let v = n_node[fb + i];
                f_node[fb + i] = v;
                f_g[fb + i] = (labels[base + v as usize].load() >> 24) & 268435455u64;
                i += nt;
            }
            sync_cube();
            if t == 0 {
                if nc > fcap {
                    phase[s] = 3u32;
                } else if ph == 6u32 {
                    phase[s] = 0u32;
                }
                misc[s * 4 + 3] = nc as u32;
                next_count[s].store(0u32);
                slot_round[s] = round + 1u32;
            }
            sync_cube();
        }
    }
    // The result: phase and tree length, then (node, parent, edge) per
    // tree node.
    let ob = s * (2 + 3 * tree_cap);
    let tl = tree_len[s] as usize;
    if t == 0 {
        out[ob] = phase[s];
        out[ob + 1] = tl as u32;
    }
    let mut i = t;
    while i < tl {
        out[ob + 2 + 3 * i] = tree_node[tb + i];
        out[ob + 3 + 3 * i] = tree_parent[tb + i];
        out[ob + 4 + 3 * i] = tree_via[tb + i];
        i += nt;
    }
}

#[cube(launch_unchecked)]
fn fill_u32(a: &mut [u32], v: u32) {
    if ABSOLUTE_POS < a.len() {
        a[ABSOLUTE_POS] = v;
    }
}

#[cube(launch_unchecked)]
fn fill_u64(a: &mut [u64], v: u64) {
    if ABSOLUTE_POS < a.len() {
        a[ABSOLUTE_POS] = v;
    }
}

/// Occupancy deltas: +1 for each listed node.
#[cube(launch_unchecked)]
fn add_occ(nodes: &[u32], occ: &[Atomic<u32>]) {
    if ABSOLUTE_POS < nodes.len() {
        occ[nodes[ABSOLUTE_POS] as usize].fetch_add(1u32);
    }
}

fn count(n: usize) -> CubeCount {
    CubeCount::Static((n as u32).div_ceil(UNITS).max(1), 1, 1)
}

fn arg(h: &Handle, len: usize) -> BufferArg {
    unsafe { BufferArg::from_raw_parts(h.clone(), len.max(1)) }
}

/// One net of a batch as the GPU gets it.
pub struct SlotNet {
    pub net: u32,
    pub source: NodeId,
    /// Sinks in search order, with their criticalities in fixed point.
    pub sinks: Vec<(NodeId, u64)>,
    pub region: chipdb::Box,
}

/// A slot's tree: nodes in order (the source first), each one's parent
/// index and the edge into it.
pub struct SlotTree {
    pub nodes: Vec<NodeId>,
    pub parent: Vec<u32>,
    pub via: Vec<u32>,
}

/// A batch's working buffers, allocated once; `gen` and `slot_round`
/// carry over between batches (labels and marks are stamped with them).
struct Buffers {
    gen: Handle,
    slot_round: Handle,
    phase: Handle,
    tree_len: Handle,
    scal: Handle,
    fmin: Handle,
    misc: Handle,
    next_count: Handle,
    tree_node: Handle,
    tree_parent: Handle,
    tree_via: Handle,
    tree_dq: Handle,
    branch: Handle,
    f_node: Handle,
    f_g: Handle,
    n_node: Handle,
    out: Handle,
}

/// The device's copy of the graph and the routing state.
pub struct Searcher {
    client: cubecl::client::Client,
    n: usize,
    edges: usize,
    edge_start: Handle,
    edge_dst: Handle,
    edge_ok: Handle,
    edge_src: Handle,
    node_box: Handle,
    delay_q: Handle,
    owner: Handle,
    has_owner: bool,
    occ: Handle,
    hist: Handle,
    labels: Handle,
    mark: Handle,
    in_tree: Handle,
    gen_max: u32,
    batch: u32,
    /// Per batch, kept between batches.
    b: Buffers,
    kx_q: u64,
    ky_q: u64,
    astar_q: u64,
    hop_q: u64,
    /// Seconds in the batches, and batches run.
    pub busy_s: f64,
    pub batches: u64,
    /// Of `busy_s`: uploading the batch, the kernel (to its first read),
    /// packing and reading the trees.
    pub phase_s: [f64; 3],
}

impl Searcher {
    pub fn new(rt: Runtime, db: &ChipDb, fx: &Fixed, owner: &[u32]) -> Result<Searcher, String> {
        let device = match rt {
            Runtime::Cuda => cubecl::Device::cuda(0),
            Runtime::Hip => cubecl::Device::rocm(0),
        }
        .map_err(|e| format!("{rt:?}: {e:?}"))?;
        let client = device.client();
        let n = db.nodes();
        let mut edge_start = vec![0u32];
        let mut edge_dst = Vec::with_capacity(db.edge_count());
        for v in 0..n as NodeId {
            edge_dst.extend(db.downhill(v).iter().map(|e| e.dst));
            edge_start.push(edge_dst.len() as u32);
        }
        if edge_dst.len() >= SEED as usize {
            return Err(format!(
                "{} edges: more than a label's 24-bit edge field",
                edge_dst.len()
            ));
        }
        let up32 = |v: &[u32]| client.create_from_slice(u32::as_bytes(v));
        let node_box: Vec<u32> = db
            .node_box
            .iter()
            .map(|b| u32::from_le_bytes([b.x0, b.y0, b.x1, b.y1]))
            .collect();
        let edge_ok: Vec<u32> = fx.edge_ok.iter().map(|&b| b as u32).collect();
        let s = Searcher {
            edges: edge_dst.len(),
            edge_start: up32(&edge_start),
            edge_dst: up32(&edge_dst),
            edge_ok: up32(&edge_ok),
            edge_src: up32(&fx.edge_src),
            node_box: up32(&node_box),
            delay_q: up32(&fx.delay_q),
            has_owner: !owner.is_empty(),
            owner: up32(if owner.is_empty() { &[u32::MAX] } else { owner }),
            occ: up32(&vec![0u32; n]),
            hist: up32(&vec![0u32; n]),
            labels: client.create_from_slice(u64::as_bytes(&vec![u64::MAX; SLOTS * n])),
            mark: up32(&vec![0u32; SLOTS * n]),
            in_tree: up32(&vec![0u32; SLOTS * n]),
            gen_max: 0,
            batch: 0,
            b: Buffers {
                gen: client.create_from_slice(u32::as_bytes(&[0u32; SLOTS])),
                slot_round: client.create_from_slice(u32::as_bytes(&[1u32; SLOTS])),
                phase: client.empty(SLOTS * 4),
                tree_len: client.empty(SLOTS * 4),
                scal: client.empty(SLOTS * 3 * 8),
                fmin: client.empty(SLOTS * 8),
                misc: client.empty(SLOTS * 4 * 4),
                next_count: client.empty(SLOTS * 4),
                tree_node: client.empty(SLOTS * TREE_CAP * 4),
                tree_parent: client.create_from_slice(u32::as_bytes(&vec![0u32; SLOTS * TREE_CAP])),
                tree_via: client.create_from_slice(u32::as_bytes(&vec![SEED; SLOTS * TREE_CAP])),
                tree_dq: client.empty(SLOTS * TREE_CAP * 8),
                branch: client.empty(SLOTS * TREE_CAP * 2 * 4),
                f_node: client.empty(SLOTS * FCAP * 4),
                f_g: client.empty(SLOTS * FCAP * 8),
                n_node: client.empty(SLOTS * FCAP * 4),
                out: client.empty(SLOTS * (2 + 3 * TREE_CAP) * 4),
            },
            kx_q: fx.kx_q,
            ky_q: fx.ky_q,
            astar_q: fx.astar_q,
            hop_q: fx.hop_q,
            busy_s: 0.0,
            batches: 0,
            phase_s: [0.0; 3],
            n,
            client,
        };
        Ok(s)
    }

    /// The routing state at an iteration's start.
    pub fn set_state(&mut self, occ: &[u32], hist: &[u32]) {
        self.occ = self.client.create_from_slice(u32::as_bytes(occ));
        self.hist = self.client.create_from_slice(u32::as_bytes(hist));
    }

    /// Committed nodes (one entry per tree node).
    pub fn commit(&mut self, nodes: &[u32]) {
        if nodes.is_empty() {
            return;
        }
        let h = self.client.create_from_slice(u32::as_bytes(nodes));
        unsafe {
            add_occ::launch_unchecked(
                &self.client,
                count(nodes.len()),
                CubeDim::new_1d(UNITS),
                arg(&h, nodes.len()),
                arg(&self.occ, self.n),
            );
        }
    }

    /// Route `nets` (at most `SLOTS`), each from its source through its
    /// sinks in order; None for a net the GPU couldn't finish (a sink out
    /// of its region, or a tree past `TREE_CAP`).
    pub fn route_batch(&mut self, nets: &[SlotNet], pf_q: u64) -> Vec<Option<SlotTree>> {
        assert!(nets.len() <= SLOTS);
        let t0 = std::time::Instant::now();
        let c = &self.client;
        let n = self.n;
        let k = nets.len();
        // Labels are stamped by generation: start every slot afresh once
        // the stamps could run out in this batch.
        let searches: u32 = nets.iter().map(|x| x.sinks.len() as u32).max().unwrap_or(0) + 1;
        if self.gen_max + searches >= 4094 {
            unsafe {
                fill_u64::launch_unchecked(
                    c,
                    count(SLOTS * n),
                    CubeDim::new_1d(UNITS),
                    arg(&self.labels, SLOTS * n),
                    u64::MAX,
                );
                fill_u32::launch_unchecked(
                    c,
                    count(SLOTS),
                    CubeDim::new_1d(UNITS),
                    arg(&self.b.gen, SLOTS),
                    0u32,
                );
            }
            self.gen_max = 0;
        }
        // One upload: per slot source, net, region, first sink, then the
        // (sink, criticality) pairs.
        let mut inp: Vec<u32> = Vec::new();
        inp.extend(nets.iter().map(|x| x.source));
        inp.extend(nets.iter().map(|x| x.net));
        inp.extend(
            nets.iter()
                .map(|x| u32::from_le_bytes([x.region.x0, x.region.y0, x.region.x1, x.region.y1])),
        );
        let mut first = 0u32;
        for x in nets {
            inp.push(first);
            first += x.sinks.len() as u32;
        }
        inp.push(first);
        let o_sinks = inp.len() as u32;
        for x in nets {
            for &(sk, cq) in &x.sinks {
                inp.push(sk);
                inp.push(cq as u32);
            }
        }
        let inp_h = c.create_from_slice(u32::as_bytes(&inp));
        self.batch += 1;
        let t_up = t0.elapsed().as_secs_f64();
        let b = &self.b;
        unsafe {
            slot_kernel::launch_unchecked(
                c,
                CubeCount::Static(k as u32, 1, 1),
                CubeDim::new_1d(UNITS),
                arg(&self.edge_start, n + 1),
                arg(&self.edge_dst, self.edges),
                arg(&self.edge_ok, self.edges),
                arg(&self.edge_src, self.edges),
                arg(&self.node_box, n),
                arg(&self.delay_q, n),
                arg(&self.occ, n),
                arg(&self.hist, n),
                arg(&self.owner, n),
                arg(&inp_h, inp.len()),
                arg(&b.out, SLOTS * (2 + 3 * TREE_CAP)),
                arg(&b.gen, SLOTS),
                arg(&b.slot_round, SLOTS),
                arg(&b.phase, SLOTS),
                arg(&b.tree_len, SLOTS),
                arg(&b.scal, SLOTS * 3),
                arg(&b.fmin, SLOTS),
                arg(&b.misc, SLOTS * 4),
                arg(&b.next_count, SLOTS),
                arg(&self.labels, SLOTS * n),
                arg(&self.mark, SLOTS * n),
                arg(&self.in_tree, SLOTS * n),
                arg(&b.tree_node, SLOTS * TREE_CAP),
                arg(&b.tree_parent, SLOTS * TREE_CAP),
                arg(&b.tree_via, SLOTS * TREE_CAP),
                arg(&b.tree_dq, SLOTS * TREE_CAP),
                arg(&b.branch, SLOTS * TREE_CAP * 2),
                arg(&b.f_node, SLOTS * FCAP),
                arg(&b.f_g, SLOTS * FCAP),
                arg(&b.n_node, SLOTS * FCAP),
                pf_q,
                self.kx_q,
                self.ky_q,
                self.astar_q,
                self.hop_q,
                self.has_owner as u32,
                self.batch,
                k as u32,
                o_sinks,
                n,
                TREE_CAP,
                FCAP,
            );
        }
        let out =
            u32::from_bytes(&c.read_one(b.out.clone()).expect("reading from the GPU")).to_vec();
        let t_kernel = t0.elapsed().as_secs_f64();
        // Searches started this batch, at most one per sink.
        self.gen_max += searches;
        let t_all = t0.elapsed().as_secs_f64();
        self.phase_s[0] += t_up;
        self.phase_s[1] += t_kernel - t_up;
        self.phase_s[2] += t_all - t_kernel;
        self.busy_s += t_all;
        self.batches += 1;
        let stride = 2 + 3 * TREE_CAP;
        (0..k)
            .map(|s| {
                let o = s * stride;
                (out[o] == DONE).then(|| {
                    let len = out[o + 1] as usize;
                    SlotTree {
                        nodes: (0..len).map(|i| out[o + 2 + 3 * i]).collect(),
                        parent: (0..len).map(|i| out[o + 3 + 3 * i]).collect(),
                        via: (0..len).map(|i| out[o + 4 + 3 * i]).collect(),
                    }
                })
            })
            .collect()
    }
}

impl crate::Router<'_> {
    /// A batch's nets on the GPU, each in the order and with the
    /// criticalities `route_net_bucketed` uses; a net the GPU gives up on
    /// is routed by the CPU's search against the same state, so the trees
    /// are the CPU's either way.
    pub(crate) fn route_batch_gpu(
        &self,
        g: &mut Searcher,
        s: &mut crate::Scratch,
        batch: &[usize],
        nets: &[crate::NetReq],
        crit: &[Vec<f32>],
    ) -> Vec<(anyhow::Result<crate::Tree>, f64)> {
        let fx = self.fixed.as_ref().expect("the GPU searches in integers");
        let pf_q = crate::bucket::q(self.present_factor);
        let slot_of = |i: usize| -> SlotNet {
            let net = &nets[i];
            let mut order: Vec<usize> = (0..net.sinks.len()).collect();
            order.sort_by(|&a, &b| {
                crit[i][b].total_cmp(&crit[i][a]).then(
                    self.distance(net.source, net.sinks[a])
                        .total_cmp(&self.distance(net.source, net.sinks[b])),
                )
            });
            SlotNet {
                net: i as u32,
                source: net.source,
                sinks: order
                    .iter()
                    .map(|&k| (net.sinks[k], Fixed::crit_q(crit[i][k])))
                    .collect(),
                region: self.region(net).unwrap_or(chipdb::Box::EVERYWHERE),
            }
        };
        let mut out = Vec::with_capacity(batch.len());
        for chunk in batch.chunks(SLOTS) {
            let slots: Vec<SlotNet> = chunk.iter().map(|&i| slot_of(i)).collect();
            let trees = g.route_batch(&slots, pf_q);
            for (&i, t) in chunk.iter().zip(trees) {
                let tree = match t {
                    Some(t) => Ok(self.tree_of_slot(&nets[i], t)),
                    None => {
                        let mut labels = std::mem::take(&mut s.labels);
                        let tree = self.route_net_bucketed(
                            fx,
                            s,
                            &mut labels,
                            i as u32,
                            &nets[i],
                            &crit[i],
                            None,
                        );
                        s.labels = labels;
                        tree
                    }
                };
                out.push((tree, 0.0));
            }
        }
        out
    }

    fn tree_of_slot(&self, net: &crate::NetReq, t: SlotTree) -> crate::Tree {
        let mut delay = Vec::with_capacity(t.nodes.len());
        for (i, &n) in t.nodes.iter().enumerate() {
            let own = self.node[n as usize].delay;
            delay.push(if i == 0 {
                own
            } else {
                delay[t.parent[i] as usize] + own
            });
        }
        let index: std::collections::HashMap<NodeId, usize> =
            t.nodes.iter().enumerate().map(|(i, &n)| (n, i)).collect();
        crate::Tree {
            sink_delay: net.sinks.iter().map(|sk| delay[index[sk]]).collect(),
            pips: t.via[1..]
                .iter()
                .map(|&e| *self.db.edge(e as usize))
                .collect(),
            nodes: t.nodes,
            parent: t.parent,
        }
    }
}
