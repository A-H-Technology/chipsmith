//! Gate-level registers and latches onto the LE (yosys' `dfflegalize` to
//! the six types cyclone2.ys allows, then `cells_map.v`; what that can't
//! express, the way Quartus builds it).
//!
//! The LE register has a rising-edge clock, a clock enable, an
//! asynchronous clear and a synchronous clear that only acts on enabled
//! edges, and powers up 0. Everything else is made from those: inverters in
//! front of a falling clock or an active-low control, the enable widened to
//! cover a reset that acts without it, and a register that resets or powers
//! up to 1 stored inverted.
//!
//! The LE has no latch and no asynchronous load. Like Quartus 13.0sp1 on
//! this family (warning 10240, and 13004/13310 for registers), a latch is a
//! LUT that feeds its output back into itself, and a register with an
//! asynchronous load (`$aldff`, `$dffsr`, an asynchronous reset to the
//! other value than it powers up to) is the "emulated" circuit of Quartus'
//! post-map netlist: a latch L holding the loaded value, an LE register
//! holding `D ^ L` cleared while loading, and `Q = load ? value : reg ^ L`.
//! What such a loop holds at power-up isn't defined by the hardware;
//! synthesis' proof assumes it holds 0, as a register would.

use anyhow::{bail, Result};
use chipsmith_netlist::{
    Bit, CellKind, Ctrl, DffControl, Ff, FfKind, GateOp, LatchReset, Module, Net, Polarity, SigBit,
    State, SyncEnable, Wire,
};
use chipsmith_synth::rewrite::{define_inits, inits, set_inits, Gates, Subst};
use rustc_hash::FxHashMap;

use super::cells::{Comb, Reg, LCELL_COMB};

/// Multiplexers in front of registers that the LAB's synchronous load can
/// take over: `$_MUX_`s (or the AND or OR a constant leg folds to) read
/// only by one register's data input, with an adder's sum (an arithmetic
/// LE's output) on one leg. The adder's LE
/// can't take the multiplexer into its LUT (arithmetic mode uses A, B and
/// the carry), so without it the multiplexer costs a LUT of its own after
/// the sum, and a LUT and a hop on the path. With it, the register sits in
/// the adder's LE, the sum on DATAIN and the other leg on SDATA (the LE's
/// input C, which arithmetic mode leaves free), as Quartus builds
/// picorv32's `reg_next_pc` (66 LUTs to chipsmith's 123 without it). A
/// constant leg is a 0 from the design's `~GND` LE (the packer's
/// `constant_le`) or a 1 from input C left unrouted: sha256's
/// `digest_update ? H + a : H0`, where H0 is a constant or `mode` bit by
/// bit.
///
/// A LAB takes 34 signals from outside (the placer's `MAX_LAB_INPUTS`),
/// and a carry chain fills its LABs 16 LEs at a time: each 16 LEs of a
/// chain whose loads are taken read their operands and the SDATA legs,
/// less what their own registers feed back. picorv32's `reg_next_pc`
/// loads `current_pc`, one of its sum's operands; a chain whose loads are
/// a third signal a bit (48 for 16 LEs) has no LAB to go to, and keeps its
/// multiplexers as LUTs.
struct SyncLoads {
    /// Each multiplexer the load takes: its output, and (A, B, S).
    mux: FxHashMap<Net, [SigBit; 3]>,
    /// Sums of arithmetic LEs read only by one such multiplexer.
    sums: rustc_hash::FxHashSet<Net>,
    /// Sums that a register with a synchronous reset reads directly, whose
    /// reset value the load stores (`legalize`). The clear can only store
    /// 0 (the power-up value) and takes the LAB's load with it (one bit
    /// for both, the clear first), so where a chain's 16 LEs have a reset
    /// to 1 the load stores every one of their resets, 0s too; a reset to
    /// 1 otherwise costs a LUT in front of the register.
    resets: rustc_hash::FxHashSet<Net>,
}

/// Signals from outside a LAB that a chain's 16 LEs may read when their
/// loads are taken: the placer's 34 less 4 for the controls (clock
/// enable, clears and the load itself).
const LAB_DATA_INPUTS: usize = 30;

impl SyncLoads {
    fn new(m: &Module) -> Self {
        let mut readers: FxHashMap<Net, u32> = FxHashMap::default();
        for c in &m.cells {
            c.kind.for_each_port(|p| {
                if p.dir != Some(chipsmith_netlist::Dir::Out) {
                    for n in p.bits.iter().filter_map(|b| b.net()) {
                        *readers.entry(n).or_default() += 1;
                    }
                }
            });
        }
        for w in &m.wires {
            if w.port.is_some_and(|d| d.drives()) {
                for n in w.bits.iter().filter_map(|b| b.net()) {
                    *readers.entry(n).or_default() += 2;
                }
            }
        }
        let one = |n: Net| readers.get(&n) == Some(&1);
        let net = |gc: &chipsmith_netlist::Generic, p: &str| gc.port(p).and_then(|p| p.sig.first()?.net());
        let mut mux = FxHashMap::default();
        // Arithmetic LEs: COMBOUT -> (DATAA, DATAB, CIN, COUT).
        let mut arith: FxHashMap<Net, [Option<Net>; 4]> = FxHashMap::default();
        // A register's D -> Q.
        let mut q_of: FxHashMap<Net, Net> = FxHashMap::default();
        // A register with a synchronous reset: its D -> (Q, reset value).
        let mut reset_of: FxHashMap<Net, (Net, bool)> = FxHashMap::default();
        for c in &m.cells {
            match &c.kind {
                CellKind::Gate(g) if g.op() == GateOp::Mux => {
                    if let SigBit::Net(y) = g.y {
                        if one(y) {
                            let i = g.inputs();
                            mux.insert(y, [i[0], i[1], i[2]]);
                        }
                    }
                }
                CellKind::Other(gc) if gc.ty == LCELL_COMB => {
                    let (cin, cout) = (net(gc, "CIN"), net(gc, "COUT"));
                    if let (true, Some(y)) = (cin.is_some() || cout.is_some(), net(gc, "COMBOUT")) {
                        arith.insert(y, [net(gc, "DATAA"), net(gc, "DATAB"), cin, cout]);
                    }
                }
                CellKind::FfGate(ff) => {
                    if let (FfKind::Dff { d: SigBit::Net(d), control, .. }, SigBit::Net(q)) = (&ff.kind, ff.q) {
                        q_of.insert(*d, q);
                        if let DffControl::Sync { value, .. } = control {
                            reset_of.insert(*d, (q, *value));
                        }
                    }
                }
                _ => {}
            }
        }
        for c in &m.cells {
            match &c.kind {
                // A multiplexer with a constant leg, as the gates fold it:
                // `s ? x : 0` is an AND, `s ? 0 : x` an ANDNOT, `s ? 1 : x`
                // an OR, `s ? x : 1` an ORNOT. Which input is the sum
                // decides `take`; (A, B, S) here takes either.
                CellKind::Gate(g)
                    if matches!(g.op(), GateOp::And | GateOp::AndNot | GateOp::Or | GateOp::OrNot) =>
                {
                    if let SigBit::Net(y) = g.y {
                        if one(y) {
                            let i = g.inputs();
                            let (zero, one_) = (SigBit::ZERO, SigBit::ONE);
                            for (x, t) in [(i[0], i[1]), (i[1], i[0])] {
                                let legs = match g.op() {
                                    GateOp::And => [zero, x, t],
                                    // Only i[0] reads straight through.
                                    GateOp::AndNot if x == i[0] => [x, zero, t],
                                    GateOp::Or => [x, one_, t],
                                    GateOp::OrNot if x == i[0] => [one_, x, t],
                                    _ => continue,
                                };
                                if x.net().is_some_and(|n| arith.contains_key(&n)) {
                                    mux.insert(y, legs);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Candidates: a multiplexer into a register, a sum read only by it
        // on one leg. Keyed by the sum.
        // Sum -> (the register's Q, the other leg, the register's D).
        let mut taken: FxHashMap<Net, (Net, SigBit, Net)> = FxHashMap::default();
        mux.retain(|&y, &mut [a, b, s]| {
            if !q_of.contains_key(&y) || s.net().is_none() {
                return false;
            }
            let sum = |x: SigBit| x.net().filter(|&n| one(n) && arith.contains_key(&n));
            match (sum(a), sum(b)) {
                (Some(n), _) => {
                    taken.insert(n, (q_of[&y], b, y));
                    true
                }
                (None, Some(n)) => {
                    taken.insert(n, (q_of[&y], a, y));
                    true
                }
                (None, None) => false,
            }
        });
        // Each chain's LEs in order, from its head (no CIN from another).
        let by_cin: FxHashMap<Net, Net> = arith
            .iter()
            .filter_map(|(&y, ops)| Some((ops[2]?, y)))
            .collect();
        let couts: rustc_hash::FxHashSet<Net> = arith.values().filter_map(|ops| ops[3]).collect();
        let mut heads: Vec<Net> = arith
            .iter()
            .filter(|(_, ops)| ops[2].is_none_or(|c| !couts.contains(&c)))
            .map(|(&y, _)| y)
            .collect();
        heads.sort_unstable();
        let mut sums = rustc_hash::FxHashSet::default();
        let mut resets_taken = rustc_hash::FxHashSet::default();
        for head in heads {
            let mut chain = vec![head];
            while let Some(&y) = arith[chain.last().expect("a head")][3].and_then(|c| by_cin.get(&c)) {
                chain.push(y);
            }
            for lab in chain.chunks(16) {
                // Resets a sum feeds straight in, if one of them is to 1.
                let reset = |y: &Net| reset_of.get(y).filter(|_| one(*y));
                let resets: Vec<(Net, Net, bool)> = if lab.iter().any(|y| reset(y).is_some_and(|r| r.1)) {
                    lab.iter()
                        .filter_map(|y| reset(y).map(|&(q, v)| (*y, q, v)))
                        .collect()
                } else {
                    Vec::new()
                };
                let own: Vec<Net> = lab
                    .iter()
                    .filter_map(|y| Some(taken.get(y)?.0))
                    .chain(resets.iter().map(|r| r.1))
                    .collect();
                let mut outside: Vec<Net> = lab
                    .iter()
                    .flat_map(|y| {
                        let ops = arith[y];
                        [ops[0], ops[1], taken.get(y).and_then(|t| t.1.net())]
                    })
                    .flatten()
                    .filter(|n| !own.contains(n))
                    .collect();
                outside.sort_unstable();
                outside.dedup();
                // A constant 0 comes from `~GND` on a LAB line (a 1 takes
                // none; counted anyway).
                let constants = [SigBit::ZERO, SigBit::ONE]
                    .into_iter()
                    .filter(|&k| {
                        lab.iter().any(|y| taken.get(y).is_some_and(|t| t.1 == k))
                            || resets.iter().any(|r| SigBit::Const(State::from(r.2)) == k)
                    })
                    .count();
                // An LE takes the LAB's clear and load together (one bit),
                // so the registers of these 16 LEs that take either must
                // take the same: clear (2), load (4) or both (6). Before
                // any load is taken they agree, as the placer had them.
                let kind = |y: &Net| -> u8 {
                    let reset = |d: &Net| reset_of.get(d).is_some_and(|r| !r.1);
                    if let Some(t) = taken.get(y) {
                        4 | if reset(&t.2) { 2 } else { 0 }
                    } else if resets.iter().any(|r| r.0 == *y) {
                        4
                    } else if reset(y) {
                        // Read elsewhere too or not, the register sits in
                        // the sum's LE.
                        2
                    } else {
                        0
                    }
                };
                let mut kinds: Vec<u8> = lab.iter().map(kind).filter(|&k| k != 0).collect();
                kinds.dedup();
                kinds.sort_unstable();
                kinds.dedup();
                if outside.len() + constants <= LAB_DATA_INPUTS && kinds.len() <= 1 {
                    sums.extend(lab.iter().filter(|y| taken.contains_key(y)));
                    resets_taken.extend(resets.iter().map(|r| r.0));
                }
            }
        }
        SyncLoads {
            mux,
            sums,
            resets: resets_taken,
        }
    }

    /// Whether the load stores the synchronous reset of a register whose
    /// data input is `d`.
    fn resets(&self, d: SigBit) -> bool {
        d.net().is_some_and(|n| self.resets.contains(&n))
    }

    /// For a register's data input: (sload, sdata, datain) with sload
    /// active high, if the LAB's synchronous load can take its
    /// multiplexer.
    fn take(&self, g: &mut Gates, d: SigBit) -> Option<(SigBit, SigBit, SigBit)> {
        // SDATA may be a constant (sha256's `digest_init ? H0 : H + a`):
        // the packer drives it from the design's constant LE.
        let [a, b, s] = *self.mux.get(&d.net()?)?;
        let sum = |x: SigBit| x.net().is_some_and(|n| self.sums.contains(&n));
        if sum(a) {
            Some((s, b, a))
        } else if sum(b) {
            let load = g.not(s);
            Some((load, a, b))
        } else {
            None
        }
    }
}

/// A control as an active-high signal.
fn active_high(g: &mut Gates, c: Ctrl) -> SigBit {
    match c.polarity {
        Polarity::Pos => c.sig,
        Polarity::Neg => g.not(c.sig),
    }
}

/// What [`map`] made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Registers, each one LE register (emulated ones included).
    pub registers: usize,
    /// Latches, each a LUT loop.
    pub latches: usize,
    /// Registers with an asynchronous load, each also a latch.
    pub emulated: usize,
}

/// What one register or latch became.
enum Mapped {
    Register,
    Latch,
    Emulated,
}

/// Maps every gate-level register to `LCELL_FF` and every latch to a LUT
/// loop.
pub fn map(m: &mut Module) -> Result<Counts> {
    define_inits(m);
    let mut init = inits(m);
    // Before the registers leave the module: their data inputs are the
    // multiplexers' readers.
    let loads = SyncLoads::new(m);
    let mut g = Gates::new(m, "ff");
    let cells = std::mem::take(&mut g.m.cells);
    let mut subst = Subst::new(g.m);
    let mut kept = Vec::new();
    let mut regs = Vec::new();
    for c in cells {
        match &c.kind {
            CellKind::FfGate(ff) => regs.push((c.name.clone(), ff.clone())),
            _ => kept.push(c),
        }
    }
    g.m.cells.extend(kept);
    let mut counts = Counts::default();
    let mut inverted = Vec::new();
    for (name, ff) in regs {
        match legalize(&mut g, &mut subst, &mut init, &mut inverted, &loads, &name, ff)? {
            Mapped::Register => counts.registers += 1,
            Mapped::Latch => counts.latches += 1,
            Mapped::Emulated => {
                counts.registers += 1;
                counts.emulated += 1;
            }
        }
    }
    name_inverted(m, &inverted);
    subst.apply(m);
    // The LE register powers up 0, and every register now does.
    init.clear();
    set_inits(m, &init);
    Ok(counts)
}

/// Names each register stored inverted after the bit it holds the
/// inverse of: `crc[3]~inv` (bit 3 from 0 of wire `crc`). Once the
/// inverter folds into the loads, nothing else ties the register to the
/// RTL's: synthesis' proof compares the name with `~crc[3]` on the RTL's
/// side, and induction needs that pair to prove what the register feeds.
/// The build purges the name with every other internal one.
fn name_inverted(m: &mut Module, inverted: &[(Net, Net)]) {
    if inverted.is_empty() {
        return;
    }
    let inner: FxHashMap<Net, Net> = inverted.iter().copied().collect();
    let mut named = Vec::new();
    for w in &m.wires {
        for (i, b) in w.bits.iter().enumerate() {
            if let Some(&r) = b.net().and_then(|q| inner.get(&q)) {
                named.push(Wire::new(format!("{}[{i}]~inv", w.name), vec![SigBit::Net(r)]));
            }
        }
    }
    m.wires.extend(named);
}

/// Whether a control with this polarity acts at this value.
fn acts(polarity: Polarity, v: bool) -> bool {
    v == (polarity == Polarity::Pos)
}

/// What an asynchronous load does: the signals it depends on (at most
/// three, the LUT's other inputs), and from their values whether it loads
/// and what.
struct Load<'a> {
    ins: Vec<SigBit>,
    value: Box<LoadFn<'a>>,
}

type LoadFn<'a> = dyn Fn(&[bool]) -> Option<bool> + 'a;

/// A latch, as Quartus makes one: a LUT whose COMBOUT comes back on its
/// DATAC (`sum_lutc_input = "datac"`), the controls on DATAA, DATAB and
/// DATAD. With `flip` it holds the complement of what it loads.
fn latch(g: &mut Gates, load: &Load, flip: bool) -> SigBit {
    assert!(load.ins.len() <= 3, "a latch LUT has three free inputs");
    let y = SigBit::Net(g.m.new_net());
    let mut mask = 0u16;
    for i in 0..16u16 {
        let bit = |k: u16| i >> k & 1 == 1;
        let ins = [bit(0), bit(1), bit(3)];
        let held = bit(2);
        let next = (load.value)(&ins[..load.ins.len()]).map_or(held, |v| v != flip);
        if next {
            mask |= 1 << i;
        }
    }
    let pin = |k: usize| load.ins.get(k).copied().unwrap_or(SigBit::ONE);
    g.add(
        Comb {
            mask,
            data: [pin(0), pin(1), y, pin(2)],
            cin: SigBit::ONE,
            combout: Some(y),
            cout: None,
        }
        .cell(),
    );
    y
}

/// Set and clear (clear wins) as a load.
fn set_clr(sr: &chipsmith_netlist::SetClr<Bit>) -> Load<'_> {
    Load {
        ins: vec![sr.set, sr.clr],
        value: Box::new(|x| {
            if acts(sr.clr_polarity, x[1]) {
                Some(false)
            } else if acts(sr.set_polarity, x[0]) {
                Some(true)
            } else {
                None
            }
        }),
    }
}

/// The register with an asynchronous load: `active` high while `load`
/// loads, `value` what. Its Q powers up 0, or 1 with `flip` (the whole
/// circuit then computes the complement).
#[allow(clippy::too_many_arguments)]
fn emulate(
    g: &mut Gates,
    subst: &mut Subst,
    q: Net,
    clk: Ctrl,
    d: SigBit,
    ena: SigBit,
    load: &Load,
    active: SigBit,
    value: SigBit,
    flip: bool,
) {
    let l = latch(g, load, flip);
    let (d, value) = if flip {
        (g.not(d), g.not(value))
    } else {
        (d, value)
    };
    let reg = SigBit::Net(g.m.new_net());
    let din = g.xor(d, l);
    g.add(
        Reg {
            d: din,
            clk: clk.sig,
            clk_inv: clk.polarity == Polarity::Neg,
            ena,
            aclr: active,
            sclr: SigBit::ZERO,
            sload: None,
            q: reg,
        }
        .cell(),
    );
    let held = g.xor(reg, l);
    let out = g.mux(held, value, active);
    let out = if flip { g.not(out) } else { out };
    subst.set(q, out);
}

fn legalize(
    g: &mut Gates,
    subst: &mut Subst,
    init: &mut FxHashMap<Net, State>,
    inverted: &mut Vec<(Net, Net)>,
    loads: &SyncLoads,
    name: &str,
    ff: Ff<Bit>,
) -> Result<Mapped> {
    let SigBit::Net(q) = ff.q else {
        bail!("register {name} drives a constant");
    };
    let powers_up_one = init.get(&q) == Some(&State::One);
    init.remove(&q);
    let (clk, d, control) = match ff.kind {
        FfKind::Dff { clk, d, control } => (clk, d, control),
        FfKind::Global { .. } => {
            bail!("register {name}: `$ff` (the formal global clock) has no hardware")
        }
        FfKind::Dlatch { en, d, reset } => {
            let load = match reset {
                LatchReset::None => Load {
                    ins: vec![en.sig, d],
                    value: Box::new(|x| acts(en.polarity, x[0]).then_some(x[1])),
                },
                LatchReset::Async { arst, value } => Load {
                    ins: vec![en.sig, d, arst.sig],
                    value: Box::new(move |x| {
                        if acts(arst.polarity, x[2]) {
                            Some(value)
                        } else {
                            acts(en.polarity, x[0]).then_some(x[1])
                        }
                    }),
                },
                // Five inputs with the loop: the controls folded first.
                LatchReset::SetClr(sr) => {
                    let set = active_high(
                        g,
                        Ctrl {
                            sig: sr.set,
                            polarity: sr.set_polarity,
                        },
                    );
                    let clr = active_high(
                        g,
                        Ctrl {
                            sig: sr.clr,
                            polarity: sr.clr_polarity,
                        },
                    );
                    let en = active_high(g, en);
                    let any = g.or(set, clr);
                    let en = g.or(en, any);
                    let d = g.or(d, set);
                    let not_clr = g.not(clr);
                    let d = g.and(d, not_clr);
                    Load {
                        ins: vec![en, d],
                        value: Box::new(|x| x[0].then_some(x[1])),
                    }
                }
            };
            hold(g, subst, q, &load, powers_up_one);
            return Ok(Mapped::Latch);
        }
        FfKind::Sr(sr) => {
            hold(g, subst, q, &set_clr(&sr), powers_up_one);
            return Ok(Mapped::Latch);
        }
    };
    let mut d = d;
    let (mut ena, mut aclr, mut sclr) = (SigBit::ONE, SigBit::ZERO, SigBit::ZERO);
    // A synchronous reset the LAB's load stores: (load, value).
    let mut reset_load: Option<(SigBit, SigBit)> = None;
    let enable = |g: &mut Gates, en: Option<Ctrl>| en.map_or(SigBit::ONE, |e| active_high(g, e));
    // Whether the register holds the inverse of Q: it must reset to 0 and
    // power up 0.
    let invert = match control {
        DffControl::Plain { en } => {
            if let Some(en) = en {
                ena = active_high(g, en);
            }
            powers_up_one
        }
        DffControl::Async { arst, value, en } if value != powers_up_one => {
            let load = Load {
                ins: vec![arst.sig],
                value: Box::new(move |x| acts(arst.polarity, x[0]).then_some(value)),
            };
            let (active, ena) = (active_high(g, arst), enable(g, en));
            let k = SigBit::Const(State::from(value));
            emulate(g, subst, q, clk, d, ena, &load, active, k, powers_up_one);
            return Ok(Mapped::Emulated);
        }
        DffControl::Async { arst, value, en } => {
            aclr = active_high(g, arst);
            if let Some(en) = en {
                ena = active_high(g, en);
            }
            value
        }
        DffControl::Sync { srst, value, en } => {
            let r = active_high(g, srst);
            // The load stores the reset value, as the clear would 0
            // (`SyncLoads::resets`); the enable acts on it as on the clear.
            let by_load = !powers_up_one && loads.resets(d);
            let gated = match en {
                SyncEnable::Always => {
                    sclr = r;
                    None
                }
                SyncEnable::EnableOverReset(e) => {
                    ena = active_high(g, e);
                    sclr = r;
                    Some(ena)
                }
                // The reset acts on every edge: enable those edges too.
                SyncEnable::ResetOverEnable(e) => {
                    let e = active_high(g, e);
                    ena = g.or(e, r);
                    sclr = r;
                    None
                }
            };
            if by_load {
                reset_load = Some((sclr, SigBit::Const(State::from(value))));
                sclr = SigBit::ZERO;
            } else if value != powers_up_one {
                // The clear can only store the power-up value: the reset
                // goes into the LUT in front instead.
                let k = SigBit::Const(State::from(value));
                d = g.mux(d, k, r);
                sclr = SigBit::ZERO;
                if let Some(e) = gated {
                    ena = e;
                } else if matches!(en, SyncEnable::Always) {
                    ena = SigBit::ONE;
                }
            }
            powers_up_one
        }
        DffControl::Load { aload, ad, en } => {
            let load = Load {
                ins: vec![aload.sig, ad],
                value: Box::new(|x| acts(aload.polarity, x[0]).then_some(x[1])),
            };
            let (active, ena) = (active_high(g, aload), enable(g, en));
            emulate(g, subst, q, clk, d, ena, &load, active, ad, powers_up_one);
            return Ok(Mapped::Emulated);
        }
        DffControl::SetClr { sr, en } => {
            let set = active_high(
                g,
                Ctrl {
                    sig: sr.set,
                    polarity: sr.set_polarity,
                },
            );
            let clr = active_high(
                g,
                Ctrl {
                    sig: sr.clr,
                    polarity: sr.clr_polarity,
                },
            );
            let active = g.or(set, clr);
            let value = g.not(clr);
            let ena = enable(g, en);
            emulate(
                g,
                subst,
                q,
                clk,
                d,
                ena,
                &set_clr(&sr),
                active,
                value,
                powers_up_one,
            );
            return Ok(Mapped::Emulated);
        }
    };
    // A register stored inverted takes an inverter on its data, so its
    // multiplexer stays a LUT.
    let sload = if invert {
        None
    } else if reset_load.is_some() {
        reset_load
    } else {
        loads.take(g, d).map(|(load, sdata, datain)| {
            d = datain;
            (load, sdata)
        })
    };
    let reg_q = if invert {
        d = g.not(d);
        let r = g.m.new_net();
        let inner = SigBit::Net(r);
        let outer = g.not(inner);
        subst.set(q, outer);
        inverted.push((q, r));
        inner
    } else {
        SigBit::Net(q)
    };
    g.add(
        Reg {
            d,
            clk: clk.sig,
            clk_inv: clk.polarity == Polarity::Neg,
            ena,
            aclr,
            sclr,
            sload,
            q: reg_q,
        }
        .cell(),
    );
    Ok(Mapped::Register)
}

/// A latch on its own: Q is the loop, or its complement to power up 1.
fn hold(g: &mut Gates, subst: &mut Subst, q: Net, load: &Load, powers_up_one: bool) {
    let l = latch(g, load, powers_up_one);
    let out = if powers_up_one { g.not(l) } else { l };
    subst.set(q, out);
}
