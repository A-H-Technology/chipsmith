//! Placing ALMs in LABs (README (w), (y)): simulated annealing on wire
//! length plus, when timing is known, criticality-weighted connection delay
//! (README (z)), with a VPR-style schedule (temperature from the acceptance
//! rate, a shrinking move range). A carry chain runs through the half-ALM
//! slots of a LAB in order (ALM 0 half 0, half 1, ALM 1 …, 20 per LAB) and
//! on into the LAB one row down, so a chain starts at ALM 0 of a LAB and
//! moves as a whole. A LAB has two clock lines, two enable lines, one
//! synchronous clear and two asynchronous clears (README (y)); no move may
//! ask a LAB for more.

use super::det::HashMap;

use anyhow::{bail, Context, Result};

use super::netlist::Netlist;
use super::pack::{Ctl, End, Packed};

/// A LAB site, or an MLAB's (for RAM only: a5pnr puts no logic there).
#[derive(Clone, Copy, Debug)]
pub struct Lab {
    pub x: u32,
    pub y: u32,
    pub mlab: bool,
}

pub fn labs(db: &str) -> Result<Vec<Lab>> {
    let text = std::fs::read_to_string(format!("{db}/floorplan.tsv")).context("floorplan.tsv")?;
    let mut out = Vec::new();
    for l in text.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        if (f[1] == "cw_la_lab" || f[1] == "cw_ml_mlab") && !f[0].contains("rsp") {
            out.push(Lab {
                x: f[3].parse()?,
                y: f[4].parse()?,
                mlab: f[1] == "cw_ml_mlab",
            });
        }
    }
    out.sort_by_key(|l| (l.x, l.y));
    Ok(out)
}

pub fn m20k_sites(db: &str) -> Result<Vec<(u32, u32)>> {
    let text = std::fs::read_to_string(format!("{db}/floorplan.tsv")).context("floorplan.tsv")?;
    let mut out = Vec::new();
    for l in text.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        if f[1] == "cw_mm_m20k" && !f[0].contains("rsp") {
            out.push((f[3].parse()?, f[4].parse()?));
        }
    }
    Ok(out)
}

/// DSP block sites (`cw_dssm`), but the spare row's (Y 5).
pub fn dsp_sites(db: &str) -> Result<Vec<(u32, u32)>> {
    let text = std::fs::read_to_string(format!("{db}/floorplan.tsv")).context("floorplan.tsv")?;
    let mut out = Vec::new();
    for l in text.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        if f[1] == "cw_dssm" && f[4] != "5" {
            out.push((f[3].parse()?, f[4].parse()?));
        }
    }
    Ok(out)
}

/// A tiny deterministic generator (xorshift), so runs repeat.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e3779b97f4a7c15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub struct Placement {
    /// Per ALM: (LAB index, k).
    pub at: Vec<(usize, u8)>,
    /// Per M20K: its block's X, Y.
    pub m20k_at: Vec<(u32, u32)>,
    pub dsp_at: Vec<(u32, u32)>,
}

/// LAB lines a LAB may use, as Quartus' legalizer counts them ("too many
/// LAB line inputs", README (z14)) [obs, its LBLK debug output: `num_ll =
/// unique + heuristic -- num_ll_arch_limit = 55`]: a net takes a line per 5
/// of its sinks in the LAB (`get_max_lab_line_fanout`), and a line per 5 of
/// the further LEIMs it takes within one ALM (`Alm::line_sinks`).
pub const LAB_LINES: u32 = 55;
/// Where V16's IOPLL takes its reset from the fabric: the HIOL tile at
/// X121 Y46 (main.rs, RPI I38).
pub const PLL_RST_XY: (u32, u32) = (121, 46);

/// ALMs from which placement starts from a global placement.
const GLOBAL_FROM: usize = 1500;

/// One LAB line carries a net to at most this many sinks.
const LINE_FANOUT: u32 = 5;

/// The budget, or A5PNR_LAB_LINES.
fn lab_lines() -> u32 {
    static N: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("A5PNR_LAB_LINES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(LAB_LINES)
    })
}

fn lines(sinks: u32, dups: u32) -> u32 {
    sinks.div_ceil(LINE_FANOUT) + dups.div_ceil(LINE_FANOUT)
}

/// How many of each control line a LAB's ALMs ask for, and whether that
/// fits: two clocks, two enables, one synchronous clear, two asynchronous
/// clears (an ALM without one still takes a line, left inactive); and per
/// net its sinks and further LEIMs on LAB lines, with the lines they take.
#[derive(Default, Clone)]
struct LabUse(HashMap<Ctl, u32>, HashMap<u32, (u32, u32)>, u32);

impl LabUse {
    fn add(&mut self, ctls: &[Ctl], ins: &[(u32, u32, u32)], d: i32) {
        for c in ctls {
            let e = self.0.entry(*c).or_default();
            *e = (*e as i32 + d) as u32;
            if *e == 0 {
                self.0.remove(c);
            }
        }
        for &(n, sinks, dups) in ins {
            let e = self.1.entry(n).or_default();
            self.2 -= lines(e.0, e.1);
            e.0 = (e.0 as i32 + d * sinks as i32) as u32;
            e.1 = (e.1 as i32 + d * dups as i32) as u32;
            self.2 += lines(e.0, e.1);
            if e.0 == 0 {
                self.1.remove(&n);
            }
        }
    }
    /// One clock (Quartus used LAB clock 0 only, also with two domains:
    /// ct), two enables, one synchronous clear, one routed asynchronous
    /// clear (CLR1, LAB_RE 86) and the other line inactive (README (y)).
    fn ok(&self) -> bool {
        let (mut clk, mut ce, mut sclr, mut aclr) = (0, 0, 0, 0);
        for c in self.0.keys() {
            match c {
                Ctl::Clock(_) => clk += 1,
                Ctl::Ce(_) => ce += 1,
                Ctl::Sclr(_) => sclr += 1,
                Ctl::Aclr(Some(_)) => aclr += 1,
                Ctl::Aclr(None) | Ctl::Wa(..) => {}
            }
        }
        clk <= 1 && ce <= 2 && sclr <= 1 && aclr <= 1 && self.2 <= lab_lines()
    }
}

/// VPR's wire-length correction for a net's bounding box by pin count.
fn crossing(pins: usize) -> f64 {
    const Q: [f64; 50] = [
        1.0, 1.0, 1.0, 1.0828, 1.1536, 1.2206, 1.2823, 1.3385, 1.3991, 1.4493, 1.4974, 1.5455,
        1.5937, 1.6418, 1.6899, 1.7304, 1.7709, 1.8114, 1.8519, 1.8924, 1.9288, 1.9652, 2.0015,
        2.0379, 2.0743, 2.1061, 2.1379, 2.1698, 2.2016, 2.2334, 2.2646, 2.2958, 2.3271, 2.3583,
        2.3895, 2.4187, 2.4479, 2.4772, 2.5064, 2.5356, 2.5610, 2.5864, 2.6117, 2.6371, 2.6625,
        2.6887, 2.7148, 2.7410, 2.7671, 2.7933,
    ];
    if pins < 50 {
        Q[pins]
    } else {
        2.7933 + 0.02616 * (pins - 50) as f64
    }
}

/// A placement cost term for timing: for each (driver ALM or fixed point,
/// sink ALM) connection, its weight (criticality^exponent) and the delay
/// estimate as a function of the distance.
pub struct TimingCost<'a> {
    /// Per connection: driver end, sink end, weight.
    pub conns: Vec<(End, End, f64)>,
    pub delay: &'a dyn Fn(i64, i64) -> f64,
    pub lambda: f64,
}

#[allow(clippy::too_many_arguments)]
pub fn place(
    p: &Packed,
    nl: &Netlist,
    labs: &[Lab],
    m20k_sites: &[(u32, u32)],
    dsp_sites: &[(u32, u32)],
    port_xy: &HashMap<usize, (u32, u32)>,
    seed: u64,
    tc: Option<&TimingCost>,
    effort: f64,
) -> Result<Placement> {
    let mut rng = Rng::new(seed);
    let lab_at: HashMap<(u32, u32), usize> = labs
        .iter()
        .enumerate()
        .map(|(i, l)| ((l.x, l.y), i))
        .collect();
    let n = p.alms.len();
    let ctls: Vec<Vec<Ctl>> = p.alms.iter().map(|a| a.ctls(nl)).collect();
    // Per ALM, what it asks of its LAB's lines (clocks ride the clock
    // network).
    let ins: Vec<Vec<(u32, u32, u32)>> = p
        .alms
        .iter()
        .map(|a| {
            a.line_sinks(nl)
                .into_iter()
                .filter(|(n, ..)| !p.clocks.contains(n))
                .collect()
        })
        .collect();
    let mut occ: HashMap<(usize, u8), usize> = HashMap::default();
    let mut uses: Vec<LabUse> = vec![LabUse::default(); labs.len()];
    let mut at = vec![(usize::MAX, 0u8); n];
    // Start near the ports' centre.
    let (cx, cy) = if port_xy.is_empty() {
        (60, 28)
    } else {
        let k = port_xy.len() as u32;
        (
            port_xy.values().map(|v| v.0).sum::<u32>() / k,
            port_xy.values().map(|v| v.1).sum::<u32>() / k,
        )
    };
    // M20Ks go to the sites nearest the ports' centre and stay there.
    let mut sites: Vec<(u32, u32)> = m20k_sites.to_vec();
    sites.sort_by_key(|&(x, y)| (x as i64 - cx as i64).abs() + (y as i64 - cy as i64).abs());
    if p.m20ks.len() > sites.len() {
        bail!("{} M20Ks, {} sites", p.m20ks.len(), sites.len());
    }
    let m20k_at: Vec<(u32, u32)> = sites[..p.m20ks.len()].to_vec();
    // DSP blocks likewise (README (z11)).
    let mut dsites: Vec<(u32, u32)> = dsp_sites.to_vec();
    dsites.sort_by_key(|&(x, y)| (x as i64 - cx as i64).abs() + (y as i64 - cy as i64).abs());
    if p.dsps.len() > dsites.len() {
        bail!("{} DSP blocks, {} sites", p.dsps.len(), dsites.len());
    }
    let dsp_at: Vec<(u32, u32)> = dsites[..p.dsps.len()].to_vec();
    let mut by_dist: Vec<usize> = (0..labs.len()).collect();
    by_dist.sort_by_key(|&i| {
        (labs[i].x as i64 - cx as i64).abs() + (labs[i].y as i64 - cy as i64).abs()
    });
    // MLAB RAMs (README (z9)) go to the MLABs nearest the ports' centre and
    // stay there, as the M20Ks do; logic stays out of MLABs.
    let mut in_ram = vec![false; n];
    {
        let mut free = by_dist.iter().copied().filter(|&l| labs[l].mlab);
        for g in &p.mlabs {
            let l = free.next().context("more MLAB RAMs than MLABs")?;
            for (k, &a) in g.iter().enumerate() {
                occ.insert((l, k as u8), a);
                at[a] = (l, k as u8);
                uses[l].add(&ctls[a], &ins[a], 1);
                in_ram[a] = true;
            }
        }
    }
    by_dist.retain(|&l| !labs[l].mlab);
    let mut chain_of = vec![usize::MAX; n];
    for (ci, c) in p.chains.iter().enumerate() {
        for &a in c {
            chain_of[a] = ci;
        }
    }
    // A chain's slots from LAB (x, y) ALM k0, if all free and legal. Chains
    // start at ALM 0: every Quartus chain does (cql's six, README (z4)), and
    // Quartus' fitter rejects one that starts at ALM 5 and runs on.
    let chain_slots = |occ: &HashMap<(usize, u8), usize>,
                       uses: &[LabUse],
                       c: &[usize],
                       x: u32,
                       y: u32,
                       k0: usize|
     -> Option<Vec<(usize, u8)>> {
        let mut slots = Vec::new();
        let mut trial: HashMap<usize, LabUse> = HashMap::default();
        for (j, &a) in c.iter().enumerate() {
            let l = *lab_at.get(&(x, y.checked_sub(((k0 + j) / 10) as u32)?))?;
            if labs[l].mlab {
                return None;
            }
            let s = (l, ((k0 + j) % 10) as u8);
            if occ.get(&s).is_some_and(|&o| !c.contains(&o)) {
                return None;
            }
            trial
                .entry(l)
                .or_insert_with(|| uses[l].clone())
                .add(&ctls[a], &ins[a], 1);
            slots.push(s);
        }
        trial.values().all(LabUse::ok).then_some(slots)
    };
    // Big designs start from chipsmith's electrostatic global placer: the
    // start fills LABs nearest each ALM's centre and the anneal only
    // refines (README (z14): sha256 97 → 104 MHz, the flop CPU 138 → 136;
    // A5PNR_GLOBAL=0/1 forces it off or on).
    let global_on = match std::env::var("A5PNR_GLOBAL").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => n >= GLOBAL_FROM,
    };
    let centres = if global_on {
        Some(global(p, labs, port_xy, &m20k_at, &dsp_at, &at, &in_ram)?)
    } else {
        None
    };
    let near = |c: (f32, f32)| -> Vec<usize> {
        let mut v = by_dist.clone();
        v.sort_by(|&a, &b| {
            let d = |l: usize| (labs[l].x as f32 - c.0).abs() + (labs[l].y as f32 - c.1).abs();
            d(a).total_cmp(&d(b))
        });
        v
    };
    for (ci, c) in p.chains.iter().enumerate() {
        let order = match &centres {
            Some(cs) => near(cs[c[0]]),
            None => by_dist.clone(),
        };
        let slots = order
            .iter()
            .map(|&l| (l, 0))
            .find_map(|(l, k0)| chain_slots(&occ, &uses, c, labs[l].x, labs[l].y, k0))
            .with_context(|| {
                format!(
                    "chain {ci} ({} ALMs) doesn't fit; its control lines: {:?}",
                    c.len(),
                    c.iter().map(|&a| &ctls[a]).collect::<Vec<_>>()
                )
            })?;
        for (j, s) in slots.into_iter().enumerate() {
            occ.insert(s, c[j]);
            at[c[j]] = s;
            uses[s.0].add(&ctls[c[j]], &ins[c[j]], 1);
        }
    }
    {
        let mut li = 0;
        for a in 0..n {
            if chain_of[a] != usize::MAX || in_ram[a] {
                continue;
            }
            let order = match &centres {
                Some(cs) => {
                    li = 0;
                    near(cs[a])
                }
                None => by_dist.clone(),
            };
            'find: loop {
                let l = *order.get(li).context("design doesn't fit")?;
                for k in 0..10u8 {
                    if let std::collections::hash_map::Entry::Vacant(slot) = occ.entry((l, k)) {
                        let mut u = uses[l].clone();
                        u.add(&ctls[a], &ins[a], 1);
                        if u.ok() {
                            slot.insert(a);
                            at[a] = (l, k);
                            uses[l] = u;
                            break 'find;
                        }
                    }
                }
                li += 1;
            }
        }
    }
    // Nets for the cost: every net but the clocks (they ride the clock network).
    let nets: Vec<&(End, Vec<End>)> = p
        .nets
        .iter()
        .filter(|(k, _)| !p.clocks.contains(k))
        .map(|(_, v)| v)
        .collect();
    let alm_of = |e: &End| -> Option<usize> {
        match e {
            End::Out { alm, .. } | End::In { alm, .. } | End::Ctl { alm, .. } => Some(*alm),
            End::Port(_)
            | End::M20k { .. }
            | End::Pll(_)
            | End::PllRst(_)
            | End::Io { .. }
            | End::Dsp { .. } => None,
        }
    };
    let mut alm_nets: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, (d, s)) in nets.iter().enumerate() {
        for e in std::iter::once(d).chain(s.iter()) {
            if let Some(a) = alm_of(e) {
                alm_nets[a].push(i);
            }
        }
    }
    for v in &mut alm_nets {
        v.sort();
        v.dedup();
    }
    let weight: Vec<f64> = nets.iter().map(|(_, s)| crossing(s.len() + 1)).collect();
    let fixed_xy = |e: &End| -> Option<(i64, i64)> {
        match e {
            End::Port(p) | End::Io { port: p, .. } => {
                port_xy.get(p).map(|&(x, y)| (x as i64, y as i64))
            }
            End::M20k { m, .. } => Some((m20k_at[*m].0 as i64, m20k_at[*m].1 as i64)),
            End::Dsp { d, .. } => Some((dsp_at[*d].0 as i64, dsp_at[*d].1 as i64)),
            _ => None,
        }
    };
    let xy = |at: &[(usize, u8)], e: &End| -> Option<(i64, i64)> {
        match alm_of(e) {
            Some(a) => {
                let l = labs[at[a].0];
                Some((l.x as i64, l.y as i64))
            }
            None => fixed_xy(e),
        }
    };
    let net_cost = |at: &[(usize, u8)], i: usize| -> f64 {
        let (d, s) = nets[i];
        let (mut x0, mut x1, mut y0, mut y1) = (i64::MAX, i64::MIN, i64::MAX, i64::MIN);
        for e in std::iter::once(d).chain(s.iter()) {
            if let Some((x, y)) = xy(at, e) {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
        if x0 > x1 {
            return 0.0;
        }
        weight[i] * ((x1 - x0) + (y1 - y0)) as f64
    };
    // Timing connections touching each ALM.
    let mut alm_conns: Vec<Vec<usize>> = vec![Vec::new(); n];
    if let Some(tc) = tc {
        for (i, (d, s, _)) in tc.conns.iter().enumerate() {
            for e in [d, s] {
                if let Some(a) = alm_of(e) {
                    alm_conns[a].push(i);
                }
            }
        }
        for v in &mut alm_conns {
            v.sort();
            v.dedup();
        }
    }
    let conn_cost = |at: &[(usize, u8)], i: usize| -> f64 {
        let tc = tc.unwrap();
        let (d, s, w) = &tc.conns[i];
        match (xy(at, d), xy(at, s)) {
            (Some(a), Some(b)) => w * (tc.delay)((a.0 - b.0).abs(), (a.1 - b.1).abs()),
            _ => 0.0,
        }
    };
    let movable: Vec<usize> = (0..n)
        .filter(|&a| chain_of[a] == usize::MAX && !in_ram[a])
        .collect();
    if n == 0 || nets.is_empty() {
        return Ok(Placement {
            at,
            m20k_at,
            dsp_at,
        });
    }
    // Wire length plus a cost per LAB in use: wire length alone doesn't
    // mind ALMs spread thinly between their nets' ends (the CPU's 2,127
    // ALMs took 522 LABs without it).
    const LAB_COST: f64 = 1.5;
    let mut ln = vec![0u32; labs.len()];
    for &(l, _) in &at {
        if l != usize::MAX {
            ln[l] += 1;
        }
    }
    let lab_term = |ln: &[u32]| LAB_COST * ln.iter().filter(|&&c| c > 0).count() as f64;
    let mut wl: f64 = (0..nets.len()).map(|i| net_cost(&at, i)).sum::<f64>() + lab_term(&ln);
    let mut tcost: f64 = if let Some(tc) = tc {
        (0..tc.conns.len()).map(|i| conn_cost(&at, i)).sum()
    } else {
        0.0
    };
    // Normalise the two terms by their starting values (VPR's
    // timing-driven cost).
    let lambda = tc.map_or(0.0, |t| t.lambda);
    let (wl_norm, t_norm) = (1.0 / wl.max(1.0), 1.0 / tcost.max(1e-9));
    let total = |wl: f64, t: f64| (1.0 - lambda) * wl * wl_norm + lambda * t * t_norm;
    let (gx, gy) = (
        labs.iter().map(|l| l.x).max().unwrap() as i64,
        labs.iter().map(|l| l.y).max().unwrap() as i64,
    );
    let mut rlim = gx.max(gy) as f64;
    let mut moves_tried: Vec<usize> = Vec::new();
    // One move: an ALM (or its chain) to a random slot within rlim, swapping
    // with what is there. Returns the cost delta if accepted.
    let try_move = |at: &mut Vec<(usize, u8)>,
                    occ: &mut HashMap<(usize, u8), usize>,
                    uses: &mut Vec<LabUse>,
                    rng: &mut Rng,
                    temp: f64,
                    rlim: f64,
                    wl: &mut f64,
                    tcost: &mut f64,
                    ln: &mut Vec<u32>|
     -> Option<bool> {
        let a = if !p.chains.is_empty() && rng.below(8) == 0 {
            p.chains[rng.below(p.chains.len())][0]
        } else {
            *movable.get(rng.below(movable.len().max(1)))?
        };
        let r = rlim.max(1.0) as i64;
        let (l0, k0) = at[a];
        let tx = labs[l0].x as i64 + rng.below(2 * r as usize + 1) as i64 - r;
        let ty = labs[l0].y as i64 + rng.below(2 * r as usize + 1) as i64 - r;
        let &tl = lab_at.get(&(tx.clamp(0, 255) as u32, ty.clamp(0, 255) as u32))?;
        if labs[tl].mlab {
            return None;
        }
        // The ALMs that move, old and new slots.
        // (ALM, from (LAB, slot), to (LAB, slot)).
        #[allow(clippy::type_complexity)]
        let mut moved: Vec<(usize, (usize, u8), (usize, u8))> = Vec::new();
        if chain_of[a] != usize::MAX {
            let c = &p.chains[chain_of[a]];
            let k0 = 0;
            for (j, &m) in c.iter().enumerate() {
                let l =
                    *lab_at.get(&(labs[tl].x, labs[tl].y.checked_sub(((k0 + j) / 10) as u32)?))?;
                if labs[l].mlab {
                    return None;
                }
                let s = (l, ((k0 + j) % 10) as u8);
                if occ.get(&s).is_some_and(|o| !c.contains(o)) {
                    return None;
                }
                moved.push((m, at[m], s));
            }
        } else {
            let tk = rng.below(10) as u8;
            if (tl, tk) == (l0, k0) {
                return None;
            }
            moved.push((a, (l0, k0), (tl, tk)));
            if let Some(&o) = occ.get(&(tl, tk)) {
                if chain_of[o] != usize::MAX {
                    return None;
                }
                moved.push((o, (tl, tk), (l0, k0)));
            }
        }
        // LAB control lines.
        let mut trial: HashMap<usize, LabUse> = HashMap::default();
        for &(m, from, to) in &moved {
            trial
                .entry(from.0)
                .or_insert_with(|| uses[from.0].clone())
                .add(&ctls[m], &ins[m], -1);
            trial
                .entry(to.0)
                .or_insert_with(|| uses[to.0].clone())
                .add(&ctls[m], &ins[m], 1);
        }
        if !trial.values().all(LabUse::ok) {
            return None;
        }
        let mut touched: Vec<usize> = moved
            .iter()
            .flat_map(|&(m, ..)| alm_nets[m].iter().copied())
            .collect();
        touched.sort();
        touched.dedup();
        let mut ttouched: Vec<usize> = moved
            .iter()
            .flat_map(|&(m, ..)| alm_conns[m].iter().copied())
            .collect();
        ttouched.sort();
        ttouched.dedup();
        let before: f64 = touched.iter().map(|&i| net_cost(at, i)).sum();
        let tbefore: f64 = ttouched.iter().map(|&i| conn_cost(at, i)).sum();
        for &(m, _, to) in &moved {
            at[m] = to;
        }
        // LABs emptied and newly used.
        let mut dl: HashMap<usize, i32> = HashMap::default();
        for &(_, from, to) in &moved {
            *dl.entry(from.0).or_default() -= 1;
            *dl.entry(to.0).or_default() += 1;
        }
        let lab_delta: f64 = dl
            .iter()
            .map(|(&l, &d)| {
                let (o, n) = (ln[l] as i32, ln[l] as i32 + d);
                LAB_COST * ((n > 0) as i32 - (o > 0) as i32) as f64
            })
            .sum();
        let after: f64 = touched.iter().map(|&i| net_cost(at, i)).sum::<f64>() + lab_delta;
        let tafter: f64 = ttouched.iter().map(|&i| conn_cost(at, i)).sum();
        let delta = total(*wl + after - before, *tcost + tafter - tbefore) - total(*wl, *tcost);
        if delta <= 0.0 || rng.unit() < (-delta / temp).exp() {
            for &(_, from, _) in &moved {
                occ.remove(&from);
            }
            for &(m, _, to) in &moved {
                occ.insert(to, m);
            }
            for (l, u) in trial {
                uses[l] = u;
            }
            *wl += after - before;
            *tcost += tafter - tbefore;
            for (l, d) in dl {
                ln[l] = (ln[l] as i32 + d) as u32;
            }
            Some(true)
        } else {
            for &(m, from, _) in &moved {
                at[m] = from;
            }
            Some(false)
        }
    };
    // Starting temperature: 20 standard deviations of random moves' deltas.
    let mut deltas = Vec::new();
    for _ in 0..n.clamp(50, 500) {
        let before = total(wl, tcost);
        if try_move(
            &mut at,
            &mut occ,
            &mut uses,
            &mut rng,
            f64::INFINITY,
            rlim,
            &mut wl,
            &mut tcost,
            &mut ln,
        )
        .is_some()
        {
            deltas.push(total(wl, tcost) - before);
        }
    }
    let mean = deltas.iter().sum::<f64>() / deltas.len().max(1) as f64;
    let sd = (deltas.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / deltas.len().max(1) as f64)
        .sqrt();
    let mut temp = 20.0 * sd.max(1e-6);
    if centres.is_some() {
        // From a global placement: refine, don't melt it.
        temp *= std::env::var("A5PNR_GLOBAL_T")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.02);
        rlim = 6.0;
    }
    let inner = ((effort * (n as f64).powf(4.0 / 3.0)) as usize).max(200);
    let nets_n = nets.len().max(1) as f64;
    let mut rounds = 0;
    loop {
        let (mut acc, mut tried) = (0usize, 0usize);
        for _ in 0..inner {
            match try_move(
                &mut at, &mut occ, &mut uses, &mut rng, temp, rlim, &mut wl, &mut tcost, &mut ln,
            ) {
                Some(true) => {
                    acc += 1;
                    tried += 1
                }
                Some(false) => tried += 1,
                None => {}
            }
        }
        let rate = acc as f64 / tried.max(1) as f64;
        moves_tried.push(tried);
        let alpha = if rate > 0.96 {
            0.5
        } else if rate > 0.8 {
            0.9
        } else if rate > 0.15 {
            0.95
        } else {
            0.8
        };
        temp *= alpha;
        rlim = (rlim * (1.0 - 0.44 + rate)).clamp(1.0, gx.max(gy) as f64);
        rounds += 1;
        // Recompute from scratch now and then: the running sums drift.
        if rounds % 10 == 0 {
            wl = (0..nets.len()).map(|i| net_cost(&at, i)).sum::<f64>() + lab_term(&ln);
            if let Some(tc) = tc {
                tcost = (0..tc.conns.len()).map(|i| conn_cost(&at, i)).sum();
            }
        }
        if temp < 0.005 * total(wl, tcost) / nets_n || rounds > 400 {
            break;
        }
    }
    // A last greedy pass at zero temperature.
    for _ in 0..inner {
        try_move(
            &mut at, &mut occ, &mut uses, &mut rng, 1e-12, 3.0, &mut wl, &mut tcost, &mut ln,
        );
    }
    let labs_used: std::collections::BTreeSet<usize> = at.iter().map(|a| a.0).collect();
    eprintln!(
        "place: {n} ALMs ({} in {} chains) in {} LABs, wire length {:.0}, {rounds} temperatures x {inner} moves",
        p.chains.iter().map(Vec::len).sum::<usize>(),
        p.chains.len(),
        labs_used.len(),
        (0..nets.len()).map(|i| net_cost(&at, i)).sum::<f64>()
    );
    Ok(Placement {
        at,
        m20k_at,
        dsp_at,
    })
}

/// Each ALM's centre (LAB x, y) from chipsmith's electrostatic placer
/// (chipsmith_place::electro, ePlace's density model): one bin per LAB
/// site, an ALM a tenth of one, a carry chain a column of LABs one row per
/// ten ALMs; ports, M20Ks, DSPs and MLAB RAM fixed.
fn global(
    p: &Packed,
    labs: &[Lab],
    port_xy: &HashMap<usize, (u32, u32)>,
    m20k_at: &[(u32, u32)],
    dsp_at: &[(u32, u32)],
    at: &[(usize, u8)],
    in_ram: &[bool],
) -> Result<Vec<(f32, f32)>> {
    use chipsmith_place::electro;
    use chipsmith_place::{Pin, PinOf};
    let (w, h) = (
        labs.iter().map(|l| l.x).max().unwrap() as usize + 1,
        labs.iter().map(|l| l.y).max().unwrap() as usize + 1,
    );
    let mut cap = vec![0f32; w * h];
    for l in labs.iter().filter(|l| !l.mlab) {
        cap[l.y as usize * w + l.x as usize] = 1.0;
    }
    let mut prob = electro::Problem::new(w, h, vec![cap]);
    let n = p.alms.len();
    // Per ALM: its object and its row offset from the object's centre.
    let mut obj: Vec<Option<(u32, f32)>> = vec![None; n];
    let mut chained = vec![false; n];
    for c in &p.chains {
        let rows = c.len().div_ceil(10);
        let o = prob.add_object(rows as u32, &[(0, c.len() as f32 / 10.0)]);
        for (k, &a) in c.iter().enumerate() {
            // A chain runs on into the LAB one row down.
            obj[a] = Some((o, (rows as f32 - 1.0) / 2.0 - (k / 10) as f32));
            chained[a] = true;
        }
    }
    let mut fixed: HashMap<(u32, u32), u32> = HashMap::default();
    let mut fix = |prob: &mut electro::Problem, xy: (u32, u32)| {
        *fixed
            .entry(xy)
            .or_insert_with(|| prob.add_fixed((xy.0 as f32 + 0.5, xy.1 as f32 + 0.5)))
    };
    for a in 0..n {
        if !chained[a] && !in_ram[a] {
            obj[a] = Some((prob.add_object(1, &[(0, 0.1)]), 0.0));
        }
    }
    for (_, (d, sinks)) in p.nets.iter().filter(|(k, _)| !p.clocks.contains(k)) {
        let mut pins = Vec::new();
        for e in std::iter::once(d).chain(sinks.iter()) {
            let pin = match e {
                End::Out { alm, .. } | End::In { alm, .. } | End::Ctl { alm, .. } => {
                    match obj[*alm] {
                        Some((o, dy)) => Some(Pin {
                            of: PinOf::Object(o),
                            dx: 0.0,
                            dy,
                        }),
                        None => {
                            let l = labs[at[*alm].0];
                            Some(Pin {
                                of: PinOf::Fixed(fix(&mut prob, (l.x, l.y))),
                                dx: 0.0,
                                dy: 0.0,
                            })
                        }
                    }
                }
                End::Port(q) | End::Io { port: q, .. } => port_xy.get(q).map(|&xy| Pin {
                    of: PinOf::Fixed(fix(&mut prob, xy)),
                    dx: 0.0,
                    dy: 0.0,
                }),
                End::M20k { m, .. } => Some(Pin {
                    of: PinOf::Fixed(fix(&mut prob, m20k_at[*m])),
                    dx: 0.0,
                    dy: 0.0,
                }),
                End::Dsp { d, .. } => Some(Pin {
                    of: PinOf::Fixed(fix(&mut prob, dsp_at[*d])),
                    dx: 0.0,
                    dy: 0.0,
                }),
                End::Pll(_) => None,
                End::PllRst(_) => Some(Pin {
                    of: PinOf::Fixed(fix(&mut prob, PLL_RST_XY)),
                    dx: 0.0,
                    dy: 0.0,
                }),
            };
            pins.extend(pin);
        }
        if pins.len() >= 2 {
            prob.add_net(pins, 1.0);
        }
    }
    let t0 = std::time::Instant::now();
    let opts = electro::Options {
        accel: electro::Accel::Threads,
        ..Default::default()
    };
    let out =
        electro::place(&prob, &mut electro::NoTiming, None, &opts).map_err(anyhow::Error::msg)?;
    eprintln!(
        "place: global placement in {} iterations, overflow {:.3}, {:.1} s",
        out.iterations,
        out.overflow[0],
        t0.elapsed().as_secs_f64()
    );
    Ok((0..n)
        .map(|a| match obj[a] {
            Some((o, dy)) => {
                let (x, y) = out.centres[o as usize];
                (x - 0.5, y - 0.5 + dy)
            }
            None => (labs[at[a].0].x as f32, labs[at[a].0].y as f32),
        })
        .collect())
}
