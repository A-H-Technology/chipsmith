//! Static timing of a packed design with the delay model (README (u), (z)):
//! a graph from flop outputs, input ports and M20K read data, through
//! routed (or estimated) connections, LUTs and carry chains, to flop and
//! M20K inputs, clock enables, synchronous clears and output ports. Arrival
//! times forward, required times backward against the worst arrival, and
//! per connection a slack and a criticality for timing-driven placement and
//! routing. One clock domain at a time; paths between domains aren't
//! analysed. No clock skew and no hold: it is the model's view, not
//! sign-off.

use super::det::HashMap;

use super::ioreg::{IoRegs, Role};
use super::netlist::{Cell, Netlist};
use super::pack::{
    pin_letter, six_pins, Alm, End, HalfLut, Logic, Packed, Slot, Src, ARITH_IN, LEIM_PIN,
};

/// M20K read data comes out 1.43 ns after the clock (mm's report_path).
pub const M20K_TCO: f32 = 1.429;
/// An M20K input's setup time [inf: not fitted; a LAB flop's tsu plus a
/// margin until an M20K path is in the fit].
pub const M20K_TSU: f32 = 0.25;
/// An HVIO input register's clock to its fabric output: uTco 2.091 plus
/// 1.793 out of the register cell [obs, one quartus_sta path, io (README
/// (z15)); IO96 assumed the same].
pub const IO_TCO: f32 = 3.884;
/// An HVIO output register's setup from its fabric input, the same path's
/// uTsu (negative: its clock arrives late) [obs, one path]; also used for
/// the I/O block's enables and synchronous clear [inf].
pub const IO_TSU: f32 = -1.297;

#[derive(Clone, Copy)]
enum Arc {
    Fixed(f32),
    /// A routed connection, by index into `Sta::conns`.
    Conn(usize),
}

pub struct Sta {
    /// Connections: (net, sink end).
    pub conns: Vec<(u32, End)>,
    pub conn_of: HashMap<(u32, End), usize>,
    n: usize,
    edges: Vec<(u32, u32, Arc)>,
    /// Launch: node, time at the node (tco), clock net.
    starts: Vec<(u32, f32, u32)>,
    /// Capture: node, extra time (setup), clock net (None: an output port),
    /// and what it is.
    ends: Vec<(u32, f32, Option<u32>, String)>,
    /// Per node, the clock domain its arrival belongs to.
    clock_of_start: HashMap<u32, u32>,
}

pub struct Report {
    /// Worst arrival at a capture point, and where.
    pub worst: f32,
    pub at: String,
    /// Per connection: criticality in 0..1.
    pub crit: Vec<f32>,
    /// The worst path from a clocked start (flop, M20K) to a clocked capture
    /// in the same domain: what quartus_sta's Fmax counts (README (z4)).
    pub core_worst: f32,
}

fn ff_clock(nl: &Netlist, ff: usize) -> u32 {
    match &nl.cells[ff] {
        Cell::Ff(f) => f.clk,
        _ => unreachable!(),
    }
}

impl Sta {
    pub fn new(nl: &Netlist, p: &Packed, d: &HashMap<String, f32>, io: &[IoRegs]) -> Sta {
        let io_clk: HashMap<usize, u32> = io.iter().map(|r| (r.port, r.clk)).collect();
        let get = |k: &str| d.get(k).copied().unwrap_or(0.0);
        // A class with a fallback (a delay table from before README (z7)).
        let get2 = |k: &str, alt: &str| d.get(k).or_else(|| d.get(alt)).copied().unwrap_or(0.0);
        let mut ids: HashMap<(u8, usize, u32), u32> = HashMap::default();
        let mut n = 0u32;
        let mut id = |k: (u8, usize, u32)| -> u32 {
            *ids.entry(k).or_insert_with(|| {
                n += 1;
                n - 1
            })
        };
        // Node keys: 0 net source, 1 ALM pin, 2 LUT out, 3 sum h, 4 cout h,
        // 5 capture point.
        let mut edges = Vec::new();
        let mut conns = Vec::new();
        let mut conn_of = HashMap::default();
        let mut starts = Vec::new();
        let mut ends = Vec::new();
        let mut clock_of_start = HashMap::default();
        let clocks: super::det::HashSet<u32> = p.clocks.iter().copied().collect();
        // Each arithmetic ALM's place in its chain: chains start at ALM 0
        // of a LAB and run on into the LAB below (place.rs), so the j-th ALM
        // sits at position j % 10.
        let mut chain_pos: HashMap<usize, (usize, usize)> = HashMap::default();
        for (ci, c) in p.chains.iter().enumerate() {
            for (j, &a) in c.iter().enumerate() {
                chain_pos.insert(a, (ci, j));
            }
        }
        for (&net, (drv, sinks)) in &p.nets {
            if clocks.contains(&net) {
                continue;
            }
            let src = id((0, 0, net));
            match *drv {
                End::Out { alm, slot } => match p.alms[alm].slots[slot as usize] {
                    Some(Slot::Ff { ff, .. }) => {
                        starts.push((src, get("tco"), ff_clock(nl, ff)));
                    }
                    Some(Slot::Wire { src: Src::Lut }) => edges.push((
                        id((2, alm, lut_key(&p.alms[alm], slot as usize))),
                        src,
                        Arc::Fixed(0.0),
                    )),
                    Some(Slot::Wire { src: Src::Sum }) => {
                        edges.push((id((3, alm, slot as u32 / 2)), src, Arc::Fixed(0.0)))
                    }
                    _ => {}
                },
                End::Io {
                    port,
                    role: Role::Q,
                } => starts.push((src, IO_TCO, io_clk[&port])),
                End::Port(_) | End::Pll(_) | End::Io { .. } => starts.push((src, 0.0, u32::MAX)),
                End::M20k { m, .. } => {
                    let Cell::M20k(c) = &nl.cells[p.m20ks[m]] else {
                        unreachable!()
                    };
                    starts.push((src, M20K_TCO, c.clk));
                }
                // A DSP block's output register, timed as an M20K's (the
                // model has no DSP arcs yet) [inf].
                End::Dsp { d, .. } => starts.push((src, M20K_TCO, p.dsps[d].clk)),
                _ => {}
            }
            for e in sinks {
                let ci = conns.len();
                conns.push((net, *e));
                conn_of.insert((net, *e), ci);
                let dst = match *e {
                    End::In { alm, pin } => id((1, alm, pin as u32)),
                    End::Ctl { alm, ctl } => {
                        let x = id((5, conns.len(), 0));
                        let clk = p.alms[alm].ctls(nl).iter().find_map(|c| {
                            if let super::pack::Ctl::Clock(k) = c {
                                Some(*k)
                            } else {
                                None
                            }
                        });
                        if matches!(
                            ctl,
                            super::pack::Ctl::Ce(_)
                                | super::pack::Ctl::Sclr(_)
                                | super::pack::Ctl::Wa(..)
                        ) {
                            ends.push((
                                x,
                                get("tsu"),
                                clk,
                                format!("{} at ALM {alm}", nl.nets[net as usize]),
                            ));
                        }
                        x
                    }
                    End::M20k { m, .. } => {
                        let x = id((5, conns.len(), 0));
                        let Cell::M20k(c) = &nl.cells[p.m20ks[m]] else {
                            unreachable!()
                        };
                        ends.push((x, M20K_TSU, Some(c.clk), format!("M20K {m}")));
                        x
                    }
                    End::Dsp { d, .. } => {
                        let x = id((5, conns.len(), 0));
                        ends.push((x, M20K_TSU, Some(p.dsps[d].clk), format!("DSP {d}")));
                        x
                    }
                    End::Io {
                        port,
                        role:
                            role @ (Role::Out | Role::Oe | Role::InCe | Role::OutCe | Role::InSclr),
                    } => {
                        let x = id((5, conns.len(), 0));
                        ends.push((
                            x,
                            IO_TSU,
                            Some(io_clk[&port]),
                            format!("{role:?} register of {}", nl.ports[port].name),
                        ));
                        x
                    }
                    End::Port(pi) | End::Io { port: pi, .. } => {
                        let x = id((5, conns.len(), 0));
                        ends.push((x, 0.0, None, format!("output {}", nl.ports[pi].name)));
                        x
                    }
                    End::Out { .. } | End::Pll(_) | End::PllRst(_) => continue,
                };
                edges.push((src, dst, Arc::Conn(ci)));
            }
        }
        for (a, alm) in p.alms.iter().enumerate() {
            match &alm.logic {
                // LUT outputs: key (2, a, h) for half h's 5-LUT, (2, a, 0) for
                // a 6-LUT; an arc from every pin the LUT reads.
                Logic::Six { pos, .. } => {
                    for &p in pos {
                        for pin in six_pins(p) {
                            edges.push((
                                id((1, a, pin as u32)),
                                id((2, a, 0)),
                                Arc::Fixed(get(&format!("lut:data{}", pin_letter(pin)))),
                            ));
                        }
                    }
                }
                Logic::Five { halves } => {
                    for (h, half) in halves.iter().enumerate() {
                        let Some(half) = half else { continue };
                        for &pin in &half.pins {
                            edges.push((
                                id((1, a, pin as u32)),
                                id((2, a, h as u32)),
                                Arc::Fixed(get(&format!("lut:data{}", pin_letter(pin)))),
                            ));
                        }
                    }
                }
                // MLAB RAM (README (z9)): the read is a LUT read at its
                // address pins; the write data, like the write address and
                // enable, is captured at the clock.
                Logic::Ram { halves } => {
                    for (h, c) in halves.iter().enumerate() {
                        let Some(c) = c else { continue };
                        let Cell::Mlab(m) = &nl.cells[*c] else {
                            unreachable!()
                        };
                        for pins in super::pack::RAM_RADDR {
                            for &pin in pins {
                                edges.push((
                                    id((1, a, pin as u32)),
                                    id((2, a, h as u32)),
                                    Arc::Fixed(get(&format!("lut:data{}", pin_letter(pin)))),
                                ));
                            }
                        }
                        ends.push((
                            id((1, a, super::pack::RAM_WDATA[h] as u32)),
                            get("tsu"),
                            Some(m.clk),
                            format!("MLAB RAM data at ALM {a}"),
                        ));
                    }
                }
                // The carry arcs as quartus_sta strings them (README (z7)):
                // one cin > cout per ALM (slower at a half-LAB's last ALM, 4
                // or 9), a data input to its own ALM's cout and, by
                // look-ahead, to the half-LAB's and the LAB's last couts;
                // carry:lab between LABs. Node (4, a, 0) is ALM a's carry out.
                Logic::Arith { halves } => {
                    let Some(&(ci, j)) = chain_pos.get(&a) else {
                        continue;
                    };
                    let chain = &p.chains[ci];
                    let k = j % 10;
                    let end = |k: usize| if k % 5 == 4 { ":e" } else { "" };
                    let cout = id((4, a, 0));
                    // The carry in: the previous ALM's carry out.
                    let cin = (j > 0).then(|| {
                        (
                            id((4, chain[j - 1], 0)),
                            if k == 0 { get("carry:lab") } else { 0.0 },
                        )
                    });
                    if let Some((c, link)) = cin {
                        edges.push((
                            c,
                            cout,
                            Arc::Fixed(
                                link + get2(&format!("arith:cin>cout{}", end(k)), "arith:cin>cout"),
                            ),
                        ));
                    }
                    for h in 0..2usize {
                        if halves[h].is_none() {
                            continue;
                        }
                        if let Some((c, link)) = cin {
                            edges.push((
                                c,
                                id((3, a, h as u32)),
                                Arc::Fixed(link + get("arith:cin>sumout")),
                            ));
                        }
                        let Cell::Arith { c, d, .. } = &nl.cells[halves[h].unwrap()] else {
                            unreachable!()
                        };
                        for (kk, (letter, net)) in [("c", c), ("d", d)].into_iter().enumerate() {
                            if net.is_none() {
                                continue;
                            }
                            let pin = id((1, a, ARITH_IN[h][kk] as u32));
                            let sum = format!("arith:data{letter}>sumout");
                            edges.push((pin, id((3, a, h as u32)), Arc::Fixed(get(&sum))));
                            // Half 0's data reaches half 1's sum by the carry inside the ALM.
                            if h == 0 && halves[1].is_some() {
                                edges.push((
                                    pin,
                                    id((3, a, 1)),
                                    Arc::Fixed(get2(&format!("{sum}:x"), &sum)),
                                ));
                            }
                            edges.push((
                                pin,
                                cout,
                                Arc::Fixed(get2(
                                    &format!("arith:data{letter}>cout{}", end(k)),
                                    &format!("arith:data{letter}>cout"),
                                )),
                            ));
                            for e in [4usize, 9] {
                                if e > k && j + e - k < chain.len() {
                                    edges.push((
                                        pin,
                                        id((4, chain[j + e - k], 0)),
                                        Arc::Fixed(get(&format!("arith:data>cout:la{}", e - k))),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            for s in 0..4usize {
                let Some(Slot::Ff { ff, src }) = alm.slots[s] else {
                    continue;
                };
                let from = match src {
                    Src::Lut => id((2, a, lut_key(alm, s))),
                    Src::Sum => id((3, a, s as u32 / 2)),
                    Src::Leim => id((1, a, LEIM_PIN[s / 2] as u32)),
                };
                let x = id((5, usize::MAX - a * 4 - s, 1));
                edges.push((from, x, Arc::Fixed(get("reg_d"))));
                let Cell::Ff(f) = &nl.cells[ff] else {
                    unreachable!()
                };
                ends.push((
                    x,
                    get("tsu"),
                    Some(f.clk),
                    format!("{} (ALM {a} slot {s})", nl.nets[f.q as usize]),
                ));
            }
        }
        for &(s, _, clk) in &starts {
            clock_of_start.insert(s, clk);
        }
        Sta {
            conns,
            conn_of,
            n: n as usize,
            edges,
            starts,
            ends,
            clock_of_start,
        }
    }

    /// Analyse with per-connection delays.
    pub fn analyse(&self, conn_delay: &[f32]) -> Report {
        let delay = |a: &Arc| match a {
            Arc::Fixed(d) => *d,
            Arc::Conn(c) => conn_delay[*c],
        };
        // Topological order (Kahn).
        let mut indeg = vec![0u32; self.n];
        let mut out: Vec<Vec<usize>> = vec![Vec::new(); self.n];
        for (i, (u, v, _)) in self.edges.iter().enumerate() {
            indeg[*v as usize] += 1;
            out[*u as usize].push(i);
        }
        let mut order = Vec::with_capacity(self.n);
        let mut stack: Vec<u32> = (0..self.n as u32)
            .filter(|&v| indeg[v as usize] == 0)
            .collect();
        while let Some(u) = stack.pop() {
            order.push(u);
            for &e in &out[u as usize] {
                let v = self.edges[e].1;
                indeg[v as usize] -= 1;
                if indeg[v as usize] == 0 {
                    stack.push(v);
                }
            }
        }
        // Arrival, and the launching clock (one per node: the latest's).
        let mut arr = vec![f32::NEG_INFINITY; self.n];
        let mut clk = vec![u32::MAX; self.n];
        for &(s, t, c) in &self.starts {
            arr[s as usize] = t;
            clk[s as usize] = c;
        }
        for &u in &order {
            if arr[u as usize] == f32::NEG_INFINITY {
                continue;
            }
            for &e in &out[u as usize] {
                let (_, v, a) = &self.edges[e];
                let t = arr[u as usize] + delay(a);
                if t > arr[*v as usize] {
                    arr[*v as usize] = t;
                    clk[*v as usize] = clk[u as usize];
                }
            }
        }
        // The same without the ports' launches, for the register-to-register
        // figure.
        let mut core = vec![f32::NEG_INFINITY; self.n];
        let mut core_clk = vec![u32::MAX; self.n];
        for &(s, t, c) in &self.starts {
            if c != u32::MAX {
                core[s as usize] = t;
                core_clk[s as usize] = c;
            }
        }
        for &u in &order {
            if core[u as usize] == f32::NEG_INFINITY {
                continue;
            }
            for &e in &out[u as usize] {
                let (_, v, a) = &self.edges[e];
                let t = core[u as usize] + delay(a);
                if t > core[*v as usize] {
                    core[*v as usize] = t;
                    core_clk[*v as usize] = core_clk[u as usize];
                }
            }
        }
        // Per capture point with a clocked arrival: that arrival plus setup.
        let core_ends: Vec<f32> = self
            .ends
            .iter()
            .filter(|(x, _, c, _)| {
                c.is_some_and(|c| core_clk[*x as usize] == c)
                    && core[*x as usize] > f32::NEG_INFINITY
            })
            .map(|(x, extra, _, _)| core[*x as usize] + extra)
            .collect();
        let core_worst = core_ends.iter().copied().fold(0.0, f32::max);
        // Capture: paths launched by another clock than the capturing one
        // (ports launch on any) aren't timed.
        let mut worst = (0.0f32, String::new());
        let mut req = vec![f32::INFINITY; self.n];
        let mut timed = Vec::new();
        for (x, extra, c, what) in &self.ends {
            let a = arr[*x as usize];
            if a == f32::NEG_INFINITY {
                continue;
            }
            let launched = clk[*x as usize];
            if c.is_some_and(|c| launched != u32::MAX && launched != c) {
                continue;
            }
            timed.push((*x, *extra));
            if a + extra > worst.0 {
                worst = (a + extra, what.clone());
            }
        }
        let dmax = worst.0.max(1e-3);
        for (x, extra) in timed {
            req[x as usize] = req[x as usize].min(dmax - extra);
        }
        for &u in order.iter().rev() {
            for &e in &out[u as usize] {
                let (_, v, a) = &self.edges[e];
                let r = req[*v as usize] - delay(a);
                if r < req[u as usize] {
                    req[u as usize] = r;
                }
            }
        }
        let mut crit = vec![0.0f32; self.conns.len()];
        for (u, v, a) in &self.edges {
            if let Arc::Conn(c) = a {
                if arr[*u as usize] == f32::NEG_INFINITY || req[*v as usize] == f32::INFINITY {
                    continue;
                }
                let s = req[*v as usize] - arr[*u as usize] - conn_delay[*c];
                crit[*c] = (1.0 - s / dmax).clamp(0.0, 1.0);
            }
        }
        let _ = &self.clock_of_start;
        Report {
            worst: worst.0,
            at: worst.1,
            crit,
            core_worst,
        }
    }
}

/// The STA key of the LUT output slot s reads: half s / 2's 5-LUT, or the
/// 6-LUT.
fn lut_key(alm: &Alm, s: usize) -> u32 {
    match alm.logic {
        Logic::Six { .. } => 0,
        _ => s as u32 / 2,
    }
}

/// ALM counts by kind, for reports: 6-LUT, two 5-LUTs, one 5-LUT (with
/// or without a pass-through half), arithmetic, flops only, MLAB RAM.
pub fn alm_kinds(p: &Packed) -> [usize; 6] {
    let mut n = [0; 6];
    for a in &p.alms {
        let k = match &a.logic {
            Logic::Six { .. } => 0,
            Logic::Five { halves } => match halves
                .iter()
                .filter(|h| matches!(h, Some(x) if matches!(x.lut, HalfLut::Cell(_))))
                .count()
            {
                2 => 1,
                1 => 2,
                _ => 4,
            },
            Logic::Arith { .. } => 3,
            Logic::Ram { .. } => 5,
        };
        n[k] += 1;
    }
    n
}
