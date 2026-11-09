//! Technology mapping of simple gates to K-input LUTs, in place of yosys'
//! `abc -lut K`: 4 for the Cyclone II's LE, 6 for the 7-series slice.
//!
//! The gates become an And-Inverter Graph first, with structural hashing
//! (ABC's `strash`): duplicate logic merges, inversions live on edges, and
//! XORs and MUXes become ANDs, which gives cut enumeration many more cuts to
//! choose from. Mapping is cut-based in the style of ABC's `if` (Mishchenko
//! et al., "Combinational and Sequential Mapping with Priority Cuts",
//! ICCAD 2007): every AND keeps a few priority cuts of at most K inputs, each
//! with its function as a truth table; a first pass picks the minimum-depth
//! cut per node, then area-flow and exact-area passes pick cheaper cuts
//! wherever that keeps every output at its required depth.
//!
//! `$_AND_`, `$_OR_`, `$_XOR_`, `$_MUX_` and friends become `$lut` cells;
//! every other cell stays as it is. [`map_module`] maps an IR module,
//! [`map`] and [`map_file`] yosys JSON (what the build runs between its two
//! yosys halves today); both read the gates into the same network and map
//! it the same way.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chipsmith_netlist::{CellKind, Const, GateOp, Lut, Module, Net, SigBit, State};

use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Map, Value};

use crate::rewrite::{Namer, Subst};

/// How many inputs a LUT has: 1 to 6, so a truth table fits a `u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LutSize(usize);

impl LutSize {
    /// The Cyclone II's LE.
    pub const FOUR: LutSize = LutSize(4);
    /// The 7-series slice's LUT6.
    pub const SIX: LutSize = LutSize(6);

    pub fn new(k: usize) -> Option<LutSize> {
        (1..=6).contains(&k).then_some(LutSize(k))
    }

    pub fn inputs(self) -> usize {
        self.0
    }

    /// The table's bits that count: `2^k` of them.
    fn mask(self) -> u64 {
        u64::MAX >> (64 - (1 << self.0))
    }
}

/// The area a LUT costs the mapper, by its number of inputs (0 to 6).
/// Uniform where every LUT takes a site of its own, as the Cyclone II's LE
/// and the 7-series LUT6 do. A fracturable 6-LUT is not uniform: Agilex's
/// ALM holds one 6-input LUT or two of up to five inputs that share two of
/// its eight pins, so a 6-input cut costs about two smaller ones and
/// minimising LUTs alone fills ALMs with half-used 6-LUTs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LutCost([u32; 7]);

impl LutCost {
    /// Every LUT 1: the mapper minimises LUTs.
    pub const UNIFORM: LutCost = LutCost([1; 7]);

    /// Cost per input count; `None` if any is 0 (a free LUT would let area
    /// recovery duplicate logic without bound).
    pub fn by_inputs(cost: [u32; 7]) -> Option<LutCost> {
        cost.iter().all(|&c| c > 0).then_some(LutCost(cost))
    }

    fn of(self, inputs: usize) -> u32 {
        self.0[inputs]
    }
}

/// What the mapper optimises beyond depth. [`MapOptions::DEFAULT`] is the
/// mapping the Cyclone II and 7-series flows have always had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapOptions {
    pub cost: LutCost,
    /// Keep a node's best cut among its candidates in the area passes. Its
    /// fanins' cut lists change between passes, so the cut may not be
    /// enumerated again, and the node can be left with no cut within its
    /// required depth: the mapping comes out deeper than the depth pass's
    /// (sha256 at K = 6: 8 levels instead of 7). ABC keeps it.
    pub keep_best_cut: bool,
    /// Also map the network with equivalent nodes merged
    /// ([`Network::swept`]) and refactored ([`Network::refactored`]), and
    /// keep whichever mapping takes the fewest LUTs at no more depth.
    pub restructure: bool,
}

impl MapOptions {
    pub const DEFAULT: MapOptions = MapOptions {
        cost: LutCost::UNIFORM,
        keep_best_cut: false,
        restructure: false,
    };
}

/// Cuts kept per node.
const CUTS: usize = 8;
/// Area-flow passes after the depth pass.
const AREA_PASSES: usize = 2;
/// Exact-area passes after those.
const EXACT_PASSES: usize = 2;

/// A signal: a yosys bit, or a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Sig {
    Bit(u64),
    Const(bool),
}

fn sig(v: &Value) -> Result<Sig> {
    if let Some(b) = v.as_u64() {
        return Ok(Sig::Bit(b));
    }
    match v.as_str() {
        Some("0") => Ok(Sig::Const(false)),
        Some("1") => Ok(Sig::Const(true)),
        // Undriven or don't-care: any value is correct; 0 keeps logic small.
        Some("x" | "z") => Ok(Sig::Const(false)),
        _ => bail!("unexpected signal {v}"),
    }
}

#[derive(Debug, Clone, Copy)]
enum Op {
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
    AndNot,
    OrNot,
    Not,
    Buf,
    /// `S ? B : A`
    Mux,
}

impl Op {
    fn of(ty: &str) -> Option<Op> {
        Some(match ty {
            "$_AND_" => Op::And,
            "$_NAND_" => Op::Nand,
            "$_OR_" => Op::Or,
            "$_NOR_" => Op::Nor,
            "$_XOR_" => Op::Xor,
            "$_XNOR_" => Op::Xnor,
            "$_ANDNOT_" => Op::AndNot,
            "$_ORNOT_" => Op::OrNot,
            "$_NOT_" => Op::Not,
            "$_BUF_" => Op::Buf,
            "$_MUX_" => Op::Mux,
            _ => return None,
        })
    }

    /// Input port names, in the order `build` takes them.
    fn ports(self) -> &'static [&'static str] {
        match self {
            Op::Not | Op::Buf => &["A"],
            Op::Mux => &["A", "B", "S"],
            _ => &["A", "B"],
        }
    }
}

/// An AIG literal: node index times two, plus one if inverted. Node 0 is
/// constant false, so literal 0 is false and 1 is true.
type Lit = u32;
const FALSE: Lit = 0;
const TRUE: Lit = 1;

fn node(l: Lit) -> u32 {
    l >> 1
}

fn inverted(l: Lit) -> bool {
    l & 1 == 1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Node {
    Const,
    /// A yosys bit the gates read but don't drive.
    Leaf(u64),
    And(Lit, Lit),
}

/// The gates of a module as a structurally hashed AIG.
struct Aig {
    nodes: Vec<Node>,
    hash: HashMap<(Lit, Lit), u32>,
    leaf_of: HashMap<u64, u32>,
}

impl Aig {
    fn new() -> Self {
        Aig {
            nodes: vec![Node::Const],
            hash: HashMap::new(),
            leaf_of: HashMap::new(),
        }
    }

    fn leaf(&mut self, bit: u64) -> Lit {
        let n = *self.leaf_of.entry(bit).or_insert_with(|| {
            self.nodes.push(Node::Leaf(bit));
            (self.nodes.len() - 1) as u32
        });
        n << 1
    }

    fn and(&mut self, a: Lit, b: Lit) -> Lit {
        let (a, b) = (a.min(b), a.max(b));
        if a == FALSE || a == b ^ 1 {
            return FALSE;
        }
        if a == TRUE || a == b {
            return b;
        }
        let n = *self.hash.entry((a, b)).or_insert_with(|| {
            self.nodes.push(Node::And(a, b));
            (self.nodes.len() - 1) as u32
        });
        n << 1
    }

    fn or(&mut self, a: Lit, b: Lit) -> Lit {
        self.and(a ^ 1, b ^ 1) ^ 1
    }

    fn xor(&mut self, a: Lit, b: Lit) -> Lit {
        let x = self.and(a, b ^ 1);
        let y = self.and(a ^ 1, b);
        self.or(x, y)
    }

    fn mux(&mut self, s: Lit, a: Lit, b: Lit) -> Lit {
        let x = self.and(s, b);
        let y = self.and(s ^ 1, a);
        self.or(x, y)
    }

    fn build(&mut self, op: Op, x: &[Lit]) -> Lit {
        match op {
            Op::And => self.and(x[0], x[1]),
            Op::Nand => self.and(x[0], x[1]) ^ 1,
            Op::Or => self.or(x[0], x[1]),
            Op::Nor => self.or(x[0], x[1]) ^ 1,
            Op::Xor => self.xor(x[0], x[1]),
            Op::Xnor => self.xor(x[0], x[1]) ^ 1,
            Op::AndNot => self.and(x[0], x[1] ^ 1),
            Op::OrNot => self.or(x[0], x[1] ^ 1),
            Op::Not => x[0] ^ 1,
            Op::Buf => x[0],
            Op::Mux => self.mux(x[2], x[0], x[1]),
        }
    }

    fn is_and(&self, n: u32) -> bool {
        matches!(self.nodes[n as usize], Node::And(..))
    }
}

fn connections(cell: &Value) -> Result<&Map<String, Value>> {
    cell["connections"]
        .as_object()
        .context("cell without connections")
}

fn one_bit(v: &Value) -> Result<Sig> {
    let bits = v.as_array().context("connection is not a bit list")?;
    ensure!(bits.len() == 1, "gate port wider than one bit");
    sig(&bits[0])
}

/// The module's gates, read into an AIG.
struct Network {
    aig: Aig,
    /// Gate outputs something outside the gates reads: (bit, literal).
    outputs: Vec<(u64, Lit)>,
    /// Cell names to drop from the module (the gates).
    gate_cells: Vec<String>,
    /// Largest bit number in the module, for fresh bits.
    max_bit: u64,
}

/// The gates of a netlist, before they become an AIG: each gate output
/// bit's operator and inputs, and the gate outputs read by anything else.
/// Bits are yosys' JSON bit numbers, or IR net indices.
struct GateList {
    driver: HashMap<u64, (Op, Vec<Sig>)>,
    /// Sorted, deduplicated.
    read: Vec<u64>,
    gate_cells: Vec<String>,
    max_bit: u64,
}

impl Network {
    fn read(module: &Value) -> Result<Network> {
        let cells = module["cells"].as_object().context("no cells")?;
        // Every gate: output bit -> (op, inputs).
        let mut driver: HashMap<u64, (Op, Vec<Sig>)> = HashMap::new();
        let mut gate_cells = Vec::new();
        let mut max_bit = 0;
        for (name, c) in cells {
            for bits in connections(c)?.values() {
                for b in bits
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                {
                    max_bit = max_bit.max(b);
                }
            }
            let Some(op) = Op::of(c["type"].as_str().unwrap_or("")) else {
                continue;
            };
            let conns = connections(c)?;
            let Sig::Bit(y) = one_bit(&conns["Y"])? else {
                bail!("{name} drives a constant")
            };
            let ins = op
                .ports()
                .iter()
                .map(|p| one_bit(&conns[*p]))
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                driver.insert(y, (op, ins)).is_none(),
                "bit {y} has two drivers"
            );
            gate_cells.push(name.clone());
        }

        // Gate outputs read by any other cell or by a module port.
        let mut read: Vec<u64> = Vec::new();
        for c in cells.values() {
            if Op::of(c["type"].as_str().unwrap_or("")).is_some() {
                continue;
            }
            for (port, bits) in connections(c)? {
                if c["port_directions"][port] != "output" {
                    read.extend(
                        bits.as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_u64),
                    );
                }
            }
        }
        for p in module["ports"].as_object().context("no ports")?.values() {
            if p["direction"] != "input" {
                read.extend(
                    p["bits"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_u64),
                );
            }
        }
        Network::build(GateList::new(driver, read, gate_cells, max_bit))
    }

    /// Builds the AIG depth first from every output, with an explicit
    /// stack (gate chains can be thousands deep).
    fn build(gl: GateList) -> Result<Network> {
        let GateList {
            driver,
            read,
            gate_cells,
            max_bit,
        } = gl;
        let mut aig = Aig::new();
        let mut lit_of: HashMap<u64, Lit> = HashMap::new();
        let mut on_stack: HashSet<u64> = HashSet::new();
        for &root in &read {
            let mut stack = vec![root];
            while let Some(&b) = stack.last() {
                if lit_of.contains_key(&b) {
                    stack.pop();
                    continue;
                }
                let (op, ins) = &driver[&b];
                let pending: Vec<u64> = ins
                    .iter()
                    .filter_map(|s| match s {
                        Sig::Bit(i) if driver.contains_key(i) && !lit_of.contains_key(i) => {
                            Some(*i)
                        }
                        _ => None,
                    })
                    .collect();
                if pending.is_empty() {
                    let x: Vec<Lit> = ins
                        .iter()
                        .map(|s| match *s {
                            Sig::Const(v) => v as Lit,
                            Sig::Bit(i) => lit_of.get(&i).copied().unwrap_or_else(|| aig.leaf(i)),
                        })
                        .collect();
                    let l = aig.build(*op, &x);
                    lit_of.insert(b, l);
                    on_stack.remove(&b);
                    stack.pop();
                } else {
                    ensure!(on_stack.insert(b), "combinational loop through bit {b}");
                    stack.extend(pending);
                }
            }
        }
        let outputs = read.iter().map(|b| (*b, lit_of[b])).collect();
        Ok(Network {
            aig,
            outputs,
            gate_cells,
            max_bit,
        })
    }
}

impl GateList {
    fn new(
        driver: HashMap<u64, (Op, Vec<Sig>)>,
        mut read: Vec<u64>,
        gate_cells: Vec<String>,
        max_bit: u64,
    ) -> GateList {
        read.retain(|b| driver.contains_key(b));
        read.sort();
        read.dedup();
        GateList {
            driver,
            read,
            gate_cells,
            max_bit,
        }
    }
}

/// Truth table of leaf `i` of a 6-input function; a K-input one takes its
/// low `2^K` bits (`LutSize::mask`).
const VAR: [u64; 6] = [
    0xaaaa_aaaa_aaaa_aaaa,
    0xcccc_cccc_cccc_cccc,
    0xf0f0_f0f0_f0f0_f0f0,
    0xff00_ff00_ff00_ff00,
    0xffff_0000_ffff_0000,
    0xffff_ffff_0000_0000,
];

/// A cut: up to K leaves (sorted AIG nodes) and the node's function of
/// them, a K-input table (`2^K` bits; a function of fewer leaves repeats).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cut {
    leaves: Vec<u32>,
    tt: u64,
}

/// Re-express `tt` over `from` leaves as a function over `to` (a superset).
fn stretch(k: LutSize, tt: u64, from: &[u32], to: &[u32]) -> u64 {
    let pos: Vec<usize> = from.iter().map(|l| to.binary_search(l).unwrap()).collect();
    let mut out = 0u64;
    for m in 0..1u64 << k.0 {
        let mut idx = 0;
        for (i, &p) in pos.iter().enumerate() {
            idx |= ((m >> p) & 1) << i;
        }
        out |= ((tt >> idx) & 1) << m;
    }
    out
}

/// Drop leaves the function doesn't depend on.
fn reduce(k: LutSize, mut cut: Cut) -> Cut {
    let mut i = 0;
    while i < cut.leaves.len() {
        let shift = 1 << i;
        let var = VAR[i] & k.mask();
        let independent = (cut.tt & var) >> shift == cut.tt & !var;
        if independent {
            let n = cut.leaves.len();
            let mut tt = 0u64;
            for m in 0..(1u64 << (n - 1)) {
                let low = m & ((1 << i) - 1);
                let high = (m >> i) << (i + 1);
                tt |= ((cut.tt >> (low | high)) & 1) << m;
            }
            // Replicate so the table stays a valid K-input function.
            let width = 1 << (n - 1);
            let mut full = 0u64;
            for r in 0..((1 << k.0) / width) {
                full |= tt << (r * width);
            }
            cut.tt = full;
            cut.leaves.remove(i);
        } else {
            i += 1;
        }
    }
    cut
}

/// Per-node mapping state.
#[derive(Clone, Default)]
struct NodeMap {
    cuts: Vec<Cut>,
    /// LUT levels to this node with its best cut (0 for leaves).
    depth: u32,
    /// Area flow of the best cut.
    flow: f32,
    best: usize,
}

struct Mapper<'a> {
    k: LutSize,
    opts: MapOptions,
    aig: &'a Aig,
    /// AIG nodes outputs read (their LUTs must exist).
    roots: Vec<u32>,
    nodes: Vec<NodeMap>,
    /// Estimated fanout of each node in the current mapping (>= 1).
    refs: Vec<f32>,
}

impl<'a> Mapper<'a> {
    fn new(net: &'a Network, k: LutSize, opts: MapOptions) -> Self {
        let aig = &net.aig;
        let n = aig.nodes.len();
        let mut nodes = vec![NodeMap::default(); n];
        let mut refs = vec![0.0f32; n];
        for (v, node) in aig.nodes.iter().enumerate() {
            match *node {
                Node::Leaf(_) => {
                    nodes[v].cuts = vec![Cut {
                        leaves: vec![v as u32],
                        tt: VAR[0] & k.mask(),
                    }];
                }
                Node::And(a, b) => {
                    refs[node_of(a)] += 1.0;
                    refs[node_of(b)] += 1.0;
                }
                Node::Const => {}
            }
        }
        let mut roots: Vec<u32> = net
            .outputs
            .iter()
            .map(|&(_, l)| node(l))
            .filter(|&v| aig.is_and(v))
            .collect();
        roots.sort();
        roots.dedup();
        for &o in &roots {
            refs[o as usize] += 1.0;
        }
        for r in &mut refs {
            *r = r.max(1.0);
        }
        Mapper {
            k,
            opts,
            aig,
            roots,
            nodes,
            refs,
        }
    }

    fn cut_depth(&self, cut: &Cut) -> u32 {
        cut.leaves
            .iter()
            .map(|&l| self.nodes[l as usize].depth)
            .max()
            .unwrap_or(0)
            + 1
    }

    fn cut_flow(&self, cut: &Cut) -> f32 {
        self.opts.cost.of(cut.leaves.len()) as f32
            + cut
                .leaves
                .iter()
                .map(|&l| {
                    let l = l as usize;
                    if self.aig.is_and(l as u32) {
                        self.nodes[l].flow / self.refs[l]
                    } else {
                        0.0
                    }
                })
                .sum::<f32>()
    }

    /// The cuts a fanin literal offers: the fanin itself as a leaf, plus its
    /// own cuts if it is an AND; each with the literal's polarity applied.
    fn fanin_cuts(&self, l: Lit) -> Vec<Cut> {
        let v = node(l);
        let mask = self.k.mask();
        let inv = if inverted(l) { mask } else { 0 };
        let mut out = vec![Cut {
            leaves: vec![v],
            tt: (VAR[0] & mask) ^ inv,
        }];
        if self.aig.is_and(v) {
            out.extend(self.nodes[v as usize].cuts.iter().map(|c| Cut {
                leaves: c.leaves.clone(),
                tt: c.tt ^ inv,
            }));
        }
        out
    }

    /// All non-trivial cuts of AND node `v`, dominated ones dropped.
    fn enumerate(&self, v: u32) -> Vec<Cut> {
        let Node::And(a, b) = self.aig.nodes[v as usize] else {
            unreachable!("only ANDs are mapped")
        };
        let (ca, cb) = (self.fanin_cuts(a), self.fanin_cuts(b));
        let mut cuts: Vec<Cut> = Vec::new();
        for x in &ca {
            for y in &cb {
                let mut leaves: Vec<u32> = x.leaves.iter().chain(&y.leaves).copied().collect();
                leaves.sort();
                leaves.dedup();
                if leaves.len() > self.k.0 {
                    continue;
                }
                let k = self.k;
                let tt =
                    stretch(k, x.tt, &x.leaves, &leaves) & stretch(k, y.tt, &y.leaves, &leaves);
                let cut = reduce(k, Cut { leaves, tt });
                if !cuts.contains(&cut) {
                    cuts.push(cut);
                }
            }
        }
        cuts.sort_by_key(|c| c.leaves.len());
        let mut kept: Vec<Cut> = Vec::new();
        for c in cuts {
            if !kept
                .iter()
                .any(|k| k.leaves.iter().all(|l| c.leaves.contains(l)))
            {
                kept.push(c);
            }
        }
        kept
    }

    /// One pass over the ANDs in topological (creation) order. `required`
    /// (from the previous pass) bounds depth during area recovery.
    fn pass(&mut self, required: Option<&[u32]>) {
        for v in 0..self.aig.nodes.len() as u32 {
            if !self.aig.is_and(v) {
                continue;
            }
            let mut cuts = self.enumerate(v);
            if self.opts.keep_best_cut && required.is_some() {
                let n = &self.nodes[v as usize];
                let best = &n.cuts[n.best];
                if !cuts.contains(best) {
                    cuts.push(best.clone());
                }
            }
            let key = |m: &Mapper, c: &Cut| (m.cut_depth(c), m.cut_flow(c), c.leaves.len());
            match required {
                None => cuts.sort_by(|a, b| {
                    let (ka, kb) = (key(self, a), key(self, b));
                    ka.0.cmp(&kb.0)
                        .then(ka.1.total_cmp(&kb.1))
                        .then(ka.2.cmp(&kb.2))
                }),
                Some(req) => {
                    let limit = req[v as usize];
                    cuts.sort_by(|a, b| {
                        let (ka, kb) = (key(self, a), key(self, b));
                        (ka.0 > limit)
                            .cmp(&(kb.0 > limit))
                            .then(ka.1.total_cmp(&kb.1))
                            .then(ka.0.cmp(&kb.0))
                            .then(ka.2.cmp(&kb.2))
                    })
                }
            }
            cuts.truncate(CUTS);
            let (depth, flow) = (self.cut_depth(&cuts[0]), self.cut_flow(&cuts[0]));
            let n = &mut self.nodes[v as usize];
            n.depth = depth;
            n.flow = flow;
            n.best = 0;
            n.cuts = cuts;
        }
    }

    fn best_leaves(&self, v: u32) -> &[u32] {
        let n = &self.nodes[v as usize];
        &n.cuts[n.best].leaves
    }

    /// The ANDs the current best cuts make into LUTs, from the roots.
    fn cover(&self) -> Vec<u32> {
        let mut used = vec![false; self.aig.nodes.len()];
        let mut stack: Vec<u32> = self.roots.clone();
        let mut luts = Vec::new();
        while let Some(v) = stack.pop() {
            if used[v as usize] || !self.aig.is_and(v) {
                continue;
            }
            used[v as usize] = true;
            luts.push(v);
            stack.extend(self.best_leaves(v));
        }
        luts.sort();
        luts
    }

    fn max_depth(&self) -> u32 {
        self.roots
            .iter()
            .map(|&o| self.nodes[o as usize].depth)
            .max()
            .unwrap_or(0)
    }

    /// Required depth of every node given the cover; roots may take the
    /// current maximum.
    fn required(&self, cover: &[u32]) -> Vec<u32> {
        let max = self.max_depth();
        let mut req = vec![u32::MAX; self.aig.nodes.len()];
        for &o in &self.roots {
            req[o as usize] = max;
        }
        for &v in cover.iter().rev() {
            if req[v as usize] == u32::MAX {
                continue;
            }
            let r = req[v as usize] - 1;
            for &l in self.best_leaves(v) {
                req[l as usize] = req[l as usize].min(r);
            }
        }
        for r in &mut req {
            if *r == u32::MAX {
                *r = max;
            }
        }
        req
    }

    fn update_refs(&mut self, cover: &[u32]) {
        let mut refs = vec![0.0f32; self.aig.nodes.len()];
        for &v in cover {
            for &l in self.best_leaves(v) {
                refs[l as usize] += 1.0;
            }
        }
        for &o in &self.roots {
            refs[o as usize] += 1.0;
        }
        // Blend with the previous estimate, as ABC does, to keep it stable.
        for (r, new) in self.refs.iter_mut().zip(refs) {
            *r = ((*r + 2.0 * new) / 3.0).max(1.0);
        }
    }

    /// References from the cover: one per LUT leaf, one per root.
    fn exact_refs(&self, cover: &[u32]) -> Vec<u32> {
        let mut refs = vec![0u32; self.aig.nodes.len()];
        for &v in cover {
            for &l in self.best_leaves(v) {
                refs[l as usize] += 1;
            }
        }
        for &o in &self.roots {
            refs[o as usize] += 1;
        }
        refs
    }

    /// Reference `leaves`; returns the area of the LUTs that brings into
    /// the cover (this one included), recursing into newly referenced ANDs.
    fn reference(&self, refs: &mut [u32], leaves: &[u32]) -> u32 {
        let mut area = self.opts.cost.of(leaves.len());
        for &l in leaves {
            refs[l as usize] += 1;
            if refs[l as usize] == 1 && self.aig.is_and(l) {
                area += self.reference(refs, self.best_leaves(l));
            }
        }
        area
    }

    /// The inverse of `reference`.
    fn dereference(&self, refs: &mut [u32], leaves: &[u32]) -> u32 {
        let mut area = self.opts.cost.of(leaves.len());
        for &l in leaves {
            refs[l as usize] -= 1;
            if refs[l as usize] == 0 && self.aig.is_and(l) {
                area += self.dereference(refs, self.best_leaves(l));
            }
        }
        area
    }

    /// Exact local area recovery (the second phase of ABC's `if`): every
    /// AND in the cover takes the cut that adds the fewest LUTs given the
    /// rest of the cover, within `required`.
    fn exact_pass(&mut self, cover: &[u32], required: &[u32]) {
        let mut refs = self.exact_refs(cover);
        for v in 0..self.aig.nodes.len() as u32 {
            let vi = v as usize;
            if !self.aig.is_and(v) {
                continue;
            }
            if refs[vi] > 0 {
                let leaves = self.best_leaves(v).to_vec();
                self.dereference(&mut refs, &leaves);
                let mut choice = (u32::MAX, self.nodes[vi].best);
                for (i, cut) in self.nodes[vi].cuts.iter().enumerate() {
                    if self.cut_depth(cut) > required[vi] {
                        continue;
                    }
                    let area = self.reference(&mut refs, &cut.leaves);
                    self.dereference(&mut refs, &cut.leaves);
                    if area < choice.0 {
                        choice = (area, i);
                    }
                }
                self.nodes[vi].best = choice.1;
                let leaves = self.best_leaves(v).to_vec();
                self.reference(&mut refs, &leaves);
            }
            let n = &self.nodes[vi];
            self.nodes[vi].depth = self.cut_depth(&n.cuts[n.best]);
        }
    }
}

fn node_of(l: Lit) -> usize {
    node(l) as usize
}

/// Most leaves of the cut [`Network::refactored`] collapses a node over.
const REFACTOR_LEAVES: usize = 10;
/// Words of a [`REFACTOR_LEAVES`]-input truth table.
const TT_WORDS: usize = 1 << (REFACTOR_LEAVES - 6);

type Table = [u64; TT_WORDS];

/// Variable `i`'s table over [`REFACTOR_LEAVES`] inputs.
fn var_table(i: usize) -> Table {
    std::array::from_fn(|w| {
        if i < 6 {
            VAR[i]
        } else if (w >> (i - 6)) & 1 == 1 {
            u64::MAX
        } else {
            0
        }
    })
}

fn tt_and(a: &Table, b: &Table) -> Table {
    std::array::from_fn(|w| a[w] & b[w])
}

fn tt_or(a: &Table, b: &Table) -> Table {
    std::array::from_fn(|w| a[w] | b[w])
}

fn tt_not(a: &Table) -> Table {
    a.map(|w| !w)
}

fn tt_zero(a: &Table) -> bool {
    a.iter().all(|&w| w == 0)
}

/// `t` with variable `i` set to `v`, as a table that no longer depends on it.
fn cofactor(t: &Table, i: usize, v: bool) -> Table {
    if i < 6 {
        let s = 1 << i;
        let m = VAR[i];
        t.map(|w| {
            if v {
                (w & m) | ((w & m) >> s)
            } else {
                (w & !m) | ((w & !m) << s)
            }
        })
    } else {
        let s = 1 << (i - 6);
        std::array::from_fn(|w| t[if v { w | s } else { w & !s }])
    }
}

/// A product term: bit `i` of `care` says variable `i` appears, bit `i` of
/// `pos` whether as itself.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cube {
    care: u16,
    pos: u16,
}

/// An irredundant sum of products of some function between `lo` and `hi`
/// (Minato-Morreale), over variables below `n`; returns it and its table.
fn isop(lo: &Table, hi: &Table, n: usize, out: &mut Vec<Cube>) -> Table {
    if tt_zero(lo) {
        return [0; TT_WORDS];
    }
    if hi.iter().all(|&w| w == u64::MAX) {
        out.push(Cube { care: 0, pos: 0 });
        return [u64::MAX; TT_WORDS];
    }
    let mut i = n;
    let x = loop {
        assert!(i > 0, "a non-constant interval depends on some variable");
        i -= 1;
        if cofactor(lo, i, false) != cofactor(lo, i, true)
            || cofactor(hi, i, false) != cofactor(hi, i, true)
        {
            break i;
        }
    };
    let (l0, l1) = (cofactor(lo, x, false), cofactor(lo, x, true));
    let (h0, h1) = (cofactor(hi, x, false), cofactor(hi, x, true));
    let start = out.len();
    let r0 = isop(&tt_and(&l0, &tt_not(&h1)), &h0, x, out);
    for c in &mut out[start..] {
        c.care |= 1 << x;
    }
    let mid = out.len();
    let r1 = isop(&tt_and(&l1, &tt_not(&h0)), &h1, x, out);
    for c in &mut out[mid..] {
        c.care |= 1 << x;
        c.pos |= 1 << x;
    }
    let rest_lo = tt_or(&tt_and(&l0, &tt_not(&r0)), &tt_and(&l1, &tt_not(&r1)));
    let rs = isop(&rest_lo, &tt_and(&h0, &h1), x, out);
    let vx = var_table(x);
    tt_or(&tt_or(&tt_and(&r0, &tt_not(&vx)), &tt_and(&r1, &vx)), &rs)
}

/// Builds a sum of products, factored: the literal most cubes share is
/// taken out (`l & (q) | r`), recursively. ANDs and ORs of several terms
/// pair the two earliest-arriving first.
fn build_factored(aig: &mut Aig, level: &mut Vec<u32>, leaves: &[Lit], cubes: &[Cube]) -> Lit {
    if cubes.is_empty() {
        return FALSE;
    }
    if cubes.iter().any(|c| c.care == 0) {
        return TRUE;
    }
    let mut count = [0u32; 2 * REFACTOR_LEAVES];
    for c in cubes {
        for i in 0..leaves.len() {
            if c.care >> i & 1 == 1 {
                count[2 * i + (c.pos >> i & 1) as usize] += 1;
            }
        }
    }
    let (best, &n) = count
        .iter()
        .enumerate()
        .max_by_key(|&(i, &n)| (n, std::cmp::Reverse(i)))
        .expect("literals");
    let lit_of = |j: usize| leaves[j / 2] ^ j.is_multiple_of(2) as Lit;
    if n <= 1 || cubes.len() == 1 {
        // No literal shared: an OR of products.
        let terms: Vec<Lit> = cubes
            .iter()
            .map(|c| {
                let lits: Vec<Lit> = (0..leaves.len())
                    .filter(|&i| c.care >> i & 1 == 1)
                    .map(|i| lit_of(2 * i + (c.pos >> i & 1) as usize))
                    .collect();
                and_balanced(aig, level, &lits)
            })
            .collect();
        let inv: Vec<Lit> = terms.iter().map(|&t| t ^ 1).collect();
        return and_balanced(aig, level, &inv) ^ 1;
    }
    let (var, positive) = (best / 2, best % 2 == 1);
    let bit = 1u16 << var;
    let (with, without): (Vec<Cube>, Vec<Cube>) = cubes
        .iter()
        .partition(|c| c.care & bit != 0 && ((c.pos & bit != 0) == positive));
    let quotient: Vec<Cube> = with
        .iter()
        .map(|c| Cube {
            care: c.care & !bit,
            pos: c.pos & !bit,
        })
        .collect();
    let q = build_factored(aig, level, leaves, &quotient);
    let t = and_balanced(aig, level, &[lit_of(best), q]);
    if without.is_empty() {
        return t;
    }
    let r = build_factored(aig, level, leaves, &without);
    and_balanced(aig, level, &[t ^ 1, r ^ 1]) ^ 1
}

fn level_of(aig: &Aig, level: &[u32], l: Lit) -> u32 {
    match aig.nodes[node_of(l)] {
        Node::And(..) => level[node_of(l)],
        _ => 0,
    }
}

/// The AND of `lits`, the two lowest-level operands paired first.
fn and_balanced(aig: &mut Aig, level: &mut Vec<u32>, lits: &[Lit]) -> Lit {
    let mut heap: std::collections::BinaryHeap<std::cmp::Reverse<(u32, Lit)>> = lits
        .iter()
        .map(|&l| std::cmp::Reverse((level_of(aig, level, l), l)))
        .collect();
    if heap.is_empty() {
        return TRUE;
    }
    while heap.len() > 1 {
        let std::cmp::Reverse((la, a)) = heap.pop().expect("two");
        let std::cmp::Reverse((lb, b)) = heap.pop().expect("two");
        let z = aig.and(a, b);
        let zi = node_of(z);
        if zi >= level.len() {
            level.resize(zi + 1, 0);
            level[zi] = 1 + la.max(lb);
        }
        heap.push(std::cmp::Reverse((level_of(aig, level, z), z)));
    }
    heap.pop().expect("one").0 .1
}

/// A reconvergence-driven cut of AND node `v` (ABC's): starting from its
/// fanins, the leaf whose expansion adds the fewest new leaves is replaced
/// by its fanins while the cut stays within [`REFACTOR_LEAVES`].
fn reconvergent_cut(aig: &Aig, v: u32) -> Vec<u32> {
    let Node::And(a, b) = aig.nodes[v as usize] else {
        unreachable!("an AND")
    };
    let mut cut: Vec<u32> = vec![node(a), node(b)];
    cut.dedup();
    cut.retain(|&x| !matches!(aig.nodes[x as usize], Node::Const));
    loop {
        let mut best: Option<(usize, u32)> = None;
        for &x in &cut {
            let Node::And(p, q) = aig.nodes[x as usize] else {
                continue;
            };
            let added = [node(p), node(q)]
                .iter()
                .filter(|&&f| !cut.contains(&f) && !matches!(aig.nodes[f as usize], Node::Const))
                .count();
            if cut.len() - 1 + added > REFACTOR_LEAVES {
                continue;
            }
            if best.is_none_or(|(c, y)| (added, std::cmp::Reverse(x)) < (c, std::cmp::Reverse(y))) {
                best = Some((added, x));
            }
        }
        let Some((_, x)) = best else {
            return cut;
        };
        let Node::And(p, q) = aig.nodes[x as usize] else {
            unreachable!("an AND")
        };
        cut.retain(|&y| y != x);
        for f in [node(p), node(q)] {
            if !cut.contains(&f) && !matches!(aig.nodes[f as usize], Node::Const) {
                cut.push(f);
            }
        }
    }
}

impl Network {
    /// The same network with nodes rebuilt from their function where that
    /// takes fewer ANDs (ABC's `refactor`): each node's function over a
    /// cut of at most [`REFACTOR_LEAVES`] leaves, as a factored irredundant
    /// sum of products of itself or its complement, replaces the ANDs only
    /// it uses (its MFFC) when it needs fewer and is no deeper.
    fn refactored(&self) -> Network {
        let old = &self.aig;
        let n = old.nodes.len();
        let mut refs = vec![0u32; n];
        for v in 0..n {
            if let Node::And(a, b) = old.nodes[v] {
                refs[node_of(a)] += 1;
                refs[node_of(b)] += 1;
            }
        }
        for &(_, l) in &self.outputs {
            refs[node_of(l)] += 1;
        }
        let mut out = Aig::new();
        let mut level: Vec<u32> = vec![0];
        let mut new_lit: Vec<Lit> = vec![FALSE; n];
        let map = |new_lit: &[Lit], l: Lit| new_lit[node_of(l)] ^ (l & 1);
        for v in 0..n {
            let (a, b) = match old.nodes[v] {
                Node::Const => continue,
                Node::Leaf(bit) => {
                    new_lit[v] = out.leaf(bit);
                    continue;
                }
                Node::And(a, b) => (a, b),
            };
            let (x, y) = (map(&new_lit, a), map(&new_lit, b));
            let plain = out.and(x, y);
            if node_of(plain) >= level.len() {
                level.resize(node_of(plain) + 1, 0);
                level[node_of(plain)] =
                    1 + level_of(&out, &level, x).max(level_of(&out, &level, y));
            }
            new_lit[v] = plain;
            let cut = reconvergent_cut(old, v as u32);
            if cut.len() < 3 {
                continue;
            }
            // The node's function over the cut, and its MFFC inside it.
            let mut tables: HashMap<u32, Table> = HashMap::new();
            for (i, &c) in cut.iter().enumerate() {
                tables.insert(c, var_table(i));
            }
            let mut cone: Vec<u32> = Vec::new();
            let f = cone_table(old, v as u32, &mut tables, &mut cone);
            let mut left = refs.clone();
            let mffc = mffc_size(old, v as u32, &cut, &mut left);
            if mffc < 2 {
                continue;
            }
            // Tables span all REFACTOR_LEAVES variables; the function
            // ignores those past the cut, so no mask is needed.
            let width = cut.len();
            let nf = tt_not(&f);
            let mut on = Vec::new();
            isop(&f, &f, width, &mut on);
            let mut off = Vec::new();
            isop(&nf, &nf, width, &mut off);
            let literals = |cs: &[Cube]| cs.iter().map(|c| c.care.count_ones()).sum::<u32>();
            let (cubes, complement) = if literals(&off) < literals(&on) {
                (off, true)
            } else {
                (on, false)
            };
            let leaves: Vec<Lit> = cut.iter().map(|&c| new_lit[c as usize]).collect();
            let before = out.nodes.len();
            let built = build_factored(&mut out, &mut level, &leaves, &cubes) ^ complement as Lit;
            let added = out.nodes.len() - before;
            // A saving of one AND moved LUTs around without fewer of them
            // (femtorv32 481 LEs against 468 asking for two, mult-selftest
            // 883 against 866).
            if added + 2 <= mffc && level_of(&out, &level, built) <= level_of(&out, &level, plain) {
                new_lit[v] = built;
            }
        }
        // Keep only what the outputs read: rejected candidates and the
        // structures they replaced would otherwise count in the mapper's
        // fanout estimates.
        let outputs: Vec<(u64, Lit)> = self
            .outputs
            .iter()
            .map(|&(b, l)| (b, map(&new_lit, l)))
            .collect();
        let (aig, outputs) = compacted(&out, &outputs);
        Network {
            outputs,
            aig,
            gate_cells: self.gate_cells.clone(),
            max_bit: self.max_bit,
        }
    }
}

/// `v`'s table over the cut whose tables `tables` starts with; every AND
/// evaluated is pushed on `cone`.
fn cone_table(aig: &Aig, v: u32, tables: &mut HashMap<u32, Table>, cone: &mut Vec<u32>) -> Table {
    if let Some(t) = tables.get(&v) {
        return *t;
    }
    let t = match aig.nodes[v as usize] {
        Node::Const => [0; TT_WORDS],
        Node::Leaf(_) => unreachable!("the cut separates every leaf"),
        Node::And(a, b) => {
            let ta = cone_table(aig, node(a), tables, cone);
            let tb = cone_table(aig, node(b), tables, cone);
            let (ia, ib) = (inverted(a) as u64 * u64::MAX, inverted(b) as u64 * u64::MAX);
            std::array::from_fn(|w| (ta[w] ^ ia) & (tb[w] ^ ib))
        }
    };
    cone.push(v);
    tables.insert(v, t);
    t
}

/// ANDs above the cut that only `v` uses, `v` included.
fn mffc_size(aig: &Aig, v: u32, cut: &[u32], refs: &mut [u32]) -> usize {
    let mut size = 1;
    let Node::And(a, b) = aig.nodes[v as usize] else {
        return 0;
    };
    for f in [node(a), node(b)] {
        if cut.contains(&f) || !aig.is_and(f) {
            continue;
        }
        refs[f as usize] -= 1;
        if refs[f as usize] == 0 {
            size += mffc_size(aig, f, cut, refs);
        }
    }
    size
}

/// The part of `aig` that `outputs` read, renumbered.
fn compacted(aig: &Aig, outputs: &[(u64, Lit)]) -> (Aig, Vec<(u64, Lit)>) {
    let n = aig.nodes.len();
    let mut live = vec![false; n];
    let mut stack: Vec<u32> = outputs.iter().map(|&(_, l)| node(l)).collect();
    while let Some(v) = stack.pop() {
        if std::mem::replace(&mut live[v as usize], true) {
            continue;
        }
        if let Node::And(a, b) = aig.nodes[v as usize] {
            stack.extend([node(a), node(b)]);
        }
    }
    let mut out = Aig::new();
    let mut new_lit = vec![FALSE; n];
    for v in 0..n {
        if !live[v] {
            continue;
        }
        new_lit[v] = match aig.nodes[v] {
            Node::Const => FALSE,
            Node::Leaf(bit) => out.leaf(bit),
            Node::And(a, b) => {
                let x = new_lit[node_of(a)] ^ (a & 1);
                let y = new_lit[node_of(b)] ^ (b & 1);
                out.and(x, y)
            }
        };
    }
    let outputs = outputs
        .iter()
        .map(|&(b, l)| (b, new_lit[node_of(l)] ^ (l & 1)))
        .collect();
    (out, outputs)
}

/// Random simulation words per node when looking for equivalent nodes.
const SWEEP_WORDS: usize = 16;
/// Most leaves of the cut over which two candidates are compared
/// exhaustively (2^12 assignments).
const SWEEP_CUT: usize = 12;
/// Expansion steps tried per candidate pair.
const SWEEP_STEPS: usize = 48;

impl Network {
    /// The same network with nodes that compute the same function as an
    /// earlier node (or its complement) replaced by it: a light form of
    /// ABC's `fraig`. Lowering builds one function many ways (a `$pmux`
    /// leg, a decoder, a comparison per use), and structural hashing only
    /// merges identical structure. Candidates come from random simulation;
    /// a pair merges only when both are the same function of a common cut
    /// of at most [`SWEEP_CUT`] nodes, checked over every assignment, so a
    /// merge never changes what an output computes.
    fn swept(&self) -> Network {
        let aig = &self.aig;
        let n = aig.nodes.len();
        // Simulation: a fixed pseudo-random pattern per leaf, from its bit
        // number, so the result doesn't depend on node order.
        let mut sim: Vec<[u64; SWEEP_WORDS]> = vec![[0; SWEEP_WORDS]; n];
        let lit_sim = |sim: &[[u64; SWEEP_WORDS]], l: Lit, w: usize| {
            sim[node_of(l)][w] ^ if inverted(l) { u64::MAX } else { 0 }
        };
        for v in 0..n {
            match aig.nodes[v] {
                Node::Const => {}
                Node::Leaf(bit) => {
                    let mut x = bit.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xd1b5_4a32_d192_ed03;
                    for word in sim[v].iter_mut() {
                        // splitmix64
                        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
                        let mut z = x;
                        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                        *word = z ^ (z >> 31);
                    }
                }
                Node::And(a, b) => {
                    for w in 0..SWEEP_WORDS {
                        sim[v][w] = lit_sim(&sim, a, w) & lit_sim(&sim, b, w);
                    }
                }
            }
        }
        // Classes by signature, complemented so pattern 0 reads 0: a node
        // and its complement land together.
        let mut classes: HashMap<[u64; SWEEP_WORDS], Vec<u32>> = HashMap::new();
        let mut repl: Vec<Option<Lit>> = vec![None; n];
        for v in 0..n as u32 {
            let s = sim[v as usize];
            let flip = s[0] & 1 == 1;
            let key = if flip { s.map(|w| !w) } else { s };
            let members = classes.entry(key).or_default();
            if aig.is_and(v) {
                for &u in members.iter().take(2) {
                    let su = sim[u as usize];
                    let phase = (su[0] & 1 == 1) != flip;
                    if same_function(aig, u, v, phase) {
                        repl[v as usize] = Some((u << 1) | phase as Lit);
                        break;
                    }
                }
            }
            if repl[v as usize].is_none() {
                members.push(v);
            }
        }
        // Rebuild with the replacements, hashing again.
        let mut out = Aig::new();
        let mut new_lit: Vec<Lit> = vec![FALSE; n];
        let map = |new_lit: &[Lit], l: Lit| new_lit[node_of(l)] ^ (l & 1);
        for v in 0..n {
            new_lit[v] = match (aig.nodes[v], repl[v]) {
                (_, Some(r)) => map(&new_lit, r),
                (Node::Const, None) => FALSE,
                (Node::Leaf(bit), None) => out.leaf(bit),
                (Node::And(a, b), None) => {
                    let (x, y) = (map(&new_lit, a), map(&new_lit, b));
                    out.and(x, y)
                }
            };
        }
        Network {
            outputs: self
                .outputs
                .iter()
                .map(|&(b, l)| (b, map(&new_lit, l)))
                .collect(),
            aig: out,
            gate_cells: self.gate_cells.clone(),
            max_bit: self.max_bit,
        }
    }
}

/// Whether AND node `v` computes node `u` (complemented if `phase`) for
/// every input: true only if some cut of both, of at most [`SWEEP_CUT`]
/// nodes, makes them the same function. `u` precedes `v` in the AIG.
fn same_function(aig: &Aig, u: u32, v: u32, phase: bool) -> bool {
    let mut cut: Vec<u32> = vec![u, v];
    cut.retain(|&x| !matches!(aig.nodes[x as usize], Node::Const));
    for _ in 0..SWEEP_STEPS {
        // Expand the latest AND of the cut into its fanins: every set so
        // reached still separates both nodes from the leaves.
        let Some(&latest) = cut.iter().filter(|&&x| aig.is_and(x)).max() else {
            return same_support(aig, &cut, u, v) && equal_over(aig, &cut, u, v, phase);
        };
        cut.retain(|&x| x != latest);
        let Node::And(a, b) = aig.nodes[latest as usize] else {
            unreachable!("latest is an AND")
        };
        for f in [node(a), node(b)] {
            if !matches!(aig.nodes[f as usize], Node::Const) && !cut.contains(&f) {
                cut.push(f);
            }
        }
        if cut.len() <= SWEEP_CUT
            && equal_over(aig, &cut, u, v, phase)
            && same_support(aig, &cut, u, v)
        {
            return true;
        }
    }
    false
}

/// Whether both nodes read every node of `cut`. A node that reads a signal
/// its function ignores (`c & !c` deep inside) is still equal, but the
/// proof models x: an x on that signal comes out of the replacement where
/// the original was defined (serv's `o_wdata0` failed `equiv_induct` so).
fn same_support(aig: &Aig, cut: &[u32], u: u32, v: u32) -> bool {
    let reached = |root: u32| -> HashSet<u32> {
        let mut seen = HashSet::new();
        let mut found = HashSet::new();
        let mut stack = vec![root];
        while let Some(x) = stack.pop() {
            if !seen.insert(x) {
                continue;
            }
            if cut.contains(&x) {
                found.insert(x);
                continue;
            }
            if let Node::And(a, b) = aig.nodes[x as usize] {
                stack.extend([node(a), node(b)]);
            }
        }
        found
    };
    let (ru, rv) = (reached(u), reached(v));
    ru.len() == cut.len() && rv.len() == cut.len()
}

/// Both nodes as functions of `cut`, compared over all 2^|cut| values.
fn equal_over(aig: &Aig, cut: &[u32], u: u32, v: u32, phase: bool) -> bool {
    if cut.len() > SWEEP_CUT {
        return false;
    }
    let words = (1usize << cut.len()).div_ceil(64);
    let mut val: HashMap<u32, Vec<u64>> = HashMap::new();
    for (i, &c) in cut.iter().enumerate() {
        let t: Vec<u64> = (0..words)
            .map(|w| {
                if i < 6 {
                    VAR[i]
                } else if (w >> (i - 6)) & 1 == 1 {
                    u64::MAX
                } else {
                    0
                }
            })
            .collect();
        val.insert(c, t);
    }
    val.insert(0, vec![0; words]);
    fn eval(aig: &Aig, val: &mut HashMap<u32, Vec<u64>>, x: u32) -> Option<Vec<u64>> {
        if let Some(t) = val.get(&x) {
            return Some(t.clone());
        }
        let Node::And(a, b) = aig.nodes[x as usize] else {
            // A leaf outside the cut: the cut doesn't separate it.
            return None;
        };
        let ta = eval(aig, val, node(a))?;
        let tb = eval(aig, val, node(b))?;
        let (ia, ib) = (inverted(a) as u64 * u64::MAX, inverted(b) as u64 * u64::MAX);
        let t: Vec<u64> = ta.iter().zip(&tb).map(|(x, y)| (x ^ ia) & (y ^ ib)).collect();
        val.insert(x, t.clone());
        Some(t)
    }
    let (Some(tu), Some(tv)) = (eval(aig, &mut val, u), eval(aig, &mut val, v)) else {
        return false;
    };
    let used = 1usize << cut.len();
    let mask = |w: usize| {
        if used >= 64 * (w + 1) {
            u64::MAX
        } else {
            (1u64 << (used - 64 * w)) - 1
        }
    };
    let inv = if phase { u64::MAX } else { 0 };
    (0..words).all(|w| (tu[w] ^ inv ^ tv[w]) & mask(w) == 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub gates: usize,
    pub and_nodes: usize,
    pub luts: usize,
    pub depth: u32,
}

/// A `$lut` cell: `tt` over `inputs` (at most K), driving `y`.
fn lut_cell(inputs: &[Value], tt: u64, y: u64) -> Value {
    let width = inputs.len().max(1);
    let inputs: Vec<Value> = if inputs.is_empty() {
        vec![json!("0")]
    } else {
        inputs.to_vec()
    };
    // yosys $lut: bit i of LUT is the output for input value i.
    let lut: String = (0..1usize << width)
        .rev()
        .map(|i| if tt >> i & 1 == 1 { '1' } else { '0' })
        .collect();
    json!({
        "hide_name": 1,
        "type": "$lut",
        "parameters": { "LUT": lut, "WIDTH": format!("{width:032b}") },
        "attributes": {},
        "port_directions": { "A": "input", "Y": "output" },
        "connections": { "A": inputs, "Y": [y] },
    })
}

/// Map the gates of module `top` in the yosys JSON at `gates`, and write
/// module `top` alone to `luts`: yosys' JSON also holds the cell library as
/// parameterless blackbox stubs, which would shadow the real cells
/// (`read_verilog -lib`) when yosys reads the result back.
pub fn map_file(gates: &Path, luts: &Path) -> Result<Stats> {
    let text =
        std::fs::read_to_string(gates).with_context(|| format!("reading {}", gates.display()))?;
    let mut json: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", gates.display()))?;
    let stats = map(&mut json)?;
    keep_only_top(&mut json)?;
    std::fs::write(luts, serde_json::to_string(&json)?)
        .with_context(|| format!("writing {}", luts.display()))?;
    Ok(stats)
}

fn keep_only_top(json: &mut Value) -> Result<()> {
    let modules = json["modules"]
        .as_object_mut()
        .context("netlist has no modules")?;
    let top = modules
        .remove("top")
        .context("netlist has no module `top`")?;
    modules.clear();
    modules.insert("top".into(), top);
    Ok(())
}

/// What a LUT input or an alias is: a bit the gates don't drive (or a LUT
/// output), or a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leaf {
    Bit(u64),
    Const(bool),
}

/// A mapped LUT: `tt` over `inputs` (at most K), driving bit `y`.
struct LutOut {
    inputs: Vec<Leaf>,
    tt: u64,
    y: u64,
}

/// The mapper's result, independent of the netlist format: the LUTs, and
/// the gate output bits that are now another signal.
struct Mapping {
    luts: Vec<LutOut>,
    aliases: Vec<(u64, Leaf)>,
    stats: Stats,
}

fn compute(net: &Network, k: LutSize, opts: MapOptions) -> Mapping {
    let plain = compute_on(net, k, opts);
    if !opts.restructure {
        return plain;
    }
    // Restructuring wins on some designs and loses on others: a merged
    // node feeds more places and a cut can no longer absorb its own copy,
    // and a refactored sum of products can take more LUTs than the
    // structure it replaced (picorv32: 736 LUTs against 664). Mapping is
    // cheap next to the rest of the build, so each network is mapped and
    // the one with the fewest LUTs kept, never one deeper than the plain
    // mapping.
    let swept = net.swept();
    let refactored = swept.refactored().swept();
    let depth = plain.stats.depth;
    let mut best = plain;
    for candidate in [&swept, &refactored] {
        let m = compute_on(candidate, k, opts);
        if m.stats.depth <= depth
            && (m.stats.luts, m.stats.depth) < (best.stats.luts, best.stats.depth)
        {
            best = m;
        }
    }
    best
}

fn compute_on(net: &Network, k: LutSize, opts: MapOptions) -> Mapping {
    let mut m = Mapper::new(net, k, opts);
    let mask = k.mask();
    m.pass(None);
    let mut cover = m.cover();
    for _ in 0..AREA_PASSES {
        let req = m.required(&cover);
        m.update_refs(&cover);
        m.pass(Some(&req));
        cover = m.cover();
    }
    for _ in 0..EXACT_PASSES {
        let req = m.required(&cover);
        m.exact_pass(&cover, &req);
        cover = m.cover();
    }
    let aig = &net.aig;

    // Each LUT in the cover drives a bit, in the polarity outputs want: an
    // output's own bit where one reads the node, otherwise a fresh one. A
    // LUT emitted inverted has that input flipped in every LUT reading it.
    let mut next_bit = net.max_bit + 1;
    let mut bit_of: HashMap<u32, u64> = HashMap::new();
    let mut flipped: HashSet<u32> = HashSet::new();
    for &(b, l) in &net.outputs {
        if aig.is_and(node(l)) && !inverted(l) {
            bit_of.entry(node(l)).or_insert(b);
        }
    }
    for &(b, l) in &net.outputs {
        if aig.is_and(node(l)) && inverted(l) && !bit_of.contains_key(&node(l)) {
            bit_of.insert(node(l), b);
            flipped.insert(node(l));
        }
    }
    for &v in &cover {
        bit_of.entry(v).or_insert_with(|| {
            next_bit += 1;
            next_bit - 1
        });
    }
    let leaf_value = |l: u32| -> Leaf {
        match aig.nodes[l as usize] {
            Node::Leaf(b) => Leaf::Bit(b),
            Node::And(..) => Leaf::Bit(bit_of[&l]),
            Node::Const => Leaf::Const(false),
        }
    };
    // A cut's table over the LUTs as emitted.
    let table = |cut: &Cut| -> u64 {
        cut.leaves
            .iter()
            .enumerate()
            .filter(|(_, l)| flipped.contains(l))
            .fold(cut.tt, |tt, (i, _)| {
                let s = 1 << i;
                let var = VAR[i] & mask;
                ((tt & var) >> s) | ((tt & !var & mask) << s)
            })
    };

    let mut luts = Vec::new();
    for &v in &cover {
        let n = &m.nodes[v as usize];
        let cut = &n.cuts[n.best];
        let inputs = cut.leaves.iter().map(|&l| leaf_value(l)).collect();
        let inv = if flipped.contains(&v) { mask } else { 0 };
        luts.push(LutOut {
            inputs,
            tt: table(cut) ^ inv,
            y: bit_of[&v],
        });
    }
    // Every other output bit computes something already available: the
    // same node in the same polarity (structural hashing merged it), a leaf,
    // or a constant. Readers of such a bit read that instead, the way yosys
    // would merge the two nets. Only an AND read in both polarities needs a
    // second LUT, its cut with the table inverted.
    let mut aliases = Vec::new();
    let mut second: HashMap<u32, u64> = HashMap::new();
    for &(b, l) in &net.outputs {
        let v = node(l);
        let want_flipped = inverted(l);
        let target = match aig.nodes[v as usize] {
            Node::And(..) if bit_of[&v] == b => continue,
            Node::And(..) if flipped.contains(&v) == want_flipped => Leaf::Bit(bit_of[&v]),
            Node::And(..) => {
                if let Some(&other) = second.get(&v) {
                    Leaf::Bit(other)
                } else {
                    let n = &m.nodes[v as usize];
                    let cut = &n.cuts[n.best];
                    let inputs = cut.leaves.iter().map(|&l| leaf_value(l)).collect();
                    let inv = if want_flipped { mask } else { 0 };
                    luts.push(LutOut {
                        inputs,
                        tt: table(cut) ^ inv,
                        y: b,
                    });
                    second.insert(v, b);
                    continue;
                }
            }
            Node::Leaf(src) if !want_flipped => Leaf::Bit(src),
            Node::Leaf(src) => {
                luts.push(LutOut {
                    inputs: vec![Leaf::Bit(src)],
                    tt: !VAR[0] & mask,
                    y: b,
                });
                continue;
            }
            Node::Const => Leaf::Const(want_flipped),
        };
        aliases.push((b, target));
    }
    let stats = Stats {
        gates: net.gate_cells.len(),
        and_nodes: aig
            .nodes
            .iter()
            .filter(|n| matches!(n, Node::And(..)))
            .count(),
        luts: luts.len(),
        depth: m.max_depth(),
    };
    Mapping {
        luts,
        aliases,
        stats,
    }
}

fn leaf_json(l: Leaf) -> Value {
    match l {
        Leaf::Bit(b) => json!(b),
        Leaf::Const(v) => json!(if v { "1" } else { "0" }),
    }
}

/// Map the gates of module `top` in `json` to `$lut` cells, in place.
pub fn map(json: &mut Value) -> Result<Stats> {
    let net = Network::read(&json["modules"]["top"])?;
    let mapping = compute(&net, LutSize::FOUR, MapOptions::DEFAULT);

    let cells = json["modules"]["top"]["cells"]
        .as_object_mut()
        .context("no cells")?;
    for name in &net.gate_cells {
        cells.remove(name);
    }
    for l in &mapping.luts {
        let inputs: Vec<Value> = l.inputs.iter().map(|&i| leaf_json(i)).collect();
        cells.insert(
            format!("$chipsmith$lut${}", l.y),
            lut_cell(&inputs, l.tt, l.y),
        );
    }
    let alias: HashMap<u64, Value> = mapping
        .aliases
        .iter()
        .map(|&(b, t)| (b, leaf_json(t)))
        .collect();
    let substitute = |bits: &mut Value| {
        for bit in bits.as_array_mut().into_iter().flatten() {
            if let Some(t) = bit.as_u64().and_then(|b| alias.get(&b)) {
                *bit = t.clone();
            }
        }
    };
    for c in cells.values_mut() {
        let outputs: HashSet<String> = c["port_directions"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, d)| *d == "output")
            .map(|(p, _)| p.clone())
            .collect();
        for (port, bits) in c["connections"].as_object_mut().into_iter().flatten() {
            if !outputs.contains(port) {
                substitute(bits);
            }
        }
    }
    let module = &mut json["modules"]["top"];
    for p in module["ports"].as_object_mut().into_iter().flatten() {
        substitute(&mut p.1["bits"]);
    }
    for n in module["netnames"].as_object_mut().into_iter().flatten() {
        substitute(&mut n.1["bits"]);
    }
    Ok(mapping.stats)
}

/// The IR's gates as the mapper's operators. `$_TBUF_` and the
/// AOI/OAI gates aren't mapped: lowering never makes them.
fn ir_op(op: GateOp) -> Option<Op> {
    Some(match op {
        GateOp::And => Op::And,
        GateOp::Nand => Op::Nand,
        GateOp::Or => Op::Or,
        GateOp::Nor => Op::Nor,
        GateOp::Xor => Op::Xor,
        GateOp::Xnor => Op::Xnor,
        GateOp::AndNot => Op::AndNot,
        GateOp::OrNot => Op::OrNot,
        GateOp::Not => Op::Not,
        GateOp::Buf => Op::Buf,
        GateOp::Mux => Op::Mux,
        _ => return None,
    })
}

fn ir_sig(b: SigBit) -> Sig {
    match b {
        SigBit::Net(n) => Sig::Bit(n.index() as u64),
        // Undriven or don't-care: any value is correct; 0 keeps logic small.
        SigBit::Const(s) => Sig::Const(s == State::One),
    }
}

/// Map a module's gates to 4-input `$lut` cells, in place: the IR form of
/// [`map`], with net indices for bit numbers.
pub fn map_module(module: &mut Module) -> Result<Stats> {
    map_module_k(module, LutSize::FOUR)
}

/// [`map_module`] to LUTs of `k` inputs.
pub fn map_module_k(module: &mut Module, k: LutSize) -> Result<Stats> {
    map_module_with(module, k, MapOptions::DEFAULT)
}

/// [`map_module_k`] with other [`MapOptions`] than the default.
pub fn map_module_with(module: &mut Module, k: LutSize, opts: MapOptions) -> Result<Stats> {
    let mut driver: HashMap<u64, (Op, Vec<Sig>)> = HashMap::new();
    let mut read = Vec::new();
    let mut gate_cells = Vec::new();
    let mut is_gate = vec![false; module.cells.len()];
    for (i, c) in module.cells.iter().enumerate() {
        let CellKind::Gate(g) = &c.kind else {
            continue;
        };
        let Some(op) = ir_op(g.op()) else {
            continue;
        };
        let SigBit::Net(y) = g.y else {
            bail!("gate {} drives a constant", c.name)
        };
        let ins = g.inputs().iter().map(|&b| ir_sig(b)).collect();
        ensure!(
            driver.insert(y.index() as u64, (op, ins)).is_none(),
            "net {} has two drivers",
            y.index()
        );
        gate_cells.push(c.name.clone());
        is_gate[i] = true;
    }
    for (i, c) in module.cells.iter().enumerate() {
        if is_gate[i] {
            continue;
        }
        c.kind.for_each_port(|p| {
            if p.dir != Some(chipsmith_netlist::Dir::Out) {
                read.extend(
                    p.bits
                        .iter()
                        .filter_map(|b| b.net())
                        .map(|n| n.index() as u64),
                );
            }
        });
    }
    for (_, w, dir) in module.ports() {
        if dir.drives() {
            read.extend(
                w.bits
                    .iter()
                    .filter_map(|b| b.net())
                    .map(|n| n.index() as u64),
            );
        }
    }
    let max_bit = module.net_count() as u64;
    // Nets by index: the mapper speaks indices, the IR hands out nets.
    let mut nets: Vec<Option<Net>> = vec![None; module.net_count()];
    let mut see = |b: &SigBit| {
        if let SigBit::Net(n) = b {
            nets[n.index()] = Some(*n);
        }
    };
    module
        .wires
        .iter()
        .for_each(|w| w.bits.iter().for_each(&mut see));
    for c in &module.cells {
        c.kind.for_each_port(|p| p.bits.iter().for_each(&mut see));
    }
    let net_at = |i: u64| nets[i as usize].expect("the mapper only names the module's nets");
    let net = Network::build(GateList::new(driver, read, gate_cells, max_bit))?;
    let mapping = compute(&net, k, opts);

    let mut keep = is_gate.into_iter();
    module
        .cells
        .retain(|_| !keep.next().expect("one flag per cell"));
    // Fresh bits past the module's nets become new nets, in order.
    let mut fresh: HashMap<u64, Net> = HashMap::new();
    let mut sig_of = |module: &mut Module, b: u64| -> SigBit {
        if b < max_bit {
            return SigBit::Net(net_at(b));
        }
        SigBit::Net(*fresh.entry(b).or_insert_with(|| module.new_net()))
    };
    let mut namer = Namer::new(module, "lut");
    for l in &mapping.luts {
        let a: Vec<SigBit> = if l.inputs.is_empty() {
            vec![SigBit::ZERO]
        } else {
            l.inputs
                .iter()
                .map(|&i| match i {
                    Leaf::Bit(b) => sig_of(module, b),
                    Leaf::Const(v) => SigBit::Const(State::from(v)),
                })
                .collect()
        };
        let width = a.len();
        let table = Const(
            (0..1usize << width)
                .map(|i| State::from(l.tt >> i & 1 == 1))
                .collect(),
        );
        let y = sig_of(module, l.y);
        module.add_cell(namer.fresh(), CellKind::Lut(Lut { a, y, table }));
    }
    let mut subst = Subst::new(module);
    for &(b, t) in &mapping.aliases {
        let target = match t {
            Leaf::Bit(t) => sig_of(module, t),
            Leaf::Const(v) => SigBit::Const(State::from(v)),
        };
        subst.set(net_at(b), target);
    }
    subst.apply(module);
    Ok(mapping.stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_and_reduce_round_trip() {
        // a & b over leaves [1, 3], stretched to [1, 2, 3]: var0 & var2.
        let k = LutSize::FOUR;
        let m = k.mask();
        let tt = stretch(k, VAR[0] & VAR[1] & m, &[1, 3], &[1, 2, 3]);
        assert_eq!(tt, VAR[0] & VAR[2] & m);
        let c = reduce(
            k,
            Cut {
                leaves: vec![1, 2, 3],
                tt,
            },
        );
        assert_eq!(c.leaves, vec![1, 3]);
        assert_eq!(c.tt, VAR[0] & VAR[1] & m);
    }

    #[test]
    fn structural_hashing_merges_and_simplifies() {
        let mut g = Aig::new();
        let (a, b) = (g.leaf(1), g.leaf(2));
        let x = g.and(a, b);
        assert_eq!(g.and(b, a), x);
        assert_eq!(g.and(a, a ^ 1), FALSE);
        assert_eq!(g.and(a, TRUE), a);
        assert_eq!(g.nodes.len(), 4); // const, two leaves, one AND
    }

    fn module(cells: Value, outputs: &[u64]) -> Value {
        json!({ "modules": { "top": {
            "ports": { "y": { "direction": "output", "bits": outputs } },
            "cells": cells,
        }}})
    }

    fn gate(ty: &str, conns: Value) -> Value {
        json!({ "type": ty, "connections": conns, "port_directions": {} })
    }

    /// Evaluate the mapped netlist's LUTs for one input assignment.
    fn simulate(json: &Value, inputs: &HashMap<u64, bool>, out: u64) -> bool {
        let cells = json["modules"]["top"]["cells"].as_object().unwrap();
        let mut val = inputs.clone();
        loop {
            let mut progress = false;
            for c in cells.values() {
                let y = c["connections"]["Y"][0].as_u64().unwrap();
                if val.contains_key(&y) {
                    continue;
                }
                let a: Option<Vec<bool>> = c["connections"]["A"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| match b.as_u64() {
                        Some(b) => val.get(&b).copied(),
                        None => Some(b == "1"),
                    })
                    .collect();
                let Some(a) = a else { continue };
                let idx: usize = a.iter().enumerate().map(|(i, &b)| (b as usize) << i).sum();
                let lut = c["parameters"]["LUT"].as_str().unwrap();
                val.insert(y, lut.as_bytes()[lut.len() - 1 - idx] == b'1');
                progress = true;
            }
            if let Some(&v) = val.get(&out) {
                return v;
            }
            assert!(progress, "stuck");
        }
    }

    #[test]
    fn a_five_input_function_needs_two_luts_and_stays_correct() {
        // y = ((a & b) ^ c) | (d & !e), and z = !(a & b) as a second output.
        let mut j = module(
            json!({
                "g1": gate("$_AND_", json!({"A": [1], "B": [2], "Y": [10]})),
                "g2": gate("$_XOR_", json!({"A": [10], "B": [3], "Y": [11]})),
                "g3": gate("$_ANDNOT_", json!({"A": [4], "B": [5], "Y": [12]})),
                "g4": gate("$_OR_", json!({"A": [11], "B": [12], "Y": [13]})),
                "g5": gate("$_NOT_", json!({"A": [10], "Y": [14]})),
            }),
            &[13, 14],
        );
        let stats = map(&mut j).unwrap();
        assert_eq!(stats.depth, 2);
        for m in 0..32u32 {
            let ins: HashMap<u64, bool> = (0..5).map(|i| (i as u64 + 1, m >> i & 1 == 1)).collect();
            let (a, b, c, d, e) = (ins[&1], ins[&2], ins[&3], ins[&4], ins[&5]);
            assert_eq!(
                simulate(&j, &ins, 13),
                ((a & b) ^ c) | (d & !e),
                "y, inputs {m:05b}"
            );
            assert_eq!(simulate(&j, &ins, 14), !(a & b), "z, inputs {m:05b}");
        }
    }

    #[test]
    fn sweeping_merges_one_function_built_two_ways() {
        // y = a & (b & c) and z = !(!(a & b) | !c): one function, two
        // structures that hashing keeps apart.
        let j = module(
            json!({
                "g1": gate("$_AND_", json!({"A": [2], "B": [3], "Y": [10]})),
                "g2": gate("$_AND_", json!({"A": [1], "B": [10], "Y": [11]})),
                "g3": gate("$_NAND_", json!({"A": [1], "B": [2], "Y": [12]})),
                "g4": gate("$_NOT_", json!({"A": [3], "Y": [13]})),
                "g5": gate("$_NOR_", json!({"A": [12], "B": [13], "Y": [14]})),
            }),
            &[11, 14],
        );
        let net = Network::read(&j["modules"]["top"]).unwrap();
        let opts = |restructure| MapOptions {
            restructure,
            ..MapOptions::DEFAULT
        };
        assert_eq!(compute(&net, LutSize::FOUR, opts(false)).stats.luts, 2);
        let swept = compute(&net, LutSize::FOUR, opts(true));
        assert_eq!(swept.stats.luts, 1);
        assert_eq!(swept.aliases.len(), 1);
    }

    #[test]
    fn sweeping_keeps_functions_that_differ() {
        // a & b & c against a & b & !c: different, whatever simulation says.
        let j = module(
            json!({
                "g1": gate("$_AND_", json!({"A": [1], "B": [2], "Y": [10]})),
                "g2": gate("$_AND_", json!({"A": [10], "B": [3], "Y": [11]})),
                "g3": gate("$_ANDNOT_", json!({"A": [10], "B": [3], "Y": [12]})),
            }),
            &[11, 12],
        );
        let net = Network::read(&j["modules"]["top"]).unwrap();
        let swept = net.swept();
        let lits: Vec<Lit> = swept.outputs.iter().map(|&(_, l)| l).collect();
        assert_ne!(lits[0], lits[1]);
        assert_ne!(lits[0], lits[1] ^ 1);
    }

    #[test]
    fn refactoring_factors_out_a_shared_literal_and_keeps_the_function() {
        // y = (a & b) | (a & c) | (a & d): five ANDs (with the ORs), and
        // a & (b | c | d) takes three.
        let j = module(
            json!({
                "g1": gate("$_AND_", json!({"A": [1], "B": [2], "Y": [10]})),
                "g2": gate("$_AND_", json!({"A": [1], "B": [3], "Y": [11]})),
                "g3": gate("$_AND_", json!({"A": [1], "B": [4], "Y": [12]})),
                "g4": gate("$_OR_", json!({"A": [10], "B": [11], "Y": [13]})),
                "g5": gate("$_OR_", json!({"A": [13], "B": [12], "Y": [14]})),
            }),
            &[14],
        );
        let net = Network::read(&j["modules"]["top"]).unwrap();
        let ands = |n: &Network| n.aig.nodes.iter().filter(|x| matches!(x, Node::And(..))).count();
        let r = net.refactored();
        assert_eq!(ands(&net), 5);
        assert_eq!(ands(&r), 3);
        let eval = |n: &Network, m: u32| -> bool {
            let mut val = vec![false; n.aig.nodes.len()];
            for (v, node) in n.aig.nodes.iter().enumerate() {
                val[v] = match *node {
                    Node::Const => false,
                    Node::Leaf(bit) => m >> (bit - 1) & 1 == 1,
                    Node::And(a, b) => {
                        (val[node_of(a)] ^ inverted(a)) && (val[node_of(b)] ^ inverted(b))
                    }
                };
            }
            let l = n.outputs[0].1;
            val[node_of(l)] ^ inverted(l)
        };
        for m in 0..16 {
            assert_eq!(eval(&net, m), eval(&r, m), "inputs {m:04b}");
        }
    }

    #[test]
    fn only_top_is_written_back() {
        let mut j = json!({ "modules": {
            "LCELL_COMB": { "attributes": { "blackbox": "1" } },
            "top": { "cells": {} },
        }});
        keep_only_top(&mut j).unwrap();
        assert_eq!(j, json!({ "modules": { "top": { "cells": {} } } }));
    }

    #[test]
    fn constants_fold_away_and_pass_throughs_become_wires() {
        // y = MUX(s=1, a, b) = b: no LUT, the port reads b itself.
        let mut j = module(
            json!({ "m": gate("$_MUX_", json!({"A": [1], "B": [2], "S": ["1"], "Y": [3]})) }),
            &[3],
        );
        let stats = map(&mut j).unwrap();
        assert_eq!(stats.luts, 0);
        assert_eq!(j["modules"]["top"]["ports"]["y"]["bits"], json!([2]));
    }
}
