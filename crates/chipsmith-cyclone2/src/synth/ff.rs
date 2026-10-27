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
    Bit, CellKind, Ctrl, DffControl, Ff, FfKind, LatchReset, Module, Net, Polarity, SigBit, State,
    SyncEnable,
};
use chipsmith_synth::rewrite::{define_inits, inits, set_inits, Gates, Subst};
use rustc_hash::FxHashMap;

use super::cells::{Comb, Reg};

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
    for (name, ff) in regs {
        match legalize(&mut g, &mut subst, &mut init, &name, ff)? {
            Mapped::Register => counts.registers += 1,
            Mapped::Latch => counts.latches += 1,
            Mapped::Emulated => {
                counts.registers += 1;
                counts.emulated += 1;
            }
        }
    }
    subst.apply(m);
    // The LE register powers up 0, and every register now does.
    init.clear();
    set_inits(m, &init);
    Ok(counts)
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
    clk: SigBit,
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
            clk,
            ena,
            aclr: active,
            sclr: SigBit::ZERO,
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
    let clk = active_high(g, clk);
    let mut d = d;
    let (mut ena, mut aclr, mut sclr) = (SigBit::ONE, SigBit::ZERO, SigBit::ZERO);
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
            if value != powers_up_one {
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
    let reg_q = if invert {
        d = g.not(d);
        let inner = SigBit::Net(g.m.new_net());
        let outer = g.not(inner);
        subst.set(q, outer);
        inner
    } else {
        SigBit::Net(q)
    };
    g.add(
        Reg {
            d,
            clk,
            ena,
            aclr,
            sclr,
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
