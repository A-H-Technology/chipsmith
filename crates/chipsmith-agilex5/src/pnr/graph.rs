//! The routing graph from tools/rgdump (`nodes.tsv`, `edges.bin`) plus
//! `a5db usable`'s edge classes, in compact arrays: 7.6 M nodes and 26 M
//! edges in about 250 MB.

use std::path::Path;

use anyhow::{ensure, Context, Result};
use rustc_hash::FxHashMap;

/// The cache of place and route's graph in a graph directory.
pub const CACHE: &str = "pnr-graph.cache";
const CACHE_MAGIC: &str = "chipsmith-agilex5 pnr graph 1";

/// Edge classes, as `a5db usable` writes them (a5db src/route.rs).
pub const NO: u8 = 0;
pub const MUX: u8 = 1;
pub const PASS: u8 = 2;
pub const CLOCK: u8 = 3;

pub struct Graph {
    pub types: Vec<String>,
    /// Per node: type index, X, Y, Z, I (`TYPE:X..Y..Z..I..`).
    pub ty: Vec<u16>,
    pub x: Vec<u8>,
    pub y: Vec<u8>,
    pub z: Vec<u16>,
    pub i: Vec<u32>,
    first: Vec<u32>,
    pub dst: Vec<u32>,
    pub class: Vec<u8>,
    /// Nodes outside LABs with more than one pass-through fanin: which one
    /// drives them is a setting nobody knows (README (y)); the router
    /// doesn't enter them over a pass-through edge.
    pub ambiguous: Vec<bool>,
    index: FxHashMap<(u16, u8, u8, u16, u32), u32>,
    type_index: FxHashMap<String, u16>,
}

fn parse(n: &str) -> Option<(&str, u32, u32, u32, u32)> {
    let (t, rest) = n.split_once(':')?;
    let rest = rest.strip_prefix('X')?;
    let (x, rest) = rest.split_once('Y')?;
    let (y, rest) = rest.split_once('Z')?;
    let (z, i) = rest.split_once('I')?;
    Some((
        t,
        x.parse().ok()?,
        y.parse().ok()?,
        z.parse().ok()?,
        i.parse().ok()?,
    ))
}

impl Graph {
    /// Per node: whether exactly one graph edge enters it. An edge into such
    /// a node with nothing to configure (class NO: the spare row's
    /// redundancy tiles, which have no bit map, README (s)) passes its
    /// signal with no choice involved, so the readback follows it
    /// (README (z3)); the router doesn't take it.
    pub fn single_fanin(&self) -> Vec<bool> {
        let mut n = vec![0u8; self.len()];
        for &d in &self.dst {
            n[d as usize] = n[d as usize].saturating_add(1);
        }
        n.into_iter().map(|c| c == 1).collect()
    }

    pub fn load(dir: &Path) -> Result<Graph> {
        if let Some(mut r) = crate::cache::Reader::open(&dir.join(CACHE), CACHE_MAGIC)? {
            let types = r.strs()?;
            let mut g = Graph {
                ty: r.u16s()?,
                x: r.u8s()?,
                y: r.u8s()?,
                z: r.u16s()?,
                i: r.u32s()?,
                first: r.u32s()?,
                dst: r.u32s()?,
                class: r.u8s()?,
                ambiguous: r.u8s()?.into_iter().map(|b| b != 0).collect(),
                index: FxHashMap::default(),
                type_index: types
                    .iter()
                    .enumerate()
                    .map(|(k, t)| (t.clone(), k as u16))
                    .collect(),
                types,
            };
            r.done()?;
            g.index.reserve(g.ty.len());
            for n in 0..g.ty.len() {
                if g.ty[n] != u16::MAX {
                    g.index
                        .insert((g.ty[n], g.x[n], g.y[n], g.z[n], g.i[n]), n as u32);
                }
            }
            return Ok(g);
        }
        let text = std::fs::read_to_string(dir.join("nodes.tsv")).context("reading nodes.tsv")?;
        let mut g = Graph {
            types: Vec::new(),
            ty: Vec::new(),
            x: Vec::new(),
            y: Vec::new(),
            z: Vec::new(),
            i: Vec::new(),
            first: Vec::new(),
            dst: Vec::new(),
            class: Vec::new(),
            ambiguous: Vec::new(),
            index: FxHashMap::default(),
            type_index: FxHashMap::default(),
        };
        for l in text.lines() {
            let mut f = l.split('\t');
            let n: usize = f.next().context("nodes.tsv")?.parse()?;
            let name = f.next().context("nodes.tsv")?;
            if g.ty.len() <= n {
                let m = n + 1;
                g.ty.resize(m, u16::MAX);
                g.x.resize(m, 0);
                g.y.resize(m, 0);
                g.z.resize(m, 0);
                g.i.resize(m, 0);
            }
            let (t, x, y, z, i) = parse(name).with_context(|| format!("node name {name:?}"))?;
            let ti = match g.type_index.get(t) {
                Some(&k) => k,
                None => {
                    g.types.push(t.to_owned());
                    g.type_index.insert(t.to_owned(), g.types.len() as u16 - 1);
                    g.types.len() as u16 - 1
                }
            };
            ensure!(
                x < 256 && y < 256 && z < 65536,
                "{name}: coordinates out of range"
            );
            (g.ty[n], g.x[n], g.y[n], g.z[n], g.i[n]) = (ti, x as u8, y as u8, z as u16, i);
            g.index
                .insert((ti, x as u8, y as u8, z as u16, i), n as u32);
        }
        drop(text);
        let n = g.ty.len();
        let raw = std::fs::read(dir.join("edges.bin")).context("reading edges.bin")?;
        let w = |k: usize| u32::from_le_bytes(raw[4 * k..4 * k + 4].try_into().unwrap());
        let mut count = vec![0u32; n + 1];
        let mut p = 0;
        let words = raw.len() / 4;
        while p < words {
            let (s, k) = (w(p) as usize, w(p + 1) as usize);
            count[s + 1] += k as u32;
            p += 2 + 3 * k;
        }
        for k in 0..n {
            count[k + 1] += count[k];
        }
        g.first = count.clone();
        let mut fill = count;
        g.dst = vec![0; g.first[n] as usize];
        // CSR by source in edges.bin order, as a5db's graph numbers edges,
        // so usable.bin (one byte per a5db edge) lines up.
        p = 0;
        while p < words {
            let (s, k) = (w(p) as usize, w(p + 1) as usize);
            for j in 0..k {
                let e = fill[s] as usize;
                g.dst[e] = w(p + 2 + 3 * j);
                fill[s] += 1;
            }
            p += 2 + 3 * k;
        }
        drop(raw);
        g.class =
            std::fs::read(dir.join("usable.bin")).context("reading usable.bin (a5db usable)")?;
        ensure!(
            g.class.len() == g.dst.len(),
            "usable.bin has {} entries for {} edges",
            g.class.len(),
            g.dst.len()
        );
        let mut fanin = vec![0u8; n];
        for e in 0..g.dst.len() {
            if g.class[e] == PASS {
                let d = g.dst[e] as usize;
                fanin[d] = fanin[d].saturating_add(1);
            }
        }
        let lab_re = g.type_index.get("LAB_RE").copied();
        g.ambiguous = (0..n)
            .map(|k| fanin[k] > 1 && Some(g.ty[k]) != lab_re)
            .collect();
        Ok(g)
    }

    pub fn len(&self) -> usize {
        self.ty.len()
    }

    /// This graph as the cache `load` reads (`crate::cache`).
    pub fn save_cache(&self, dir: &Path) -> Result<()> {
        let mut w = crate::cache::Writer::new(CACHE_MAGIC);
        w.strs(&self.types);
        w.u16s(&self.ty);
        w.u8s(&self.x);
        w.u8s(&self.y);
        w.u16s(&self.z);
        w.u32s(&self.i);
        w.u32s(&self.first);
        w.u32s(&self.dst);
        w.u8s(&self.class);
        w.u8s(
            &self
                .ambiguous
                .iter()
                .map(|&b| u8::from(b))
                .collect::<Vec<_>>(),
        );
        w.save(&dir.join(CACHE))
    }

    pub fn fanout(&self, n: u32) -> std::ops::Range<usize> {
        self.first[n as usize] as usize..self.first[n as usize + 1] as usize
    }

    pub fn type_name(&self, n: u32) -> &str {
        &self.types[self.ty[n as usize] as usize]
    }

    pub fn name(&self, n: u32) -> String {
        format!(
            "{}:X{}Y{}Z{}I{}",
            self.type_name(n),
            self.x[n as usize],
            self.y[n as usize],
            self.z[n as usize],
            self.i[n as usize]
        )
    }

    pub fn node(&self, name: &str) -> Option<u32> {
        let (t, x, y, z, i) = parse(name)?;
        let ti = *self.type_index.get(t)?;
        self.index
            .get(&(ti, x as u8, y as u8, z as u16, i))
            .copied()
    }

    pub fn type_id(&self, t: &str) -> Option<u16> {
        self.type_index.get(t).copied()
    }

    /// A general-purpose wire (H/V, LL, LEIM): never on a clock route.
    pub fn is_wire(&self, n: u32) -> bool {
        let t = self.type_name(n);
        t == "LL"
            || t.starts_with("LEIM")
            || ((t.starts_with('H') || t.starts_with('V')) && t[1..].parse::<u32>().is_ok())
    }
}
