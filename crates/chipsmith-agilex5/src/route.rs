//! Routed nets → CRAM bits through the graph join, and the check against a
//! Quartus-routed `.sof` (README (k)).

use std::collections::{BTreeMap, HashMap, HashSet};

use rustc_hash::FxHashMap;

use anyhow::{Context, Result};

use crate::db::{BitPos, Db};
use crate::graph::{Graph, Join, Miss};

/// Fitter element types as DYGR names them: the fitter drops DYGR's
/// underscores (`HVIOWRX1RGMIIWRCONNRE` is `HVIOWR_X1_RGMII_WR_CONN_RE`).
pub fn type_aliases(g: &Graph) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for n in &g.names {
        if let Some((t, _)) = n.split_once(':') {
            if t.contains('_') && !out.contains_key(&t.replace('_', "")) {
                out.insert(t.replace('_', ""), t.to_owned());
            }
        }
    }
    out
}

/// A fitter routing-node name (`R0_X120_Y48_N0_I27`) as DYGR names it
/// (`H0:X120Y48Z0I27`); other names pass through.
pub fn dygr_name(fit: &str, aliases: &HashMap<String, String>) -> String {
    let Some((head, i)) = fit.rsplit_once("_I") else {
        return fit.to_owned();
    };
    let parse = || -> Option<String> {
        let (head, n) = head.rsplit_once("_N")?;
        let (head, y) = head.rsplit_once("_Y")?;
        let (t, x) = head.rsplit_once("_X")?;
        for v in [i, n, y, x] {
            v.parse::<u32>().ok()?;
        }
        let t = match t {
            "LOCAL_INTERCONNECT" => "LL".to_owned(),
            "BLOCK_INPUT_MUX_PASSTHROUGH" => "LEIM".to_owned(),
            "BLOCK_INPUT_MUX_PASSTHROUGH_WB" => "LEIM_WB".to_owned(),
            "HIO_BUFFER" => "HIOB".to_owned(),
            "VIO_BUFFER" => "VIOB".to_owned(),
            _ => match (t.as_bytes()[0], t[1..].parse::<u32>()) {
                (b'R', Ok(k)) => format!("H{k}"),
                (b'C', Ok(k)) => format!("V{k}"),
                _ => aliases.get(t).cloned().unwrap_or_else(|| t.to_owned()),
            },
        };
        Some(format!("{t}:X{x}Y{y}Z{n}I{i}"))
    };
    parse().unwrap_or_else(|| fit.to_owned())
}

/// Routing paths from `tools/rgdump/routes.tcl` output ("C …", "R <hop>
/// <fanout>"…, "E"), or one path of DYGR/fitter node names per line. Hops
/// that are netlist ports (contain '/') are dropped.
pub fn read_routes(path: &str, aliases: &HashMap<String, String>) -> Result<Vec<Vec<String>>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let mut out = Vec::new();
    let mut cur: Option<Vec<String>> = None;
    for l in text.lines() {
        if l.starts_with("C ") {
            cur = Some(Vec::new());
        } else if let Some(h) = l.strip_prefix("R ") {
            let name = h.split_whitespace().next().unwrap_or("");
            if let Some(c) = cur.as_mut() {
                if !name.contains('/') {
                    c.push(dygr_name(name, aliases));
                }
            }
        } else if l == "E" {
            out.extend(cur.take());
        } else if !["X ", "A ", "D "].iter().any(|p| l.starts_with(p))
            && !l.trim().is_empty()
            && cur.is_none()
        {
            out.push(
                l.split_whitespace()
                    .map(|n| dygr_name(n, aliases))
                    .collect(),
            );
        }
    }
    Ok(out)
}

/// Every image bit some edge configuration writes, with its polarity and
/// the first (uid id, config id) that writes it, and the misses of edges
/// that resolve to no bits.
pub struct Universe {
    /// (polarity, idle value, uid id, config id)
    pub bits: FxHashMap<(usize, u8), (bool, bool, u32, u32)>,
    pub misses: BTreeMap<Miss, usize>,
    pub edges: usize,
    pub resolved: usize,
    pub pairs: usize,
}

pub fn universe(join: &mut Join, graph: &Graph) -> Result<Universe> {
    let mut seen: FxHashMap<(u32, u32), usize> = FxHashMap::default();
    for e in 0..graph.dst.len() {
        if graph.cfg[e] != 0 {
            *seen.entry((graph.uid[e], graph.cfg[e])).or_default() += 1;
        }
    }
    let mut u = Universe {
        bits: FxHashMap::default(),
        misses: BTreeMap::new(),
        edges: 0,
        resolved: 0,
        pairs: seen.len(),
    };
    for e in 0..graph.dst.len() {
        if graph.cfg[e] == 0 {
            if let Some(Ok(idle)) = join.rule_edge(e, true)? {
                for (p, v) in idle {
                    u.bits
                        .entry((p.byte, p.bit))
                        .or_insert((p.equal, v, graph.uid[e], 0));
                }
            }
        }
    }
    for (&(uid, cfg), &n) in &seen {
        u.edges += n;
        match join.pair(uid, cfg)? {
            Some(Ok(_)) => {
                u.resolved += n;
                if let Some(Ok(idle)) = join.pair_idle(uid, cfg)? {
                    for (p, v) in idle {
                        u.bits
                            .entry((p.byte, p.bit))
                            .or_insert((p.equal, v, uid, cfg));
                    }
                }
            }
            Some(Err(m)) => *u.misses.entry(m).or_default() += n,
            None => {}
        }
    }
    Ok(u)
}

pub struct Applied {
    pub hops: usize,
    pub no_edge: Vec<(String, String)>,
    pub unknown_node: HashSet<String>,
    pub configured: usize,
    pub pass_through: usize,
    pub misses: BTreeMap<Miss, usize>,
}

/// Write the bits of every edge along the paths into `img`.
pub fn apply(
    join: &mut Join,
    graph: &Graph,
    idx: &FxHashMap<&str, u32>,
    paths: &[Vec<String>],
    img: &mut [u8],
) -> Result<Applied> {
    let mut a = Applied {
        hops: 0,
        no_edge: Vec::new(),
        unknown_node: HashSet::new(),
        configured: 0,
        pass_through: 0,
        misses: BTreeMap::new(),
    };
    let mut done: HashSet<usize> = HashSet::new();
    for path in paths {
        for w in path.windows(2) {
            a.hops += 1;
            let (Some(&s), Some(&d)) = (idx.get(w[0].as_str()), idx.get(w[1].as_str())) else {
                for n in w {
                    if !idx.contains_key(n.as_str()) {
                        a.unknown_node.insert(n.clone());
                    }
                }
                continue;
            };
            let Some(e) = graph.edge(s, d) else {
                a.no_edge.push((w[0].clone(), w[1].clone()));
                continue;
            };
            if !done.insert(e) {
                continue;
            }
            match join.edge(e)? {
                Some(Ok(ws)) => {
                    a.configured += 1;
                    for (p, v) in ws {
                        p.set(img, v);
                    }
                }
                Some(Err(m)) if m.is_pass_through() => a.pass_through += 1,
                Some(Err(m)) => *a.misses.entry(m).or_default() += 1,
                None => {}
            }
        }
    }
    Ok(a)
}

/// Set every routing bit to its mux's idle state.
pub fn clear(u: &Universe, img: &mut [u8]) {
    for (&(byte, bit), &(equal, idle, ..)) in &u.bits {
        BitPos { byte, bit, equal }.set(img, idle);
    }
}

/// A routing bit's block and mux, from the first configuration writing it.
pub fn bit_name(u: &Universe, graph: &Graph, at: (usize, u8)) -> String {
    let (_, _, uid, cfg) = u.bits[&at];
    format!(
        "{} {}",
        graph.uids[uid as usize],
        graph.cfgs[cfg as usize].split(' ').next().unwrap_or("")
    )
}

/// Edges whose mux setting the image holds (every written bit matches and
/// the setting isn't the mux's idle state): the routing read back from a
/// bitstream. Returns edge indices.
pub fn decode(join: &mut Join, graph: &Graph, img: &[u8]) -> Result<Vec<usize>> {
    decode_in(join, graph, img, None)
}

/// `decode`, looking only at muxes in `dirty`, the instances with a bit
/// that differs from the idle image: a mux whose bits are all idle selects
/// nothing, since the idle image selects no edge (a design's extract lists
/// only its own edges).
pub fn decode_in(
    join: &mut Join,
    graph: &Graph,
    img: &[u8],
    dirty: Option<&rustc_hash::FxHashSet<&str>>,
) -> Result<Vec<usize>> {
    let mut active: FxHashMap<(u32, u32), bool> = FxHashMap::default();
    let in_dirty: Vec<bool> = graph
        .uids
        .iter()
        .map(|u| dirty.is_none_or(|d| d.contains(u.as_str())))
        .collect();
    let mut out = Vec::new();
    for e in 0..graph.dst.len() {
        let (u, c) = (graph.uid[e], graph.cfg[e]);
        if c == 0 {
            if let (Some(Ok(w)), Some(Ok(idle))) = (
                join.rule_edge_in(e, false, dirty)?,
                join.rule_edge_in(e, true, dirty)?,
            ) {
                if w.iter().all(|(p, v)| p.get(img) == *v)
                    && w.iter().zip(&idle).any(|(a, b)| a.1 != b.1)
                {
                    out.push(e);
                }
            }
            continue;
        }
        if !in_dirty[u as usize] {
            continue;
        }
        let on = match active.get(&(u, c)) {
            Some(&on) => on,
            None => {
                let on = match (join.pair(u, c)?, join.pair_idle(u, c)?) {
                    (Some(Ok(w)), Some(Ok(idle))) => {
                        w.iter().all(|(p, v)| p.get(img) == *v)
                            && w.iter().zip(&idle).any(|(a, b)| a.1 != b.1)
                    }
                    _ => false,
                };
                active.insert((u, c), on);
                on
            }
        };
        if on {
            out.push(e);
        }
    }
    Ok(out)
}

/// Route one net: a breadth-first search from any of `from` to `to` over
/// edges that are hardwired, pass-through or resolve to bits, through
/// nodes nobody else drives. Returns the path's edges.
pub fn find_path(
    join: &mut Join,
    graph: &Graph,
    from: &[u32],
    to: u32,
    busy: &HashSet<u32>,
    clock: bool,
) -> Result<Option<Vec<usize>>> {
    // A clock net stays on the clock network: no general-purpose wires.
    let general = |n: u32| {
        let t = graph.names[n as usize].split(':').next().unwrap_or("");
        t == "LL"
            || t.starts_with("LEIM")
            || ((t.starts_with('H') || t.starts_with('V')) && t[1..].parse::<u32>().is_ok())
    };
    let mut prev: FxHashMap<u32, usize> = FxHashMap::default();
    let mut q: std::collections::VecDeque<u32> = from.iter().copied().collect();
    let start: HashSet<u32> = from.iter().copied().collect();
    let mut usable: FxHashMap<(u32, u32), bool> = FxHashMap::default();
    while let Some(n) = q.pop_front() {
        if n == to {
            let mut path = Vec::new();
            let mut cur = to;
            while !start.contains(&cur) {
                let e = prev[&cur];
                path.push(e);
                cur = edge_src(graph, e);
            }
            path.reverse();
            return Ok(Some(path));
        }
        for e in graph.fanout(n) {
            let d = graph.dst[e];
            if start.contains(&d)
                || prev.contains_key(&d)
                || (busy.contains(&d) && d != to)
                || (clock && general(d))
            {
                continue;
            }
            let (u, c) = (graph.uid[e], graph.cfg[e]);
            let ok = c == 0
                || *match usable.get(&(u, c)) {
                    Some(b) => b,
                    None => {
                        let b = match join.pair(u, c)? {
                            Some(Ok(_)) => true,
                            Some(Err(m)) => m.is_pass_through(),
                            None => true,
                        };
                        usable.entry((u, c)).or_insert(b)
                    }
                };
            if ok {
                prev.insert(d, e);
                q.push_back(d);
            }
        }
    }
    Ok(None)
}

/// The source node of edge e (edges are stored by source).
pub fn edge_src(graph: &Graph, e: usize) -> u32 {
    let (mut lo, mut hi) = (0usize, graph.names.len());
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if graph.fanout(mid as u32).start <= e {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // Several gids can share a start when the earlier ones have no edges.
    while !graph.fanout(lo as u32).contains(&e) {
        lo -= 1;
    }
    lo as u32
}

/// The database, with every block map loaded, and the graph; the two load
/// side by side (the io96 map's xz and the graph's 630 MB take about the
/// same time).
pub fn open(dbdir: &str, gdir: &str) -> Result<(Db, Graph)> {
    let t = std::time::Instant::now();
    let (db, g) = std::thread::scope(|s| {
        let db = s.spawn(|| -> Result<Db> {
            let mut db = Db::open(dbdir)?;
            for ty in db.types()? {
                db.load(&ty)?;
            }
            Ok(db)
        });
        let g = Graph::load(std::path::Path::new(gdir));
        (db.join().expect("database loader panicked"), g)
    });
    let (db, g) = (db?, g?);
    eprintln!(
        "graph: {} nodes, {} edges; database and graph loaded in {:.0} s",
        g.names.iter().filter(|n| !n.is_empty()).count(),
        g.dst.len(),
        t.elapsed().as_secs_f64()
    );
    Ok((db, g))
}

/// Router edge classes, one byte per graph edge (`a5db usable`, used by
/// tools/a5pnr): what a router may do with each edge.
pub mod usable {
    pub const NO: u8 = 0;
    /// A fabric routing mux that resolves to bits.
    pub const MUX: u8 = 1;
    /// A fabric edge with nothing to set: hardwired (LEIM → LAB_RE) or a
    /// pass-through (RPII → RPI, HIOL 4:1 data mux, plain buffers).
    pub const PASS: u8 = 2;
    /// A clock-network mux that resolves to bits (edge API or clock rule).
    pub const CLOCK: u8 = 3;
    /// A mux (fabric or clock) whose setting for this edge is the mux's
    /// idle state: it writes no bit, an image can't show it, and whether an
    /// unconfigured mux passes that input is unknown (README (y)), so a
    /// router leaves it alone.
    pub const IDLE: u8 = 4;
}

/// Classify every edge for a router. Only routing tiles (`cw_rt_*`) and the
/// clock network (`cw_cc_*`) are the router's: I/O block internals are set
/// by the pin template and the redundancy muxes stay idle (README (t)). A
/// hardwired edge (no edge API) counts when both its ends are node types
/// that some router edge touches, or clock-network types.
pub fn usable(join: &mut Join, graph: &Graph) -> Result<Vec<u8>> {
    use usable::*;
    let ty = |n: u32| graph.names[n as usize].split(':').next().unwrap_or("");
    let mut src = vec![0u32; graph.dst.len()];
    for s in 0..graph.names.len() as u32 {
        for e in graph.fanout(s) {
            src[e] = s;
        }
    }
    let mut out = vec![NO; graph.dst.len()];
    let mut seen: FxHashMap<(u32, u32), u8> = FxHashMap::default();
    let mut fabric: HashSet<String> = HashSet::new();
    for e in 0..graph.dst.len() {
        let (u, c) = (graph.uid[e], graph.cfg[e]);
        let class = if c == 0 {
            match (join.rule_edge(e, false)?, join.rule_edge(e, true)?) {
                (Some(Ok(w)), Some(Ok(i))) if w.iter().zip(&i).all(|(a, b)| a.1 == b.1) => IDLE,
                (Some(Ok(_)), _) => CLOCK,
                _ => continue,
            }
        } else {
            let uid = &graph.uids[u as usize];
            let clock = uid.starts_with("cw_cc_");
            if !clock && !uid.starts_with("cw_rt_") {
                continue;
            }
            match seen.get(&(u, c)) {
                Some(&k) => k,
                None => {
                    let idle = match (join.pair(u, c)?, join.pair_idle(u, c)?) {
                        (Some(Ok(w)), Some(Ok(i))) => w.iter().zip(&i).all(|(a, b)| a.1 == b.1),
                        _ => false,
                    };
                    let k = match join.pair(u, c)? {
                        Some(Ok(_)) if idle => IDLE,
                        Some(Ok(_)) if clock => CLOCK,
                        Some(Ok(_)) => MUX,
                        Some(Err(m)) if m.is_pass_through() => PASS,
                        _ => NO,
                    };
                    seen.insert((u, c), k);
                    k
                }
            }
        };
        out[e] = class;
        if class != NO {
            fabric.insert(ty(src[e]).to_owned());
            fabric.insert(ty(graph.dst[e]).to_owned());
        }
    }
    // The DCM's periphery-to-core muxes are wired through node types no
    // router edge touches (HSSI_P2C_RE, CW_CC_DCM_*_MUX_*): the clock network
    // counts as fabric, its DFT cells don't.
    let ours = |t: &str| {
        fabric.contains(t)
            || ((t.starts_with("CW_CC_") || t.starts_with("CE_CC_") || t == "HSSI_P2C_RE")
                && !t.contains("DFT"))
    };
    for e in 0..graph.dst.len() {
        if graph.cfg[e] == 0
            && out[e] == NO
            && graph.uid[e] == 0
            && ours(ty(src[e]))
            && ours(ty(graph.dst[e]))
        {
            out[e] = PASS;
        }
    }
    Ok(out)
}
