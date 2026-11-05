//! Analytic global placement, independent of the device: the back ends
//! describe their design as objects, fixed points and nets over a grid of
//! bins, supply a [`Legalizer`] that snaps objects onto their sites, and
//! get back the targets of the best placement found.
//!
//! The loop is HeAP's (nextpnr) and SimPL's: a bound-to-bound quadratic
//! model of half-perimeter wirelength (Spindler, Schlichtmann, Johannes
//! 2008) solved by conjugate gradients, overfull regions of the grid
//! spread by recursive bisection, the result legalised by the device, and
//! the next solve pulled towards that legal placement by anchors whose
//! weight grows each round. Everything is deterministic: the only
//! parallelism is over rows of the matrix, each row summed in a fixed
//! order.
//!
//! Coordinates are the device's distance units (what its wirelength
//! counts): a bin is one unit square, so a device whose x advances by two
//! per logic column hands positions in halved x.

pub mod electro;
#[cfg(feature = "gpu")]
pub mod gpu;

use rayon::prelude::*;

/// A point in placement space.
pub type Pos = (f32, f32);

/// Something the placer moves: a cell, or a group of cells that keep a
/// shape (a carry chain). Its pins hang off its position by offsets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Object {
    /// Which kind of site it needs. Class 0 is the fabric the bin grid
    /// describes and is spread for density; other classes (RAM blocks)
    /// only the legaliser places.
    pub class: u32,
    /// How much of a bin's capacity it takes, in the device's units (an
    /// LE in a 16-LE LAB: 1 of 16).
    pub area: f32,
    /// How many rows it spans, downwards from its position (a carry chain
    /// of 32 LEs: 2 LABs); its area is spread over them.
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOf {
    Object(u32),
    Fixed(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pin {
    pub of: PinOf,
    /// Offset from the object's position (a chain's k-th LE sits k rows
    /// along it); zero for fixed points.
    pub dx: f32,
    pub dy: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Net {
    pub pins: Vec<Pin>,
    pub weight: f32,
}

/// The density grid for class 0: bins of one unit square at integer
/// coordinates, each with a capacity (0 where there is no site).
#[derive(Debug, Clone, PartialEq)]
pub struct Bins {
    pub width: usize,
    pub height: usize,
    /// Row-major, `height * width`.
    pub capacity: Vec<f32>,
}

impl Bins {
    fn cap(&self, x: usize, y: usize) -> f32 {
        self.capacity[y * self.width + x]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub objects: Vec<Object>,
    pub fixed: Vec<Pos>,
    pub nets: Vec<Net>,
    pub bins: Bins,
}

/// What the device does between rounds: snap every object to a legal
/// site near its target, and (if it is timing-driven) reweight the nets
/// from the legal placement.
pub trait Legalizer {
    /// Each object's legal position, in the same order as the targets.
    fn legalize(&mut self, targets: &[Pos]) -> Vec<Pos>;
    /// Called with the legal placement; may change the nets' weights for
    /// the next solve, and the objects' areas for the next spread (a
    /// device whose sites take fewer objects than their slots, because
    /// of shared control signals, asks for more room). The default keeps
    /// both.
    fn reweight(&mut self, _legal: &[Pos], _weights: &mut [f32], _areas: &mut [f32]) {}
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Rounds of solve, spread and legalise.
    pub rounds: usize,
    /// Anchor weight per round: the pull of each object towards its last
    /// legal position, relative to a unit-weight two-pin net of unit
    /// length.
    pub anchor: f32,
    /// A bin counts as overfull above this share of its capacity.
    pub target_density: f32,
    /// Stop when the legal placement's wirelength has not improved in
    /// this many rounds.
    pub patience: usize,
    /// Print each round's wirelengths to stderr.
    pub trace: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            rounds: 30,
            anchor: 0.05,
            target_density: 0.9,
            patience: 4,
            trace: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// The targets whose legalisation had the lowest wirelength; the
    /// device legalises them once more (deterministically) to get it.
    pub targets: Vec<Pos>,
    pub legal_wirelength: f64,
    pub rounds: usize,
}

/// Half-perimeter wirelength of a placement, weights included.
pub fn wirelength(p: &Problem, pos: &[Pos], weights: &[f32]) -> f64 {
    p.nets
        .iter()
        .zip(weights)
        .map(|(net, &w)| {
            let (mut lx, mut hx, mut ly, mut hy) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
            for pin in &net.pins {
                let (x, y) = pin_pos(p, pos, pin);
                lx = lx.min(x);
                hx = hx.max(x);
                ly = ly.min(y);
                hy = hy.max(y);
            }
            (w * ((hx - lx) + (hy - ly))) as f64
        })
        .sum()
}

fn pin_pos(p: &Problem, pos: &[Pos], pin: &Pin) -> Pos {
    match pin.of {
        PinOf::Object(o) => {
            let (x, y) = pos[o as usize];
            (x + pin.dx, y + pin.dy)
        }
        PinOf::Fixed(f) => p.fixed[f as usize],
    }
}

/// Global placement: returns the best round's targets.
pub fn place(p: &Problem, legalizer: &mut impl Legalizer, opts: Options) -> Outcome {
    let n = p.objects.len();
    let mut weights: Vec<f32> = p.nets.iter().map(|n| n.weight).collect();
    let mut areas: Vec<f32> = p.objects.iter().map(|o| o.area).collect();
    let ones = vec![1.0f32; p.nets.len()];
    // Start everything at the fixed pins' centre, so the first solve is
    // the pure quadratic optimum; objects on no net with a fixed point
    // then sit at the centre of the grid.
    let centre = fixed_centre(p);
    let mut pos: Vec<Pos> = vec![centre; n];
    let mut legal: Option<Vec<Pos>> = None;
    let mut best: Option<Outcome> = None;
    let mut since_best = 0;
    // The CG solves' share of the time, for the GPU question
    // (experiments/gpu-bench).
    let mut solving = std::time::Duration::ZERO;
    for round in 0..opts.rounds {
        let anchor = legal
            .as_ref()
            .map(|l| (l.as_slice(), opts.anchor * round as f32));
        let started = std::time::Instant::now();
        pos = solve(p, &weights, &pos, anchor);
        solving += started.elapsed();
        let solved = opts.trace.then(|| wirelength(p, &pos, &weights));
        if opts.trace {
            eprintln!("place: solves so far {:.2} s", solving.as_secs_f64());
        }
        spread(p, &areas, &mut pos, opts.target_density);
        let spread_wl = opts.trace.then(|| wirelength(p, &pos, &weights));
        if opts.trace {
            let (w, h) = (p.bins.width, p.bins.height);
            let mut demand = vec![0.0f32; w * h];
            for (o, &(x, y)) in pos.iter().enumerate() {
                if p.objects[o].class == 0 {
                    let (bx, by) = (
                        (x.max(0.0) as usize).min(w - 1),
                        (y.max(0.0) as usize).min(h - 1),
                    );
                    let height = p.objects[o].height.max(1) as usize;
                    for i in 0..height {
                        demand[by.saturating_sub(i) * w + bx] += areas[o] / height as f32;
                    }
                }
            }
            let over = demand
                .iter()
                .zip(&p.bins.capacity)
                .filter(|(d, c)| **d > **c * opts.target_density)
                .count();
            let worst = demand
                .iter()
                .zip(&p.bins.capacity)
                .map(|(d, c)| d / c.max(1e-3))
                .fold(0.0f32, f32::max);
            let (xs, ys): (Vec<f32>, Vec<f32>) = pos.iter().copied().unzip();
            eprintln!(
                "place: after spread: {over} overfull bins, worst {worst:.2} of capacity; x {:.1}..{:.1} y {:.1}..{:.1} of {w}x{h}",
                xs.iter().copied().fold(f32::MAX, f32::min), xs.iter().copied().fold(f32::MIN, f32::max),
                ys.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MIN, f32::max)
            );
        }
        let l = legalizer.legalize(&pos);
        // Judged unweighted: the weights change between rounds.
        let wl = wirelength(p, &l, &ones);
        if let (Some(solved), Some(spread_wl)) = (solved, spread_wl) {
            eprintln!(
                "place: round {}: solved {solved:.0}, spread {spread_wl:.0}, legal {:.0} (weighted), {wl:.0} unweighted",
                round + 1,
                wirelength(p, &l, &weights)
            );
        }
        legalizer.reweight(&l, &mut weights, &mut areas);
        if best.as_ref().is_none_or(|b| wl < b.legal_wirelength) {
            best = Some(Outcome {
                targets: pos.clone(),
                legal_wirelength: wl,
                rounds: round + 1,
            });
            since_best = 0;
        } else {
            since_best += 1;
            if since_best >= opts.patience {
                break;
            }
        }
        legal = Some(l);
    }
    best.unwrap_or(Outcome {
        targets: pos,
        legal_wirelength: 0.0,
        rounds: 0,
    })
}

fn fixed_centre(p: &Problem) -> Pos {
    if p.fixed.is_empty() {
        return (p.bins.width as f32 / 2.0, p.bins.height as f32 / 2.0);
    }
    let (sx, sy) = p.fixed.iter().fold((0.0f64, 0.0f64), |(a, b), &(x, y)| {
        (a + x as f64, b + y as f64)
    });
    (
        (sx / p.fixed.len() as f64) as f32,
        (sy / p.fixed.len() as f64) as f32,
    )
}

/// Pins closer than this count as a unit apart in the bound-to-bound
/// weights, so coincident pins don't blow up.
const MIN_DISTANCE: f32 = 1.0;

/// One axis' sparse system: `diag[i] x_i - sum_j off[i][j] x_j = rhs[i]`,
/// off-diagonal terms in CSR.
struct System {
    diag: Vec<f32>,
    rhs: Vec<f32>,
    row_start: Vec<usize>,
    col: Vec<u32>,
    val: Vec<f32>,
}

impl System {
    fn build(n: usize, mut terms: Vec<(u32, u32, f32)>, diag: Vec<f32>, rhs: Vec<f32>) -> Self {
        // Rows in order, columns in order within a row: the sum of each row
        // is then the same whatever thread computes it.
        terms.par_sort_unstable_by_key(|&(i, j, _)| (i, j));
        let mut row_start = vec![0; n + 1];
        for &(i, _, _) in &terms {
            row_start[i as usize + 1] += 1;
        }
        for i in 0..n {
            row_start[i + 1] += row_start[i];
        }
        let col = terms.iter().map(|&(_, j, _)| j).collect();
        let val = terms.iter().map(|&(_, _, v)| v).collect();
        System {
            diag,
            rhs,
            row_start,
            col,
            val,
        }
    }

    fn mul(&self, x: &[f32], y: &mut [f32]) {
        y.par_iter_mut().enumerate().for_each(|(i, yi)| {
            let mut acc = self.diag[i] * x[i];
            for k in self.row_start[i]..self.row_start[i + 1] {
                acc -= self.val[k] * x[self.col[k] as usize];
            }
            *yi = acc;
        });
    }

    /// Jacobi-preconditioned conjugate gradients from `x`.
    fn solve(&self, x: &mut [f32]) {
        let n = x.len();
        let mut r = vec![0.0; n];
        let mut ax = vec![0.0; n];
        self.mul(x, &mut ax);
        for i in 0..n {
            r[i] = self.rhs[i] - ax[i];
        }
        let inv_diag: Vec<f32> = self
            .diag
            .iter()
            .map(|&d| if d > 0.0 { 1.0 / d } else { 0.0 })
            .collect();
        let mut z: Vec<f32> = r.iter().zip(&inv_diag).map(|(r, d)| r * d).collect();
        let mut p = z.clone();
        let mut rz: f64 = dot(&r, &z);
        let r0: f64 = dot(&r, &r);
        if r0 == 0.0 {
            return;
        }
        let mut q = vec![0.0; n];
        for _ in 0..CG_MAX_ITERATIONS {
            self.mul(&p, &mut q);
            let pq = dot(&p, &q);
            if pq <= 0.0 {
                break;
            }
            let alpha = (rz / pq) as f32;
            for i in 0..n {
                x[i] += alpha * p[i];
                r[i] -= alpha * q[i];
            }
            if dot(&r, &r) < CG_TOLERANCE * CG_TOLERANCE * r0 {
                break;
            }
            for i in 0..n {
                z[i] = r[i] * inv_diag[i];
            }
            let rz_new = dot(&r, &z);
            let beta = (rz_new / rz) as f32;
            rz = rz_new;
            for i in 0..n {
                p[i] = z[i] + beta * p[i];
            }
        }
    }
}

const CG_MAX_ITERATIONS: usize = 300;
/// Relative residual to stop at; positions are wanted to a fraction of a
/// bin, not more.
const CG_TOLERANCE: f64 = 1e-4;

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum()
}

/// Solve both axes with bound-to-bound weights linearised at `at`, pulled
/// towards `anchor` positions by the given weight.
fn solve(p: &Problem, weights: &[f32], at: &[Pos], anchor: Option<(&[Pos], f32)>) -> Vec<Pos> {
    let n = p.objects.len();
    let axes: Vec<Vec<f32>> = [0usize, 1]
        .into_par_iter()
        .map(|axis| {
            let coord = |pos: Pos| if axis == 0 { pos.0 } else { pos.1 };
            let offset = |pin: &Pin| if axis == 0 { pin.dx } else { pin.dy };
            let mut terms: Vec<(u32, u32, f32)> = Vec::new();
            let mut diag = vec![0.0f32; n];
            let mut rhs = vec![0.0f32; n];
            // A spring of weight `w` between two pins; a pin is an object
            // plus an offset or a fixed coordinate.
            let mut spring = |a: &Pin, b: &Pin, w: f32| match (a.of, b.of) {
                (PinOf::Object(i), PinOf::Object(j)) => {
                    if i == j {
                        return;
                    }
                    let (oi, oj) = (offset(a), offset(b));
                    diag[i as usize] += w;
                    diag[j as usize] += w;
                    terms.push((i, j, w));
                    terms.push((j, i, w));
                    rhs[i as usize] += w * (oj - oi);
                    rhs[j as usize] += w * (oi - oj);
                }
                (PinOf::Object(i), PinOf::Fixed(f)) | (PinOf::Fixed(f), PinOf::Object(i)) => {
                    let (obj, pin) = if matches!(a.of, PinOf::Object(_)) {
                        (i, a)
                    } else {
                        (i, b)
                    };
                    diag[obj as usize] += w;
                    rhs[obj as usize] += w * (coord(p.fixed[f as usize]) - offset(pin));
                }
                (PinOf::Fixed(_), PinOf::Fixed(_)) => {}
            };
            for (net, &w) in p.nets.iter().zip(weights) {
                let k = net.pins.len();
                if k < 2 || w <= 0.0 {
                    continue;
                }
                let c = |pin: &Pin| coord(pin_pos(p, at, pin));
                if k == 2 {
                    let d = (c(&net.pins[0]) - c(&net.pins[1])).abs().max(MIN_DISTANCE);
                    spring(&net.pins[0], &net.pins[1], w / d);
                    continue;
                }
                let (mut lo, mut hi) = (0, 0);
                for (i, pin) in net.pins.iter().enumerate() {
                    if c(pin) < c(&net.pins[lo]) {
                        lo = i;
                    }
                    if c(pin) > c(&net.pins[hi]) {
                        hi = i;
                    }
                }
                if lo == hi {
                    hi = if lo == 0 { 1 } else { 0 };
                }
                let scale = w * 2.0 / (k - 1) as f32;
                let (plo, phi) = (&net.pins[lo], &net.pins[hi]);
                spring(plo, phi, scale / (c(phi) - c(plo)).abs().max(MIN_DISTANCE));
                for (i, pin) in net.pins.iter().enumerate() {
                    if i == lo || i == hi {
                        continue;
                    }
                    spring(pin, plo, scale / (c(pin) - c(plo)).abs().max(MIN_DISTANCE));
                    spring(pin, phi, scale / (c(pin) - c(phi)).abs().max(MIN_DISTANCE));
                }
            }
            if let Some((legal, w)) = anchor {
                for i in 0..n {
                    // Bound-to-bound too: a linear pull towards the legal spot.
                    let d = (coord(at[i]) - coord(legal[i])).abs().max(MIN_DISTANCE);
                    diag[i] += w / d;
                    rhs[i] += w / d * coord(legal[i]);
                }
            }
            let system = System::build(n, terms, diag, rhs);
            let mut x: Vec<f32> = at.iter().map(|&pos| coord(pos)).collect();
            system.solve(&mut x);
            x
        })
        .collect();
    axes[0]
        .iter()
        .zip(&axes[1])
        .map(|(&x, &y)| (x, y))
        .collect()
}

/// A rectangle of bins, half-open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl Region {
    fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
    fn overlaps(&self, o: &Region) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }
    fn union(&self, o: &Region) -> Region {
        Region {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }
}

/// Spread class-0 objects out of overfull bins: each overfull bin grows
/// into a region with room for what is in it (merging with regions it
/// runs into), and each region is bisected recursively, the objects on
/// each side stretched linearly into their half, until the pieces are
/// single bins. Other classes are left to the legaliser.
fn spread(p: &Problem, areas: &[f32], pos: &mut [Pos], target_density: f32) {
    let bins = &p.bins;
    let (w, h) = (bins.width, bins.height);
    let bin_of = |pos: Pos| -> (usize, usize) {
        (
            (pos.0.max(0.0) as usize).min(w - 1),
            (pos.1.max(0.0) as usize).min(h - 1),
        )
    };
    let mut demand = vec![0.0f32; w * h];
    let fabric: Vec<u32> = (0..p.objects.len() as u32)
        .filter(|&o| p.objects[o as usize].class == 0)
        .collect();
    for &o in &fabric {
        let (x, y) = bin_of(pos[o as usize]);
        demand[y * w + x] += areas[o as usize];
    }
    let cap = |r: &Region| -> f32 {
        (r.y0..r.y1)
            .flat_map(|y| (r.x0..r.x1).map(move |x| (x, y)))
            .map(|(x, y)| bins.cap(x, y) * target_density)
            .sum()
    };
    let dem = |r: &Region| -> f32 {
        (r.y0..r.y1)
            .flat_map(|y| (r.x0..r.x1).map(move |x| (x, y)))
            .map(|(x, y)| demand[y * w + x])
            .sum()
    };

    // Overfull bins, most overfull first, each grown into a region.
    let mut overfull: Vec<(usize, usize)> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .filter(|&(x, y)| demand[y * w + x] > bins.cap(x, y) * target_density)
        .collect();
    overfull.sort_by(|a, b| {
        let excess = |&(x, y): &(usize, usize)| demand[y * w + x] - bins.cap(x, y) * target_density;
        excess(b).total_cmp(&excess(a)).then(a.cmp(b))
    });
    let mut regions: Vec<Region> = Vec::new();
    for (x, y) in overfull {
        if regions.iter().any(|r| r.contains(x, y)) {
            continue;
        }
        let mut r = Region {
            x0: x,
            y0: y,
            x1: x + 1,
            y1: y + 1,
        };
        let mut step = 0;
        loop {
            // Absorb regions it overlaps.
            let mut i = 0;
            while i < regions.len() {
                if regions[i].overlaps(&r) {
                    r = r.union(&regions.swap_remove(i));
                } else {
                    i += 1;
                }
            }
            if dem(&r) <= cap(&r) || (r.x0 == 0 && r.y0 == 0 && r.x1 == w && r.y1 == h) {
                break;
            }
            // Grow a ring, alternating axes, within the grid.
            let grown = match step % 4 {
                0 if r.x1 < w => Region { x1: r.x1 + 1, ..r },
                1 if r.y1 < h => Region { y1: r.y1 + 1, ..r },
                2 if r.x0 > 0 => Region { x0: r.x0 - 1, ..r },
                3 if r.y0 > 0 => Region { y0: r.y0 - 1, ..r },
                _ => r,
            };
            step += 1;
            r = grown;
        }
        regions.push(r);
    }

    for r in regions {
        let mut members: Vec<u32> = fabric
            .iter()
            .copied()
            .filter(|&o| {
                let (x, y) = bin_of(pos[o as usize]);
                r.contains(x, y)
            })
            .collect();
        bisect(p, areas, pos, &mut members, r, target_density);
    }
}

fn bisect(
    p: &Problem,
    areas: &[f32],
    pos: &mut [Pos],
    members: &mut [u32],
    r: Region,
    target_density: f32,
) {
    let bins = &p.bins;
    let (bw, bh) = (r.x1 - r.x0, r.y1 - r.y0);
    if members.is_empty() {
        return;
    }
    if bw == 1 && bh == 1 {
        // Into the bin, keeping each object's place within it.
        for &o in members.iter() {
            let (x, y) = &mut pos[o as usize];
            *x = x.clamp(r.x0 as f32, r.x1 as f32 - 1e-3);
            *y = y.clamp(r.y0 as f32, r.y1 as f32 - 1e-3);
        }
        return;
    }
    // Cut across the longer side (one with more than one bin).
    let along_x = if bh == 1 {
        true
    } else if bw == 1 {
        false
    } else {
        bw >= bh
    };
    let coord = |pos: Pos| if along_x { pos.0 } else { pos.1 };
    members.sort_by(|&a, &b| {
        coord(pos[a as usize])
            .total_cmp(&coord(pos[b as usize]))
            .then(a.cmp(&b))
    });
    // Capacity of each slice of bins along the axis, and the split that
    // leaves the two sides equally full.
    let slices = if along_x { bw } else { bh };
    let slice_cap: Vec<f32> = (0..slices)
        .map(|s| {
            let cells: Box<dyn Iterator<Item = (usize, usize)>> = if along_x {
                Box::new((r.y0..r.y1).map(move |y| (r.x0 + s, y)))
            } else {
                Box::new((r.x0..r.x1).map(move |x| (x, r.y0 + s)))
            };
            cells.map(|(x, y)| bins.cap(x, y) * target_density).sum()
        })
        .collect();
    let total_cap: f32 = slice_cap.iter().sum();
    let total_area: f32 = members.iter().map(|&o| areas[o as usize]).sum();
    // Split the bins where the left capacity is nearest half.
    let mut left_cap = 0.0;
    let mut split = 1;
    let mut best_gap = f32::MAX;
    let mut acc = 0.0;
    for s in 1..slices {
        acc += slice_cap[s - 1];
        let gap = (acc - total_cap / 2.0).abs();
        if gap < best_gap {
            best_gap = gap;
            split = s;
            left_cap = acc;
        }
    }
    if total_cap <= 0.0 {
        return;
    }
    // Objects left of the cut take the left side's share of the area,
    // whole objects, keeping sort order.
    let want_left = total_area * left_cap / total_cap;
    let mut k = 0;
    let mut area = 0.0;
    while k < members.len() && area + areas[members[k] as usize] / 2.0 <= want_left {
        area += areas[members[k] as usize];
        k += 1;
    }
    // A side with capacity must not be left empty-handed while the other
    // overflows; a side without capacity gets nothing.
    if left_cap <= 0.0 {
        k = 0;
    } else if left_cap >= total_cap {
        k = members.len();
    }
    let (left, right) = if along_x {
        (
            Region {
                x1: r.x0 + split,
                ..r
            },
            Region {
                x0: r.x0 + split,
                ..r
            },
        )
    } else {
        (
            Region {
                y1: r.y0 + split,
                ..r
            },
            Region {
                y0: r.y0 + split,
                ..r
            },
        )
    };
    let (lm, rm) = members.split_at_mut(k);
    stretch(pos, lm, &left, along_x);
    stretch(pos, rm, &right, along_x);
    bisect(p, areas, pos, lm, left, target_density);
    bisect(p, areas, pos, rm, right, target_density);
}

/// Map the sorted objects' coordinates linearly onto the region's span,
/// so their order is kept and they fill it.
fn stretch(pos: &mut [Pos], members: &[u32], r: &Region, along_x: bool) {
    if members.is_empty() {
        return;
    }
    let (lo, hi) = if along_x {
        (r.x0 as f32, r.x1 as f32)
    } else {
        (r.y0 as f32, r.y1 as f32)
    };
    let coord = |pos: Pos| if along_x { pos.0 } else { pos.1 };
    let (cmin, cmax) = (
        coord(pos[members[0] as usize]),
        coord(pos[members[members.len() - 1] as usize]),
    );
    let span = (cmax - cmin).max(1e-6);
    // Each object lands by its rank as much as by its coordinate, so a
    // cluster at one point still spreads out.
    for (rank, &o) in members.iter().enumerate() {
        let by_coord = (coord(pos[o as usize]) - cmin) / span;
        let by_rank = (rank as f32 + 0.5) / members.len() as f32;
        let t = 0.5 * (by_coord + by_rank);
        let v = lo + t * (hi - lo);
        let v = v.clamp(lo, hi - 1e-3);
        if along_x {
            pos[o as usize].0 = v;
        } else {
            pos[o as usize].1 = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Snaps to integer bins with capacity 1, first come first served.
    struct Grid {
        bins: Bins,
    }

    impl Legalizer for Grid {
        fn legalize(&mut self, targets: &[Pos]) -> Vec<Pos> {
            let mut used = vec![false; self.bins.capacity.len()];
            targets
                .iter()
                .map(|&(x, y)| {
                    let mut best = None;
                    for by in 0..self.bins.height {
                        for bx in 0..self.bins.width {
                            let i = by * self.bins.width + bx;
                            if used[i] || self.bins.capacity[i] == 0.0 {
                                continue;
                            }
                            let d = (bx as f32 - x).abs() + (by as f32 - y).abs();
                            if best.is_none_or(|(bd, _)| d < bd) {
                                best = Some((d, i));
                            }
                        }
                    }
                    let (_, i) = best.expect("room");
                    used[i] = true;
                    ((i % self.bins.width) as f32, (i / self.bins.width) as f32)
                })
                .collect()
        }
    }

    fn two_pin(a: PinOf, b: PinOf) -> Net {
        let pin = |of| Pin {
            of,
            dx: 0.0,
            dy: 0.0,
        };
        Net {
            pins: vec![pin(a), pin(b)],
            weight: 1.0,
        }
    }

    #[test]
    fn a_chain_of_cells_between_two_pins_spreads_along_the_line() {
        // Fixed at (0, 2) and (9, 2); eight cells in a chain between them
        // on a 10x5 grid of unit bins.
        let n = 8;
        let objects = vec![
            Object {
                class: 0,
                area: 1.0,
                height: 1
            };
            n
        ];
        let mut nets = vec![two_pin(PinOf::Fixed(0), PinOf::Object(0))];
        for i in 0..n - 1 {
            nets.push(two_pin(
                PinOf::Object(i as u32),
                PinOf::Object(i as u32 + 1),
            ));
        }
        nets.push(two_pin(PinOf::Object(n as u32 - 1), PinOf::Fixed(1)));
        let bins = Bins {
            width: 10,
            height: 5,
            capacity: vec![1.0; 50],
        };
        let p = Problem {
            objects,
            fixed: vec![(0.0, 2.0), (9.0, 2.0)],
            nets,
            bins: bins.clone(),
        };
        let out = place(&p, &mut Grid { bins }, Options::default());
        // In order along x, within the grid, near row 2.
        let xs: Vec<f32> = out.targets.iter().map(|p| p.0).collect();
        assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
        assert!(out
            .targets
            .iter()
            .all(|&(x, y)| (0.0..10.0).contains(&x) && (0.0..5.0).contains(&y)));
        assert!(
            out.targets.iter().all(|&(_, y)| (y - 2.0).abs() < 1.5),
            "{:?}",
            out.targets
        );
    }

    #[test]
    fn overfull_bins_are_spread_to_capacity() {
        // 20 unconnected-but-anchored cells all pulled to one corner pin;
        // the grid holds one per bin at 90%, so they must spread out.
        let n = 20;
        let objects = vec![
            Object {
                class: 0,
                area: 1.0,
                height: 1
            };
            n
        ];
        let nets: Vec<Net> = (0..n)
            .map(|i| two_pin(PinOf::Fixed(0), PinOf::Object(i as u32)))
            .collect();
        let bins = Bins {
            width: 6,
            height: 6,
            capacity: vec![1.0; 36],
        };
        let p = Problem {
            objects,
            fixed: vec![(0.0, 0.0)],
            nets,
            bins: bins.clone(),
        };
        let out = place(&p, &mut Grid { bins }, Options::default());
        let mut occupied = std::collections::HashSet::new();
        for &(x, y) in &out.targets {
            occupied.insert((x as i32, y as i32));
        }
        assert!(occupied.len() >= n - 2, "{:?}", out.targets);
    }

    #[test]
    fn the_same_problem_places_the_same_way() {
        let n = 30;
        let objects = vec![
            Object {
                class: 0,
                area: 1.0,
                height: 1
            };
            n
        ];
        let nets: Vec<Net> = (0..n)
            .map(|i| {
                two_pin(
                    PinOf::Object(i as u32),
                    PinOf::Object(((i * 7 + 3) % n) as u32),
                )
            })
            .chain([two_pin(PinOf::Fixed(0), PinOf::Object(0))])
            .collect();
        let bins = Bins {
            width: 8,
            height: 8,
            capacity: vec![1.0; 64],
        };
        let p = Problem {
            objects,
            fixed: vec![(7.0, 7.0)],
            nets,
            bins: bins.clone(),
        };
        let a = place(&p, &mut Grid { bins: bins.clone() }, Options::default());
        let b = place(&p, &mut Grid { bins }, Options::default());
        assert_eq!(a, b);
    }
}
