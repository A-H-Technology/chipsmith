//! Technology mapping of simple gates to 4-input LUTs, in place of yosys'
//! `abc -lut 4`.
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
//! Input and output are yosys JSON: `$_AND_`, `$_OR_`, `$_XOR_`, `$_MUX_`
//! and friends become `$lut` cells; every other cell stays as it is.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Map, Value};

const K: usize = 4;
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
        read.retain(|b| driver.contains_key(b));
        read.sort();
        read.dedup();

        // Build the AIG depth first from every output, with an explicit
        // stack (gate chains can be thousands deep).
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

/// Truth table of leaf `i` of a K-input function.
const VAR: [u16; K] = [0xaaaa, 0xcccc, 0xf0f0, 0xff00];

/// A cut: up to K leaves (sorted AIG nodes) and the node's function of them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cut {
    leaves: Vec<u32>,
    tt: u16,
}

/// Re-express `tt` over `from` leaves as a function over `to` (a superset).
fn stretch(tt: u16, from: &[u32], to: &[u32]) -> u16 {
    let pos: Vec<usize> = from.iter().map(|l| to.binary_search(l).unwrap()).collect();
    let mut out = 0u16;
    for m in 0..16u16 {
        let mut idx = 0;
        for (i, &p) in pos.iter().enumerate() {
            idx |= ((m >> p) & 1) << i;
        }
        out |= ((tt >> idx) & 1) << m;
    }
    out
}

/// Drop leaves the function doesn't depend on.
fn reduce(mut cut: Cut) -> Cut {
    let mut i = 0;
    while i < cut.leaves.len() {
        let shift = 1 << i;
        let independent = (cut.tt & VAR[i]) >> shift == cut.tt & !VAR[i];
        if independent {
            let n = cut.leaves.len();
            let mut tt = 0u16;
            for m in 0..(1u16 << (n - 1)) {
                let low = m & ((1 << i) - 1);
                let high = (m >> i) << (i + 1);
                tt |= ((cut.tt >> (low | high)) & 1) << m;
            }
            // Replicate so the table stays a valid K-input function.
            let width = 1 << (n - 1);
            let mut full = 0u16;
            for r in 0..(16 / width) {
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
    aig: &'a Aig,
    /// AIG nodes outputs read (their LUTs must exist).
    roots: Vec<u32>,
    nodes: Vec<NodeMap>,
    /// Estimated fanout of each node in the current mapping (>= 1).
    refs: Vec<f32>,
}

impl<'a> Mapper<'a> {
    fn new(net: &'a Network) -> Self {
        let aig = &net.aig;
        let n = aig.nodes.len();
        let mut nodes = vec![NodeMap::default(); n];
        let mut refs = vec![0.0f32; n];
        for (v, node) in aig.nodes.iter().enumerate() {
            match *node {
                Node::Leaf(_) => {
                    nodes[v].cuts = vec![Cut {
                        leaves: vec![v as u32],
                        tt: VAR[0],
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
        1.0 + cut
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
        let inv = if inverted(l) { 0xffff } else { 0 };
        let mut out = vec![Cut {
            leaves: vec![v],
            tt: VAR[0] ^ inv,
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
                if leaves.len() > K {
                    continue;
                }
                let tt = stretch(x.tt, &x.leaves, &leaves) & stretch(y.tt, &y.leaves, &leaves);
                let cut = reduce(Cut { leaves, tt });
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

    /// Reference `leaves`; returns how many LUTs that brings into the cover
    /// (this one included), recursing into newly referenced ANDs.
    fn reference(&self, refs: &mut [u32], leaves: &[u32]) -> u32 {
        let mut area = 1;
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
        let mut area = 1;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub gates: usize,
    pub and_nodes: usize,
    pub luts: usize,
    pub depth: u32,
}

/// A `$lut` cell: `tt` over `inputs` (at most K), driving `y`.
fn lut_cell(inputs: &[Value], tt: u16, y: u64) -> Value {
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

/// Map the gates of module `top` in `json` to `$lut` cells, in place.
pub fn map(json: &mut Value) -> Result<Stats> {
    let net = Network::read(&json["modules"]["top"])?;
    let mut m = Mapper::new(&net);
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
    let leaf_value = |l: u32| -> Value {
        match aig.nodes[l as usize] {
            Node::Leaf(b) => json!(b),
            Node::And(..) => json!(bit_of[&l]),
            Node::Const => json!("0"),
        }
    };
    // A cut's table over the LUTs as emitted.
    let table = |cut: &Cut| -> u16 {
        cut.leaves
            .iter()
            .enumerate()
            .filter(|(_, l)| flipped.contains(l))
            .fold(cut.tt, |tt, (i, _)| {
                let s = 1 << i;
                ((tt & VAR[i]) >> s) | ((tt & !VAR[i]) << s)
            })
    };

    let cells = json["modules"]["top"]["cells"]
        .as_object_mut()
        .context("no cells")?;
    for name in &net.gate_cells {
        cells.remove(name);
    }
    let mut luts = 0;
    let mut emit = |cells: &mut Map<String, Value>, inputs: Vec<Value>, tt: u16, y: u64| {
        cells.insert(format!("$chipsmith$lut${y}"), lut_cell(&inputs, tt, y));
        luts += 1;
    };
    for &v in &cover {
        let n = &m.nodes[v as usize];
        let cut = &n.cuts[n.best];
        let inputs = cut.leaves.iter().map(|&l| leaf_value(l)).collect();
        let inv = if flipped.contains(&v) { 0xffff } else { 0 };
        emit(cells, inputs, table(cut) ^ inv, bit_of[&v]);
    }
    // Every other output bit computes something already available: the
    // same node in the same polarity (structural hashing merged it), a leaf,
    // or a constant. Readers of such a bit read that instead, the way yosys
    // would merge the two nets. Only an AND read in both polarities needs a
    // second LUT, its cut with the table inverted.
    let mut alias: HashMap<u64, Value> = HashMap::new();
    let mut second: HashMap<u32, u64> = HashMap::new();
    for &(b, l) in &net.outputs {
        let v = node(l);
        let want_flipped = inverted(l);
        let target = match aig.nodes[v as usize] {
            Node::And(..) if bit_of[&v] == b => continue,
            Node::And(..) if flipped.contains(&v) == want_flipped => json!(bit_of[&v]),
            Node::And(..) => {
                if let Some(&other) = second.get(&v) {
                    json!(other)
                } else {
                    let n = &m.nodes[v as usize];
                    let cut = &n.cuts[n.best];
                    let inputs = cut.leaves.iter().map(|&l| leaf_value(l)).collect();
                    let inv = if want_flipped { 0xffff } else { 0 };
                    emit(cells, inputs, table(cut) ^ inv, b);
                    second.insert(v, b);
                    continue;
                }
            }
            Node::Leaf(src) if !want_flipped => json!(src),
            Node::Leaf(src) => {
                emit(cells, vec![json!(src)], !VAR[0], b);
                continue;
            }
            Node::Const => json!(if want_flipped { "1" } else { "0" }),
        };
        alias.insert(b, target);
    }
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
    Ok(Stats {
        gates: net.gate_cells.len(),
        and_nodes: aig
            .nodes
            .iter()
            .filter(|n| matches!(n, Node::And(..)))
            .count(),
        luts,
        depth: m.max_depth(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_and_reduce_round_trip() {
        // a & b over leaves [1, 3], stretched to [1, 2, 3]: var0 & var2.
        let tt = stretch(VAR[0] & VAR[1], &[1, 3], &[1, 2, 3]);
        assert_eq!(tt, VAR[0] & VAR[2]);
        let c = reduce(Cut {
            leaves: vec![1, 2, 3],
            tt,
        });
        assert_eq!(c.leaves, vec![1, 3]);
        assert_eq!(c.tt, VAR[0] & VAR[1]);
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
