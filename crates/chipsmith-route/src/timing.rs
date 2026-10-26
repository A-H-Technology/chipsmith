//! Static timing for one clock domain, slow corner (speed grade 6).
//!
//! The numbers are Quartus' own, from cyclone2-re's docs/timing-model.md:
//! cell delays are the decoded DMF values (`ddb_magellan4-6.dmf.ddb`), which
//! match quartus_sta exactly; routing delays are quartus_sta's median per
//! element type, because the per-wire RC model that Quartus uses isn't
//! decoded yet (those medians are within about 30% of the real spread).
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

/// LUT input to output (DMF `LE_COMB/P2P/InOut`, worst transition) for
/// the LE at index `le` of its LAB. The carry out is slower where the chain
/// crosses the LAB's half-way buffer (LE 7 to 8, `CHAIN_THROUGH_LCB`) and
/// where it leaves the LAB (LE 15, `CHAIN_ACROSS_LAB`).
fn lut_arc(input: &str, output: &str, le: u8) -> Option<Ps> {
    let crossing = le == 7 || le == 15;
    Some(match (input, output) {
        ("DATAA", "COMBOUT") => 438.0,
        ("DATAB", "COMBOUT") => 420.0,
        ("DATAC", "COMBOUT") => 275.0,
        ("DATAD", "COMBOUT") => 150.0,
        ("CIN", "COMBOUT") => 410.0,
        // CHAIN_BASE; the crossing variants of DATAA aren't identified.
        ("DATAA", "COUT") => 414.0,
        ("DATAB", "COUT") if crossing => 485.0,
        ("DATAB", "COUT") => 393.0,
        // The stored cin->cout less its inverted-carry correction:
        // 230 - 71 through the LCB, 219 - 73 across LABs, 129 - 58 otherwise.
        ("CIN", "COUT") if le == 7 => 159.0,
        ("CIN", "COUT") if le == 15 => 146.0,
        ("CIN", "COUT") => 71.0,
        _ => return None,
    })
}

/// Register clock-to-output (DMF `LE_FF` TCO) plus the register's output
/// stage, which quartus_sta reports as a 0.088 ns `CYCLONEII_LCELL_FF` element.
const TCO: Ps = 250.0 + 88.0;
/// Register setup (DMF `TSU`, −36 ps), counted where the data enters.
const TSU: Ps = -36.0;

/// What a register input needs before the capturing clock edge.
fn register_setup(port: &str) -> Option<Ps> {
    Some(match port {
        // DMF `I:DATAIN B:FF` 84, `I:SDATA B:FF` 366.
        "DATAIN" => 84.0 + TSU,
        "SDATA" => 366.0 + TSU,
        // LAB-wide enable and sync clear reach the register through the LAB
        // control lines; their own DMF arcs aren't identified yet, so they
        // count like SDATA. Unknown, not measured.
        "ENA" | "SCLR" => 366.0 + TSU,
        _ => return None,
    })
}

/// M4K read clock to data out, with no output register (the address is
/// registered, the read data isn't). quartus_sta on an M4K feeding a LUT and
/// register: address register uTco 209 ps plus 2,993 ps to `portadataout`,
/// plus the 124 ps the block's clock arrives after an LE register's (cell
/// delay 0.661 vs 0.537 ns; skew isn't otherwise modelled).
const M4K_TCO: Ps = 209.0 + 2993.0 + 124.0;
/// M4K input register setup: quartus_sta's 142 ps cell delay into the
/// address register and its -35 ps uTsu, less the same 124 ps of later
/// clock.
const M4K_TSU: Ps = 142.0 - 35.0 - 124.0;

/// Cyclone II I/O-cell registers, for a register-to-register path that
/// starts or ends in a pin's I/O cell. quartus_sta on examples/de2-io
/// (2026-10-26): an input register's uTco 143 ps plus 390 ps out through
/// `regout`; an output or OE register's 269-279 ps cell delay from
/// `datain` (column 279, row 269) and its −78 ps uTsu. The I/O register's
/// clock reached it within 3 ps of an LE register's there (more wire,
/// faster cell), so no skew is modelled.
const IOE_TCO: Ps = 143.0 + 390.0;
const IOE_TSU: Ps = 279.0 - 78.0;

/// A delay from one timing node to another.
#[derive(Debug, Clone, Copy)]
pub struct Arc {
    pub from: u32,
    pub to: u32,
    pub delay: Ps,
}

/// Cell ports as timing nodes, and the arcs through cells. Net arcs (the
/// routing) are supplied to `analyse`, since they change as the router works.
#[derive(Debug, Default)]
pub struct Graph {
    names: Vec<String>,
    index: HashMap<(String, String), u32>,
    cell_arcs: Vec<Arc>,
    /// Arrival time at the nodes where paths start (register outputs).
    launch: Vec<Option<Ps>>,
    /// Setup time at the nodes where paths end (register data inputs).
    setup: Vec<Option<Ps>>,
}

impl Graph {
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
        self.index.insert(key, n);
        n
    }

    /// Add a cell's arcs, start points and end points, given the ports it
    /// has connected and, for a LUT, its LE index in the LAB.
    pub fn add_cell(&mut self, cell: &str, ty: &str, ports: &[&str], le: u8) {
        match ty {
            "LCELL_COMB" => {
                for &i in ports {
                    for &o in ports {
                        if let Some(delay) = lut_arc(i, o, le) {
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
                    } else if p != "CLK0" {
                        self.setup[n as usize] = Some(M4K_TSU);
                    }
                }
            }
            _ => {}
        }
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

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn name(&self, n: u32) -> &str {
        &self.names[n as usize]
    }

    /// Arrival and required times over the cell arcs plus `net_arcs`, with
    /// the clock period set to the critical path itself (so the worst slack
    /// is 0 and criticality is relative, as VPR does without a constraint).
    pub fn analyse(&self, net_arcs: &[Arc]) -> Analysis {
        let n = self.len();
        let arcs: Vec<Arc> = self.cell_arcs.iter().chain(net_arcs).copied().collect();
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
        while i < order.len() {
            for &a in &fanout[order[i] as usize] {
                let to = arcs[a].to as usize;
                indegree[to] -= 1;
                if indegree[to] == 0 {
                    order.push(to as u32);
                }
            }
            i += 1;
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
            Arc {
                from: g.node("a", "REGOUT"),
                to: g.node("m", "WA0"),
                delay: 500.0,
            },
        ];
        let t = g.analyse(&nets);
        assert_eq!(t.critical, M4K_TCO + 500.0 + 84.0 + TSU);
        assert!(t.criticality(&nets[1]) < 1.0);
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
        assert_eq!(t.critical, TCO + 600.0 + 438.0 + 84.0 + TSU);
        assert_eq!(t.criticality(&nets[0]), 1.0);
        // The short path has (600 + 438) - 300 of slack.
        let slack = 600.0 + 438.0 - 300.0;
        assert!((t.criticality(&nets[2]) - (1.0 - slack / t.critical)).abs() < 1e-4);
        assert_eq!(t.path.first().map(|p| p.0), Some(aq));
        assert_eq!(t.path.last().map(|p| p.0), Some(bd));
    }
}
