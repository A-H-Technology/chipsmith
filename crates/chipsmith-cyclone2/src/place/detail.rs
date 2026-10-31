//! Detailed placement in rounds of independent moves (docs/gpu-pnr.md,
//! "Detailed placement"): every LE proposes a move towards the middle of
//! its nets, into a free slot or swapping with an LE there, and each
//! proposal is scored against the placement as it stands: the change in
//! timing-weighted wirelength, and whether both LABs stay legal. Then the
//! best proposals are taken greedily, each only if it touches no LAB a
//! proposal taken this round touches, changes the box of no net one of
//! them touches, and touches no net whose box one of them changed. Moves
//! that leave a net's box alone can share it: together they can only
//! shrink it further, so a round gains at least what its scores add up
//! to. (Sharing nothing, one LE on a reset net blocked all the others.)
//!
//! Proposing and scoring read the placement and nothing else, so they run
//! on rayon's threads (or one, bit-identical), and the whole round is
//! deterministic: taking is in a fixed order.

use rayon::prelude::*;

use super::*;

/// One proposal: `le` to slot `k` of `lab`, swapping with `other` if that
/// slot is taken; `delta` is the change in weighted wirelength.
#[derive(Debug, Clone)]
pub(super) struct Proposal {
    pub le: usize,
    pub lab: usize,
    pub k: usize,
    pub other: Option<usize>,
    pub delta: f64,
    /// Every net either LE is on, sorted.
    pub nets: Vec<usize>,
    /// Which of `nets` the move changes the box of.
    pub changed: Vec<bool>,
}

/// How many LABs nearest an LE's target it tries, and how many of each
/// LAB's occupants it tries swapping with when there is no free slot.
const TARGET_LABS: usize = 3;
const SWAPS: usize = 3;

/// Rounds stop when a round gains less than this share of the cost.
const MIN_GAIN: f64 = 1e-5;
const MAX_ROUNDS: usize = 300;

/// splitmix64, for picking occupants to try: a function of the round and
/// the LE, so the rounds are reproducible.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

impl State<'_> {
    /// The cells of `le` and of `other`, with where they would go.
    fn moved_cells(&self, le: usize, lab: usize, other: Option<usize>) -> [(Option<usize>, Xy); 4] {
        let d = self.design;
        let (from, _) = self.pos[le];
        let (to_xy, from_xy) = (self.grid.xy[lab], self.grid.xy[from]);
        let Le { lut, ff } = d.les[le];
        let (olut, off) = other.map_or((None, None), |o| (d.les[o].lut, d.les[o].ff));
        [(lut, to_xy), (ff, to_xy), (olut, from_xy), (off, from_xy)]
    }

    /// Score `le` to slot `k` of `lab` (swapping with its occupant), or
    /// None if a LAB would refuse it.
    pub(super) fn score(&self, le: usize, lab: usize, k: usize) -> Option<Proposal> {
        let d = self.design;
        let (from, fk) = self.pos[le];
        if lab == from {
            return None;
        }
        let other = self.slots[lab][k];
        if other.is_some_and(|o| d.chain_of[o].is_some()) {
            return None;
        }
        let mut src = self.slots[from];
        let mut dst = self.slots[lab];
        src[fk] = other;
        dst[k] = Some(le);
        let lab_of = |x: usize| {
            if x == le {
                lab
            } else if Some(x) == other {
                from
            } else {
                self.pos[x].0
            }
        };
        if self.lab_check_of(lab, &dst, lab_of).is_err() || self.lab_check_of(from, &src, lab_of).is_err() {
            return None;
        }
        let mut nets: Vec<usize> = d.nets_of_le[le].iter().map(|&(n, _)| n).collect();
        if let Some(o) = other {
            nets.extend(d.nets_of_le[o].iter().map(|&(n, _)| n));
        }
        nets.sort_unstable();
        nets.dedup();
        let cells = self.moved_cells(le, lab, other);
        let xy = |c: usize| {
            cells
                .iter()
                .find(|(cell, _)| *cell == Some(c))
                .map_or(self.xy[c], |&(_, xy)| xy)
        };
        let mut delta = 0.0f64;
        let mut changed = Vec::with_capacity(nets.len());
        for &n in &nets {
            let mut b = self.bbox[n];
            let mut exact = true;
            for &(cell, to) in &cells {
                let Some(c) = cell else { continue };
                // A cell on the net as many times as the net lists it.
                for _ in d.nets[n].iter().filter(|&&m| m == c) {
                    exact &= b.moved(self.xy[c], to);
                }
            }
            if !exact {
                b = Bbox::of(d.nets[n].iter().map(|&c| xy(c)));
            }
            let old = &self.bbox[n];
            changed.push((b.lx, b.hx, b.ly, b.hy) != (old.lx, old.hx, old.ly, old.hy));
            delta += (self.weight[n] * b.hpwl() as f32) as f64 - self.net_cost[n] as f64;
        }
        Some(Proposal {
            le,
            lab,
            k,
            other,
            delta,
            nets,
            changed,
        })
    }

    /// The best move `le` has towards the middle of its nets, if any gains.
    fn propose(&self, le: usize, round: usize) -> Option<Proposal> {
        let d = self.design;
        if d.chain_of[le].is_some() || self.pos[le].0 == usize::MAX {
            return None;
        }
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for &(n, _) in &d.nets_of_le[le] {
            let b = &self.bbox[n];
            xs.extend([b.lx, b.hx]);
            ys.extend([b.ly, b.hy]);
        }
        if xs.is_empty() {
            return None;
        }
        xs.sort_unstable();
        ys.sort_unstable();
        let (tx, ty) = (xs[xs.len() / 2], ys[ys.len() / 2]);
        // The LABs nearest the target, nearest first.
        let mut labs: Vec<(i32, usize)> = Vec::new();
        'ring: for r in 0..6i32 {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dy.abs()) != r {
                        continue;
                    }
                    if let Some(lab) = self.grid.lab_at((tx + 2 * dx, ty + dy)) {
                        labs.push((dx.abs() + dy.abs(), lab));
                    }
                }
            }
            if labs.len() >= TARGET_LABS {
                break 'ring;
            }
        }
        labs.sort_unstable();
        let mut best: Option<Proposal> = None;
        let mut consider = |p: Option<Proposal>| {
            if let Some(p) = p {
                if p.delta < 0.0 && best.as_ref().is_none_or(|b| p.delta < b.delta) {
                    best = Some(p);
                }
            }
        };
        for &(_, lab) in labs.iter().take(TARGET_LABS) {
            if let Some(k) = (0..LES_PER_LAB).find(|&k| self.slots[lab][k].is_none()) {
                consider(self.score(le, lab, k));
            }
            let h = mix((round as u64) << 32 ^ le as u64 ^ (lab as u64) << 48);
            for i in 0..SWAPS {
                let k = ((h >> (8 * i)) % LES_PER_LAB as u64) as usize;
                if self.slots[lab][k].is_some() {
                    consider(self.score(le, lab, k));
                }
            }
        }
        best
    }

    /// Make `p`, keeping boxes and costs current.
    fn apply(&mut self, p: &Proposal) {
        let (from, fk) = self.pos[p.le];
        self.slots[from][fk] = None;
        if let Some(o) = p.other {
            self.put(o, from, fk);
        }
        self.put(p.le, p.lab, p.k);
        for &n in &p.nets {
            self.bbox[n] = self.compute_bbox(n);
            self.net_cost[n] = self.cost(n);
        }
    }
}

/// What a detailed placement did.
pub(super) struct Done {
    pub rounds: usize,
    pub moves: u64,
    /// Seconds proposing and scoring, and taking and applying.
    pub score_s: f64,
    pub take_s: f64,
}

/// Rounds of independent moves until a round gains little, the nets
/// reweighted for timing before each.
pub(super) fn refine(s: &mut State, par: bool, trace: bool) -> Done {
    let d = s.design;
    // Who proposes: everyone at first; after that the LEs whose proposal
    // lost to a clash, and those on a net a taken move changed (a later
    // round re-proposing everyone found little more for several times the
    // scoring).
    let mut active: Vec<usize> = (0..d.les.len()).filter(|&le| d.chain_of[le].is_none()).collect();
    let mut wake = vec![0u32; d.les.len()];
    let mut lab_stamp = vec![0u32; s.slots.len()];
    // Per net, the round in which a taken move touched it, and changed
    // its box.
    let mut net_touched = vec![0u32; d.nets.len()];
    let mut net_changed = vec![0u32; d.nets.len()];
    let (mut moves, mut score_s, mut take_s) = (0u64, 0.0, 0.0);
    let mut rounds = 0;
    for round in 1..=MAX_ROUNDS {
        rounds = round;
        let critical = s.reweight();
        let t = std::time::Instant::now();
        let state = &*s;
        let mut props: Vec<Proposal> = if par {
            active.par_iter().filter_map(|&le| state.propose(le, round)).collect()
        } else {
            active.iter().filter_map(|&le| state.propose(le, round)).collect()
        };
        score_s += t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        props.sort_by(|a, b| a.delta.total_cmp(&b.delta).then(a.le.cmp(&b.le)));
        let stamp = round as u32;
        let mut gain = 0.0;
        let mut taken = 0;
        for p in &props {
            let (from, _) = s.pos[p.le];
            let clash = p.nets.iter().zip(&p.changed).any(|(&n, &c)| {
                net_changed[n] == stamp || (c && net_touched[n] == stamp)
            });
            if lab_stamp[from] == stamp || lab_stamp[p.lab] == stamp || clash {
                wake[p.le] = stamp;
                continue;
            }
            lab_stamp[from] = stamp;
            lab_stamp[p.lab] = stamp;
            for (&n, &c) in p.nets.iter().zip(&p.changed) {
                net_touched[n] = stamp;
                if c {
                    net_changed[n] = stamp;
                }
            }
            s.apply(p);
            for (&n, &c) in p.nets.iter().zip(&p.changed) {
                if c {
                    for &cell in &d.nets[n] {
                        if let Some(le) = d.le_of[cell] {
                            wake[le] = stamp;
                        }
                    }
                }
            }
            gain -= p.delta;
            taken += 1;
        }
        moves += taken;
        take_s += t.elapsed().as_secs_f64();
        let total = s.total();
        if trace {
            eprintln!(
                "place: detail round {round}: {} proposals, {taken} taken, gain {gain:.1} of {total:.0}, critical {:.2} ns, wirelength {}",
                props.len(),
                critical / 1000.0,
                s.wirelength()
            );
        }
        active = (0..d.les.len())
            .filter(|&le| wake[le] == stamp && d.chain_of[le].is_none())
            .collect();
        if taken == 0 || gain < MIN_GAIN * total {
            break;
        }
    }
    Done {
        rounds,
        moves,
        score_s,
        take_s,
    }
}
