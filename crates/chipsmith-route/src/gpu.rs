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
//! frontier. A label is `(255 - generation) << 56 | cost << 24 | edge`
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
/// A slot adds at most this many nodes to the tree it starts from; a net
/// that needs more is routed on the CPU.
pub const OUT_CAP: usize = 1024;
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
    init_node: &[u32],
    init_dq: &[u64],
    own_node: &[u32],
    own_mark: &mut [u32],
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
    bucket_q: u64,
    batch: u32,
    slots: u32,
    o_sinks: u32,
    #[comptime] n: usize,
    #[comptime] out_cap: usize,
    #[comptime] fcap: usize,
) {
    // inp: per slot source, net, region, first sink; then one more first
    // sink; then per slot where its starting tree is in `init_*`, its
    // length, and where its tree goes in the tree pool; then where its
    // previous tree is in `own_node` and its length; then (sink,
    // criticality) pairs.
    let s = CUBE_POS as usize;
    let k = slots as usize;
    let t = UNIT_POS as usize;
    let nt = CUBE_DIM as usize;
    let base = s * n;
    let fb = s * fcap;
    let o_tree = 4 * k + 1;
    let io = inp[o_tree + s] as usize;
    let il = inp[o_tree + k + s] as usize;
    let tb = inp[o_tree + 2 * k + s] as usize;
    let bb = s * out_cap * 2;
    // The starting tree: the source, and what the net keeps of its last
    // routing (or, for one sink of a wide net, its tree so far).
    let mut i0 = t;
    while i0 < il {
        let node = init_node[io + i0];
        tree_node[tb + i0] = node;
        tree_dq[tb + i0] = init_dq[io + i0];
        in_tree[base + node as usize] = batch;
        i0 += nt;
    }
    // The net's previous tree, still in the occupancy: not another net's.
    let oo = inp[o_tree + 3 * k + s] as usize;
    let ol = inp[o_tree + 4 * k + s] as usize;
    let mut i1 = t;
    while i1 < ol {
        own_mark[base + own_node[oo + i1] as usize] = batch;
        i1 += nt;
    }
    // scal: bound, sink_g, crit; misc: cur_sink, sink_box, next_sink, frontier size.
    if t == 0 {
        tree_len[s] = il as u32;
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
                let reached = (sl >> 56) == (255u64 - gen[s] as u64);
                let sg = (sl >> 24) & 4294967295u64;
                let fm = fmin[s].load();
                let empty = fsize == 0;
                if reached && (empty || sg <= fm) {
                    ph = 1u32;
                } else if empty {
                    ph = 3u32;
                } else {
                    scal[s * 3] = fm + bucket_q;
                    if reached {
                        scal[s * 3 + 1] = sg;
                    } else {
                        scal[s * 3 + 1] = 4294967296u64;
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
                        branch[bb + len as usize * 2] = v;
                        branch[bb + len as usize * 2 + 1] = via;
                        len += 1u32;
                        v = edge_src[via as usize];
                        if len as usize >= out_cap {
                            walking = false;
                            whole = false;
                        }
                    }
                }
                let mut cut = len;
                let mut found = false;
                for i in 0..len {
                    if !found && in_tree[base + branch[bb + i as usize * 2] as usize] == batch {
                        cut = i;
                        found = true;
                    }
                }
                let mut attach = v;
                if cut < len {
                    attach = branch[bb + cut as usize * 2];
                }
                let tl = tree_len[s];
                let mut parent = 0u32;
                for i in 0..tl {
                    if tree_node[tb + i as usize] == attach {
                        parent = i;
                    }
                }
                let fits = whole && (tl + cut) as usize <= il + out_cap;
                let mut j = cut;
                while j > 0u32 && fits {
                    j -= 1u32;
                    let node = branch[bb + j as usize * 2];
                    let idx = tree_len[s];
                    tree_node[tb + idx as usize] = node;
                    tree_parent[tb + idx as usize] = parent;
                    tree_via[tb + idx as usize] = branch[bb + j as usize * 2 + 1];
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
            let tag = (255u64 - gen[s] as u64) << 56;
            let cq = scal[s * 3 + 2];
            if ph == 6u32 {
                // The new search's seeds: every tree node.
                let tl = tree_len[s] as usize;
                let mut i = t;
                while i < tl {
                    let node = tree_node[tb + i];
                    let mut g = cq * tree_dq[tb + i] / 1024u64;
                    if g > 4294967295u64 {
                        g = 4294967295u64;
                    }
                    labels[base + node as usize].fetch_min(tag | (g << 24) | 16777215u64);
                    mark[base + node as usize].store(round);
                    if i < fcap {
                        n_node[fb + i] = node;
                    }
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
                                    let mut o = occ[d];
                                    if own_mark[base + d] == batch {
                                        o -= 1u32;
                                    }
                                    let cong =
                                        (1u64 + hist[d] as u64) * (1024u64 + pf_q * o as u64);
                                    let mut cost =
                                        ((1024u64 - cq) * cong + cq * delay_q[d] as u64) / 1024u64;
                                    if cost > 2147483648u64 {
                                        cost = 2147483648u64;
                                    }
                                    let mut g2 = gv + cost;
                                    if g2 > 4294967295u64 {
                                        g2 = 4294967295u64;
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
                f_g[fb + i] = (labels[base + v as usize].load() >> 24) & 4294967295u64;
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
    // The result: phase and how many nodes the slot added to its starting
    // tree, then (node, parent, edge) per added node.
    let ob = s * (2 + 3 * out_cap);
    let tl = tree_len[s] as usize;
    if t == 0 {
        out[ob] = phase[s];
        out[ob + 1] = (tl - il) as u32;
    }
    let mut i = il + t;
    while i < tl {
        out[ob + 2 + 3 * (i - il)] = tree_node[tb + i];
        out[ob + 3 + 3 * (i - il)] = tree_parent[tb + i];
        out[ob + 4 + 3 * (i - il)] = tree_via[tb + i];
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

/// Occupancy deltas: -1 for each listed node.
#[cube(launch_unchecked)]
fn sub_occ(nodes: &[u32], occ: &[Atomic<u32>]) {
    if ABSOLUTE_POS < nodes.len() {
        occ[nodes[ABSOLUTE_POS] as usize].fetch_sub(1u32);
    }
}

fn count(n: usize) -> CubeCount {
    CubeCount::Static((n as u32).div_ceil(UNITS).max(1), 1, 1)
}

fn arg(h: &Handle, len: usize) -> BufferArg {
    unsafe { BufferArg::from_raw_parts(h.clone(), len.max(1)) }
}

/// One net of a batch as the GPU gets it.
pub struct SlotNet<'a> {
    pub net: u32,
    /// The tree the slot starts from, source first, parents before
    /// children: each node with its delay from the source in `Fixed`
    /// units.
    pub init: &'a [NodeId],
    pub init_dq: &'a [u64],
    /// The net's previous tree where it is still in the occupancy (else
    /// empty): the search doesn't count it as another net's.
    pub own: &'a [NodeId],
    /// Sinks in search order, with their criticalities in fixed point.
    pub sinks: Vec<(NodeId, u64)>,
    pub region: chipdb::Box,
}

/// What a slot added to its starting tree: nodes in order, each one's
/// parent (an index into the whole tree) and the edge into it.
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
    /// The tree pool's capacity in nodes.
    pool_cap: usize,
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
    /// Whether `occ` and `hist` hold this iteration's state; then the
    /// nodes trees took and gave up since they were last brought up to
    /// date.
    state_set: bool,
    placed: Vec<u32>,
    lifted: Vec<u32>,
    labels: Handle,
    mark: Handle,
    own_mark: Handle,
    in_tree: Handle,
    gen_max: u32,
    batch: u32,
    /// Per batch, kept between batches.
    b: Buffers,
    kx_q: u64,
    ky_q: u64,
    astar_q: u64,
    hop_q: u64,
    bucket_q: u64,
    /// Seconds in the batches, and batches run.
    pub busy_s: f64,
    pub batches: u64,
    /// Slots the GPU gave up on (the CPU routed them).
    pub gave_up: u64,
    /// Of `busy_s`: uploading the batch, the kernel (to its first read),
    /// packing and reading the trees.
    pub phase_s: [f64; 3],
}

/// Keep compiled kernels in the user's cache directory (CubeCL's
/// `persistence`), so a run doesn't recompile them (about 1.5 s). Before the
/// first device: CubeCL's configuration is fixed once read, and whichever of
/// the placer and the router gets here first sets the same one.
fn cache_kernels() {
    use cubecl::config::{cache::CacheConfig, CubeClRuntimeConfig, RuntimeConfig};
    let mut config = CubeClRuntimeConfig::default();
    config.compilation.cache = true;
    config.environment.path = CacheConfig::Global;
    CubeClRuntimeConfig::try_set(config);
}

impl Searcher {
    pub fn new(rt: Runtime, db: &ChipDb, fx: &Fixed, owner: &[u32]) -> Result<Searcher, String> {
        cache_kernels();
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
            state_set: false,
            placed: Vec::new(),
            lifted: Vec::new(),
            // Per slot, per node, filled on the device: uploaded from the
            // host, these 1.1 GB (sha256) took 12 of a 13 s setup.
            labels: client.empty(SLOTS * n * 8),
            mark: client.empty(SLOTS * n * 4),
            own_mark: client.empty(SLOTS * n * 4),
            in_tree: client.empty(SLOTS * n * 4),
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
                pool_cap: 0,
                tree_node: client.empty(4),
                tree_parent: client.empty(4),
                tree_via: client.empty(4),
                tree_dq: client.empty(8),
                branch: client.empty(SLOTS * OUT_CAP * 2 * 4),
                f_node: client.empty(SLOTS * FCAP * 4),
                f_g: client.empty(SLOTS * FCAP * 8),
                n_node: client.empty(SLOTS * FCAP * 4),
                out: client.empty(SLOTS * (2 + 3 * OUT_CAP) * 4),
            },
            kx_q: fx.kx_q,
            ky_q: fx.ky_q,
            astar_q: fx.astar_q,
            hop_q: fx.hop_q,
            bucket_q: fx.bucket_q,
            busy_s: 0.0,
            batches: 0,
            gave_up: 0,
            phase_s: [0.0; 3],
            n,
            client,
        };
        unsafe {
            fill_u64::launch_unchecked(
                &s.client,
                count(SLOTS * n),
                CubeDim::new_1d(UNITS),
                arg(&s.labels, SLOTS * n),
                u64::MAX,
            );
            for h in [&s.mark, &s.own_mark, &s.in_tree] {
                fill_u32::launch_unchecked(
                    &s.client,
                    count(SLOTS * n),
                    CubeDim::new_1d(UNITS),
                    arg(h, SLOTS * n),
                    0u32,
                );
            }
        }
        Ok(s)
    }

    /// History changed (an iteration ended): the next batch uploads the
    /// whole state.
    pub fn invalidate(&mut self) {
        self.state_set = false;
        self.placed.clear();
        self.lifted.clear();
    }

    /// A tree's nodes went onto the occupancy.
    pub fn note_placed(&mut self, nodes: &[u32]) {
        if self.state_set {
            self.placed.extend_from_slice(nodes);
        }
    }

    /// A tree's nodes came off the occupancy.
    pub fn note_lifted(&mut self, nodes: &[u32]) {
        if self.state_set {
            self.lifted.extend_from_slice(nodes);
        }
    }

    /// Bring the device's occupancy and history up to date: all of it
    /// after `invalidate`, else the deltas noted since.
    fn sync(&mut self, node: &[crate::NodeInfo]) {
        if !self.state_set {
            let occ: Vec<u32> = node.iter().map(|n| n.occ as u32).collect();
            let hist: Vec<u32> = node.iter().map(|n| n.hist as u32).collect();
            self.occ = self.client.create_from_slice(u32::as_bytes(&occ));
            self.hist = self.client.create_from_slice(u32::as_bytes(&hist));
            self.state_set = true;
            return;
        }
        for (list, add) in [
            (std::mem::take(&mut self.placed), true),
            (std::mem::take(&mut self.lifted), false),
        ] {
            if list.is_empty() {
                continue;
            }
            let h = self.client.create_from_slice(u32::as_bytes(&list));
            unsafe {
                if add {
                    add_occ::launch_unchecked(
                        &self.client,
                        count(list.len()),
                        CubeDim::new_1d(UNITS),
                        arg(&h, list.len()),
                        arg(&self.occ, self.n),
                    );
                } else {
                    sub_occ::launch_unchecked(
                        &self.client,
                        count(list.len()),
                        CubeDim::new_1d(UNITS),
                        arg(&h, list.len()),
                        arg(&self.occ, self.n),
                    );
                }
            }
        }
    }

    /// Route `nets` (at most `SLOTS`), each from its starting tree through
    /// its sinks in order; None for a net the GPU couldn't finish (a sink
    /// out of its region, a frontier past `FCAP`, more than `OUT_CAP` new
    /// nodes).
    pub fn route_batch(&mut self, nets: &[SlotNet], pf_q: u64) -> Vec<Option<SlotTree>> {
        assert!(nets.len() <= SLOTS);
        let t0 = std::time::Instant::now();
        let c = &self.client;
        let n = self.n;
        let k = nets.len();
        // Labels are stamped by generation: start every slot afresh once
        // the stamps could run out in this batch.
        let searches: u32 = nets.iter().map(|x| x.sinks.len() as u32).max().unwrap_or(0) + 1;
        assert!(searches < 254, "a slot of {searches} searches");
        if self.gen_max + searches >= 254 {
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
        // The tree pool: each slot's starting tree and room for what it
        // adds.
        let mut tree_off = Vec::with_capacity(k);
        let mut pool = 0usize;
        for x in nets {
            tree_off.push(pool as u32);
            pool += x.init.len() + OUT_CAP;
        }
        if pool > self.b.pool_cap {
            let cap = pool.next_power_of_two();
            self.b.tree_node = c.empty(cap * 4);
            self.b.tree_parent = c.empty(cap * 4);
            self.b.tree_via = c.empty(cap * 4);
            self.b.tree_dq = c.empty(cap * 8);
            self.b.pool_cap = cap;
        }
        let init_node: Vec<u32> = nets.iter().flat_map(|x| x.init.iter().copied()).collect();
        let init_dq: Vec<u64> = nets
            .iter()
            .flat_map(|x| x.init_dq.iter().copied())
            .collect();
        // One upload: per slot source, net, region, first sink, then where
        // its trees go, then the (sink, criticality) pairs.
        let mut inp: Vec<u32> = Vec::new();
        inp.extend(nets.iter().map(|x| x.init[0]));
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
        let mut at = 0u32;
        for x in nets {
            inp.push(at);
            at += x.init.len() as u32;
        }
        inp.extend(nets.iter().map(|x| x.init.len() as u32));
        inp.extend(&tree_off);
        let mut at = 0u32;
        for x in nets {
            inp.push(at);
            at += x.own.len() as u32;
        }
        inp.extend(nets.iter().map(|x| x.own.len() as u32));
        let own_node: Vec<u32> = nets.iter().flat_map(|x| x.own.iter().copied()).collect();
        let o_sinks = inp.len() as u32;
        for x in nets {
            for &(sk, cq) in &x.sinks {
                inp.push(sk);
                inp.push(cq as u32);
            }
        }
        let inp_h = c.create_from_slice(u32::as_bytes(&inp));
        let init_node_h = c.create_from_slice(u32::as_bytes(&init_node));
        let init_dq_h = c.create_from_slice(u64::as_bytes(&init_dq));
        // Never empty: a buffer of nothing can't be made.
        let own_node_h = c.create_from_slice(u32::as_bytes(if own_node.is_empty() {
            &[0u32]
        } else {
            &own_node
        }));
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
                arg(&init_node_h, init_node.len()),
                arg(&init_dq_h, init_dq.len()),
                arg(&own_node_h, own_node.len()),
                arg(&self.own_mark, SLOTS * n),
                arg(&b.out, SLOTS * (2 + 3 * OUT_CAP)),
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
                arg(&b.tree_node, b.pool_cap),
                arg(&b.tree_parent, b.pool_cap),
                arg(&b.tree_via, b.pool_cap),
                arg(&b.tree_dq, b.pool_cap),
                arg(&b.branch, SLOTS * OUT_CAP * 2),
                arg(&b.f_node, SLOTS * FCAP),
                arg(&b.f_g, SLOTS * FCAP),
                arg(&b.n_node, SLOTS * FCAP),
                pf_q,
                self.kx_q,
                self.ky_q,
                self.astar_q,
                self.hop_q,
                self.has_owner as u32,
                self.bucket_q,
                self.batch,
                k as u32,
                o_sinks,
                n,
                OUT_CAP,
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
        let stride = 2 + 3 * OUT_CAP;
        self.gave_up += (0..k).filter(|&s| out[s * stride] != DONE).count() as u64;
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
    /// A batch's nets on the GPU, each from the tree `route_net_bucketed`
    /// starts from (what survives of `olds`, with `keep`) and with its
    /// sink order and criticalities; a net the GPU gives up on is routed by
    /// the CPU's search against the same state, so the trees are the CPU's
    /// either way.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn route_batch_gpu(
        &self,
        g: &mut Searcher,
        s: &mut crate::Scratch,
        batch: &[usize],
        nets: &[crate::NetReq],
        crit: &[Vec<f32>],
        olds: &[crate::Tree],
        keep: bool,
        present: bool,
    ) -> Vec<(anyhow::Result<crate::Tree>, f64)> {
        let fx = self.fixed.as_ref().expect("the GPU searches in integers");
        g.sync(&self.node);
        let pf_q = crate::bucket::q(self.present_factor);
        let mut out = Vec::with_capacity(batch.len());
        for (chunk, olds) in batch.chunks(SLOTS).zip(olds.chunks(SLOTS)) {
            let starts: Vec<crate::bucket::Start> = chunk
                .iter()
                .zip(olds)
                .map(|(&i, old)| {
                    self.start_tree(fx, s, &nets[i], &crit[i], keep.then_some(old), present)
                })
                .collect();
            let slots: Vec<SlotNet> = chunk
                .iter()
                .zip(&starts)
                .zip(olds)
                .map(|((&i, st), old)| SlotNet {
                    net: i as u32,
                    init: &st.tree.nodes,
                    init_dq: &st.dq,
                    own: if present { &old.nodes } else { &[] },
                    sinks: self
                        .sink_order(&nets[i], &crit[i])
                        .iter()
                        .map(|&k| (nets[i].sinks[k], Fixed::crit_q(crit[i][k])))
                        .collect(),
                    region: self.search_region(i as u32, &nets[i]),
                })
                .collect();
            let trees = g.route_batch(&slots, pf_q);
            drop(slots);
            for (((&i, t), st), old) in chunk.iter().zip(trees).zip(starts).zip(olds) {
                let tree = match t {
                    Some(t) => Ok(self.tree_of_slot(&nets[i], st.tree, t)),
                    None => self.route_net_bucketed(
                        fx,
                        s,
                        i as u32,
                        &nets[i],
                        &crit[i],
                        keep.then_some(old),
                        present.then_some(old),
                    ),
                };
                out.push((tree, 0.0));
            }
        }
        out
    }

    /// One integer search per sink in `todo`, side by side on the GPU, each
    /// from the tree in `st`: the branch back from the sink (None where
    /// the GPU gave up).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn search_group_gpu(
        &self,
        g: &mut Searcher,
        index: u32,
        st: &crate::bucket::Start,
        todo: &[usize],
        net: &crate::NetReq,
        crit: &[f32],
        region: chipdb::Box,
    ) -> Vec<Option<crate::Branch>> {
        g.sync(&self.node);
        let pf_q = crate::bucket::q(self.present_factor);
        let mut out = Vec::with_capacity(todo.len());
        for chunk in todo.chunks(SLOTS) {
            let slots: Vec<SlotNet> = chunk
                .iter()
                .map(|&i| SlotNet {
                    net: index,
                    init: &st.tree.nodes,
                    init_dq: &st.dq,
                    own: &[],
                    sinks: vec![(net.sinks[i], Fixed::crit_q(crit[i]))],
                    region,
                })
                .collect();
            for t in g.route_batch(&slots, pf_q) {
                // What the slot added is the branch, from where it meets
                // the tree out to the sink.
                out.push(t.filter(|t| !t.nodes.is_empty()).map(|t| {
                    let seed = st.tree.nodes[t.parent[0] as usize];
                    let branch = t
                        .nodes
                        .iter()
                        .zip(&t.via)
                        .rev()
                        .map(|(&n, &e)| (n, *self.db.edge(e as usize)))
                        .collect();
                    (branch, seed)
                }));
            }
        }
        out
    }

    /// A net's tree: its starting tree and what the GPU added.
    fn tree_of_slot(&self, net: &crate::NetReq, mut tree: crate::Tree, t: SlotTree) -> crate::Tree {
        for ((&n, &p), &e) in t.nodes.iter().zip(&t.parent).zip(&t.via) {
            crate::add_node(&mut tree, n, *self.db.edge(e as usize), p);
        }
        let mut delay = Vec::with_capacity(tree.nodes.len());
        for (i, &n) in tree.nodes.iter().enumerate() {
            let own = self.node[n as usize].delay;
            delay.push(if i == 0 {
                own
            } else {
                delay[tree.parent[i] as usize] + own
            });
        }
        let index: std::collections::HashMap<NodeId, usize> = tree
            .nodes
            .iter()
            .enumerate()
            .map(|(i, &n)| (n, i))
            .collect();
        tree.sink_delay = net.sinks.iter().map(|sk| delay[index[sk]]).collect();
        tree
    }
}
