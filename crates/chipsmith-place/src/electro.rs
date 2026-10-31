//! Electrostatic global placement of every small object (an LE, not a
//! cluster of them): ePlace's density model (Lu et al., TODAES 2015) with
//! one density field per resource, as elfPlace does for FPGAs (Li et al.,
//! ICCAD 2019), so "a LAB takes one set of register controls" is a
//! density like "a LAB takes 16 LEs".
//!
//! The objective is the weighted-average smooth wirelength of the nets
//! plus, per resource, lambda times the electrostatic energy of its
//! density, minimised by Nesterov's method with ePlace's Lipschitz step.
//! Every object is a charge spread over a footprint one bin wide and as
//! many rows tall as it spans; each bin's free capacity is filled by
//! fillers (objects on no net), and bins without the resource hold a
//! fixed charge, so the field pushes objects out of overfull bins and off
//! sites they can't use. The field comes from Poisson's equation, solved
//! by a cosine transform over the bin grid.
//!
//! The arithmetic runs on an [`Engine`]: serial, on rayon's threads, or
//! (feature `gpu`) on a GPU. The serial and threaded engines are the same
//! code and give bit-identical results: every item is computed in a fixed
//! order, sums over many items are taken in fixed-size chunks then the
//! chunks in order, and density is accumulated in fixed point.

use std::sync::atomic::{AtomicI64, Ordering};

use rayon::prelude::*;

use crate::{Pin, PinOf, Pos};

/// A placement problem: a grid of unit bins, resources with a capacity in
/// each bin, objects that demand them, fixed points, and nets. Built by
/// [`Problem::new`] and the `add_*` methods, which check every index, so
/// a `Problem` is always consistent.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    width: usize,
    height: usize,
    /// Per resource, per bin (row-major): the share of the bin the resource
    /// fills, 0 to 1.
    capacity: Vec<Vec<f32>>,
    objects: Vec<Object>,
    fixed: Vec<Pos>,
    nets: Vec<Net>,
}

#[derive(Debug, Clone, PartialEq)]
struct Object {
    /// Rows it spans, centred on its position.
    height: u32,
    /// What it takes of each resource, in units of a full bin.
    demand: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
struct Net {
    pins: Vec<Pin>,
    weight: f32,
}

impl Problem {
    /// `capacity[r][y * width + x]`: how much of bin (x, y) resource `r`
    /// fills.
    pub fn new(width: usize, height: usize, capacity: Vec<Vec<f32>>) -> Problem {
        assert!(width > 0 && height > 0, "an empty grid");
        for c in &capacity {
            assert_eq!(c.len(), width * height, "a capacity per bin");
            assert!(
                c.iter().all(|v| (0.0..=1.0).contains(v)),
                "capacities are shares of a bin"
            );
        }
        Problem {
            width,
            height,
            capacity,
            objects: Vec::new(),
            fixed: Vec::new(),
            nets: Vec::new(),
        }
    }

    /// An object `height` rows tall taking `amount` of each listed resource.
    pub fn add_object(&mut self, height: u32, demand: &[(usize, f32)]) -> u32 {
        assert!(
            height >= 1 && height as usize <= self.height,
            "object height"
        );
        let mut d = vec![0.0; self.capacity.len()];
        for &(r, a) in demand {
            assert!(
                a >= 0.0 && a.is_finite(),
                "a demand is a non-negative amount"
            );
            d[r] += a;
        }
        self.objects.push(Object { height, demand: d });
        self.objects.len() as u32 - 1
    }

    pub fn add_fixed(&mut self, pos: Pos) -> u32 {
        self.fixed.push(pos);
        self.fixed.len() as u32 - 1
    }

    /// A net over `pins`; a pin's offsets are from its object's centre.
    pub fn add_net(&mut self, pins: Vec<Pin>, weight: f32) {
        for p in &pins {
            match p.of {
                PinOf::Object(o) => assert!(
                    (o as usize) < self.objects.len(),
                    "pin of an unknown object"
                ),
                PinOf::Fixed(f) => assert!(
                    (f as usize) < self.fixed.len(),
                    "pin of an unknown fixed point"
                ),
            }
        }
        self.nets.push(Net { pins, weight });
    }

    pub fn objects(&self) -> usize {
        self.objects.len()
    }

    pub fn nets(&self) -> usize {
        self.nets.len()
    }

    /// Half-perimeter wirelength of the nets with objects at `centres`,
    /// unweighted.
    pub fn hpwl(&self, centres: &[Pos]) -> f64 {
        self.nets
            .iter()
            .map(|net| {
                let (mut lx, mut hx, mut ly, mut hy) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                for pin in &net.pins {
                    let (x, y) = match pin.of {
                        PinOf::Object(o) => {
                            let (x, y) = centres[o as usize];
                            (x + pin.dx, y + pin.dy)
                        }
                        PinOf::Fixed(f) => self.fixed[f as usize],
                    };
                    lx = lx.min(x);
                    hx = hx.max(x);
                    ly = ly.min(y);
                    hy = hy.max(y);
                }
                if net.pins.len() < 2 {
                    0.0
                } else {
                    ((hx - lx) + (hy - ly)) as f64
                }
            })
            .sum()
    }
}

/// What the device does while the placement runs: reweight the nets for
/// timing from the objects' centres as they stand.
pub trait Timing {
    fn reweight(&mut self, centres: &[Pos], weights: &mut [f32]);
}

/// No timing: the nets keep their weights.
pub struct NoTiming;

impl Timing for NoTiming {
    fn reweight(&mut self, _centres: &[Pos], _weights: &mut [f32]) {}
}

/// Where the arithmetic runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accel {
    /// One thread.
    Serial,
    /// rayon's threads; bit-identical to `Serial`.
    Threads,
    #[cfg(feature = "gpu")]
    Gpu(crate::gpu::Runtime),
}

impl Accel {
    /// `cpu` (threads), `serial`, and with the `gpu` feature `cuda` or `hip`.
    pub fn parse(s: &str) -> Result<Accel, String> {
        match s {
            "cpu" | "threads" => Ok(Accel::Threads),
            "serial" => Ok(Accel::Serial),
            #[cfg(feature = "gpu")]
            "cuda" => Ok(Accel::Gpu(crate::gpu::Runtime::Cuda)),
            #[cfg(feature = "gpu")]
            "hip" => Ok(Accel::Gpu(crate::gpu::Runtime::Hip)),
            #[cfg(not(feature = "gpu"))]
            "cuda" | "hip" => Err(format!(
                "{s}: this chipsmith was built without the gpu feature"
            )),
            _ => Err(format!("{s}: not one of cpu, serial, cuda, hip")),
        }
    }

    /// `CHIPSMITH_ACCEL`, or the CPU's threads.
    pub fn from_env() -> Result<Accel, String> {
        match std::env::var("CHIPSMITH_ACCEL") {
            Ok(v) => Accel::parse(&v),
            Err(_) => Ok(Accel::Threads),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// The share of each resource's capacity the spread placement fills
    /// (raised to just above the design's utilisation when that is more).
    pub target_density: f32,
    /// Stop once no resource has more than this share of its demand in
    /// overfull bins.
    pub stop_overflow: f32,
    pub max_iterations: usize,
    /// Initial density weight, relative to the gradients' balance.
    pub density_weight: f32,
    /// Reweight the nets for timing every this many iterations once the
    /// overflow is below `timing_from`; 0: never.
    pub timing_every: usize,
    pub timing_from: f32,
    /// A filler's demand, in bins.
    pub filler: f32,
    /// The HPWL change, relative to the HPWL, at which the density
    /// weight stops growing (RePlAce's reference delta).
    pub hpwl_ref: f32,
    /// Smoothing of the wirelength, in bins, at an overflow of 0.5.
    pub gamma: f32,
    /// The density weight's largest growth per iteration.
    pub mu_max: f32,
    pub trace: bool,
    pub accel: Accel,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            target_density: 0.9,
            stop_overflow: 0.1,
            max_iterations: 2000,
            density_weight: 0.1,
            gamma: 0.5,
            mu_max: 1.1,
            timing_every: 25,
            timing_from: 0.5,
            filler: 0.5,
            hpwl_ref: 0.01,
            trace: false,
            accel: Accel::Threads,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Each object's centre.
    pub centres: Vec<Pos>,
    pub iterations: usize,
    /// Per resource, the share of demand in overfull bins at the end.
    pub overflow: Vec<f32>,
    /// Seconds spent in the engine (evaluate and step) and in the timing
    /// callback.
    pub engine_s: f64,
    pub timing_s: f64,
}

/// The problem flattened for the engines: objects (fillers after the
/// real ones) and pins in arrays, nets and each object's pins as CSR.
#[derive(Debug, Clone)]
pub struct Flat {
    pub width: usize,
    pub height: usize,
    pub resources: usize,
    /// Real objects, then fillers.
    pub n_real: usize,
    pub n: usize,
    /// Half of each object's height, in rows.
    pub half_h: Vec<f32>,
    /// `demand[o * resources + r]`.
    pub demand: Vec<f32>,
    pub net_start: Vec<u32>,
    /// The object a pin is on, or `FIXED` (then `pin_dx`/`pin_dy` are its
    /// position).
    pub pin_obj: Vec<u32>,
    pub pin_dx: Vec<f32>,
    pub pin_dy: Vec<f32>,
    pub obj_pin_start: Vec<u32>,
    pub obj_pins: Vec<u32>,
    /// The net each pin is on.
    pub pin_net: Vec<u32>,
    /// `background[r * bins + b]`: fixed charge where resource `r` has
    /// less than a full bin, so the field treats it as already full.
    pub background: Vec<f32>,
    /// Per resource: the density objects are spread to.
    pub target: Vec<f32>,
    /// Per resource: the real objects' total demand.
    pub real_demand: Vec<f64>,
    /// Per resource, per bin: capacity times target.
    pub limit: Vec<f32>,
}

pub const FIXED: u32 = u32::MAX;

/// Iterations without a 1% drop in the worst overflow before stopping.
const STALL: usize = 50;
/// Stalls count only once the overflow is this low: early on it
/// plateaus while the density weight is still small.
const STALL_FROM: f32 = 0.3;

/// Fixed-point scale of the density accumulators: exact integer sums are
/// the same in any order.
pub const DENSITY_SCALE: f64 = (1u64 << 24) as f64;

/// The transform's tables and factors for a grid.
#[derive(Debug, Clone)]
pub struct Dct {
    /// `cos_x[u * w + x] = cos(pi u (x + 0.5) / w)`, likewise `sin_x`,
    /// `cos_y`, `sin_y`.
    pub cos_x: Vec<f32>,
    pub sin_x: Vec<f32>,
    pub cos_y: Vec<f32>,
    pub sin_y: Vec<f32>,
    /// `kx[v * w + u]`: the factor that takes coefficient (u, v) of the
    /// density to that of the field's x component (inverse-transform
    /// normalisation included); 0 at (0, 0). Likewise `ky`.
    pub kx: Vec<f32>,
    pub ky: Vec<f32>,
    /// Transposes: `cos_x_t[x * w + u]`, `cos_y_t[y * h + v]`,
    /// `sin_y_t[y * h + v]`.
    pub cos_x_t: Vec<f32>,
    pub cos_y_t: Vec<f32>,
    pub sin_y_t: Vec<f32>,
}

impl Dct {
    pub fn new(w: usize, h: usize) -> Dct {
        let table = |n: usize, f: fn(f64) -> f64| -> Vec<f32> {
            (0..n * n)
                .map(|i| {
                    let (u, x) = (i / n, i % n);
                    f(std::f64::consts::PI * u as f64 * (x as f64 + 0.5) / n as f64) as f32
                })
                .collect()
        };
        let mut kx = vec![0.0; w * h];
        let mut ky = vec![0.0; w * h];
        for v in 0..h {
            for u in 0..w {
                if u == 0 && v == 0 {
                    continue;
                }
                let wu = std::f64::consts::PI * u as f64 / w as f64;
                let wv = std::f64::consts::PI * v as f64 / h as f64;
                let norm = (if u == 0 { 1.0 } else { 2.0 } / w as f64)
                    * (if v == 0 { 1.0 } else { 2.0 } / h as f64);
                let d = wu * wu + wv * wv;
                kx[v * w + u] = (norm * wu / d) as f32;
                ky[v * w + u] = (norm * wv / d) as f32;
            }
        }
        let transpose = |t: &[f32], n: usize| -> Vec<f32> {
            (0..n * n).map(|i| t[(i % n) * n + i / n]).collect()
        };
        let (cos_x, sin_x, cos_y, sin_y) = (
            table(w, f64::cos),
            table(w, f64::sin),
            table(h, f64::cos),
            table(h, f64::sin),
        );
        Dct {
            cos_x_t: transpose(&cos_x, w),
            cos_y_t: transpose(&cos_y, h),
            sin_y_t: transpose(&sin_y, h),
            cos_x,
            sin_x,
            cos_y,
            sin_y,
            kx,
            ky,
        }
    }
}

/// Numbers one evaluation reports to the control loop.
#[derive(Debug, Clone, PartialEq)]
pub struct Eval {
    /// Unweighted HPWL at the reference point.
    pub hpwl: f64,
    /// L1 norms of the wirelength gradient and each resource's density
    /// gradient (before weighting and preconditioning); the density
    /// norms on the first evaluation only, zeros after.
    pub wl_l1: f64,
    pub density_l1: Vec<f64>,
    pub overflow: Vec<f32>,
    /// Squared norms of the change in reference point and in
    /// (preconditioned) gradient since the last evaluation.
    pub dv2: f64,
    pub dg2: f64,
}

/// The arithmetic of one Nesterov iteration, wherever it runs.
pub trait Engine {
    /// Gradient at the reference point with smoothing `gamma` and density
    /// weights `lambda`, kept for `step`.
    fn evaluate(&mut self, gamma: f32, lambda: &[f32]) -> Eval;
    /// `u' = v - alpha g`, `v' = u' + coef (u' - u)`, both kept inside the
    /// grid.
    fn step(&mut self, alpha: f32, coef: f32);
    /// The major point's positions, real objects only.
    fn positions(&mut self) -> Vec<Pos>;

    fn set_weights(&mut self, weights: &[f32]);
    /// Where the time went, for the trace.
    fn report(&self) -> String {
        String::new()
    }
}

impl Flat {
    pub fn new(p: &Problem, opts: &Options) -> Flat {
        let (w, h) = (p.width, p.height);
        let bins = w * h;
        let r_count = p.capacity.len();
        let real_demand: Vec<f64> = (0..r_count)
            .map(|r| p.objects.iter().map(|o| o.demand[r] as f64).sum())
            .collect();
        let target: Vec<f32> = (0..r_count)
            .map(|r| {
                let cap: f64 = p.capacity[r].iter().map(|&c| c as f64).sum();
                let util = if cap > 0.0 { real_demand[r] / cap } else { 0.0 };
                // A bin can't be filled to a share finer than its objects:
                // a resource whose demand comes mostly in whole bins (a
                // LAB's register controls on sha256, where 1,804 registers
                // each have their own clear) is spread to full bins, or
                // its overflow never drops below the gap. The demand-
                // weighted mean share says how coarse it is; the largest
                // object would let a few such registers (picorv32x24 has
                // some) fill every LAB's controls.
                let total: f64 = p.objects.iter().map(|o| o.demand[r] as f64).sum();
                let coarse = if total > 0.0 {
                    p.objects.iter().map(|o| (o.demand[r] as f64).powi(2)).sum::<f64>() / total
                } else {
                    0.0
                };
                (opts.target_density as f64)
                    .max(util + 0.02)
                    .max(coarse)
                    .min(1.0) as f32
            })
            .collect();
        let mut half_h: Vec<f32> = p.objects.iter().map(|o| o.height as f32 / 2.0).collect();
        let mut demand: Vec<f32> = p
            .objects
            .iter()
            .flat_map(|o| o.demand.iter().copied())
            .collect();
        // Fillers: each resource's free capacity at its target, in pieces
        // of `opts.filler`.
        for r in 0..r_count {
            let cap: f64 = p.capacity[r].iter().map(|&c| c as f64).sum();
            let free = cap * target[r] as f64 - real_demand[r];
            // A resource nothing real uses needs no field, so no fillers.
            let count = if real_demand[r] > 0.0 {
                (free / opts.filler as f64).floor().max(0.0) as usize
            } else {
                0
            };
            for _ in 0..count {
                half_h.push(0.5);
                let mut d = vec![0.0; r_count];
                d[r] = opts.filler;
                demand.extend(d);
            }
        }
        let n = half_h.len();
        let mut net_start = vec![0u32];
        let (mut pin_obj, mut pin_dx, mut pin_dy, mut pin_net) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (i, net) in p.nets.iter().enumerate() {
            for pin in &net.pins {
                match pin.of {
                    PinOf::Object(o) => {
                        pin_obj.push(o);
                        pin_dx.push(pin.dx);
                        pin_dy.push(pin.dy);
                    }
                    PinOf::Fixed(f) => {
                        let (x, y) = p.fixed[f as usize];
                        pin_obj.push(FIXED);
                        pin_dx.push(x);
                        pin_dy.push(y);
                    }
                }
                pin_net.push(i as u32);
            }
            net_start.push(pin_obj.len() as u32);
        }
        let mut obj_pin_start = vec![0u32; n + 1];
        for &o in &pin_obj {
            if o != FIXED {
                obj_pin_start[o as usize + 1] += 1;
            }
        }
        for i in 0..n {
            obj_pin_start[i + 1] += obj_pin_start[i];
        }
        let mut fill = obj_pin_start.clone();
        let mut obj_pins = vec![0u32; *obj_pin_start.last().unwrap() as usize];
        for (k, &o) in pin_obj.iter().enumerate() {
            if o != FIXED {
                obj_pins[fill[o as usize] as usize] = k as u32;
                fill[o as usize] += 1;
            }
        }
        let mut background = vec![0.0; r_count * bins];
        let mut limit = vec![0.0; r_count * bins];
        for r in 0..r_count {
            for b in 0..bins {
                background[r * bins + b] = (1.0 - p.capacity[r][b]) * target[r];
                limit[r * bins + b] = p.capacity[r][b] * target[r];
            }
        }
        Flat {
            width: w,
            height: h,
            resources: r_count,
            n_real: p.objects.len(),
            n,
            half_h,
            demand,
            net_start,
            pin_obj,
            pin_dx,
            pin_dy,
            obj_pin_start,
            obj_pins,
            pin_net,
            background,
            target,
            real_demand,
            limit,
        }
    }

    /// Clamp a centre so the footprint stays on the grid.
    pub fn clamp(&self, o: usize, (x, y): Pos) -> Pos {
        let hh = self.half_h[o];
        (
            x.clamp(0.5, self.width as f32 - 0.5),
            y.clamp(hh, self.height as f32 - hh),
        )
    }
}

/// Each bin a footprint one bin wide and `2 hh` tall centred on (cx, cy)
/// overlaps, with the overlap's area.
#[inline]
pub fn footprint(w: usize, h: usize, cx: f32, cy: f32, hh: f32, mut f: impl FnMut(usize, f32)) {
    let x0 = cx - 0.5;
    let bx0 = x0.floor();
    let fx = x0 - bx0;
    let bx0 = bx0 as i64;
    let (y0, y1) = (cy - hh, cy + hh);
    let by0 = y0.floor() as i64;
    let by1 = (y1.ceil() as i64).max(by0 + 1);
    for by in by0..by1 {
        if by < 0 || by >= h as i64 {
            continue;
        }
        let oy = (y1.min(by as f32 + 1.0) - y0.max(by as f32)).max(0.0);
        if oy <= 0.0 {
            continue;
        }
        for (bx, ox) in [(bx0, 1.0 - fx), (bx0 + 1, fx)] {
            if bx < 0 || bx >= w as i64 || ox <= 0.0 {
                continue;
            }
            f(by as usize * w + bx as usize, ox * oy);
        }
    }
}

/// Run `f` over `0..n`, writing `out[i]`; on rayon's threads or one.
fn fill<T: Send>(par: bool, out: &mut [T], f: impl Fn(usize) -> T + Sync + Send) {
    if par {
        out.par_iter_mut().enumerate().for_each(|(i, o)| *o = f(i));
    } else {
        out.iter_mut().enumerate().for_each(|(i, o)| *o = f(i));
    }
}

/// Sum of `f` over `0..n` in chunks of `CHUNK` (the chunks in order): the
/// same total on any number of threads.
fn sum(par: bool, n: usize, f: impl Fn(usize) -> f64 + Sync + Send) -> f64 {
    const CHUNK: usize = 1024;
    let chunk = |c: usize| -> f64 { (c * CHUNK..((c + 1) * CHUNK).min(n)).map(&f).sum() };
    let chunks = n.div_ceil(CHUNK);
    if par {
        let parts: Vec<f64> = (0..chunks).into_par_iter().map(chunk).collect();
        parts.iter().sum()
    } else {
        (0..chunks).map(chunk).collect::<Vec<f64>>().iter().sum()
    }
}

/// `out = a b` for row-major `a` (m x k) and `b` (k x n); each entry
/// summed over k in order, rows on threads.
pub fn mm(par: bool, out: &mut [f32], a: &[f32], b: &[f32], k: usize, n: usize) {
    let row = |(i, out): (usize, &mut [f32])| {
        out.fill(0.0);
        for kk in 0..k {
            let s = a[i * k + kk];
            for (o, &v) in out.iter_mut().zip(&b[kk * n..(kk + 1) * n]) {
                *o += s * v;
            }
        }
    };
    if par {
        out.par_chunks_mut(n).enumerate().for_each(row);
    } else {
        out.chunks_mut(n).enumerate().for_each(row);
    }
}

/// The CPU engine: serial or on rayon's threads, bit-identical.
pub struct Cpu {
    par: bool,
    f: Flat,
    dct: Dct,
    weight: Vec<f32>,
    /// Per object: the sum of its pins' net weights (the wirelength part of
    /// the preconditioner).
    wl_degree: Vec<f32>,
    ux: Vec<f32>,
    uy: Vec<f32>,
    vx: Vec<f32>,
    vy: Vec<f32>,
    vx_prev: Vec<f32>,
    vy_prev: Vec<f32>,
    gx: Vec<f32>,
    gy: Vec<f32>,
    gx_prev: Vec<f32>,
    gy_prev: Vec<f32>,
    /// Weighted wirelength gradient per pin.
    pin_g: Vec<(f32, f32)>,
    net_hpwl: Vec<f32>,
    /// Per resource, per bin: the field.
    ex: Vec<f32>,
    ey: Vec<f32>,
    evaluated: usize,
    /// Seconds per phase: wirelength, density, field, objects, norms.
    pub phase_s: [f64; 5],
}

impl Cpu {
    pub fn new(f: Flat, weights: &[f32], start: &[Pos], par: bool) -> Cpu {
        let n = f.n;
        let (ux, uy): (Vec<f32>, Vec<f32>) = start.iter().copied().unzip();
        let bins = f.width * f.height;
        let mut c = Cpu {
            par,
            dct: Dct::new(f.width, f.height),
            weight: weights.to_vec(),
            wl_degree: vec![0.0; n],
            vx: ux.clone(),
            vy: uy.clone(),
            vx_prev: ux.clone(),
            vy_prev: uy.clone(),
            ux,
            uy,
            gx: vec![0.0; n],
            gy: vec![0.0; n],
            gx_prev: vec![0.0; n],
            gy_prev: vec![0.0; n],
            pin_g: vec![(0.0, 0.0); f.pin_obj.len()],
            net_hpwl: vec![0.0; f.net_start.len() - 1],
            ex: vec![0.0; f.resources * bins],
            ey: vec![0.0; f.resources * bins],
            evaluated: 0,
            phase_s: [0.0; 5],
            f,
        };
        c.set_weights(weights);
        c
    }
}

/// One net's weighted-average sums along one axis: its extremes and, for
/// the max and the min side, the sum of exp((x - hi) / gamma) (and
/// exp((lo - x) / gamma)) and of x times it. Never overflows: every
/// exponent is at most 0.
#[derive(Debug, Clone, Copy, Default)]
pub struct WaSums {
    pub lo: f32,
    pub hi: f32,
    pub sp: f32,
    pub sxp: f32,
    pub sn: f32,
    pub sxn: f32,
}

impl WaSums {
    pub fn of(coords: impl Iterator<Item = f32> + Clone, gamma: f32) -> WaSums {
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for c in coords.clone() {
            lo = lo.min(c);
            hi = hi.max(c);
        }
        let mut s = WaSums {
            lo,
            hi,
            ..WaSums::default()
        };
        for c in coords {
            let ep = ((c - hi) / gamma).exp();
            let en = ((lo - c) / gamma).exp();
            s.sp += ep;
            s.sxp += c * ep;
            s.sn += en;
            s.sxn += c * en;
        }
        s
    }

    /// The gradient of the net's weighted-average span at a pin at `c`.
    pub fn grad(&self, c: f32, gamma: f32) -> f32 {
        let (wp, wn) = (self.sxp / self.sp, self.sxn / self.sn);
        let ep = ((c - self.hi) / gamma).exp();
        let en = ((self.lo - c) / gamma).exp();
        ep * (1.0 + (c - wp) / gamma) / self.sp - en * (1.0 - (c - wn) / gamma) / self.sn
    }
}

impl Engine for Cpu {
    fn evaluate(&mut self, gamma: f32, lambda: &[f32]) -> Eval {
        let mut clock = std::time::Instant::now();
        let mut lap = |phase: &mut f64| {
            *phase += clock.elapsed().as_secs_f64();
            clock = std::time::Instant::now();
        };
        let mut phase_s = self.phase_s;
        let par = self.par;
        let f = &self.f;
        let (w, h, rc) = (f.width, f.height, f.resources);
        let bins = w * h;
        let (vx, vy) = (&self.vx, &self.vy);
        // Wirelength: per net, the weighted averages' sums; then per pin,
        // its gradient.
        let pin_pos = |k: usize| -> Pos {
            let o = f.pin_obj[k];
            if o == FIXED {
                (f.pin_dx[k], f.pin_dy[k])
            } else {
                (vx[o as usize] + f.pin_dx[k], vy[o as usize] + f.pin_dy[k])
            }
        };
        let nets = f.net_start.len() - 1;
        let mut stats = vec![[WaSums::default(); 2]; nets];
        fill(par, &mut stats, |i| {
            let (s, e) = (f.net_start[i] as usize, f.net_start[i + 1] as usize);
            [
                WaSums::of((s..e).map(|k| pin_pos(k).0), gamma),
                WaSums::of((s..e).map(|k| pin_pos(k).1), gamma),
            ]
        });
        {
            let weight = &self.weight;
            fill(par, &mut self.pin_g, |k| {
                let i = f.pin_net[k] as usize;
                let (s, e) = (f.net_start[i] as usize, f.net_start[i + 1] as usize);
                if e - s < 2 {
                    return (0.0, 0.0);
                }
                let (x, y) = pin_pos(k);
                (
                    stats[i][0].grad(x, gamma) * weight[i],
                    stats[i][1].grad(y, gamma) * weight[i],
                )
            });
        }
        fill(par, &mut self.net_hpwl, |i| {
            if f.net_start[i + 1] - f.net_start[i] < 2 {
                0.0
            } else {
                (stats[i][0].hi - stats[i][0].lo) + (stats[i][1].hi - stats[i][1].lo)
            }
        });
        let hpwl = sum(par, self.net_hpwl.len(), |i| self.net_hpwl[i] as f64);

        lap(&mut phase_s[0]);
        // Density per resource, all objects and real ones only.
        let acc: Vec<AtomicI64> = (0..2 * rc * bins).map(|_| AtomicI64::new(0)).collect();
        let scatter = |o: usize| {
            let (cx, cy, hh) = (vx[o], vy[o], f.half_h[o]);
            let area = 2.0 * hh;
            for r in 0..rc {
                let d = f.demand[o * rc + r];
                if d == 0.0 {
                    continue;
                }
                footprint(w, h, cx, cy, hh, |b, ov| {
                    let q = (d as f64 * ov as f64 / area as f64 * DENSITY_SCALE).round() as i64;
                    acc[r * bins + b].fetch_add(q, Ordering::Relaxed);
                    if o < f.n_real {
                        acc[(rc + r) * bins + b].fetch_add(q, Ordering::Relaxed);
                    }
                });
            }
        };
        if par {
            (0..f.n).into_par_iter().for_each(scatter);
        } else {
            (0..f.n).for_each(scatter);
        }
        let dens =
            |r: usize, b: usize| acc[r * bins + b].load(Ordering::Relaxed) as f64 / DENSITY_SCALE;
        let overflow: Vec<f32> = (0..rc)
            .map(|r| {
                if f.real_demand[r] <= 0.0 {
                    return 0.0;
                }
                let over = sum(par, bins, |b| {
                    (dens(rc + r, b) - f.limit[r * bins + b] as f64).max(0.0)
                });
                (over / f.real_demand[r]) as f32
            })
            .collect();

        lap(&mut phase_s[1]);
        // The field per resource: rho -> coefficients -> E.
        let dct = &self.dct;
        for r in 0..rc {
            if f.real_demand[r] <= 0.0 {
                self.ex[r * bins..(r + 1) * bins].fill(0.0);
                self.ey[r * bins..(r + 1) * bins].fill(0.0);
                continue;
            }
            let rho: Vec<f32> = (0..bins)
                .map(|b| (dens(r, b) + f.background[r * bins + b] as f64) as f32)
                .collect();
            // a = cos_y rho cos_x^T: the density's coefficients.
            let mut t1 = vec![0.0; bins];
            mm(par, &mut t1, &rho, &dct.cos_x_t, w, w);
            let mut a = vec![0.0; bins];
            mm(par, &mut a, &dct.cos_y, &t1, h, w);
            let bx: Vec<f32> = (0..bins).map(|i| a[i] * dct.kx[i]).collect();
            let by: Vec<f32> = (0..bins).map(|i| a[i] * dct.ky[i]).collect();
            // ex = cos_y^T bx sin_x, ey = sin_y^T by cos_x.
            let mut t2 = vec![0.0; bins];
            mm(par, &mut t2, &bx, &dct.sin_x, w, w);
            mm(
                par,
                &mut self.ex[r * bins..(r + 1) * bins],
                &dct.cos_y_t,
                &t2,
                h,
                w,
            );
            let mut t3 = vec![0.0; bins];
            mm(par, &mut t3, &by, &dct.cos_x, w, w);
            mm(
                par,
                &mut self.ey[r * bins..(r + 1) * bins],
                &dct.sin_y_t,
                &t3,
                h,
                w,
            );
        }

        lap(&mut phase_s[2]);
        // Per object: gradient, preconditioned.
        let (ex, ey, pin_g, wl_degree) = (&self.ex, &self.ey, &self.pin_g, &self.wl_degree);
        let per_object = |o: usize| -> [f32; 4] {
            let (mut gwx, mut gwy) = (0.0f32, 0.0f32);
            for k in f.obj_pin_start[o]..f.obj_pin_start[o + 1] {
                let (a, b) = pin_g[f.obj_pins[k as usize] as usize];
                gwx += a;
                gwy += b;
            }
            let (cx, cy, hh) = (vx[o], vy[o], f.half_h[o]);
            let (mut gdx, mut gdy, mut dl1) = (0.0f32, 0.0f32, 0.0f32);
            let mut charge = 0.0f32;
            for r in 0..rc {
                let d = f.demand[o * rc + r];
                if d == 0.0 {
                    continue;
                }
                let (mut sx, mut sy, mut so) = (0.0f32, 0.0f32, 0.0f32);
                footprint(w, h, cx, cy, hh, |b, ov| {
                    sx += ex[r * bins + b] * ov;
                    sy += ey[r * bins + b] * ov;
                    so += ov;
                });
                let (fx, fy) = (-d * sx / so.max(1e-6), -d * sy / so.max(1e-6));
                gdx += lambda[r] * fx;
                gdy += lambda[r] * fy;
                dl1 += fx.abs() + fy.abs();
                charge += lambda[r] * d;
            }
            let pre = (wl_degree[o] + charge).max(1.0);
            [
                (gwx + gdx) / pre,
                (gwy + gdy) / pre,
                gwx.abs() + gwy.abs(),
                dl1,
            ]
        };
        let mut g = vec![[0.0f32; 4]; f.n];
        fill(par, &mut g, per_object);
        lap(&mut phase_s[3]);
        let wl_l1 = sum(par, f.n, |o| g[o][2] as f64);
        // Each resource's density gradient norm, for the initial weights:
        // only the first evaluation's is used.
        let density_l1 = if self.evaluated == 0 {
            density_l1(f, vx, vy, ex, ey)
        } else {
            vec![0.0; rc]
        };
        std::mem::swap(&mut self.gx, &mut self.gx_prev);
        std::mem::swap(&mut self.gy, &mut self.gy_prev);
        for o in 0..f.n {
            self.gx[o] = g[o][0];
            self.gy[o] = g[o][1];
        }
        let (dv2, dg2) = if self.evaluated == 0 {
            (0.0, 0.0)
        } else {
            let (vx, vy, px, py) = (&self.vx, &self.vy, &self.vx_prev, &self.vy_prev);
            let (gx, gy, qx, qy) = (&self.gx, &self.gy, &self.gx_prev, &self.gy_prev);
            (
                sum(par, f.n, |o| {
                    let (a, b) = ((vx[o] - px[o]) as f64, (vy[o] - py[o]) as f64);
                    a * a + b * b
                }),
                sum(par, f.n, |o| {
                    let (a, b) = ((gx[o] - qx[o]) as f64, (gy[o] - qy[o]) as f64);
                    a * a + b * b
                }),
            )
        };
        lap(&mut phase_s[4]);
        self.phase_s = phase_s;
        self.evaluated += 1;
        Eval {
            hpwl,
            wl_l1,
            density_l1,
            overflow,
            dv2,
            dg2,
        }
    }

    fn step(&mut self, alpha: f32, coef: f32) {
        let par = self.par;
        let f = &self.f;
        let (ux, uy, vx, vy, gx, gy) = (&self.ux, &self.uy, &self.vx, &self.vy, &self.gx, &self.gy);
        let mut next = vec![[0.0f32; 4]; f.n];
        fill(par, &mut next, |o| {
            let (nx, ny) = f.clamp(o, (vx[o] - alpha * gx[o], vy[o] - alpha * gy[o]));
            let (rx, ry) = f.clamp(o, (nx + coef * (nx - ux[o]), ny + coef * (ny - uy[o])));
            [nx, ny, rx, ry]
        });
        std::mem::swap(&mut self.vx, &mut self.vx_prev);
        std::mem::swap(&mut self.vy, &mut self.vy_prev);
        for (o, &[nx, ny, rx, ry]) in next.iter().enumerate() {
            self.ux[o] = nx;
            self.uy[o] = ny;
            self.vx[o] = rx;
            self.vy[o] = ry;
        }
    }

    fn report(&self) -> String {
        let p = self.phase_s;
        format!(
            "wirelength {:.2} s, density {:.2} s, field {:.2} s, objects {:.2} s, norms {:.2} s",
            p[0], p[1], p[2], p[3], p[4]
        )
    }

    fn positions(&mut self) -> Vec<Pos> {
        (0..self.f.n_real)
            .map(|o| (self.ux[o], self.uy[o]))
            .collect()
    }

    fn set_weights(&mut self, weights: &[f32]) {
        self.weight.copy_from_slice(weights);
        let f = &self.f;
        let weight = &self.weight;
        let mut deg = vec![0.0f32; f.n];
        fill(self.par, &mut deg, |o| {
            (f.obj_pin_start[o]..f.obj_pin_start[o + 1])
                .map(|k| weight[f.pin_net[f.obj_pins[k as usize] as usize] as usize])
                .sum()
        });
        self.wl_degree = deg;
    }
}

/// Each resource's density gradient L1 norm (unweighted), for sizing the
/// first density weights.
pub fn density_l1(f: &Flat, vx: &[f32], vy: &[f32], ex: &[f32], ey: &[f32]) -> Vec<f64> {
    let (w, h, rc) = (f.width, f.height, f.resources);
    let bins = w * h;
    (0..rc)
        .map(|r| {
            sum(false, f.n, |o| {
                let d = f.demand[o * rc + r];
                if d == 0.0 {
                    return 0.0;
                }
                let (cx, cy, hh) = (vx[o], vy[o], f.half_h[o]);
                let (mut sx, mut sy, mut so) = (0.0f32, 0.0f32, 0.0f32);
                footprint(w, h, cx, cy, hh, |b, ov| {
                    sx += ex[r * bins + b] * ov;
                    sy += ey[r * bins + b] * ov;
                    so += ov;
                });
                ((d * sx / so.max(1e-6)).abs() + (d * sy / so.max(1e-6)).abs()) as f64
            })
        })
        .collect()
}

/// xorshift64*, for the start's jitter and the fillers' places.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// The start: real objects at `start` (or the fixed points' centre), each
/// jittered a little so objects with the same nets separate; fillers
/// scattered over the bins that have their resource.
fn initial(p: &Problem, f: &Flat, start: Option<&[Pos]>) -> Vec<Pos> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let centre = if p.fixed.is_empty() {
        (p.width as f32 / 2.0, p.height as f32 / 2.0)
    } else {
        let (sx, sy) = p.fixed.iter().fold((0.0f64, 0.0f64), |(a, b), &(x, y)| {
            (a + x as f64, b + y as f64)
        });
        (
            (sx / p.fixed.len() as f64) as f32,
            (sy / p.fixed.len() as f64) as f32,
        )
    };
    let mut pos = Vec::with_capacity(f.n);
    for o in 0..f.n_real {
        let (x, y) = start.map_or(centre, |s| s[o]);
        let j = (rng.unit() - 0.5, rng.unit() - 0.5);
        pos.push(f.clamp(o, (x + j.0, y + j.1)));
    }
    let bins = f.width * f.height;
    for o in f.n_real..f.n {
        let r = (0..f.resources)
            .find(|&r| f.demand[o * f.resources + r] > 0.0)
            .expect("a filler fills a resource");
        let sites: usize = (0..bins)
            .filter(|&b| f.limit[r * bins + b] > 0.0)
            .count()
            .max(1);
        let pick = (rng.next() % sites as u64) as usize;
        let b = (0..bins)
            .filter(|&b| f.limit[r * bins + b] > 0.0)
            .nth(pick)
            .unwrap_or(0);
        let (bx, by) = ((b % f.width) as f32, (b / f.width) as f32);
        pos.push(f.clamp(o, (bx + rng.unit(), by + rng.unit())));
    }
    pos
}

/// Place `p`: returns every object's centre. `start` (the real objects'
/// centres) seeds the placement; without it everything starts at the
/// fixed points' centre.
/// Errs only when the GPU asked for isn't there.
pub fn place(
    p: &Problem,
    timing: &mut impl Timing,
    start: Option<&[Pos]>,
    opts: &Options,
) -> Result<Outcome, String> {
    let f = Flat::new(p, opts);
    let pos = initial(p, &f, start);
    let mut weights: Vec<f32> = p.nets.iter().map(|n| n.weight).collect();
    let mut engine: Box<dyn Engine> = match opts.accel {
        Accel::Serial => Box::new(Cpu::new(f.clone(), &weights, &pos, false)),
        Accel::Threads => Box::new(Cpu::new(f.clone(), &weights, &pos, true)),
        #[cfg(feature = "gpu")]
        Accel::Gpu(rt) => Box::new(crate::gpu::Gpu::new(rt, f.clone(), &weights, &pos)?),
    };
    Ok(run(p, &f, engine.as_mut(), timing, &mut weights, opts))
}

fn run(
    p: &Problem,
    f: &Flat,
    engine: &mut dyn Engine,
    timing: &mut impl Timing,
    weights: &mut [f32],
    opts: &Options,
) -> Outcome {
    let rc = f.resources;
    let started = std::time::Instant::now();
    let mut timing_s = 0.0;
    let gamma_of = |overflow: f32| -> f32 {
        // ePlace's schedule, 10^(20/9 overflow - 11/9) times a base: here
        // from 5 bins when everything overlaps to 0.05 bin when spread.
        // ePlace's base is several of its bins; a bin here is a whole LAB.
        let base = opts.gamma;
        base * 10f32.powf(20.0 / 9.0 * overflow.min(1.0) - 11.0 / 9.0)
    };
    // The first evaluation, with no density weight, sizes the weights.
    let mut lambda = vec![0.0f32; rc];
    let first = engine.evaluate(gamma_of(1.0), &lambda);
    for r in 0..rc {
        if first.density_l1[r] > 0.0 {
            lambda[r] = (opts.density_weight as f64 * first.wl_l1 / first.density_l1[r]) as f32;
        }
    }
    let mut a = 1.0f64;
    let mut prev_hpwl: Option<f64> = None;
    let mut overflow = first.overflow.clone();
    let mut iterations = 0;
    let max_over = |o: &[f32]| o.iter().copied().fold(0.0f32, f32::max);
    let mut best_overflow = f32::MAX;
    let mut since_progress = 0;
    let mut best: Option<Vec<Pos>> = None;
    let mut stalled = false;
    for k in 0..opts.max_iterations {
        iterations = k + 1;
        let e = engine.evaluate(gamma_of(max_over(&overflow)), &lambda);
        // A step that blew up (lambda past f32, a NaN) ends the placement
        // at the last finite one.
        if !e.hpwl.is_finite() || !e.dg2.is_finite() || lambda.iter().any(|l| !l.is_finite()) {
            if opts.trace {
                eprintln!("electro: {k}: diverged; keeping the best overflow's placement");
            }
            stalled = true;
            break;
        }
        overflow = e.overflow.clone();
        // No progress on the overflow in a while: further density weight
        // only stretches the wires, so the placement at the best overflow
        // is the result. (Objects that each fill a bin, like sha256's
        // registers in the control-set field, never tile the bins
        // exactly: that field stalls near 0.12.)
        let worst = max_over(&overflow);
        if worst < best_overflow * 0.99 {
            best_overflow = worst;
            best = Some(engine.positions());
            since_progress = 0;
        } else if best_overflow < STALL_FROM {
            since_progress += 1;
            if since_progress >= STALL {
                if opts.trace {
                    eprintln!("electro: {k}: overflow stalled at {worst:.3}; keeping the best, {best_overflow:.3}");
                }
                stalled = true;
                break;
            }
        }
        let alpha = if k == 0 || e.dg2 <= 0.0 {
            // A tenth of a bin for the average object's gradient.
            let mean = e.wl_l1.max(1e-9) / f.n as f64;
            ((0.1 / mean) as f32).min(1.0)
        } else {
            (e.dv2 / e.dg2).sqrt() as f32
        };
        if opts.trace && (k % 50 == 0 || max_over(&overflow) < opts.stop_overflow) {
            eprintln!(
                "electro: {k}: hpwl {:.0} overflow {:?} lambda {:?} alpha {alpha:.3e} gamma {:.2} ({:.2} s)",
                e.hpwl,
                overflow.iter().map(|o| (o * 1000.0).round() / 1000.0).collect::<Vec<_>>(),
                lambda.iter().map(|l| format!("{l:.2e}")).collect::<Vec<_>>(),
                gamma_of(max_over(&overflow)),
                started.elapsed().as_secs_f64()
            );
        }
        if max_over(&overflow) < opts.stop_overflow {
            break;
        }
        let a_next = (1.0 + (4.0 * a * a + 1.0).sqrt()) / 2.0;
        engine.step(alpha, ((a - 1.0) / a_next) as f32);
        a = a_next;
        // Density weight: grows fast while wirelength holds, slower as it
        // rises (RePlAce's schedule).
        let mu = match prev_hpwl {
            Some(prev) => {
                let p = (e.hpwl - prev) / (opts.hpwl_ref as f64 * e.hpwl.max(1.0));
                let mu_max = opts.mu_max as f64;
                if p < 0.0 {
                    mu_max
                } else {
                    mu_max.powf(1.0 - p).max(0.95)
                }
            }
            None => opts.mu_max as f64,
        };
        prev_hpwl = Some(e.hpwl);
        for l in lambda.iter_mut() {
            *l = (*l as f64 * mu) as f32;
        }
        if opts.timing_every > 0
            && k % opts.timing_every == 0
            && max_over(&overflow) < opts.timing_from
        {
            let t = std::time::Instant::now();
            let centres = engine.positions();
            timing.reweight(&centres, weights);
            engine.set_weights(weights);
            timing_s += t.elapsed().as_secs_f64();
        }
    }
    let centres = match (stalled, best) {
        (true, Some(best)) => best,
        _ => engine.positions(),
    };
    if opts.trace {
        eprintln!("electro: {}", engine.report());
    }
    let _ = p;
    Outcome {
        centres,
        iterations,
        overflow,
        engine_s: started.elapsed().as_secs_f64() - timing_s,
        timing_s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(of: PinOf) -> Pin {
        Pin {
            of,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// 200 unit-ish objects on random two- and three-pin nets with a few
    /// fixed pins on the edge of a 16 x 12 grid, one resource, capacity 1
    /// everywhere except a dead column.
    fn problem() -> Problem {
        let (w, h) = (16, 12);
        let mut cap = vec![1.0; w * h];
        for y in 0..h {
            cap[y * w + 7] = 0.0;
        }
        let mut p = Problem::new(w, h, vec![cap]);
        let n = 200;
        for _ in 0..n {
            p.add_object(1, &[(0, 1.0 / 16.0)]);
        }
        let fx: Vec<u32> = (0..6).map(|i| p.add_fixed((0.0, 2.0 * i as f32))).collect();
        let mut rng = Rng(7);
        for i in 0..n as u32 {
            let j = (rng.next() % n as u64) as u32;
            let k = (rng.next() % n as u64) as u32;
            p.add_net(
                vec![
                    pin(PinOf::Object(i)),
                    pin(PinOf::Object(j)),
                    pin(PinOf::Object(k)),
                ],
                1.0,
            );
        }
        for (i, &f) in fx.iter().enumerate() {
            p.add_net(
                vec![pin(PinOf::Fixed(f)), pin(PinOf::Object(i as u32 * 30))],
                1.0,
            );
        }
        p
    }

    #[test]
    fn serial_and_threads_place_identically() {
        let p = problem();
        let opts = |accel| Options {
            accel,
            max_iterations: 300,
            ..Options::default()
        };
        let a = place(&p, &mut NoTiming, None, &opts(Accel::Serial)).unwrap();
        let b = place(&p, &mut NoTiming, None, &opts(Accel::Threads)).unwrap();
        assert_eq!(a.centres, b.centres);
        assert_eq!(a.iterations, b.iterations);
    }

    #[test]
    fn objects_spread_out_of_overfull_bins_and_off_dead_bins() {
        let p = problem();
        let out = place(
            &p,
            &mut NoTiming,
            None,
            &Options {
                trace: std::env::var_os("ELECTRO_TRACE").is_some(),
                ..Options::default()
            },
        )
        .unwrap();
        assert!(
            out.overflow[0] < 0.1,
            "{:?} after {}",
            out.overflow,
            out.iterations
        );
        // Few on the dead column x = 7.
        let dead = out
            .centres
            .iter()
            .filter(|&&(x, _)| (7.0..8.0).contains(&x))
            .count();
        assert!(dead < 20, "{dead} on the dead column");
    }

    #[test]
    fn the_wa_gradient_matches_a_difference_quotient() {
        let xs = [1.0f32, 3.5, 2.0, 7.25];
        let gamma = 0.8;
        let sums = WaSums::of(xs.iter().copied(), gamma);
        let g: Vec<f32> = xs.iter().map(|&x| sums.grad(x, gamma)).collect();
        let wa = |xs: &[f32]| -> f64 {
            let gamma = gamma as f64;
            let (mut sp, mut sxp, mut sn, mut sxn) = (0.0, 0.0, 0.0, 0.0);
            for &x in xs {
                let x = x as f64;
                sp += (x / gamma).exp();
                sxp += x * (x / gamma).exp();
                sn += (-x / gamma).exp();
                sxn += x * (-x / gamma).exp();
            }
            sxp / sp - sxn / sn
        };
        for k in 0..4 {
            let mut a = xs;
            let mut b = xs;
            a[k] += 1e-3;
            b[k] -= 1e-3;
            let num = (wa(&a) - wa(&b)) / 2e-3;
            assert!(
                (num - g[k] as f64).abs() < 1e-3,
                "pin {k}: {num} against {}",
                g[k]
            );
        }
    }

    /// Needs a CUDA device; passes (saying so) without one.
    #[cfg(feature = "gpu")]
    #[test]
    fn the_gpu_repeats_itself_and_lands_near_the_cpu() {
        let p = problem();
        let opts = |accel| Options {
            accel,
            ..Options::default()
        };
        let gpu = Accel::Gpu(crate::gpu::Runtime::Cuda);
        let a = match place(&p, &mut NoTiming, None, &opts(gpu)) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("no GPU to test on: {e}");
                return;
            }
        };
        let b = place(&p, &mut NoTiming, None, &opts(gpu)).unwrap();
        assert_eq!(a.centres, b.centres, "two GPU runs differ");
        let cpu = place(&p, &mut NoTiming, None, &opts(Accel::Threads)).unwrap();
        let (hg, hc) = (p.hpwl(&a.centres), p.hpwl(&cpu.centres));
        assert!(
            (hg - hc).abs() < 0.1 * hc,
            "GPU hpwl {hg} against the CPU's {hc}"
        );
        assert!(a.overflow[0] < 0.1, "{:?}", a.overflow);
    }
}
