//! Static timing for one clock domain, slow corner (speed grade 6).
//!
//! Cell delays are chipsmith's own (docs/timing-silicon.md): measured on
//! the DE2's EP2C35 with ring oscillators chipsmith places and routes
//! (bench/ring), scaled to the slow corner by one factor per class of cell,
//! and the register, M4K and I/O-cell constants, which a ring can't isolate,
//! fitted to quartus_sta's path totals on chipsmith's own placements, in
//! aggregate. Routing delays are quartus_sta's median per element type
//! (cyclone2-re docs/timing-model.md), because a per-wire model isn't there
//! yet (those medians are within about 30% of the real spread).
//!
//! Paths run register to register, which is what Quartus reports for a
//! design with only a clock constraint: pins are unconstrained, so they
//! neither launch nor capture.

use std::collections::HashMap;

/// Picoseconds.
pub type Ps = f32;

/// quartus_sta's median delay of a routing element, by chip database wire
/// type (docs/timing-model.md, "fdi" table, STA median column).
pub fn wire_delay(wire_type: &str) -> Ps {
    match wire_type {
        "R4" => 235.0,
        "C4" => 285.0,
        "R24" => 433.0,
        "C16" => 465.0,
        // Block interconnect: the LAB lines feeding LE inputs (STA LAB_LINE).
        "LOCAL_INTERCONNECT" => 362.0,
        // An LE's output back into its own LAB (STA LOCAL_LINE).
        "LOCAL_LINE" => 229.0,
        "LE_BUFFER" => 56.0,
        // M4K read data onto the routing. One connection measured, not a
        // median: M4K_BUFFER, two R4s and a LAB line in 975 ps.
        "M4K_BUFFER" => 143.0,
        // Multiplier product bits onto the routing: taken as the M4K's
        // (one quartus_sta path, DSP_BUFFER to an LE's sdata through an R4
        // and a LAB line, was 1,232 ps; the model's R4 + LAB line + sdata
        // account for most of it).
        "DSP_BUFFER" => 143.0,
        "IO_DATAIN" => 66.0,
        "BLK_CLK_BUF" => 96.0,
        "LAB_CLK" => 363.0,
        "GLOBAL_CLK_V" => 181.0,
        "GLOBAL_CLK_H" => 385.0,
        "CLK_BUFFER" => 118.0,
        // Pin and LE-internal wires: their delay is part of a cell arc.
        _ => 0.0,
    }
}

// The cell delays: bench/ring/emit.py writes this block from the ring
// oscillator fits (bench/ring/fit.py, derive.py) and the path fit
// (path_fit.py). README "Timing model" says where each comes from.
// BEGIN bench/ring/emit.py
// Silicon: ring oscillators on the DE2's EP2C35 at room temperature
// (bench/ring/measure.sh, fit.py, derive.py), scaled to the slow corner by
// one factor per class: LUT 1.946, carry chain 1.516, multiplier
// 1.526, with the model's own LOCAL_LINE (229 ps) taken off a LUT arc.
// Fitted: the factors and the register constants, least squares over
// quartus_sta's path totals on chipsmith's placements (path_fit.py;
// aggregate, against the model's own routing).
// Guardband: 0 ps on every path start (emit.py).
/// LUT input to COMBOUT through each physical input A to D, the LE input's
/// interconnect included. Silicon.
const LUT_PIN: [Ps; 4] = [517.0, 497.0, 343.0, 233.0];
/// A carry chain's head: input A or B to COUT. Silicon (the head and the
/// tail always come together on a path; the tail is taken as input C's
/// arc, which is where CIN enters the LUT).
const CARRY_HEAD: [Ps; 2] = [579.0, 548.0];
/// CIN to COMBOUT (a chain's sum out). Input C's arc, see CARRY_HEAD.
const CIN_COMBOUT: Ps = 343.0;
/// CIN to COMBOUT as the placer sees it: the DMF value the placer was tuned
/// on, not silicon's. With 343 ps chains look cheaper and the placer stops
/// pulling them together: sha256 over seeds 1-6 under quartus_sta, 68.3 MHz
/// with 343, 69.7 with 410 here only (STA and routing keep 343, the
/// estimate within -0.7..+1.8% of quartus_sta). docs/timing-silicon.md.
const PLACER_CIN_COMBOUT: Ps = 410.0;
/// CIN to COUT inside a LAB half. Silicon.
const CIN_COUT: Ps = 72.0;
/// What a carry costs on top where it crosses the LAB's half-way buffer
/// (LE 7 to 8) and where it leaves the LAB (LE 15 to the next LAB's 0).
/// Silicon.
const CARRY_LCB: Ps = 78.0;
const CARRY_LAB: Ps = 66.0;
/// An unregistered multiplier, data in to product out: a full 18 x 18
/// block and one whose operands fit a nine-bit row, each the slowest
/// product bit measured. Silicon.
const MULT_COMB: Ps = 3024.0;
const MULT9_COMB: Ps = 2018.0;
/// LE register clock to REGOUT. Fitted, plus the guardband.
const TCO: Ps = 287.0;
/// What each register input needs before the capturing clock edge, its
/// interconnect included (SDATA's physical input C's too). Fitted; DATAIN
/// is held (only TCO + TSU is identifiable) and the others are relative
/// to it.
const TSU_DATAIN: Ps = 0.0;
const TSU_SDATA: Ps = 434.0;
const TSU_ENA: Ps = 471.0;
const TSU_SCLR: Ps = 308.0;
const TSU_SLOAD: Ps = 618.0;
/// M4K read clock to data out without and with the output register, and
/// its input registers' setup. Fitted (the launch ones plus the
/// guardband); they take up the block's clock arriving later than an LE
/// register's.
const M4K_TCO: Ps = 3085.0;
const M4K_OUTREG_TCO: Ps = 184.0;
const M4K_TSU: Ps = 187.0;
/// Cyclone II I/O-cell registers, for a register-to-register path that
/// starts or ends in a pin's I/O cell. Fitted (the launch plus the
/// guardband).
const IOE_TCO: Ps = 391.0;
const IOE_TSU: Ps = 424.0;
// END bench/ring/emit.py

/// The multiplier's own registers: the LE register's (they're in the same
/// rows as the LEs', on the same row clocks; no chipsmith design's paths
/// start or end in one often enough to fit them).
const MULT_TSU: Ps = TSU_DATAIN;
const MULT_TCO: Ps = TCO;

/// What a LUT input costs over input D, the fastest: carried by the LE
/// input's routing node (`LE<k>.A` to `.D`), since the router picks which
/// physical input a LUT input takes (the LUT mask is permuted to match).
/// The LUT arcs below are input D's for every input, so a connection pays
/// for the pin it ends on.
pub fn lut_pin_extra(pin: usize) -> Ps {
    LUT_PIN[pin] - LUT_PIN[3]
}

/// What a connection into a LUT's data input pays for its pin when the
/// pin isn't known yet (placement): the mean over the four.
pub const LUT_PIN_EXPECTED: Ps = (LUT_PIN[0] + LUT_PIN[1] + LUT_PIN[2] - 3.0 * LUT_PIN[3]) / 4.0;

/// LUT input to output for the LE at index `le` of its LAB, less the
/// physical input's [`lut_pin_extra`]. The carry out is slower where the
/// chain crosses the LAB's half-way buffer (LE 7 to 8) and where it leaves
/// the LAB (LE 15). Carry LEs keep their inputs where they are (A and B).
fn lut_arc(input: &str, output: &str, le: u8) -> Option<Ps> {
    let crossing = match le {
        7 => CARRY_LCB,
        15 => CARRY_LAB,
        _ => 0.0,
    };
    Some(match (input, output) {
        ("DATAA" | "DATAB" | "DATAC" | "DATAD", "COMBOUT") => LUT_PIN[3],
        ("CIN", "COMBOUT") => CIN_COMBOUT,
        ("DATAA", "COUT") => CARRY_HEAD[0] + crossing - lut_pin_extra(0),
        ("DATAB", "COUT") => CARRY_HEAD[1] + crossing - lut_pin_extra(1),
        ("CIN", "COUT") => CIN_COUT + crossing,
        _ => return None,
    })
}

/// What a register input needs before the capturing clock edge. LAB-wide
/// enable, sync clear and sync load reach the register through the LAB
/// control lines (routed as such); SDATA is the LE's input C, whose node
/// carries part of it (`lut_pin_extra`).
fn register_setup(port: &str) -> Option<Ps> {
    Some(match port {
        "DATAIN" => TSU_DATAIN,
        "SDATA" => TSU_SDATA - lut_pin_extra(2),
        "ENA" => TSU_ENA,
        "SCLR" => TSU_SCLR,
        "SLOAD" => TSU_SLOAD,
        _ => return None,
    })
}

/// A delay from one timing node to another.
#[derive(Debug, Clone, Copy)]
pub struct Arc {
    pub from: u32,
    pub to: u32,
    pub delay: Ps,
}

/// Cell ports as timing nodes, and the arcs through cells. Net arcs (the
/// routing) are supplied to `analyse`, since they change as the router works.
#[derive(Debug, Clone)]
pub struct Graph {
    /// CIN to COMBOUT as this graph's user times it: [`CIN_COMBOUT`] for
    /// STA and routing, [`PLACER_CIN_COMBOUT`] for placement.
    cin_combout: Ps,
    names: Vec<String>,
    index: HashMap<(String, String), u32>,
    cell_arcs: Vec<Arc>,
    /// Arrival time at the nodes where paths start (register outputs).
    launch: Vec<Option<Ps>>,
    /// Setup time at the nodes where paths end (register data inputs).
    setup: Vec<Option<Ps>>,
    /// The clock of a path start or end, for designs with several (an
    /// opaque id the caller hands out, e.g. the clock net). Nodes without
    /// one belong to clock 0.
    clocks: HashMap<u32, u32>,
    /// The LUT a node is a port of (an index into `luts`), for cutting
    /// latch loops.
    lut: Vec<Option<u32>>,
    luts: u32,
    /// Cut any other combinational loop too (`cut_loops`).
    cut_loops: bool,
}

impl Default for Graph {
    /// A graph for STA and routing: silicon's delays.
    fn default() -> Self {
        Graph {
            cin_combout: CIN_COMBOUT,
            names: Vec::new(),
            index: HashMap::new(),
            cell_arcs: Vec::new(),
            launch: Vec::new(),
            setup: Vec::new(),
            clocks: HashMap::new(),
            lut: Vec::new(),
            luts: 0,
            cut_loops: false,
        }
    }
}

impl Graph {
    /// A graph for the placer: carry chains a little dearer than silicon
    /// ([`PLACER_CIN_COMBOUT`]).
    pub fn placement() -> Self {
        Graph {
            cin_combout: PLACER_CIN_COMBOUT,
            ..Graph::default()
        }
    }

    /// The timing node of a cell port, created on first use.
    pub fn node(&mut self, cell: &str, port: &str) -> u32 {
        let key = (cell.to_string(), port.to_string());
        if let Some(&n) = self.index.get(&key) {
            return n;
        }
        let n = self.names.len() as u32;
        self.names.push(format!("{cell}.{port}"));
        self.launch.push(None);
        self.setup.push(None);
        self.lut.push(None);
        self.index.insert(key, n);
        n
    }

    /// Add a cell's arcs, start points and end points, given the ports it
    /// has connected and, for a LUT, its LE index in the LAB.
    pub fn add_cell(&mut self, cell: &str, ty: &str, ports: &[&str], le: u8) {
        match ty {
            "LCELL_COMB" => {
                let lut = self.luts;
                self.luts += 1;
                for &p in ports {
                    let n = self.node(cell, p);
                    self.lut[n as usize] = Some(lut);
                }
                for &i in ports {
                    for &o in ports {
                        if let Some(mut delay) = lut_arc(i, o, le) {
                            if (i, o) == ("CIN", "COMBOUT") {
                                delay = self.cin_combout;
                            }
                            let (from, to) = (self.node(cell, i), self.node(cell, o));
                            self.cell_arcs.push(Arc { from, to, delay });
                        }
                    }
                }
            }
            "LCELL_FF" => {
                for &p in ports {
                    if p == "REGOUT" {
                        let n = self.node(cell, p);
                        self.launch[n as usize] = Some(TCO);
                    } else if let Some(s) = register_setup(p) {
                        let n = self.node(cell, p);
                        self.setup[n as usize] = Some(s);
                    }
                }
            }
            "CY2_M4K" => {
                for &p in ports {
                    let n = self.node(cell, p);
                    if p.starts_with("DO") {
                        self.launch[n as usize] = Some(M4K_TCO);
                    } else if !matches!(p, "CLK0" | "CLK1") {
                        self.setup[n as usize] = Some(M4K_TSU);
                    }
                }
            }
            "CY2_MULT" => {
                let outs: Vec<&str> = ports
                    .iter()
                    .copied()
                    .filter(|p| p.starts_with("MULTOUT"))
                    .collect();
                let data = |p: &&&str| {
                    p.starts_with("MULTA") || p.starts_with("MULTB") || p.starts_with("SIGN")
                };
                for &i in ports.iter().filter(data) {
                    for &o in &outs {
                        let (from, to) = (self.node(cell, i), self.node(cell, o));
                        self.cell_arcs.push(Arc {
                            from,
                            to,
                            delay: MULT_COMB,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// An M4K's read data pins that its output register drives
    /// (`add_cell` timed them all as unregistered).
    pub fn add_m4k_output_registers<'a>(
        &mut self,
        cell: &str,
        pins: impl IntoIterator<Item = &'a str>,
    ) {
        for p in pins {
            if let Some(&n) = self.index.get(&(cell.to_string(), p.to_string())) {
                self.launch[n as usize] = Some(M4K_OUTREG_TCO);
            }
        }
    }

    /// A Cyclone II multiplier's registers (`add_cell` gave it the
    /// combinational arcs): an operand register captures its data pins,
    /// the product register launches the product pins; a registered
    /// operand's path to an unregistered product starts at the register,
    /// an unregistered operand's to a registered product ends at it. A
    /// multiplier whose operands fit a nine-bit row (`nine`) is faster
    /// through than a full block.
    pub fn add_mult_registers(&mut self, cell: &str, a: bool, b: bool, p: bool, nine: bool) {
        let nodes: Vec<(String, u32)> = self
            .index
            .iter()
            .filter(|((c, _), _)| c == cell)
            .map(|((_, port), &n)| (port.clone(), n))
            .collect();
        let comb = if nine { MULT9_COMB } else { MULT_COMB };
        let own: std::collections::HashSet<u32> = nodes.iter().map(|(_, n)| *n).collect();
        for arc in &mut self.cell_arcs {
            if own.contains(&arc.from) && own.contains(&arc.to) {
                arc.delay = comb;
            }
        }
        let registered_in =
            |port: &str| (a && port.starts_with("MULTA")) || (b && port.starts_with("MULTB"));
        for (port, n) in &nodes {
            let n = *n as usize;
            if port.starts_with("MULTOUT") {
                if p {
                    self.launch[n] = Some(MULT_TCO);
                } else if a || b {
                    self.launch[n] = Some(MULT_TCO + comb);
                }
            } else if registered_in(port) {
                self.setup[n] = Some(MULT_TSU);
            } else if p
                && !port.starts_with("SIGN")
                && !matches!(port.as_str(), "CLK" | "ENA" | "ACLR")
            {
                self.setup[n] = Some(MULT_TSU + comb);
            }
        }
        // Arcs through a register are no longer combinational.
        let regs: std::collections::HashSet<u32> = nodes
            .iter()
            .filter(|(port, _)| registered_in(port) || (p && port.starts_with("MULTOUT")))
            .map(|(_, n)| *n)
            .collect();
        self.cell_arcs
            .retain(|arc| !regs.contains(&arc.from) && !regs.contains(&arc.to));
    }

    /// A Cyclone II pin's I/O-cell registers: the input register launches
    /// at REGOUT, the output and OE registers capture DATAOUT and OE. A pin
    /// without them neither launches nor captures (pins are unconstrained).
    pub fn add_ioe_registers(&mut self, cell: &str, input: bool, output: bool, oe: bool) {
        if input {
            let n = self.node(cell, "REGOUT");
            self.launch[n as usize] = Some(IOE_TCO);
        }
        for (used, port) in [(output, "DATAOUT"), (oe, "OE")] {
            if used {
                let n = self.node(cell, port);
                self.setup[n as usize] = Some(IOE_TSU);
            }
        }
    }

    /// A delay through a cell, for devices whose cells `add_cell` doesn't
    /// know (7-series).
    pub fn add_arc(&mut self, from: u32, to: u32, delay: Ps) {
        self.cell_arcs.push(Arc { from, to, delay });
    }

    /// A path start: a register output, `delay` after the clock edge.
    pub fn set_launch(&mut self, node: u32, delay: Ps) {
        self.launch[node as usize] = Some(delay);
    }

    /// A path end: a register input needing `setup` before the edge.
    pub fn set_setup(&mut self, node: u32, setup: Ps) {
        self.setup[node as usize] = Some(setup);
    }

    /// Cut every combinational loop, as Vivado's timer does, rather than
    /// leave what is in or after one untimed: synthesis can leave a loop no
    /// value ever takes (two muxes selecting each other), and everything
    /// downstream of it would go unseen.
    pub fn cut_loops(&mut self) {
        self.cut_loops = true;
    }

    /// Put a path start or end on clock `clock`.
    pub fn set_clock(&mut self, node: u32, clock: u32) {
        self.clocks.insert(node, clock);
    }

    fn clock(&self, node: u32) -> u32 {
        self.clocks.get(&node).copied().unwrap_or(0)
    }

    /// The critical path of every pair of launching and capturing clocks
    /// that has one: each clock's own Fmax is its pair with itself.
    /// Crossings are listed as found; whether they matter depends on how
    /// the clocks relate, which the graph doesn't know.
    pub fn clock_pairs(&self, net_arcs: &[Arc]) -> Vec<ClockPair> {
        let mut launch_clocks: Vec<u32> = (0..self.len() as u32)
            .filter(|&v| self.launch[v as usize].is_some())
            .map(|v| self.clock(v))
            .collect();
        launch_clocks.sort();
        launch_clocks.dedup();
        let mut out = Vec::new();
        for &from in &launch_clocks {
            let mut g = self.clone();
            for v in 0..g.len() {
                if g.launch[v].is_some() && self.clock(v as u32) != from {
                    g.launch[v] = None;
                }
            }
            let a = g.analyse(net_arcs);
            let mut worst: HashMap<u32, Ps> = HashMap::new();
            for (v, (arr, s)) in a.arrival.iter().zip(&self.setup).enumerate() {
                if let (Some(arr), Some(s)) = (arr, s) {
                    let w = worst.entry(self.clock(v as u32)).or_insert(0.0);
                    *w = w.max(arr + s);
                }
            }
            let mut to: Vec<(u32, Ps)> = worst.into_iter().collect();
            to.sort_by_key(|&(c, _)| c);
            out.extend(
                to.into_iter()
                    .map(|(to, critical)| ClockPair { from, to, critical }),
            );
        }
        out
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn name(&self, n: u32) -> &str {
        &self.names[n as usize]
    }

    /// A path end's setup time; `None` for a node no path ends at.
    pub fn setup(&self, n: u32) -> Option<Ps> {
        self.setup[n as usize]
    }

    /// Arrival and required times over the cell arcs plus `net_arcs`, with
    /// the clock period set to the critical path itself (so the worst slack
    /// is 0 and criticality is relative, as VPR does without a constraint).
    ///
    /// A LUT's output wired back into its own input is a latch (Quartus maps
    /// latches that way); like quartus_sta, which times combinational loops
    /// as latches, the feedback arc is cut, so paths are timed from the
    /// latch's data and enable.
    pub fn analyse(&self, net_arcs: &[Arc]) -> Analysis {
        let n = self.len();
        let feedback = |a: &Arc| {
            self.lut[a.from as usize].is_some()
                && self.lut[a.from as usize] == self.lut[a.to as usize]
        };
        let arcs: Vec<Arc> = self
            .cell_arcs
            .iter()
            .chain(net_arcs.iter().filter(|a| !feedback(a)))
            .copied()
            .collect();
        let mut fanout: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut indegree = vec![0u32; n];
        for (i, a) in arcs.iter().enumerate() {
            fanout[a.from as usize].push(i);
            indegree[a.to as usize] += 1;
        }
        // Kahn's order. A combinational loop would leave nodes out; they
        // keep no arrival and so don't count.
        let mut order: Vec<u32> = (0..n as u32)
            .filter(|&v| indegree[v as usize] == 0)
            .collect();
        let mut i = 0;
        let mut cut = vec![false; arcs.len()];
        loop {
            while i < order.len() {
                for &a in &fanout[order[i] as usize] {
                    // A cut arc already gave up its claim on its target.
                    if cut[a] {
                        continue;
                    }
                    let to = arcs[a].to as usize;
                    indegree[to] -= 1;
                    if indegree[to] == 0 {
                        order.push(to as u32);
                    }
                }
                i += 1;
            }
            if !self.cut_loops || order.len() == n {
                break;
            }
            // Walk back from a node still waiting through others still
            // waiting until one repeats: that closes a loop; cut the arc
            // that closed it and go on.
            let mut fanin: Vec<Vec<usize>> = vec![Vec::new(); n];
            for (k, a) in arcs.iter().enumerate() {
                if !cut[k] && indegree[a.to as usize] > 0 && indegree[a.from as usize] > 0 {
                    fanin[a.to as usize].push(k);
                }
            }
            let Some(start) = (0..n).find(|&v| indegree[v] > 0 && !fanin[v].is_empty()) else {
                break;
            };
            let mut seen = vec![false; n];
            let mut v = start;
            let closing = loop {
                seen[v] = true;
                let k = fanin[v][0];
                let from = arcs[k].from as usize;
                if seen[from] {
                    break k;
                }
                v = from;
            };
            cut[closing] = true;
            let to = arcs[closing].to as usize;
            indegree[to] -= 1;
            if indegree[to] == 0 {
                order.push(to as u32);
            }
        }
        for (k, c) in cut.iter().enumerate() {
            if *c {
                let from = arcs[k].from as usize;
                fanout[from].retain(|&a| a != k);
            }
        }

        let mut arrival: Vec<Option<Ps>> = self.launch.clone();
        let mut from_arc: Vec<Option<usize>> = vec![None; n];
        for &v in &order {
            let Some(t) = arrival[v as usize] else {
                continue;
            };
            for &a in &fanout[v as usize] {
                let arc = arcs[a];
                let t2 = t + arc.delay;
                if arrival[arc.to as usize].is_none_or(|old| t2 > old) {
                    arrival[arc.to as usize] = Some(t2);
                    from_arc[arc.to as usize] = Some(a);
                }
            }
        }

        // Critical path: the worst arrival plus setup at an end point.
        let mut critical = 0.0;
        let mut worst_end = None;
        for (v, (a, s)) in arrival.iter().zip(&self.setup).enumerate() {
            if let (Some(a), Some(s)) = (a, s) {
                if a + s > critical {
                    critical = a + s;
                    worst_end = Some(v as u32);
                }
            }
        }

        let mut required: Vec<Option<Ps>> =
            self.setup.iter().map(|s| s.map(|s| critical - s)).collect();
        for &v in order.iter().rev() {
            for &a in &fanout[v as usize] {
                let arc = arcs[a];
                if let Some(r) = required[arc.to as usize] {
                    let r2 = r - arc.delay;
                    if required[v as usize].is_none_or(|old| r2 < old) {
                        required[v as usize] = Some(r2);
                    }
                }
            }
        }

        let mut path = Vec::new();
        let mut at = worst_end;
        while let Some(v) = at {
            path.push((v, arrival[v as usize].unwrap_or(0.0)));
            at = from_arc[v as usize].map(|a| arcs[a].from);
        }
        path.reverse();

        Analysis {
            critical,
            arrival,
            required,
            path,
        }
    }
}

/// The longest path from registers on clock `from` to registers on clock
/// `to`, setup included.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockPair {
    pub from: u32,
    pub to: u32,
    pub critical: Ps,
}

#[derive(Debug)]
pub struct Analysis {
    /// Longest register-to-register path, setup included: the minimum
    /// clock period, ignoring clock skew.
    pub critical: Ps,
    arrival: Vec<Option<Ps>>,
    required: Vec<Option<Ps>>,
    /// The critical path: (node, arrival).
    pub path: Vec<(u32, Ps)>,
}

impl Analysis {
    /// The latest arrival at `node`, if a timed path reaches it.
    pub fn arrival(&self, node: u32) -> Option<Ps> {
        self.arrival[node as usize]
    }

    /// How close `arc` is to the critical path: 1 on it, 0 for an arc with
    /// a whole critical path of slack or on no timed path at all.
    pub fn criticality(&self, arc: &Arc) -> f32 {
        let (Some(a), Some(r)) = (
            self.arrival[arc.from as usize],
            self.required[arc.to as usize],
        ) else {
            return 0.0;
        };
        if self.critical <= 0.0 {
            return 0.0;
        }
        let slack = r - a - arc.delay;
        (1.0 - slack / self.critical).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An M4K's read data into a register, and a register into its address.
    #[test]
    fn an_m4k_launches_and_captures() {
        let mut g = Graph::default();
        g.add_cell("m", "CY2_M4K", &["CLK0", "WA0", "DO0"], 0);
        g.add_cell("a", "LCELL_FF", &["REGOUT"], 0);
        g.add_cell("b", "LCELL_FF", &["DATAIN"], 0);
        let nets = [
            Arc {
                from: g.node("m", "DO0"),
                to: g.node("b", "DATAIN"),
                delay: 500.0,
            },
            // Short enough that the read path stays the critical one
            // behind the output register too.
            Arc {
                from: g.node("a", "REGOUT"),
                to: g.node("m", "WA0"),
                delay: 100.0,
            },
        ];
        const { assert!(TCO + 100.0 + M4K_TSU < M4K_OUTREG_TCO + 500.0 + TSU_DATAIN) };
        let t = g.analyse(&nets);
        assert_eq!(t.critical, M4K_TCO + 500.0 + TSU_DATAIN);
        // Behind its output register the read data launches earlier.
        g.add_m4k_output_registers("m", ["DO0"]);
        let t = g.analyse(&nets);
        assert_eq!(t.critical, M4K_OUTREG_TCO + 500.0 + TSU_DATAIN);
        assert!(t.criticality(&nets[1]) < 1.0);
    }

    /// a (clock 1) -> b (clock 1) and a -> c (clock 2): two pairs.
    #[test]
    fn clock_pairs_split_by_launch_and_capture() {
        let mut g = Graph::default();
        g.add_cell("a", "LCELL_FF", &["REGOUT"], 0);
        g.add_cell("b", "LCELL_FF", &["DATAIN"], 0);
        g.add_cell("c", "LCELL_FF", &["DATAIN"], 0);
        let (aq, bd, cd) = (
            g.node("a", "REGOUT"),
            g.node("b", "DATAIN"),
            g.node("c", "DATAIN"),
        );
        g.set_clock(aq, 1);
        g.set_clock(bd, 1);
        g.set_clock(cd, 2);
        let nets = [
            Arc {
                from: aq,
                to: bd,
                delay: 100.0,
            },
            Arc {
                from: aq,
                to: cd,
                delay: 900.0,
            },
        ];
        let p = g.clock_pairs(&nets);
        assert_eq!(
            p,
            vec![
                ClockPair {
                    from: 1,
                    to: 1,
                    critical: TCO + 100.0 + TSU_DATAIN
                },
                ClockPair {
                    from: 1,
                    to: 2,
                    critical: TCO + 900.0 + TSU_DATAIN
                },
            ]
        );
    }

    /// reg a -> latch x (COMBOUT back into DATAC) -> reg b: timed from a.
    #[test]
    fn a_latch_loop_is_cut() {
        let mut g = Graph::default();
        g.add_cell("a", "LCELL_FF", &["REGOUT"], 0);
        g.add_cell("x", "LCELL_COMB", &["DATAA", "DATAC", "COMBOUT"], 0);
        g.add_cell("b", "LCELL_FF", &["DATAIN"], 0);
        let (aq, xa, xc, xo, bd) = (
            g.node("a", "REGOUT"),
            g.node("x", "DATAA"),
            g.node("x", "DATAC"),
            g.node("x", "COMBOUT"),
            g.node("b", "DATAIN"),
        );
        let arc = |from, to| Arc {
            from,
            to,
            delay: 100.0,
        };
        let t = g.analyse(&[arc(aq, xa), arc(xo, xc), arc(xo, bd)]);
        // The LUT arc is input D's; the routing carries the rest of A's.
        assert_eq!(t.critical, TCO + 100.0 + LUT_PIN[3] + 100.0 + TSU_DATAIN);
    }

    /// reg a -> x -> y -> x (a loop of two cells) -> reg b: untimed unless
    /// the loop is cut, then timed from a once round.
    #[test]
    fn a_loop_of_two_is_cut_on_request() {
        let mut g = Graph::default();
        let (aq, xi, xl, xo, yi, yo, bd) = (
            g.node("a", "Q"),
            g.node("x", "I0"),
            g.node("x", "I1"),
            g.node("x", "O"),
            g.node("y", "I0"),
            g.node("y", "O"),
            g.node("b", "D"),
        );
        g.set_launch(aq, 0.0);
        g.set_setup(bd, 0.0);
        g.add_arc(xi, xo, 10.0);
        g.add_arc(xl, xo, 10.0);
        g.add_arc(yi, yo, 10.0);
        let arc = |from, to| Arc {
            from,
            to,
            delay: 100.0,
        };
        let nets = [arc(aq, xi), arc(xo, yi), arc(yo, xl), arc(xo, bd)];
        assert_eq!(g.analyse(&nets).critical, 0.0);
        g.cut_loops();
        let t = g.analyse(&nets);
        assert!(t.arrival(bd).is_some());
        assert!(t.critical >= 210.0, "{}", t.critical);
    }

    /// reg a -> LUT x (DATAA) -> reg b, and reg a -> reg c directly.
    #[test]
    fn critical_path_and_criticality() {
        let mut g = Graph::default();
        g.add_cell("a", "LCELL_FF", &["REGOUT"], 0);
        g.add_cell("x", "LCELL_COMB", &["DATAA", "COMBOUT"], 0);
        g.add_cell("b", "LCELL_FF", &["DATAIN"], 0);
        g.add_cell("c", "LCELL_FF", &["DATAIN"], 0);
        let (aq, xa, xo, bd, cd) = (
            g.node("a", "REGOUT"),
            g.node("x", "DATAA"),
            g.node("x", "COMBOUT"),
            g.node("b", "DATAIN"),
            g.node("c", "DATAIN"),
        );
        let nets = [
            Arc {
                from: aq,
                to: xa,
                delay: 600.0,
            },
            Arc {
                from: xo,
                to: bd,
                delay: 0.0,
            },
            Arc {
                from: aq,
                to: cd,
                delay: 300.0,
            },
        ];
        let t = g.analyse(&nets);
        let lut = LUT_PIN[3];
        assert_eq!(t.critical, TCO + 600.0 + lut + TSU_DATAIN);
        assert_eq!(t.criticality(&nets[0]), 1.0);
        // The short path has (600 + the LUT) - 300 of slack.
        let slack = 600.0 + lut - 300.0;
        assert!((t.criticality(&nets[2]) - (1.0 - slack / t.critical)).abs() < 1e-4);
        assert_eq!(t.path.first().map(|p| p.0), Some(aq));
        assert_eq!(t.path.last().map(|p| p.0), Some(bd));
    }

    /// Through each physical input a LUT takes that input's delay: input
    /// D's arc plus what the input's routing node adds.
    #[test]
    fn a_lut_input_costs_its_physical_pin() {
        for (pin, arc) in LUT_PIN.into_iter().enumerate() {
            assert_eq!(
                lut_arc("DATAA", "COMBOUT", 0).unwrap() + lut_pin_extra(pin),
                arc
            );
        }
        // Carry LEs keep their pins: A and B's carry arcs with the pin,
        // and B's at the LAB's half-way buffer.
        let a = lut_arc("DATAA", "COUT", 0).unwrap() + lut_pin_extra(0);
        assert_eq!(a, CARRY_HEAD[0]);
        let b = lut_arc("DATAB", "COUT", 7).unwrap() + lut_pin_extra(1);
        assert_eq!(b, CARRY_HEAD[1] + CARRY_LCB);
        assert_eq!(
            register_setup("SDATA").unwrap() + lut_pin_extra(2),
            TSU_SDATA
        );
    }
}
