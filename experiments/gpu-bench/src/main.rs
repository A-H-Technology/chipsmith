//! The arithmetic a GPU could take off chipsmith's placer and router,
//! timed on the GPU (CubeCL: CUDA, or HIP on AMD) and on the CPU, at the
//! sizes chipsmith's designs have and beyond, so the verdict rests on
//! numbers:
//!
//! - one conjugate-gradient iteration of the analytic placer's quadratic
//!   solve (`chipsmith-place`): a CSR matrix-vector product, three dot
//!   products, three vector updates. Wirelength and density gradients of
//!   an ePlace-style placer have the same shape (a segmented sum over
//!   nets' pins, an elementwise pass over bins).
//! - PathFinder's per-iteration congestion update over every routing node
//!   (`chipsmith-route`): history cost and overuse count. The searches
//!   that fill the occupancy run on the CPU, so the GPU version pays for
//!   copying it over each iteration.
//!
//! `gpu-bench [cuda|hip]` prints one table row per size; `gpu-bench route
//! <dump> [cuda|hip]` times a router's searches (`route.rs`).

use std::time::Instant;

use cubecl::prelude::*;
use rayon::prelude::*;

mod route;

const UNITS: u32 = 256;

#[cube(launch_unchecked)]
fn spmv(row_start: &[u32], col: &[u32], val: &[f32], diag: &[f32], x: &[f32], y: &mut [f32]) {
    let i = ABSOLUTE_POS;
    if i < y.len() {
        let mut acc = diag[i] * x[i];
        let s = row_start[i] as usize;
        let e = row_start[i + 1] as usize;
        for k in s..e {
            acc -= val[k] * x[col[k] as usize];
        }
        y[i] = acc;
    }
}

/// Each unit sums its own contiguous chunk; `finish` adds the partials.
#[cube(launch_unchecked)]
fn dot_partial(a: &[f32], b: &[f32], partial: &mut [f32]) {
    let t = ABSOLUTE_POS;
    if t < partial.len() {
        let n = a.len();
        let chunk = (n + partial.len() - 1) / partial.len();
        let s = t * chunk;
        let mut e = s + chunk;
        if e > n {
            e = n;
        }
        let mut acc = 0.0f32;
        for k in s..e {
            acc += a[k] * b[k];
        }
        partial[t] = acc;
    }
}

#[cube(launch_unchecked)]
fn finish(partial: &[f32], out: &mut [f32]) {
    if ABSOLUTE_POS == 0 {
        let mut acc = 0.0f32;
        for k in 0..partial.len() {
            acc += partial[k];
        }
        out[0] = acc;
    }
}

/// `y += (num[0] / den[0]) * x`: the scalar stays on the device.
#[cube(launch_unchecked)]
fn axpy(num: &[f32], den: &[f32], x: &[f32], y: &mut [f32]) {
    let i = ABSOLUTE_POS;
    if i < y.len() {
        y[i] += num[0] / den[0] * x[i];
    }
}

/// PathFinder: history grows by each node's overuse; overused nodes are
/// counted per unit's chunk.
#[cube(launch_unchecked)]
fn congestion(occ: &[u32], hist: &mut [f32], over: &mut [u32]) {
    let t = ABSOLUTE_POS;
    if t < over.len() {
        let n = occ.len();
        let chunk = (n + over.len() - 1) / over.len();
        let s = t * chunk;
        let mut e = s + chunk;
        if e > n {
            e = n;
        }
        let mut count = 0u32;
        for k in s..e {
            let o = occ[k];
            if o > 1 {
                hist[k] += (o - 1) as f32;
                count += 1;
            }
        }
        over[t] = count;
    }
}

/// A banded random CSR matrix, `per_row` entries a row, like the
/// placer's: each object connects to others nearby in index order.
struct Csr {
    row_start: Vec<u32>,
    col: Vec<u32>,
    val: Vec<f32>,
    diag: Vec<f32>,
}

fn csr(n: usize, per_row: usize) -> Csr {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut row_start = vec![0u32];
    let (mut col, mut val) = (Vec::new(), Vec::new());
    for i in 0..n {
        for _ in 0..per_row {
            let off = (next() % 2001) as i64 - 1000;
            col.push((i as i64 + off).clamp(0, n as i64 - 1) as u32);
            val.push(0.5 / per_row as f32);
        }
        row_start.push(col.len() as u32);
    }
    Csr {
        row_start,
        col,
        val,
        diag: vec![1.0; n],
    }
}

fn cpu_spmv(m: &Csr, x: &[f32], y: &mut [f32], par: bool) {
    let row = |i: usize| {
        let mut acc = m.diag[i] * x[i];
        for k in m.row_start[i] as usize..m.row_start[i + 1] as usize {
            acc -= m.val[k] * x[m.col[k] as usize];
        }
        acc
    };
    if par {
        y.par_iter_mut().enumerate().for_each(|(i, y)| *y = row(i));
    } else {
        y.iter_mut().enumerate().for_each(|(i, y)| *y = row(i));
    }
}

fn cpu_dot(a: &[f32], b: &[f32], par: bool) -> f32 {
    if par {
        a.par_iter().zip(b).map(|(x, y)| x * y).sum()
    } else {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }
}

fn cpu_axpy(a: f32, x: &[f32], y: &mut [f32], par: bool) {
    if par {
        y.par_iter_mut().zip(x).for_each(|(y, x)| *y += a * x);
    } else {
        y.iter_mut().zip(x).for_each(|(y, x)| *y += a * x);
    }
}

/// One CG iteration's arithmetic on the CPU (results are discarded; the
/// point is the time).
fn cpu_cg_iteration(m: &Csr, v: &mut [Vec<f32>; 4], par: bool) {
    let [p, q, r, x] = v;
    cpu_spmv(m, p, q, par);
    let pq = cpu_dot(p, q, par);
    let a = 1e-3 / pq.max(1e-6);
    cpu_axpy(a, p, x, par);
    cpu_axpy(-a, q, r, par);
    let rr = cpu_dot(r, r, par);
    let rz = cpu_dot(r, q, par);
    cpu_axpy(rz / rr.max(1e-6) * 1e-3, r, p, par);
}

fn time<F: FnMut()>(reps: usize, mut f: F) -> f64 {
    f();
    let t = Instant::now();
    for _ in 0..reps {
        f();
    }
    t.elapsed().as_secs_f64() / reps as f64 * 1e6
}

fn count(n: usize) -> CubeCount {
    CubeCount::Static((n as u32).div_ceil(UNITS).max(1), 1, 1)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("route") {
        let device = match args.get(3).map(String::as_str) {
            Some("hip") => cubecl::Device::Hip(Default::default()),
            _ => cubecl::Device::Cuda(Default::default()),
        };
        route::run(args.get(2).expect("gpu-bench route <dump> [cuda|hip]"), device);
        return;
    }
    let device = match std::env::args().nth(1).as_deref() {
        Some("hip") => cubecl::Device::Hip(Default::default()),
        _ => cubecl::Device::Cuda(Default::default()),
    };
    let client = device.client();
    let sync = || cubecl::future::block_on(client.sync()).unwrap();
    let dim = CubeDim::new_1d(UNITS);
    println!("runtime {:?}, CPU threads {}", client.name(), rayon::current_num_threads());

    println!("\nCG iteration (SpMV, 3 dots, 3 axpys), 12 entries a row; microseconds");
    println!("| rows | CPU 1 thread | CPU rayon | GPU, data resident | GPU kernels launched per iteration |");
    println!("|---|---|---|---|---|");
    for n in [2_000usize, 30_000, 300_000, 3_000_000] {
        let m = csr(n, 12);
        let mut v = [vec![1.0f32; n], vec![0.0f32; n], vec![1.0f32; n], vec![0.0f32; n]];
        let reps = (30_000_000 / (n * 12)).clamp(5, 2000);
        let cpu1 = time(reps, || cpu_cg_iteration(&m, &mut v, false));
        let cpu_par = time(reps, || cpu_cg_iteration(&m, &mut v, true));

        let up = |s: &[u8]| client.create_from_slice(s);
        let rs = up(u32::as_bytes(&m.row_start));
        let col = up(u32::as_bytes(&m.col));
        let val = up(f32::as_bytes(&m.val));
        let diag = up(f32::as_bytes(&m.diag));
        let ones = vec![1.0f32; n];
        let (p, q, r, x) = (up(f32::as_bytes(&ones)), up(f32::as_bytes(&ones)), up(f32::as_bytes(&ones)), up(f32::as_bytes(&ones)));
        let parts = 1024usize;
        let partial = client.empty(parts * 4);
        let (pq, rr, one) = (client.empty(4), client.empty(4), up(f32::as_bytes(&[1.0f32])));
        let arg = |h: &cubecl::server::Handle, len: usize| unsafe { BufferArg::from_raw_parts(h.clone(), len) };
        let gpu_iteration = || unsafe {
            spmv::launch_unchecked(&client, count(n), dim, arg(&rs, n + 1), arg(&col, m.col.len()), arg(&val, m.val.len()), arg(&diag, n), arg(&p, n), arg(&q, n));
            dot_partial::launch_unchecked(&client, count(parts), dim, arg(&p, n), arg(&q, n), arg(&partial, parts));
            finish::launch_unchecked(&client, CubeCount::Static(1, 1, 1), CubeDim::new_1d(1), arg(&partial, parts), arg(&pq, 1));
            axpy::launch_unchecked(&client, count(n), dim, arg(&one, 1), arg(&pq, 1), arg(&p, n), arg(&x, n));
            axpy::launch_unchecked(&client, count(n), dim, arg(&one, 1), arg(&pq, 1), arg(&q, n), arg(&r, n));
            dot_partial::launch_unchecked(&client, count(parts), dim, arg(&r, n), arg(&r, n), arg(&partial, parts));
            finish::launch_unchecked(&client, CubeCount::Static(1, 1, 1), CubeDim::new_1d(1), arg(&partial, parts), arg(&rr, 1));
            dot_partial::launch_unchecked(&client, count(parts), dim, arg(&r, n), arg(&q, n), arg(&partial, parts));
            finish::launch_unchecked(&client, CubeCount::Static(1, 1, 1), CubeDim::new_1d(1), arg(&partial, parts), arg(&pq, 1));
            axpy::launch_unchecked(&client, count(n), dim, arg(&pq, 1), arg(&rr, 1), arg(&r, n), arg(&p, n));
        };
        // Batches of iterations between syncs, as a CG on the device would
        // run them (a convergence check every 10).
        let batch = 10;
        let gpu = time(reps.max(10), || {
            for _ in 0..batch {
                gpu_iteration();
            }
            sync();
        }) / batch as f64;
        println!("| {n} | {cpu1:.1} | {cpu_par:.1} | {gpu:.1} | 10 |");
    }

    println!("\nPathFinder congestion update over every routing node; microseconds");
    println!("| nodes | CPU 1 thread | CPU rayon | GPU kernel only | GPU incl. occupancy upload and count download |");
    println!("|---|---|---|---|---|");
    for n in [300_000usize, 1_000_000, 4_000_000] {
        let occ: Vec<u32> = (0..n as u32).map(|i| (i % 97 == 0) as u32 + 1).collect();
        let mut hist = vec![0.0f32; n];
        let reps = 50;
        let cpu1 = time(reps, || {
            let mut c = 0;
            for (h, &o) in hist.iter_mut().zip(&occ) {
                if o > 1 {
                    *h += (o - 1) as f32;
                    c += 1;
                }
            }
            std::hint::black_box(c);
        });
        let cpu_par = time(reps, || {
            let c: usize = hist
                .par_iter_mut()
                .zip(&occ)
                .map(|(h, &o)| {
                    if o > 1 {
                        *h += (o - 1) as f32;
                        1
                    } else {
                        0
                    }
                })
                .sum();
            std::hint::black_box(c);
        });
        let parts = 4096usize;
        let occ_h = client.create_from_slice(u32::as_bytes(&occ));
        let hist_h = client.create_from_slice(f32::as_bytes(&hist));
        let over_h = client.empty(parts * 4);
        let run = |occ_h: &cubecl::server::Handle| unsafe {
            congestion::launch_unchecked(
                &client,
                count(parts),
                dim,
                BufferArg::from_raw_parts(occ_h.clone(), n),
                BufferArg::from_raw_parts(hist_h.clone(), n),
                BufferArg::from_raw_parts(over_h.clone(), parts),
            );
        };
        let kernel = time(reps, || {
            run(&occ_h);
            sync();
        });
        let with_copies = time(reps, || {
            let o = client.create_from_slice(u32::as_bytes(&occ));
            run(&o);
            let bytes = client.read_one(over_h.clone()).unwrap();
            let c: u32 = u32::from_bytes(&bytes).iter().sum();
            std::hint::black_box(c);
        });
        println!("| {n} | {cpu1:.1} | {cpu_par:.1} | {kernel:.1} | {with_copies:.1} |");
    }
}
