//! LE-level global placement (ADR-0012): every LE, carry chain and block
//! is an object of `chipsmith_place::electro`, spread by one density
//! field per resource (LE slots, register control sets, each block kind);
//! the result is legalised LE by LE into LABs, and the parent's anneal
//! refines it.

use chipsmith_place::electro::{self, Accel, Timing};

use super::*;

/// The resources' order in the problem: LE slots, register control sets,
/// then one per block kind (`Block::ALL` order).
const LE_SLOTS: usize = 0;
const CONTROLS: usize = 1;
const BLOCKS: usize = 2;

/// What an object of the problem is.
#[derive(Debug, Clone, Copy)]
enum Thing {
    Le(usize),
    Chain(usize),
    Block(usize),
}

/// Where a cell is in the problem: on an object, at a row offset from its
/// centre, or fixed.
#[derive(Debug, Clone, Copy)]
enum At {
    Object(u32, f32),
    Fixed(Pos),
}

pub(super) struct LeProblem {
    problem: electro::Problem,
    things: Vec<Thing>,
    cell_at: Vec<At>,
    /// Each register LE's control-set index, for the legaliser.
    control_set: Vec<Option<u32>>,
}

/// A register's controls as a LAB sees them: two registers with the same
/// key can always share a LAB.
type ControlKey = (Ctl, Ctl, Ctl, Option<NetId>, u8);

fn control_key(f: &FfCtl) -> ControlKey {
    (f.clk, f.ena, f.sclr, f.aclr, f.sync_kind)
}

/// A chain's LE k sits k / 16 LABs below its head; the object's centre is
/// the middle of its LABs.
fn chain_dy(len: usize, k: usize) -> f32 {
    let h = len.div_ceil(LES_PER_LAB) as f32;
    (h - 1.0) / 2.0 - (k / LES_PER_LAB) as f32
}

pub(super) fn problem(d: &Design, grid: &Grid, fixed_xy: &[Option<Xy>]) -> LeProblem {
    let width = grid.width as usize / 2 + 1;
    let height = grid.height as usize;
    let bins = width * height;
    let mut lab = vec![0.0f32; bins];
    for &(x, y) in &grid.xy {
        lab[y as usize * width + (x / 2) as usize] = 1.0;
    }
    let mut capacity = vec![lab.clone(), lab];
    for b in Block::ALL {
        let mut c = vec![0.0f32; bins];
        for (i, &(x, y)) in grid.site_xy.iter().enumerate() {
            if grid.site_block[i] == b {
                c[y as usize * width + (x / 2) as usize] = 1.0;
            }
        }
        capacity.push(c);
    }
    let mut problem = electro::Problem::new(width, height, capacity);

    let mut sets: HashMap<ControlKey, u32> = HashMap::default();
    let mut set_size: Vec<u32> = Vec::new();
    let control_set: Vec<Option<u32>> = d
        .sigs
        .iter()
        .map(|s| {
            let key = control_key(s.ff.as_ref()?);
            let next = sets.len() as u32;
            let i = *sets.entry(key).or_insert(next);
            if i as usize == set_size.len() {
                set_size.push(0);
            }
            set_size[i as usize] += 1;
            Some(i)
        })
        .collect();
    // A set of k registers needs ceil(k / 16) LABs' controls.
    let control_share = |le: usize| -> f32 {
        control_set[le].map_or(0.0, |s| {
            let k = set_size[s as usize] as usize;
            k.div_ceil(LES_PER_LAB) as f32 / k as f32
        })
    };

    let mut things = Vec::new();
    let mut obj_of_le: Vec<Option<(u32, f32)>> = vec![None; d.les.len()];
    for le in 0..d.les.len() {
        if d.chain_of[le].is_some() {
            continue;
        }
        let o = problem.add_object(
            1,
            &[
                (LE_SLOTS, 1.0 / LES_PER_LAB as f32),
                (CONTROLS, control_share(le)),
            ],
        );
        obj_of_le[le] = Some((o, 0.0));
        things.push(Thing::Le(le));
    }
    for (c, chain) in d.chains.iter().enumerate() {
        let labs = chain.len().div_ceil(LES_PER_LAB);
        let with_registers = chain
            .chunks(LES_PER_LAB)
            .filter(|lab| lab.iter().any(|&le| d.les[le].ff.is_some()))
            .count();
        let o = problem.add_object(
            labs as u32,
            &[
                (LE_SLOTS, chain.len() as f32 / LES_PER_LAB as f32),
                (CONTROLS, with_registers as f32),
            ],
        );
        for (k, &le) in chain.iter().enumerate() {
            obj_of_le[le] = Some((o, chain_dy(chain.len(), k)));
        }
        things.push(Thing::Chain(c));
    }
    let block_obj: Vec<u32> = (0..d.blocks.len())
        .map(|r| {
            let kind = Block::ALL
                .iter()
                .position(|&b| b == d.block_kind(r))
                .expect("a block kind");
            things.push(Thing::Block(r));
            problem.add_object(1, &[(BLOCKS + kind, 1.0)])
        })
        .collect();
    let mut fixed_index: Vec<Option<u32>> = vec![None; d.cells.len()];
    for (c, xy) in fixed_xy.iter().enumerate() {
        if let Some(xy) = xy {
            fixed_index[c] = Some(problem.add_fixed(unit(*xy)));
        }
    }
    let cell_at: Vec<At> = (0..d.cells.len())
        .map(|c| match (d.le_of[c], d.block_of[c]) {
            (Some(le), _) => {
                let (o, dy) = obj_of_le[le].expect("every LE has an object");
                At::Object(o, dy)
            }
            (_, Some(r)) => At::Object(block_obj[r], 0.0),
            _ => At::Fixed(fixed_xy[c].map_or((0.0, 0.0), unit)),
        })
        .collect();
    // One net per design net, in order, so the timing hook's weights line
    // up with them.
    for members in &d.nets {
        let mut pins: Vec<Pin> = Vec::new();
        for &c in members {
            let pin = match cell_at[c] {
                At::Object(o, dy) => Pin {
                    of: PinOf::Object(o),
                    dx: 0.0,
                    dy,
                },
                At::Fixed(_) => Pin {
                    of: PinOf::Fixed(fixed_index[c].expect("a cell is an LE, a block or fixed")),
                    dx: 0.0,
                    dy: 0.0,
                },
            };
            if !pins.contains(&pin) {
                pins.push(pin);
            }
        }
        problem.add_net(pins, 1.0);
    }
    LeProblem {
        problem,
        things,
        cell_at,
        control_set,
    }
}

/// The anneal's delay model on continuous positions (`State::conn_delay`
/// with distances in bins rather than whole LABs).
struct LeTiming<'a> {
    d: &'a Design,
    cell_at: &'a [At],
}

impl LeTiming<'_> {
    fn pos(&self, centres: &[Pos], c: usize) -> Pos {
        match self.cell_at[c] {
            At::Object(o, dy) => {
                let (x, y) = centres[o as usize];
                (x, y + dy)
            }
            At::Fixed(p) => p,
        }
    }
}

impl Timing for LeTiming<'_> {
    fn reweight(&mut self, centres: &[Pos], weights: &mut [f32]) {
        let d = self.d;
        let arcs: Vec<Arc> = d
            .conns
            .iter()
            .map(|c| {
                let delay = if c.internal {
                    0.0
                } else {
                    let ((x0, y0), (x1, y1)) =
                        (self.pos(centres, c.from_cell), self.pos(centres, c.to_cell));
                    let (dx, dy) = ((x0 - x1).abs(), (y0 - y1).abs());
                    if dx < 0.5 && dy < 0.5 {
                        timing::wire_delay("LOCAL_LINE")
                    } else {
                        timing::wire_delay("LE_BUFFER")
                            + timing::wire_delay("LOCAL_INTERCONNECT")
                            + dx as Ps * timing::wire_delay("R4") / 4.0
                            + dy as Ps * timing::wire_delay("C4") / 4.0
                    }
                };
                Arc {
                    from: c.from,
                    to: c.to,
                    delay,
                }
            })
            .collect();
        let analysis = d.timing.analyse(&arcs);
        weights.iter_mut().for_each(|w| *w = 1.0);
        for (c, a) in d.conns.iter().zip(&arcs) {
            if let Some(n) = c.net {
                let w = 1.0 + TIMING_WEIGHT * analysis.criticality(a).powf(CRIT_EXP);
                weights[n] = weights[n].max(w);
            }
        }
    }
}

/// How far, in bins, a register goes to join a LAB that already has its
/// control set rather than open one.
const OPEN_PENALTY: f32 = 1.5;

/// Put every object of `lp` into `state` near its centre in `centres`:
/// blocks, chains longest first, then registers, then the other LEs, each
/// into the LAB nearest its target (plus `OPEN_PENALTY` for a register
/// opening a LAB) that has a free slot and that `lab_check` accepts.
/// Errs naming what found no place.
fn legalize(state: &mut State, lp: &LeProblem, centres: &[Pos]) -> std::result::Result<(), String> {
    let d = state.design;
    let grid = state.grid;
    state.clear();
    let (w, h) = (grid.width / 2 + 1, grid.height);
    let mut spiral: Vec<(i32, i32)> = (-h..=h)
        .flat_map(|dy| (-w..=w).map(move |dx| (dx, dy)))
        .collect();
    spiral.sort_by_key(|&(dx, dy)| (dx.abs() + dy.abs(), dy, dx));
    // Bin -> its LABs (tile columns 2b and 2b + 1).
    let labs_in = |bx: i32, by: i32| {
        [2 * bx, 2 * bx + 1]
            .into_iter()
            .filter_map(move |x| grid.lab_at((x, by)))
    };
    let head = |o: usize, c: usize| -> Pos {
        let (x, y) = centres[o];
        (x, y + chain_dy(d.chains[c].len(), 0))
    };

    let mut order: Vec<usize> = (0..lp.things.len()).collect();
    let key = |o: usize| -> (u8, i64, i64, i64) {
        let (x, y) = centres[o];
        let sweep = ((x * 64.0) as i64, (y * 64.0) as i64);
        match lp.things[o] {
            Thing::Block(_) => (0, 0, sweep.0, sweep.1),
            Thing::Chain(c) => (1, -(d.chains[c].len() as i64), sweep.0, sweep.1),
            Thing::Le(le) if d.les[le].ff.is_some() => (2, 0, sweep.0, sweep.1),
            Thing::Le(_) => (3, 0, sweep.0, sweep.1),
        }
    };
    order.sort_by_key(|&o| (key(o), o));

    for o in order {
        match lp.things[o] {
            Thing::Block(r) => {
                let (tx, ty) = centres[o];
                let kind = d.block_kind(r);
                let site = (0..grid.site_xy.len())
                    .filter(|&i| grid.site_block[i] == kind && state.site_of[i].is_none())
                    .min_by(|&a, &b| {
                        let dist = |i: usize| {
                            let (x, y) = unit(grid.site_xy[i]);
                            (x - tx).abs() + (y - ty).abs()
                        };
                        dist(a).total_cmp(&dist(b)).then(a.cmp(&b))
                    })
                    .ok_or_else(|| format!("no free site for {}", d.cells[d.blocks[r]].name))?;
                state.put_block(r, site);
            }
            Thing::Chain(c) => {
                let chain = &d.chains[c];
                let (tx, ty) = head(o, c);
                let (bx, by) = (tx.floor() as i32, ty.floor() as i32);
                let mut placed = false;
                'search: for &(dx, dy) in &spiral {
                    for top in labs_in(bx + dx, by + dy) {
                        let Some(labs) = grid.chain_labs(top, chain.len()) else {
                            continue;
                        };
                        if !(0..chain.len())
                            .all(|i| state.slots[labs[i / LES_PER_LAB]][i % LES_PER_LAB].is_none())
                        {
                            continue;
                        }
                        for (i, &le) in chain.iter().enumerate() {
                            state.put(le, labs[i / LES_PER_LAB], i % LES_PER_LAB);
                        }
                        if labs.iter().all(|&l| state.lab_ok(l)) {
                            placed = true;
                            break 'search;
                        }
                        for &le in chain {
                            state.unput(le);
                        }
                    }
                }
                if !placed {
                    return Err(format!("no LABs for a carry chain of {} LEs", chain.len()));
                }
            }
            Thing::Le(le) => {
                let (tx, ty) = centres[o];
                let (bx, by) = (tx.floor() as i32, ty.floor() as i32);
                let set = lp.control_set[le];
                let mut best: Option<(f32, usize)> = None;
                for &(dx, dy) in &spiral {
                    // A LAB in a bin this far off is at least this far
                    // from the target.
                    let floor = (dx.abs() + dy.abs()) as f32 - 1.0;
                    if best.is_some_and(|(c, _)| floor > c) {
                        break;
                    }
                    for lab in labs_in(bx + dx, by + dy) {
                        let Some(k) = (0..LES_PER_LAB).find(|&k| state.slots[lab][k].is_none())
                        else {
                            continue;
                        };
                        let (lx, ly) = unit(grid.xy[lab]);
                        let mut cost = (lx - tx).abs() + (ly - ty).abs();
                        if let Some(s) = set {
                            let joins = state.slots[lab]
                                .iter()
                                .flatten()
                                .any(|&other| lp.control_set[other] == Some(s));
                            if !joins {
                                cost += OPEN_PENALTY;
                            }
                        }
                        if best.is_some_and(|(c, l)| (c, l) <= (cost, lab)) {
                            continue;
                        }
                        state.put(le, lab, k);
                        let ok = state.lab_ok(lab);
                        state.unput(le);
                        if ok {
                            best = Some((cost, lab));
                        }
                    }
                }
                let (_, lab) = best.ok_or_else(|| {
                    format!(
                        "no LAB takes {}",
                        d.cells[d.les[le].lut.or(d.les[le].ff).unwrap()].name
                    )
                })?;
                let k = (0..LES_PER_LAB)
                    .find(|&k| state.slots[lab][k].is_none())
                    .expect("the slot is still free");
                state.put(le, lab, k);
            }
        }
    }
    state.recompute_costs();
    Ok(())
}

/// What LE-level placement did, for the trace and `Stats`.
pub(super) struct Placed {
    pub iterations: usize,
}

/// Global placement of every LE, then legalisation into `state`. Errs
/// (leaving `state` cleared) when the legaliser can't fit the design; the
/// caller then falls back to clusters.
pub(super) fn place(
    state: &mut State,
    fixed_xy: &[Option<Xy>],
    accel: Accel,
    trace: bool,
) -> std::result::Result<Placed, String> {
    let d = state.design;
    let started = std::time::Instant::now();
    let lp = problem(d, state.grid, fixed_xy);
    let env = |name: &str, default: f32| -> f32 {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let defaults = electro::Options::default();
    let opts = electro::Options {
        trace,
        accel,
        target_density: env("CHIPSMITH_ELECTRO_DENSITY", defaults.target_density),
        stop_overflow: env("CHIPSMITH_ELECTRO_OVERFLOW", defaults.stop_overflow),
        density_weight: env("CHIPSMITH_ELECTRO_LAMBDA", defaults.density_weight),
        gamma: env("CHIPSMITH_ELECTRO_GAMMA", defaults.gamma),
        timing_every: env(
            "CHIPSMITH_ELECTRO_TIMING_EVERY",
            defaults.timing_every as f32,
        ) as usize,
        ..defaults
    };
    let mut timing = LeTiming {
        d,
        cell_at: &lp.cell_at,
    };
    let out = electro::place(&lp.problem, &mut timing, None, &opts)?;
    if trace {
        eprintln!(
            "place: electrostatic {} objects, {} nets: {} iterations, overflow {:?}, hpwl {:.0}, {:.2} s ({:.2} s engine, {:.2} s timing)",
            lp.problem.objects(),
            lp.problem.nets(),
            out.iterations,
            out.overflow,
            lp.problem.hpwl(&out.centres),
            started.elapsed().as_secs_f64(),
            out.engine_s,
            out.timing_s
        );
    }
    let t = std::time::Instant::now();
    let result = legalize(state, &lp, &out.centres);
    if trace {
        eprintln!(
            "place: LE legalisation {} in {:.2} s",
            match &result {
                Ok(()) => format!("wirelength {}", state.wirelength()),
                Err(e) => format!("failed: {e}"),
            },
            t.elapsed().as_secs_f64()
        );
    }
    if let Err(e) = result {
        state.clear();
        return Err(e);
    }
    Ok(Placed {
        iterations: out.iterations,
    })
}
