//! The routing graph from `tools/rgdump` (README (i)) and its join to the
//! bit maps: every edge whose edge API names a mux setting becomes the named
//! bits that setting writes in its block (README (k)).
//!
//! Files in the graph directory: `nodes.tsv` (gid − 0x80000000, DYGR name,
//! fanout), `edges.bin` (per node `u32 src, u32 n`, then n × `u32 dst,
//! u32 uid id, u32 config id`), `uids.txt` and `configs.txt` (line k = id k,
//! id 0 = none).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};

use crate::db::{BitPos, Db};

pub struct Graph {
    /// DYGR name by gid ("" for gids the dump doesn't list).
    pub names: Vec<String>,
    /// CSR fanout: edges of gid g are `first[g]..first[g + 1]`.
    first: Vec<u32>,
    pub dst: Vec<u32>,
    pub uid: Vec<u32>,
    pub cfg: Vec<u32>,
    pub uids: Vec<String>,
    pub cfgs: Vec<String>,
}

impl Graph {
    pub fn load(dir: &Path) -> Result<Graph> {
        let text = std::fs::read_to_string(dir.join("nodes.tsv")).context("reading nodes.tsv")?;
        let mut names = Vec::new();
        for l in text.lines() {
            let mut f = l.split('\t');
            let g: usize = f.next().context("nodes.tsv")?.parse()?;
            if names.len() <= g {
                names.resize(g + 1, String::new());
            }
            names[g] = f.next().context("nodes.tsv")?.to_owned();
        }
        drop(text);
        let raw = std::fs::read(dir.join("edges.bin")).context("reading edges.bin")?;
        ensure!(raw.len().is_multiple_of(4), "edges.bin is not whole words");
        let w: Vec<u32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        drop(raw);
        let mut count = vec![0u32; names.len() + 1];
        let mut p = 0;
        while p < w.len() {
            let (src, n) = (w[p] as usize, w[p + 1] as usize);
            ensure!(
                src < names.len(),
                "edges.bin names gid {src} beyond nodes.tsv"
            );
            count[src] += n as u32;
            p += 2 + 3 * n;
        }
        let mut first = vec![0u32; names.len() + 1];
        for g in 0..names.len() {
            first[g + 1] = first[g] + count[g];
        }
        let total = first[names.len()] as usize;
        let (mut dst, mut uid, mut cfg) = (vec![0u32; total], vec![0u32; total], vec![0u32; total]);
        let mut fill = first.clone();
        p = 0;
        while p < w.len() {
            let (src, n) = (w[p] as usize, w[p + 1] as usize);
            for k in 0..n {
                let r = p + 2 + 3 * k;
                let e = fill[src] as usize;
                (dst[e], uid[e], cfg[e]) = (w[r], w[r + 1], w[r + 2]);
                fill[src] += 1;
            }
            p += 2 + 3 * n;
        }
        let lines = |f: &str| -> Result<Vec<String>> {
            Ok(std::fs::read_to_string(dir.join(f))
                .with_context(|| format!("reading {f}"))?
                .split('\n')
                .map(str::to_owned)
                .collect())
        };
        Ok(Graph {
            names,
            first,
            dst,
            uid,
            cfg,
            uids: lines("uids.txt")?,
            cfgs: lines("configs.txt")?,
        })
    }

    pub fn fanout(&self, g: u32) -> std::ops::Range<usize> {
        self.first[g as usize] as usize..self.first[g as usize + 1] as usize
    }

    pub fn edge(&self, src: u32, dst: u32) -> Option<usize> {
        self.fanout(src).find(|&e| self.dst[e] == dst)
    }

    /// The gids of just these names: one pass over the names, much cheaper
    /// than `index` when a design names a few thousand nodes.
    pub fn lookup<'a>(
        &self,
        want: impl IntoIterator<Item = &'a str>,
    ) -> rustc_hash::FxHashMap<&'a str, u32> {
        let mut want: rustc_hash::FxHashMap<&'a str, u32> =
            want.into_iter().map(|n| (n, u32::MAX)).collect();
        for (g, n) in self.names.iter().enumerate() {
            if let Some(v) = want.get_mut(n.as_str()) {
                *v = g as u32;
            }
        }
        want.retain(|_, v| *v != u32::MAX);
        want
    }

    pub fn index(&self) -> rustc_hash::FxHashMap<&str, u32> {
        self.names
            .iter()
            .enumerate()
            .filter(|(_, n)| !n.is_empty())
            .map(|(g, n)| (n.as_str(), g as u32))
            .collect()
    }
}

/// One mux choice of an edge configuration: `<mux path>/sel` set to a
/// setting. Sub-mux stages (`;s2 clkin[0];s1 clk[0]`) are `<mux>/s2/sel`
/// and `<mux>/s1/sel`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Choice {
    pub attr: String,
    pub input: String,
}

/// `mux input[;stage input]...` → choices. The top mux's own `sel` drives no
/// bits when it has stages (it is `<VIRTUAL>` in the BCM), but it is kept so
/// the caller can tell.
pub fn parse_config(cfg: &str) -> Result<(Choice, Vec<Choice>)> {
    let mut parts = cfg.split(';');
    let (mux, input) = parts
        .next()
        .unwrap()
        .split_once(' ')
        .with_context(|| format!("bad config {cfg:?}"))?;
    let top = Choice {
        attr: format!("{mux}/sel"),
        input: input.to_owned(),
    };
    let mut stages = Vec::new();
    for s in parts {
        let (stage, input) = s
            .split_once(' ')
            .with_context(|| format!("bad config {cfg:?}"))?;
        stages.push(Choice {
            attr: format!("{mux}/{stage}/sel"),
            input: input.to_owned(),
        });
    }
    Ok((top, stages))
}

/// The BCM setting an edge input selects: `name[k]` is the setting that
/// holds `NAMEk` between `_` boundaries (`muxin[6]` → `MUXIN6_`,
/// `clk_in[0]` → `CLK_IN0_`, sub-mux `clk[0]` → `CLK24_CLK16_CLK8_CLK0_`),
/// a bare `name` is `NAME` (`stitch` → `STITCH`, `clk_in` → `CLK_IN`).
pub fn setting_matches(input: &str, setting: &str) -> bool {
    let want = match input.split_once('[') {
        Some((n, k)) => format!("{}{}", n.to_uppercase(), k.trim_end_matches(']')),
        None => input.to_uppercase(),
    };
    setting == want || format!("_{setting}").contains(&format!("_{want}_"))
}

/// Why an edge configuration has no bits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Miss {
    /// The element has no BCM model (a plain buffer): nothing to configure.
    NoModel(String),
    /// The element's BCM leaf has no `sel` (programmable inverter, delay
    /// chain): the edge is a pass-through with no mux bits.
    NoSel(String),
    /// The leaf's `sel` drives no bits at all (`ce_rt_xleim_inv`).
    NoSelBits(String),
    /// A hyper-register clock input (`clk`, `clk_delayed`, `nclk`): set by
    /// the leaf's `flop_mode`, not by `sel`; normal routing never uses it.
    HyperClock(String),
    NoBitMap(String),
    NoAttribute(String),
    NoSetting(String),
    UnmappedBit(String),
}

/// Named bit writes relative to a block, (bit path, value): the edge's
/// setting and, for the same bits, the mux's idle setting (`VCC` where the
/// mux has one, as unused clock gates are tied, else `MUX_DIS`, else 0).
pub struct Named {
    writes: Vec<(String, bool)>,
    idle: Vec<(String, bool)>,
    /// `flop_mode = FAST_WIRE` of a mux with a hyper-register: Quartus sets
    /// it on every mux it uses, except LEIMs into LAB control inputs (README (q)).
    flop: Vec<(String, bool)>,
}

/// A block instance: its uid and block type.
type Instance = (String, String);

/// An edge's configuration: None if it has none (hardwired, or no edge
/// API), else its bit writes or why they can't be resolved.
pub type EdgeBits = Option<std::result::Result<Vec<(BitPos, bool)>, Miss>>;

/// Resolves edges to bit writes. Settings are resolved once per (block
/// type, configuration); only the anchor differs between instances.
pub struct Join<'a> {
    pub db: &'a mut Db,
    graph: &'a Graph,
    types: HashMap<u32, Option<String>>,
    named: HashMap<(String, u32), std::result::Result<std::rc::Rc<Named>, Miss>>,
    /// Per (block family, x, y): the instance there and its block type.
    at: HashMap<(&'static str, u32, u32), Option<Instance>>,
}

impl<'a> Join<'a> {
    pub fn new(db: &'a mut Db, graph: &'a Graph) -> Join<'a> {
        Join {
            db,
            graph,
            types: HashMap::new(),
            named: HashMap::new(),
            at: HashMap::new(),
        }
    }

    pub fn btype(&mut self, uid: u32) -> Option<String> {
        if let Some(t) = self.types.get(&uid) {
            return t.clone();
        }
        let t = self.db.type_by_prefix(&self.graph.uids[uid as usize]);
        self.types.insert(uid, t.clone());
        t
    }

    /// Bit writes of edge e, or why it has none; Ok(None) if the edge has no
    /// configuration (hardwired, or no edge API).
    pub fn edge(&mut self, e: usize) -> Result<EdgeBits> {
        if self.graph.cfg[e] == 0 {
            return self.rule_edge(e, false);
        }
        // In a LAB's routing tile, LEIM I80 and up feed the LAB's control
        // signals (clock enables, clears); their muxes keep flop_mode DIS.
        let control = self.graph.uids[self.graph.uid[e] as usize].starts_with("cw_rt_lab_base")
            && matches!(parse_node(&self.graph.names[self.graph.dst[e] as usize]), Some(("LEIM", _, _, _, i)) if i >= 80);
        self.pair_full(self.graph.uid[e], self.graph.cfg[e], false, !control)
    }

    /// Bits of a clock-network edge that has no edge API but does select a
    /// mux input (`clock_rule`); `idle` gives the muxes' idle state instead.
    pub fn rule_edge(&mut self, e: usize, idle: bool) -> Result<EdgeBits> {
        self.rule_edge_in(e, idle, None)
    }

    /// `rule_edge`, or Ok(None) when the mux's instance isn't in `dirty`.
    pub fn rule_edge_in(
        &mut self,
        e: usize,
        idle: bool,
        dirty: Option<&rustc_hash::FxHashSet<&str>>,
    ) -> Result<EdgeBits> {
        let Some((family, x, y, choices)) = clock_rule(self.graph, e) else {
            return Ok(None);
        };
        if !self.at.contains_key(&(family, x, y)) {
            let found = self.db.uid_at(family, x, y);
            self.at.insert((family, x, y), found);
        }
        let Some((uid, btype)) = self.at[&(family, x, y)].clone() else {
            return Ok(Some(Err(Miss::NoBitMap(format!("{family} at X{x} Y{y}")))));
        };
        if dirty.is_some_and(|d| !d.contains(uid.as_str())) {
            return Ok(None);
        }
        let named = match self.resolve_choices(&btype, choices)? {
            Ok(n) => n,
            Err(m) => return Ok(Some(Err(m))),
        };
        let map = self.db.map(&btype);
        let list = if idle { &named.idle } else { &named.writes };
        let mut out = Vec::new();
        for (b, v) in list {
            match map.pos(&uid, b) {
                Some(p) => out.push((p, *v)),
                None => {
                    return Ok(Some(Err(Miss::UnmappedBit(format!(
                        "{btype} {}",
                        generic(b)
                    )))))
                }
            }
        }
        Ok(Some(Ok(out)))
    }

    /// The mux setting's bits alone (what decoding an image checks).
    pub fn pair(&mut self, u: u32, c: u32) -> Result<EdgeBits> {
        self.pair_with(u, c, false)
    }

    fn pair_with(&mut self, u: u32, c: u32, idle: bool) -> Result<EdgeBits> {
        self.pair_full(u, c, idle, false)
    }

    /// The idle state of the bits an edge's setting writes.
    pub fn pair_idle(&mut self, u: u32, c: u32) -> Result<EdgeBits> {
        self.pair_with(u, c, true)
    }

    fn pair_full(&mut self, u: u32, c: u32, idle: bool, flop: bool) -> Result<EdgeBits> {
        if c == 0 {
            return Ok(None);
        }
        let uid = &self.graph.uids[u as usize];
        let Some(btype) = self.btype(u) else {
            return Ok(Some(Err(Miss::NoBitMap(generic(uid)))));
        };
        let key = (btype.clone(), c);
        if !self.named.contains_key(&key) {
            let r = self.resolve(&btype, c)?;
            self.named.insert(key.clone(), r);
        }
        let named = match &self.named[&key] {
            Ok(n) => n.clone(),
            Err(m) => return Ok(Some(Err(m.clone()))),
        };
        let map = self.db.map(&btype);
        let list = if idle { &named.idle } else { &named.writes };
        let mut out = Vec::with_capacity(list.len());
        for (b, v) in list.iter().chain(if flop { &named.flop[..] } else { &[] }) {
            match map.pos(uid, b) {
                Some(p) => out.push((p, *v)),
                None => {
                    return Ok(Some(Err(Miss::UnmappedBit(format!(
                        "{btype} {}",
                        generic(b)
                    )))))
                }
            }
        }
        Ok(Some(Ok(out)))
    }

    fn resolve(
        &mut self,
        btype: &str,
        c: u32,
    ) -> Result<std::result::Result<std::rc::Rc<Named>, Miss>> {
        let (top, stages) = parse_config(&self.graph.cfgs[c as usize])?;
        let choices = if stages.is_empty() { vec![top] } else { stages };
        self.resolve_choices(btype, choices)
    }

    fn resolve_choices(
        &mut self,
        btype: &str,
        choices: Vec<Choice>,
    ) -> Result<std::result::Result<std::rc::Rc<Named>, Miss>> {
        let (mut writes, mut idle, mut flop) = (Vec::new(), Vec::new(), Vec::new());
        for ch in choices {
            let mux = ch.attr.strip_suffix("/sel").unwrap_or(&ch.attr).to_owned();
            if let Some(f) = self.db.attribute(btype, &format!("{mux}/flop_mode")) {
                if let Some(s) = self
                    .db
                    .bcm
                    .settings(&f)
                    .iter()
                    .find(|s| s.name == "FAST_WIRE")
                {
                    flop.extend(
                        s.bits
                            .iter()
                            .zip(&s.values)
                            .map(|(b, &(v, _))| (format!("{}{b}", f.prefix), v)),
                    );
                }
            }
            let Some(a) = self.db.attribute(btype, &ch.attr) else {
                let mux = ch.attr.strip_suffix("/sel").unwrap_or(&ch.attr);
                return Ok(Err(match self.db.bcm.leaf(btype, mux) {
                    None => Miss::NoModel(format!("{btype} {}", generic(mux))),
                    Some(leaf) if !self.db.bcm.has_custom(&leaf, "sel") => Miss::NoSel(leaf),
                    Some(_) => Miss::NoAttribute(format!("{btype} {}", generic(&ch.attr))),
                }));
            };
            let all = self.db.bcm.settings(&a);
            // A programmable inverter's input passes uninverted (README (k)).
            let input = if ch.input == "rpi_in" {
                "non_inv"
            } else {
                ch.input.as_str()
            };
            let found: Vec<_> = all
                .iter()
                .filter(|s| setting_matches(input, &s.name))
                .collect();
            let s = match found.as_slice() {
                [s] => s,
                [] if all.is_empty() => return Ok(Err(Miss::NoSelBits(a.leaf.clone()))),
                [] if matches!(input, "clk" | "clk_delayed" | "nclk")
                    && self.db.bcm.has_custom(&a.leaf, "flop_mode") =>
                {
                    return Ok(Err(Miss::HyperClock(a.leaf.clone())))
                }
                [] => {
                    return Ok(Err(Miss::NoSetting(format!(
                        "{} {} {}",
                        a.leaf, a.name, ch.input
                    ))))
                }
                _ => bail!(
                    "{btype} {}: {} settings match {}",
                    ch.attr,
                    found.len(),
                    ch.input
                ),
            };
            for (b, &(v, _)) in s.bits.iter().zip(&s.values) {
                writes.push((format!("{}{b}", a.prefix), v));
            }
            let rest = ["VCC", "MUX_DIS"]
                .iter()
                .find_map(|n| all.iter().find(|x| x.name == *n));
            for b in &s.bits {
                let v = rest
                    .and_then(|r| r.bits.iter().position(|x| x == b).map(|i| r.values[i].0))
                    .unwrap_or(false);
                idle.push((format!("{}{b}", a.prefix), v));
            }
        }
        Ok(Ok(std::rc::Rc::new(Named { writes, idle, flop })))
    }
}

/// `TYPE:X..Y..Z..I..` → (type, x, y, z, i).
pub fn parse_node(n: &str) -> Option<(&str, u32, u32, u32, u32)> {
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

/// Clock-network edges with no edge API that still select a mux input
/// (README (k), (n)): the spine tap's 5:1 mux, a row clock's 32:1 mux and
/// predrive mux, and the DCM's P2C 4:1 muxes. Returns the block family, its X/Y and the choices.
pub fn clock_rule(g: &Graph, e: usize) -> Option<(&'static str, u32, u32, Vec<Choice>)> {
    let src = crate::route::edge_src(g, e);
    let (st, x, y, sz, si) = parse_node(&g.names[src as usize])?;
    let (dt, _, _, dz, di) = parse_node(&g.names[g.dst[e] as usize])?;
    let ch = |attr: String, k: u32| Choice {
        attr,
        input: format!("clkin[{k}]"),
    };
    match (st, dt) {
        // CE_CC_CLKMUX51 input n of the tap cell that drives spine k.
        ("CE_CC_CLKMUX51_INPUT", "CE_CC_CLKMUX51_OUTPUT") if sz == dz => {
            // The tap cell's output reaches SPINE_TAP_RE I<k> through the
            // cell's own nodes; k is the cell.
            let mut front = vec![g.dst[e]];
            for _ in 0..4 {
                let mut next = Vec::new();
                for n in front {
                    for f in g.fanout(n) {
                        let d = g.dst[f];
                        match parse_node(&g.names[d as usize]) {
                            Some(("SPINE_TAP_RE", _, _, _, k)) => {
                                return Some((
                                    "cw_cc_tap_cs_bf",
                                    x,
                                    y,
                                    vec![ch(format!("cell_{k}/mux51_dolsgn_cs_dolsgn_0/sel"), si)],
                                ))
                            }
                            Some(("CW_CC_TAP_CS_CELL_RE", ..)) => next.push(d),
                            _ => {}
                        }
                    }
                }
                front = next;
            }
            None
        }
        // The DCM's P2C 4:1 muxes (README (n)): the mux whose output reaches
        // HSSI_P2C_RE I<n> is clock_int(n mod 4) of the group mux(4m+3)to(4m),
        // m = n / 4, under mp2c_mux_bot for n < 16 (through the MUX_BOT
        // nodes) and mp2c_mux_top above; the input index is CLKIN<i>.
        ("CE_CC_CLKMUX41_LOW_INPUT", "CE_CC_CLKMUX41_LOW_OUTPUT") if sz == dz => {
            let mut front = vec![g.dst[e]];
            let mut side = None;
            for _ in 0..4 {
                let mut next = Vec::new();
                for n in front {
                    for f in g.fanout(n) {
                        let d = g.dst[f];
                        match parse_node(&g.names[d as usize]) {
                            Some(("HSSI_P2C_RE", _, _, _, n)) if n < 32 => {
                                let half = if n < 16 { "bot" } else { "top" };
                                if side.is_some_and(|s| s != half) {
                                    return None;
                                }
                                let m = 4 * (n / 4);
                                let attr = format!(
                                    "mp2c_mux_{half}/mux{}to{m}/clock_int{}/sel",
                                    m + 3,
                                    n % 4
                                );
                                return Some(("cw_cc_dcm_hssi_p2c_bf", x, y, vec![ch(attr, si)]));
                            }
                            Some(("CW_CC_DCM_HSSI_P2C_MUX_BOT_RE", ..)) => {
                                side = Some("bot");
                                next.push(d)
                            }
                            Some(("CW_CC_DCM_HSSI_P2C_MUX_TOP_RE", ..)) => {
                                side = Some("top");
                                next.push(d)
                            }
                            Some(("CW_CC_DCM_P2C_MUX_X4_RE", ..)) => next.push(d),
                            _ => {}
                        }
                    }
                }
                front = next;
            }
            None
        }
        // Spine input k into row clock j's 32:1 mux (output node 44 + 7j):
        // stage 1 picks k mod 8 in every group, stage 2 the group k / 8.
        ("SCLKMUX_RE", "SCLKMUX_RE")
            if si < 32 && di >= 44 && (di - 44) % 7 == 0 && (di - 44) / 7 < 12 =>
        {
            let j = (di - 44) / 7;
            let m = format!(
                "xrowclkmux/xrmx{}{}/rmux32to1_dolsgn_rc_dolsgn_0",
                if j < 6 { 'l' } else { 'r' },
                j % 6
            );
            Some((
                "cw_cc_ts_base",
                x,
                y,
                vec![
                    ch(format!("{m}/s1/sel"), si),
                    ch(format!("{m}/s2/sel"), si / 8),
                ],
            ))
        }
        // Row clock j's mux output straight into its predrive mux (input 1;
        // input 0 comes through the delay chain).
        ("SCLKMUX_RE", "SCLKMUX_RE")
            if si >= 44 && (si - 44) % 7 == 0 && di == si + 6 && (si - 44) / 7 < 12 =>
        {
            let j = (si - 44) / 7;
            let m = format!(
                "xrowclkmux/mux_predrive_{}{}/output_mux_dolsgn_delay_chain_dolsgn_0/sel",
                if j < 6 { 'l' } else { 'r' },
                j % 6
            );
            Some(("cw_cc_ts_base", x, y, vec![ch(m, 1)]))
        }
        // Row clock j's delay chain (README (q)): from the mux output b = 44 + 7j
        // through b + 1 to the taps b + 2 .. b + 4, whose 4:1 mux drives b + 5,
        // which is the predrive mux's input 0. Tap k is CLKIN<k> [inf: only
        // CLKIN0 seen].
        ("SCLKMUX_RE", "SCLKMUX_RE")
            if si >= 46
                && di >= 49
                && (di - 49) % 7 == 0
                && (di - 49) / 7 < 12
                && (di - 3..di).contains(&si) =>
        {
            let j = (di - 49) / 7;
            let m = format!(
                "xrowclkmux/delay_chain_{}{}/mux41_dolsgn_delay_dolsgn_0/sel",
                if j < 6 { 'l' } else { 'r' },
                j % 6
            );
            Some(("cw_cc_ts_base", x, y, vec![ch(m, si + 3 - di)]))
        }
        ("SCLKMUX_RE", "SCLKMUX_RE")
            if si >= 49 && (si - 49) % 7 == 0 && di == si + 1 && (si - 49) / 7 < 12 =>
        {
            let j = (si - 49) / 7;
            let m = format!(
                "xrowclkmux/mux_predrive_{}{}/output_mux_dolsgn_delay_chain_dolsgn_0/sel",
                if j < 6 { 'l' } else { 'r' },
                j % 6
            );
            Some(("cw_cc_ts_base", x, y, vec![ch(m, 0)]))
        }
        _ => None,
    }
}

/// An attribute path with its indices dropped, for grouping misses.
pub fn generic(path: &str) -> String {
    path.chars().filter(|c| !c.is_ascii_digit()).collect()
}

impl Miss {
    /// A pass-through: the edge exists but configures nothing.
    pub fn is_pass_through(&self) -> bool {
        matches!(self, Miss::NoModel(_) | Miss::NoSel(_) | Miss::NoSelBits(_))
    }
}
