//! The electrostatic placer's engine on a GPU (CubeCL: CUDA, or HIP on
//! AMD). The problem and every vector stay on the device for the whole
//! placement; per iteration the host uploads a few scalars and reads back
//! one buffer of partial sums.
//!
//! Same arithmetic and structure as the CPU engine: per net, per pin, per
//! object and per bin kernels, density accumulated in fixed point (integer
//! atomics, exact in any order), partial sums in fixed chunks added on the
//! host in order. So a run repeats exactly on the same GPU, driver and
//! binary. It doesn't match the CPU bit for bit: the GPU's `exp` and fused
//! multiply-adds round differently (docs/gpu-pnr.md, "Determinism").

use cubecl::prelude::*;
use cubecl::server::Handle;

use crate::electro::{Dct, Engine, Eval, Flat};
use crate::Pos;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    Cuda,
    Hip,
}

const UNITS: u32 = 256;
/// Partial sums: each covers this many items.
const CHUNK: u32 = 1024;

/// `params`: gamma, then alpha, coef, then lambda per resource.
const P_GAMMA: usize = 0;
const P_ALPHA: usize = 1;
const P_COEF: usize = 2;
const P_LAMBDA: usize = 3;

#[cube(launch_unchecked)]
fn pin_pos(
    pin_obj: &[u32],
    pin_dx: &[f32],
    pin_dy: &[f32],
    vx: &[f32],
    vy: &[f32],
    px: &mut [f32],
    py: &mut [f32],
) {
    let k = ABSOLUTE_POS;
    if k < px.len() {
        let o = pin_obj[k];
        if o == u32::MAX {
            px[k] = pin_dx[k];
            py[k] = pin_dy[k];
        } else {
            px[k] = vx[o as usize] + pin_dx[k];
            py[k] = vy[o as usize] + pin_dy[k];
        }
    }
}

/// Per net and axis: lo, hi, sp, sxp, sn, sxn (`electro::WaSums`), in
/// `sums[(net * 2 + axis) * 6 ..]`.
#[cube(launch_unchecked)]
fn net_sums(net_start: &[u32], px: &[f32], py: &[f32], params: &[f32], sums: &mut [f32]) {
    let t = ABSOLUTE_POS;
    let nets = net_start.len() - 1;
    if t < nets * 2 {
        let i = t / 2;
        let axis = t % 2;
        let gamma = params[0];
        let s = net_start[i] as usize;
        let e = net_start[i + 1] as usize;
        let mut lo = f32::new(3.0e38f32);
        let mut hi = f32::new(-3.0e38f32);
        for k in s..e {
            let mut c = px[k];
            if axis == 1 {
                c = py[k];
            }
            lo = f32::min(lo, c);
            hi = f32::max(hi, c);
        }
        let mut sp = 0.0f32;
        let mut sxp = 0.0f32;
        let mut sn = 0.0f32;
        let mut sxn = 0.0f32;
        for k in s..e {
            let mut c = px[k];
            if axis == 1 {
                c = py[k];
            }
            let ep = f32::exp((c - hi) / gamma);
            let en = f32::exp((lo - c) / gamma);
            sp += ep;
            sxp += c * ep;
            sn += en;
            sxn += c * en;
        }
        let b = t * 6;
        sums[b] = lo;
        sums[b + 1] = hi;
        sums[b + 2] = sp;
        sums[b + 3] = sxp;
        sums[b + 4] = sn;
        sums[b + 5] = sxn;
    }
}

/// Per pin: its weighted wirelength gradient.
#[cube(launch_unchecked)]
fn pin_grad(
    net_start: &[u32],
    pin_net: &[u32],
    px: &[f32],
    py: &[f32],
    sums: &[f32],
    weight: &[f32],
    params: &[f32],
    gx: &mut [f32],
    gy: &mut [f32],
) {
    let k = ABSOLUTE_POS;
    if k < gx.len() {
        let i = pin_net[k] as usize;
        let gamma = params[0];
        let degree = net_start[i + 1] - net_start[i];
        if degree < 2 {
            gx[k] = 0.0;
            gy[k] = 0.0;
        } else {
            for axis in 0..2usize {
                let b = (i * 2 + axis) * 6;
                let lo = sums[b];
                let hi = sums[b + 1];
                let sp = sums[b + 2];
                let sxp = sums[b + 3];
                let sn = sums[b + 4];
                let sxn = sums[b + 5];
                let mut c = px[k];
                if axis == 1 {
                    c = py[k];
                }
                let wp = sxp / sp;
                let wn = sxn / sn;
                let ep = f32::exp((c - hi) / gamma);
                let en = f32::exp((lo - c) / gamma);
                let g = (ep * (1.0 + (c - wp) / gamma) / sp - en * (1.0 - (c - wn) / gamma) / sn)
                    * weight[i];
                if axis == 0 {
                    gx[k] = g;
                } else {
                    gy[k] = g;
                }
            }
        }
    }
}

#[cube(launch_unchecked)]
fn zero_u64(a: &mut [u64]) {
    if ABSOLUTE_POS < a.len() {
        a[ABSOLUTE_POS] = 0u64;
    }
}

/// Per object: its demand spread over the bins its footprint overlaps,
/// in fixed point; `acc[r * bins + b]` all objects, `acc[(rc + r) * bins
/// + b]` real ones.
#[cube(launch_unchecked)]
fn scatter(
    vx: &[f32],
    vy: &[f32],
    half_h: &[f32],
    demand: &[f32],
    acc: &[Atomic<u64>],
    #[comptime] w: usize,
    #[comptime] h: usize,
    #[comptime] rc: usize,
    #[comptime] n_real: usize,
) {
    let o = ABSOLUTE_POS;
    if o < vx.len() {
        let bins = w * h;
        let cx = vx[o];
        let cy = vy[o];
        let hh = half_h[o];
        let area = 2.0 * hh;
        let x0 = cx - 0.5;
        let fbx = f32::floor(x0);
        let fx = x0 - fbx;
        let bx0 = fbx as i32;
        let y0 = cy - hh;
        let y1 = cy + hh;
        let by0 = f32::floor(y0) as i32;
        let mut by1 = f32::ceil(y1) as i32;
        if by1 < by0 + 1 {
            by1 = by0 + 1;
        }
        for r in 0..rc {
            let d = demand[o * rc + r];
            if d != 0.0 {
                for by in by0..by1 {
                    if by >= 0 && by < h as i32 {
                        let oy =
                            f32::max(f32::min(y1, by as f32 + 1.0) - f32::max(y0, by as f32), 0.0);
                        if oy > 0.0 {
                            for side in 0..2i32 {
                                let bx = bx0 + side;
                                let mut ox = 1.0 - fx;
                                if side == 1 {
                                    ox = fx;
                                }
                                if bx >= 0 && bx < w as i32 && ox > 0.0 {
                                    let b = by as usize * w + bx as usize;
                                    let q = f32::round(d * ox * oy / area * 16777216.0) as u64;
                                    acc[r * bins + b].fetch_add(q);
                                    if o < n_real {
                                        acc[(rc + r) * bins + b].fetch_add(q);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// `rho[r * bins + b]` from the accumulators plus the background.
#[cube(launch_unchecked)]
fn rho(acc: &[u64], background: &[f32], rho: &mut [f32]) {
    let i = ABSOLUTE_POS;
    if i < rho.len() {
        rho[i] = (acc[i] as f64 / 16777216.0) as f32 + background[i];
    }
}

/// Per resource r: `out_r = a_r b_r` (m x k times k x n), where a
/// stride or b stride of 0 shares the matrix between resources.
#[cube(launch_unchecked)]
fn mm(
    a: &[f32],
    b: &[f32],
    out: &mut [f32],
    #[comptime] m: usize,
    #[comptime] k: usize,
    #[comptime] n: usize,
    #[comptime] a_stride: usize,
    #[comptime] b_stride: usize,
) {
    let t = ABSOLUTE_POS;
    if t < out.len() {
        let r = t / (m * n);
        let ij = t % (m * n);
        let i = ij / n;
        let j = ij % n;
        let mut acc = 0.0f32;
        for kk in 0..k {
            acc += a[r * a_stride + i * k + kk] * b[r * b_stride + kk * n + j];
        }
        out[t] = acc;
    }
}

/// `bx = a * kx`, `by = a * ky`, per resource.
#[cube(launch_unchecked)]
fn coeff(
    a: &[f32],
    kx: &[f32],
    ky: &[f32],
    bx: &mut [f32],
    by: &mut [f32],
    #[comptime] bins: usize,
) {
    let t = ABSOLUTE_POS;
    if t < bx.len() {
        let i = t % bins;
        bx[t] = a[t] * kx[i];
        by[t] = a[t] * ky[i];
    }
}

/// Per resource and chunk of bins: the real demand above the limit.
#[cube(launch_unchecked)]
fn overflow(
    acc: &[u64],
    limit: &[f32],
    out: &mut [f64],
    #[comptime] bins: usize,
    #[comptime] rc: usize,
) {
    let t = ABSOLUTE_POS;
    let chunks = (bins + 1023) / 1024;
    if t < rc * chunks {
        let r = t / chunks;
        let c = t % chunks;
        let s = c * 1024;
        let mut e = s + 1024;
        if e > bins {
            e = bins;
        }
        let mut over = 0.0f64;
        for b in s..e {
            let d = acc[(rc + r) * bins + b] as f64 / 16777216.0;
            let l = limit[r * bins + b] as f64;
            if d > l {
                over += d - l;
            }
        }
        out[t] = over;
    }
}

/// Per object: the preconditioned gradient (into gx, gy) and the
/// wirelength gradient's L1 norm (into wl).
#[cube(launch_unchecked)]
fn object_grad(
    obj_pin_start: &[u32],
    obj_pins: &[u32],
    pgx: &[f32],
    pgy: &[f32],
    vx: &[f32],
    vy: &[f32],
    half_h: &[f32],
    demand: &[f32],
    ex: &[f32],
    ey: &[f32],
    wl_degree: &[f32],
    params: &[f32],
    gx: &mut [f32],
    gy: &mut [f32],
    wl: &mut [f32],
    #[comptime] w: usize,
    #[comptime] h: usize,
    #[comptime] rc: usize,
) {
    let o = ABSOLUTE_POS;
    if o < gx.len() {
        let bins = w * h;
        let mut gwx = 0.0f32;
        let mut gwy = 0.0f32;
        for k in obj_pin_start[o]..obj_pin_start[o + 1] {
            let p = obj_pins[k as usize] as usize;
            gwx += pgx[p];
            gwy += pgy[p];
        }
        let cx = vx[o];
        let cy = vy[o];
        let hh = half_h[o];
        let x0 = cx - 0.5;
        let fbx = f32::floor(x0);
        let fx = x0 - fbx;
        let bx0 = fbx as i32;
        let y0 = cy - hh;
        let y1 = cy + hh;
        let by0 = f32::floor(y0) as i32;
        let mut by1 = f32::ceil(y1) as i32;
        if by1 < by0 + 1 {
            by1 = by0 + 1;
        }
        let mut gdx = 0.0f32;
        let mut gdy = 0.0f32;
        let mut charge = 0.0f32;
        for r in 0..rc {
            let d = demand[o * rc + r];
            if d != 0.0 {
                let mut sx = 0.0f32;
                let mut sy = 0.0f32;
                let mut so = 0.0f32;
                for by in by0..by1 {
                    if by >= 0 && by < h as i32 {
                        let oy =
                            f32::max(f32::min(y1, by as f32 + 1.0) - f32::max(y0, by as f32), 0.0);
                        if oy > 0.0 {
                            for side in 0..2i32 {
                                let bx = bx0 + side;
                                let mut ox = 1.0 - fx;
                                if side == 1 {
                                    ox = fx;
                                }
                                if bx >= 0 && bx < w as i32 && ox > 0.0 {
                                    let b = by as usize * w + bx as usize;
                                    let ov = ox * oy;
                                    sx += ex[r * bins + b] * ov;
                                    sy += ey[r * bins + b] * ov;
                                    so += ov;
                                }
                            }
                        }
                    }
                }
                let lam = params[3 + r];
                gdx += lam * (-d * sx / f32::max(so, 1e-6));
                gdy += lam * (-d * sy / f32::max(so, 1e-6));
                charge += lam * d;
            }
        }
        let pre = f32::max(wl_degree[o] + charge, 1.0);
        gx[o] = (gwx + gdx) / pre;
        gy[o] = (gwy + gdy) / pre;
        wl[o] = f32::abs(gwx) + f32::abs(gwy);
    }
}

/// Per chunk of objects: [|g_wl|, |dv|^2, |dg|^2] partial sums; per chunk
/// of nets: the HPWL partial sum.
#[cube(launch_unchecked)]
fn norms(
    wl: &[f32],
    vx: &[f32],
    vy: &[f32],
    px: &[f32],
    py: &[f32],
    gx: &[f32],
    gy: &[f32],
    qx: &[f32],
    qy: &[f32],
    sums: &[f32],
    out: &mut [f64],
    #[comptime] n: usize,
    #[comptime] nets: usize,
) {
    let t = ABSOLUTE_POS;
    let obj_chunks = (n + 1023) / 1024;
    let net_chunks = (nets + 1023) / 1024;
    if t < obj_chunks {
        let s = t * 1024;
        let mut e = s + 1024;
        if e > n {
            e = n;
        }
        let mut a = 0.0f64;
        let mut b = 0.0f64;
        let mut c = 0.0f64;
        for o in s..e {
            a += wl[o] as f64;
            let dx = (vx[o] - px[o]) as f64;
            let dy = (vy[o] - py[o]) as f64;
            b += dx * dx + dy * dy;
            let ex = (gx[o] - qx[o]) as f64;
            let ey = (gy[o] - qy[o]) as f64;
            c += ex * ex + ey * ey;
        }
        out[t * 3] = a;
        out[t * 3 + 1] = b;
        out[t * 3 + 2] = c;
    } else if t < obj_chunks + net_chunks {
        let ci = t - obj_chunks;
        let s = ci * 1024;
        let mut e = s + 1024;
        if e > nets {
            e = nets;
        }
        let mut a = 0.0f64;
        for i in s..e {
            let bx = i * 12;
            a += (sums[bx + 1] - sums[bx] + sums[bx + 7] - sums[bx + 6]) as f64;
        }
        out[obj_chunks * 3 + ci] = a;
    }
}

/// The Nesterov step for every object, each kept on the grid.
#[cube(launch_unchecked)]
fn step(
    ux: &[f32],
    uy: &[f32],
    vx: &[f32],
    vy: &[f32],
    gx: &[f32],
    gy: &[f32],
    half_h: &[f32],
    params: &[f32],
    nux: &mut [f32],
    nuy: &mut [f32],
    nvx: &mut [f32],
    nvy: &mut [f32],
    #[comptime] w: usize,
    #[comptime] h: usize,
) {
    let o = ABSOLUTE_POS;
    if o < ux.len() {
        let alpha = params[1];
        let coef = params[2];
        let hh = half_h[o];
        let xmax = w as f32 - 0.5;
        let ymax = h as f32 - hh;
        let nx = f32::clamp(vx[o] - alpha * gx[o], 0.5, xmax);
        let ny = f32::clamp(vy[o] - alpha * gy[o], hh, ymax);
        nux[o] = nx;
        nuy[o] = ny;
        nvx[o] = f32::clamp(nx + coef * (nx - ux[o]), 0.5, xmax);
        nvy[o] = f32::clamp(ny + coef * (ny - uy[o]), hh, ymax);
    }
}

fn count(n: usize) -> CubeCount {
    CubeCount::Static((n as u32).div_ceil(UNITS).max(1), 1, 1)
}

pub struct Gpu {
    client: cubecl::client::Client,
    f: Flat,
    // The problem.
    pin_obj: Handle,
    pin_dx: Handle,
    pin_dy: Handle,
    pin_net: Handle,
    net_start: Handle,
    obj_pin_start: Handle,
    obj_pins: Handle,
    half_h: Handle,
    demand: Handle,
    background: Handle,
    limit: Handle,
    dct: [Handle; 8],
    // The state.
    ux: Handle,
    uy: Handle,
    vx: Handle,
    vy: Handle,
    vx_prev: Handle,
    vy_prev: Handle,
    gx: Handle,
    gy: Handle,
    gx_prev: Handle,
    gy_prev: Handle,
    weight: Handle,
    wl_degree: Handle,
    // Scratch.
    px: Handle,
    py: Handle,
    sums: Handle,
    pgx: Handle,
    pgy: Handle,
    acc: Handle,
    rho: Handle,
    t1: Handle,
    a: Handle,
    bx: Handle,
    by: Handle,
    t2: Handle,
    t3: Handle,
    ex: Handle,
    ey: Handle,
    wl: Handle,
    partial: Handle,
    over: Handle,
    evaluated: usize,
}

impl Gpu {
    /// Upload `f` and the start. Errs when the runtime has no device.
    pub fn new(rt: Runtime, f: Flat, weights: &[f32], start: &[Pos]) -> Result<Gpu, String> {
        let device = match rt {
            Runtime::Cuda => cubecl::Device::cuda(0),
            Runtime::Hip => cubecl::Device::rocm(0),
        }
        .map_err(|e| format!("{rt:?}: {e:?}"))?;
        let client = device.client();
        let up_u32 = |v: &[u32]| client.create_from_slice(u32::as_bytes(v));
        let up_f32 = |v: &[f32]| client.create_from_slice(f32::as_bytes(v));
        let zeros = |n: usize| client.create_from_slice(f32::as_bytes(&vec![0.0f32; n.max(1)]));
        let dct = Dct::new(f.width, f.height);
        let (ux, uy): (Vec<f32>, Vec<f32>) = start.iter().copied().unzip();
        let (n, pins, nets, rc, bins) = (
            f.n,
            f.pin_obj.len(),
            f.net_start.len() - 1,
            f.resources,
            f.width * f.height,
        );
        let partials = n.div_ceil(CHUNK as usize) * 3 + nets.div_ceil(CHUNK as usize);
        let mut g = Gpu {
            pin_obj: up_u32(&f.pin_obj),
            pin_dx: up_f32(&f.pin_dx),
            pin_dy: up_f32(&f.pin_dy),
            pin_net: up_u32(&f.pin_net),
            net_start: up_u32(&f.net_start),
            obj_pin_start: up_u32(&f.obj_pin_start),
            obj_pins: up_u32(if f.obj_pins.is_empty() {
                &[0]
            } else {
                &f.obj_pins
            }),
            half_h: up_f32(&f.half_h),
            demand: up_f32(&f.demand),
            background: up_f32(&f.background),
            limit: up_f32(&f.limit),
            dct: [
                up_f32(&dct.cos_x),
                up_f32(&dct.sin_x),
                up_f32(&dct.cos_x_t),
                up_f32(&dct.cos_y),
                up_f32(&dct.cos_y_t),
                up_f32(&dct.sin_y_t),
                up_f32(&dct.kx),
                up_f32(&dct.ky),
            ],
            ux: up_f32(&ux),
            uy: up_f32(&uy),
            vx: up_f32(&ux),
            vy: up_f32(&uy),
            vx_prev: up_f32(&ux),
            vy_prev: up_f32(&uy),
            gx: zeros(n),
            gy: zeros(n),
            gx_prev: zeros(n),
            gy_prev: zeros(n),
            weight: up_f32(weights),
            wl_degree: zeros(n),
            px: zeros(pins),
            py: zeros(pins),
            sums: zeros(nets * 12),
            pgx: zeros(pins),
            pgy: zeros(pins),
            acc: client.create_from_slice(u64::as_bytes(&vec![0u64; 2 * rc * bins])),
            rho: zeros(rc * bins),
            t1: zeros(rc * bins),
            a: zeros(rc * bins),
            bx: zeros(rc * bins),
            by: zeros(rc * bins),
            t2: zeros(rc * bins),
            t3: zeros(rc * bins),
            ex: zeros(rc * bins),
            ey: zeros(rc * bins),
            wl: zeros(n),
            partial: client.create_from_slice(f64::as_bytes(&vec![0.0f64; partials])),
            over: client.create_from_slice(f64::as_bytes(&vec![
                0.0f64;
                rc * bins.div_ceil(CHUNK as usize)
            ])),
            evaluated: 0,
            client,
            f,
        };
        g.set_weights(weights);
        Ok(g)
    }

    fn read_f32(&self, h: &Handle) -> Vec<f32> {
        let bytes = self
            .client
            .read_one(h.clone())
            .expect("reading from the GPU");
        f32::from_bytes(&bytes).to_vec()
    }
}

/// A buffer argument over `h` holding `len` elements.
fn arg(h: &Handle, len: usize) -> BufferArg {
    unsafe { BufferArg::from_raw_parts(h.clone(), len.max(1)) }
}

impl Engine for Gpu {
    fn evaluate(&mut self, gamma: f32, lambda: &[f32]) -> Eval {
        let f = &self.f;
        let (w, h, rc) = (f.width, f.height, f.resources);
        let bins = w * h;
        let (n, pins, nets) = (f.n, f.pin_obj.len(), f.net_start.len() - 1);
        let mut p = vec![0.0f32; P_LAMBDA + rc];
        p[P_GAMMA] = gamma;
        p[P_LAMBDA..].copy_from_slice(lambda);
        let params = self.client.create_from_slice(f32::as_bytes(&p));
        let c = &self.client;
        let dim = CubeDim::new_1d(UNITS);
        std::mem::swap(&mut self.gx, &mut self.gx_prev);
        std::mem::swap(&mut self.gy, &mut self.gy_prev);
        unsafe {
            pin_pos::launch_unchecked(
                c,
                count(pins),
                dim,
                arg(&self.pin_obj, pins),
                arg(&self.pin_dx, pins),
                arg(&self.pin_dy, pins),
                arg(&self.vx, n),
                arg(&self.vy, n),
                arg(&self.px, pins),
                arg(&self.py, pins),
            );
            net_sums::launch_unchecked(
                c,
                count(nets * 2),
                dim,
                arg(&self.net_start, nets + 1),
                arg(&self.px, pins),
                arg(&self.py, pins),
                arg(&params, p.len()),
                arg(&self.sums, nets * 12),
            );
            pin_grad::launch_unchecked(
                c,
                count(pins),
                dim,
                arg(&self.net_start, nets + 1),
                arg(&self.pin_net, pins),
                arg(&self.px, pins),
                arg(&self.py, pins),
                arg(&self.sums, nets * 12),
                arg(&self.weight, nets),
                arg(&params, p.len()),
                arg(&self.pgx, pins),
                arg(&self.pgy, pins),
            );
            zero_u64::launch_unchecked(c, count(2 * rc * bins), dim, arg(&self.acc, 2 * rc * bins));
            scatter::launch_unchecked(
                c,
                count(n),
                dim,
                arg(&self.vx, n),
                arg(&self.vy, n),
                arg(&self.half_h, n),
                arg(&self.demand, n * rc),
                arg(&self.acc, 2 * rc * bins),
                w,
                h,
                rc,
                f.n_real,
            );
            rho::launch_unchecked(
                c,
                count(rc * bins),
                dim,
                arg(&self.acc, rc * bins),
                arg(&self.background, rc * bins),
                arg(&self.rho, rc * bins),
            );
            // a = cos_y rho cos_x^T; ex = cos_y^T (a kx) sin_x; ey = sin_y^T (a ky) cos_x.
            let [cos_x, sin_x, cos_x_t, cos_y, cos_y_t, sin_y_t, kx, ky] = &self.dct;
            let rb = rc * bins;
            mm::launch_unchecked(
                c,
                count(rb),
                dim,
                arg(&self.rho, rb),
                arg(cos_x_t, w * w),
                arg(&self.t1, rb),
                h,
                w,
                w,
                bins,
                0,
            );
            mm::launch_unchecked(
                c,
                count(rb),
                dim,
                arg(cos_y, h * h),
                arg(&self.t1, rb),
                arg(&self.a, rb),
                h,
                h,
                w,
                0,
                bins,
            );
            coeff::launch_unchecked(
                c,
                count(rb),
                dim,
                arg(&self.a, rb),
                arg(kx, bins),
                arg(ky, bins),
                arg(&self.bx, rb),
                arg(&self.by, rb),
                bins,
            );
            mm::launch_unchecked(
                c,
                count(rb),
                dim,
                arg(&self.bx, rb),
                arg(sin_x, w * w),
                arg(&self.t2, rb),
                h,
                w,
                w,
                bins,
                0,
            );
            mm::launch_unchecked(
                c,
                count(rb),
                dim,
                arg(cos_y_t, h * h),
                arg(&self.t2, rb),
                arg(&self.ex, rb),
                h,
                h,
                w,
                0,
                bins,
            );
            mm::launch_unchecked(
                c,
                count(rb),
                dim,
                arg(&self.by, rb),
                arg(cos_x, w * w),
                arg(&self.t3, rb),
                h,
                w,
                w,
                bins,
                0,
            );
            mm::launch_unchecked(
                c,
                count(rb),
                dim,
                arg(sin_y_t, h * h),
                arg(&self.t3, rb),
                arg(&self.ey, rb),
                h,
                h,
                w,
                0,
                bins,
            );
            let over_len = rc * bins.div_ceil(CHUNK as usize);
            overflow::launch_unchecked(
                c,
                count(over_len),
                dim,
                arg(&self.acc, 2 * rb),
                arg(&self.limit, rb),
                arg(&self.over, over_len),
                bins,
                rc,
            );
            object_grad::launch_unchecked(
                c,
                count(n),
                dim,
                arg(&self.obj_pin_start, n + 1),
                arg(&self.obj_pins, f.obj_pins.len()),
                arg(&self.pgx, pins),
                arg(&self.pgy, pins),
                arg(&self.vx, n),
                arg(&self.vy, n),
                arg(&self.half_h, n),
                arg(&self.demand, n * rc),
                arg(&self.ex, rb),
                arg(&self.ey, rb),
                arg(&self.wl_degree, n),
                arg(&params, p.len()),
                arg(&self.gx, n),
                arg(&self.gy, n),
                arg(&self.wl, n),
                w,
                h,
                rc,
            );
            let parts = n.div_ceil(CHUNK as usize) * 3 + nets.div_ceil(CHUNK as usize);
            norms::launch_unchecked(
                c,
                count(parts),
                dim,
                arg(&self.wl, n),
                arg(&self.vx, n),
                arg(&self.vy, n),
                arg(&self.vx_prev, n),
                arg(&self.vy_prev, n),
                arg(&self.gx, n),
                arg(&self.gy, n),
                arg(&self.gx_prev, n),
                arg(&self.gy_prev, n),
                arg(&self.sums, nets * 12),
                arg(&self.partial, parts),
                n,
                nets,
            );
        }
        // Both partial sums in one transfer: the iteration's only wait.
        let read = self
            .client
            .read(vec![self.partial.clone(), self.over.clone()])
            .expect("reading from the GPU");
        let (partial, over) = (
            f64::from_bytes(&read[0]).to_vec(),
            f64::from_bytes(&read[1]).to_vec(),
        );
        let chunks = n.div_ceil(CHUNK as usize);
        let (mut wl_l1, mut dv2, mut dg2) = (0.0, 0.0, 0.0);
        for t in 0..chunks {
            wl_l1 += partial[t * 3];
            dv2 += partial[t * 3 + 1];
            dg2 += partial[t * 3 + 2];
        }
        let hpwl: f64 = partial[chunks * 3..].iter().sum();
        let bin_chunks = bins.div_ceil(CHUNK as usize);
        let overflow: Vec<f32> = (0..rc)
            .map(|r| {
                if f.real_demand[r] <= 0.0 {
                    return 0.0;
                }
                (over[r * bin_chunks..(r + 1) * bin_chunks]
                    .iter()
                    .sum::<f64>()
                    / f.real_demand[r]) as f32
            })
            .collect();
        // The density norms size the first weights only: computed on the
        // host from the device's field, once.
        let density_l1 = if self.evaluated == 0 {
            let (vx, vy) = (self.read_f32(&self.vx), self.read_f32(&self.vy));
            let (ex, ey) = (self.read_f32(&self.ex), self.read_f32(&self.ey));
            crate::electro::density_l1(f, &vx, &vy, &ex, &ey)
        } else {
            vec![0.0; rc]
        };
        if self.evaluated == 0 {
            dv2 = 0.0;
            dg2 = 0.0;
        }
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
        let f = &self.f;
        let n = f.n;
        let mut p = vec![0.0f32; P_LAMBDA + f.resources];
        p[P_ALPHA] = alpha;
        p[P_COEF] = coef;
        let params = self.client.create_from_slice(f32::as_bytes(&p));
        // The new major point goes into the old reference's buffers, which
        // become the previous reference.
        let (nux, nuy) = (self.client.empty(n * 4), self.client.empty(n * 4));
        let (nvx, nvy) = (self.vx_prev.clone(), self.vy_prev.clone());
        unsafe {
            step::launch_unchecked(
                &self.client,
                count(n),
                CubeDim::new_1d(UNITS),
                arg(&self.ux, n),
                arg(&self.uy, n),
                arg(&self.vx, n),
                arg(&self.vy, n),
                arg(&self.gx, n),
                arg(&self.gy, n),
                arg(&self.half_h, n),
                arg(&params, p.len()),
                arg(&nux, n),
                arg(&nuy, n),
                arg(&nvx, n),
                arg(&nvy, n),
                f.width,
                f.height,
            );
        }
        self.vx_prev = std::mem::replace(&mut self.vx, nvx);
        self.vy_prev = std::mem::replace(&mut self.vy, nvy);
        self.ux = nux;
        self.uy = nuy;
    }

    fn positions(&mut self) -> Vec<Pos> {
        let (ux, uy) = (self.read_f32(&self.ux), self.read_f32(&self.uy));
        (0..self.f.n_real).map(|o| (ux[o], uy[o])).collect()
    }

    fn set_weights(&mut self, weights: &[f32]) {
        self.weight = self.client.create_from_slice(f32::as_bytes(weights));
        let f = &self.f;
        let deg: Vec<f32> = (0..f.n)
            .map(|o| {
                (f.obj_pin_start[o]..f.obj_pin_start[o + 1])
                    .map(|k| weights[f.pin_net[f.obj_pins[k as usize] as usize] as usize])
                    .sum()
            })
            .collect();
        self.wl_degree = self.client.create_from_slice(f32::as_bytes(&deg));
    }
}
