//! Memories onto block RAM (RAMB18E1, RAMB36E1; `crate::bram`), after the
//! Cyclone II's M4K mapping (chipsmith-cyclone2 `synth/m4k.rs`), whose
//! analysis of a memory's ports this follows: ports widened where a word is
//! written or read several at once (mixed widths), the ports' roles on the
//! block's two (single port, simple dual port, true dual port), the
//! cheapest arrangement of blocks deep and wide, collisions between two
//! writing ports resolved in logic.
//!
//! What is the 7-series block's own:
//!
//! - each port has its own clock (either edge), enable, width (x1 to x18,
//!   x36 on a RAMB36E1) and write mode; in SDP mode one side is twice as
//!   wide (x36, x72), read on port A and written on port B;
//! - a read of the word its own port writes returns the new data
//!   (WRITE_FIRST) or the old (READ_FIRST), so either transparency maps;
//!   a read of the word the *other* port writes on the same edge returns
//!   the old data only if the writing port is READ_FIRST, and is undefined
//!   otherwise. So a write the other port reads old data of is READ_FIRST,
//!   and if its own port reads its new data, that read is bypassed in
//!   logic (a register of the written data where the write hit);
//! - a read held while its port writes (`if (we) m[a] <= d; else q <=
//!   m[a];`) is NO_CHANGE;
//! - the read data's power-up value is the block's INIT; a synchronous
//!   reset of the read data (gated by the port's enable) is the latch's
//!   RSTRAM, to SRVAL;
//! - a register after the read data is the block's output register, with
//!   its own enable (REGCE) and synchronous reset (RSTREG), on the port's
//!   clock.
//!
//! Every memory with synchronous reads a block can take goes into blocks
//! (as Vivado does from a few hundred bits up), unless its declaration says
//! `ram_style` "logic", "registers" or "distributed"; one of 64 words or
//! fewer stays in LUTs and registers, as Vivado keeps it. The blocks are
//! RAMB18E1s ([`SIZES`]; Vivado picks a RAMB36E1 over two where they hold
//! the same); the cheapest arrangement is the fewest of them, then the
//! least soft logic.

use std::collections::HashMap;

use chipsmith_netlist::{
    attr, CellKind, Const, Ctrl, DffControl, Dir, Ff, FfKind, Mem, MemRd, MemRdSync, MemWr, Module,
    ParamValue, Polarity, SigBit, SigSpec, State, SyncEnable, Word,
};
use chipsmith_synth::rewrite::{inits, set_inits, Gates, Subst};

use super::cells::Ramb;
use crate::bram::{Config, Inversions, Port as BlockPort, Size, Width, WriteMode};

/// A memory of at most this many words stays in logic, as Vivado keeps
/// one: in LUT RAM or registers.
const LOGIC_WORDS: usize = 64;

/// RAMB18E1s only: a RAMB36E1 holds no more than its two halves do, and
/// its byte enables don't split over its halves as two RAMB18E1s' do (each
/// half's four WE pins are byte lanes of the 36-bit word, every other bit),
/// which the decoder (`crate::decode`, half by half) doesn't model. A
/// RAMB36E1 instantiated in a design is still built.
const SIZES: [Size; 1] = [Size::Half];

fn log2(n: usize) -> usize {
    n.trailing_zeros() as usize
}

/// A write port, widened: `factor` consecutive words at `addr` (whose bits
/// below log2(factor) are ignored), word 0 first in `data` and `en`.
#[derive(Debug, Clone)]
struct WrPort {
    clk: Ctrl,
    addr: SigSpec,
    factor: usize,
    data: SigSpec,
    en: SigSpec,
    /// Which of the memory's write ports it was made of.
    ids: Vec<usize>,
}

/// A synchronous read port, widened likewise. `transparent` and
/// `collision_x` are per widened write port.
#[derive(Debug, Clone)]
struct RdPort {
    clk: Ctrl,
    en: SigBit,
    srst: SigBit,
    ce_over_srst: bool,
    srst_value: Const,
    init_value: Const,
    addr: SigSpec,
    factor: usize,
    data: SigSpec,
    transparent: Vec<bool>,
    collision_x: Vec<bool>,
}

struct Ports {
    wr: Vec<WrPort>,
    rd: Vec<RdPort>,
    /// Two write ports that never write one word on one edge: one's enable
    /// requires their addresses to differ. Without it, collisions get logic
    /// (`Resolve`).
    disjoint: bool,
}

/// Merges port pairs `{a, 0}` and `{a, 1}` (at the bit above their width)
/// until none are left.
fn widen<P: Clone>(
    mut ports: Vec<P>,
    addr: impl Fn(&P) -> &SigSpec,
    factor: impl Fn(&P) -> usize,
    compatible: impl Fn(&P, &P) -> bool,
    merge: impl Fn(&P, &P) -> P,
) -> Vec<P> {
    'again: loop {
        for i in 0..ports.len() {
            for j in 0..ports.len() {
                let (p, q) = (&ports[i], &ports[j]);
                if i == j || factor(p) != factor(q) || !compatible(p, q) {
                    continue;
                }
                let lg = log2(factor(p));
                let (a, b) = (addr(p), addr(q));
                if a.len() != b.len() || lg >= a.len() {
                    continue;
                }
                let pair = a[lg] == SigBit::ZERO
                    && b[lg] == SigBit::ONE
                    && (0..a.len()).all(|k| k == lg || a[k] == b[k]);
                if pair {
                    let m = merge(p, q);
                    let (lo, hi) = (i.min(j), i.max(j));
                    ports.remove(hi);
                    ports[lo] = m;
                    continue 'again;
                }
            }
        }
        return ports;
    }
}

/// `widened`: ports pairs at `{a, 0}` and `{a, 1}` merged (not for a
/// [`split`] memory, whose halves are meant for the block's two ports).
fn ports(mem: &Mem, widened: bool) -> Option<Ports> {
    if mem.offset != 0 || mem.size <= LOGIC_WORDS || mem.width == 0 {
        return None;
    }
    let mut wr = Vec::new();
    for (i, w) in mem.wr.iter().enumerate() {
        let clk = w.clk?;
        if w.wide_continuation {
            return None;
        }
        wr.push(WrPort {
            clk,
            addr: w.addr.clone(),
            factor: 1,
            data: w.data.clone(),
            en: w.en.clone(),
            ids: vec![i],
        });
    }
    // Writes of different bits of one word on one clock, as a write of a
    // byte at a time is in Verilog: one port, each bit with its own enable.
    let mut merged: Vec<WrPort> = Vec::new();
    for w in wr {
        let into = merged.iter_mut().find(|m| {
            m.clk == w.clk
                && m.addr == w.addr
                && m.en
                    .iter()
                    .zip(&w.en)
                    .all(|(&a, &b)| a == SigBit::ZERO || b == SigBit::ZERO)
        });
        match into {
            Some(m) => {
                for k in 0..m.en.len() {
                    if w.en[k] != SigBit::ZERO {
                        m.en[k] = w.en[k];
                        m.data[k] = w.data[k];
                    }
                }
                m.ids.extend(w.ids);
            }
            None => merged.push(w),
        }
    }
    let wr = if !widened {
        merged
    } else {
        widen(
            merged,
            |p| &p.addr,
            |p| p.factor,
            |p, q| p.clk == q.clk,
            |p, q| WrPort {
                clk: p.clk,
                addr: p.addr.clone(),
                factor: 2 * p.factor,
                data: [&p.data[..], &q.data[..]].concat(),
                en: [&p.en[..], &q.en[..]].concat(),
                ids: [&p.ids[..], &q.ids[..]].concat(),
            },
        )
    };
    let mut rd = Vec::new();
    for r in &mem.rd {
        let MemRd {
            addr,
            data,
            wide_continuation: false,
            sync: Some(s),
        } = r
        else {
            return None;
        };
        let MemRdSync {
            clk,
            en,
            arst,
            srst,
            ce_over_srst,
            init_value,
            srst_value,
            transparent,
            collision_x,
            ..
        } = s;
        // The block's read data has no asynchronous reset.
        if *arst != SigBit::ZERO {
            return None;
        }
        // Per widened write port: every write port it was made of that
        // can write this read's word alike (a constant address bit apart
        // never collides).
        let disjoint = |a: &SigSpec, b: &SigSpec| {
            a.iter()
                .zip(b)
                .any(|(x, y)| matches!((x, y), (SigBit::Const(p), SigBit::Const(q)) if p != q))
        };
        // A write never on this read's word (`apart`) collides with it never:
        // as if undefined.
        let per = |flags: &Vec<bool>, apart: bool| -> Option<Vec<bool>> {
            wr.iter()
                .map(|w| {
                    let mut it = w
                        .ids
                        .iter()
                        .filter(|&&i| !disjoint(&mem.wr[i].addr, addr))
                        .map(|&i| flags[i]);
                    let f = it.next().unwrap_or(apart);
                    it.all(|x| x == f).then_some(f)
                })
                .collect()
        };
        rd.push(RdPort {
            clk: *clk,
            en: *en,
            srst: *srst,
            ce_over_srst: *ce_over_srst,
            srst_value: srst_value.clone(),
            init_value: init_value.clone(),
            addr: addr.clone(),
            factor: 1,
            data: data.clone(),
            transparent: per(transparent, false)?,
            collision_x: per(collision_x, true)?,
        });
    }
    let rd = if !widened {
        rd
    } else {
        widen(
            rd,
            |p| &p.addr,
            |p| p.factor,
            |p, q| {
                p.clk == q.clk
                    && p.en == q.en
                    && p.srst == q.srst
                    && p.ce_over_srst == q.ce_over_srst
                    && p.transparent == q.transparent
                    && p.collision_x == q.collision_x
            },
            |p, q| RdPort {
                addr: p.addr.clone(),
                factor: 2 * p.factor,
                data: [&p.data[..], &q.data[..]].concat(),
                srst_value: Const([&p.srst_value.0[..], &q.srst_value.0[..]].concat()),
                init_value: Const([&p.init_value.0[..], &q.init_value.0[..]].concat()),
                ..p.clone()
            },
        )
    };
    Some(Ports {
        wr,
        rd,
        disjoint: false,
    })
}

/// What one block port does: a write port, a read port, or both at the
/// same address.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Use {
    wr: Option<usize>,
    rd: Option<usize>,
}

fn partial(w: &WrPort) -> bool {
    w.en.iter().any(|&e| e != w.en[0])
}

/// A read on the other block port than write `w` that has to see the
/// write's new data can, with logic: where the write hits the read's word
/// on its edge, a register of the written data stands in for what the
/// block read. Same clock, same word.
fn cross_port_emulated(p: &Ports, r: usize, w: usize) -> bool {
    let (r, w) = (&p.rd[r], &p.wr[w]);
    r.clk == w.clk && r.factor == w.factor && r.addr.len() == w.addr.len()
}

/// A read that has to see write `w`'s new data on the same edge.
fn sees_new(r: &RdPort, w: usize) -> bool {
    r.transparent[w] && !r.collision_x[w]
}

/// A read that has to see what write `w` overwrote on the same edge.
fn sees_old(r: &RdPort, w: usize) -> bool {
    !r.transparent[w] && !r.collision_x[w]
}

/// Each block port's write mode, and whether its own read of its write's
/// new data is bypassed in logic: a write whose old data the other port
/// reads is READ_FIRST; one whose own read sees its new data otherwise
/// WRITE_FIRST. None if the other port has to see the new data and logic
/// can't stand in.
fn modes(p: &Ports, sides: &[Use; 2]) -> Option<[(WriteMode, bool); 2]> {
    let mut out = [(WriteMode::ReadFirst, false); 2];
    for s in 0..2 {
        let Some(w) = sides[s].wr else { continue };
        let other = sides[1 - s].rd;
        if let Some(o) = other {
            if sees_new(&p.rd[o], w) && !cross_port_emulated(p, o, w) {
                return None;
            }
        }
        let old_wanted = other.is_some_and(|o| sees_old(&p.rd[o], w));
        out[s] = match sides[s].rd {
            Some(r) if sees_new(&p.rd[r], w) => {
                if old_wanted {
                    (WriteMode::ReadFirst, true)
                } else {
                    (WriteMode::WriteFirst, false)
                }
            }
            _ => (WriteMode::ReadFirst, false),
        };
    }
    Some(out)
}

/// The reads that see the other block port's write's new data, in logic,
/// with that write.
fn emulated_reads(p: &Ports, sides: &[Use; 2]) -> Vec<(usize, usize)> {
    (0..2)
        .filter_map(|s| {
            let r = sides[s].rd?;
            let w = sides[1 - s].wr?;
            sees_new(&p.rd[r], w).then_some((r, w))
        })
        .collect()
}

/// Both block ports writing one word on one edge: the block writes
/// neither reliably, so the netlist makes sure only one port writes each
/// bit, and it writes what `memory_map` would leave there (the write yosys
/// gives priority, `hi`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolve {
    /// The loser's word lies within the winner's (no wider), which writes
    /// all of it: the loser's write enable is masked.
    MaskLoser,
    /// The winner's word lies within the loser's narrower span, which
    /// writes all of it: the winner is masked and the loser writes the
    /// winner's data there.
    Redirect,
}

#[derive(Debug, Clone, Copy)]
struct Collision {
    hi: usize,
    lo: usize,
    how: Resolve,
}

/// How two writing block ports' collisions are resolved: `Ok(None)` if
/// at most one port writes, `Err` if neither way works (a port with byte
/// enables where the other must be masked by a single signal).
fn collision(mem: &Mem, p: &Ports, sides: &[Use; 2]) -> Result<Option<Collision>, ()> {
    let (Some(w0), Some(w1)) = (sides[0].wr, sides[1].wr) else {
        return Ok(None);
    };
    if p.disjoint {
        return Ok(None);
    }
    let hi_first = mem.wr[p.wr[w0].ids[0]].priority.get(p.wr[w1].ids[0]) == Some(&true);
    let (hi, lo) = if hi_first { (w0, w1) } else { (w1, w0) };
    let (fh, fl) = (p.wr[hi].factor, p.wr[lo].factor);
    let how = if fl <= fh && !partial(&p.wr[hi]) {
        Resolve::MaskLoser
    } else if fh < fl && !partial(&p.wr[lo]) {
        Resolve::Redirect
    } else {
        return Err(());
    };
    Ok(Some(Collision { hi, lo, how }))
}

/// The reads that a resolved collision changes from what the block gives:
/// one beside a write the netlist changed, reading its own new data.
fn bypassed_reads(p: &Ports, sides: &[Use; 2], c: Collision) -> Vec<usize> {
    let changed = match c.how {
        Resolve::MaskLoser => vec![c.lo],
        Resolve::Redirect => vec![c.lo, c.hi],
    };
    sides
        .iter()
        .filter(|u| u.wr.is_some_and(|w| changed.contains(&w)))
        .filter_map(|u| {
            let (w, r) = (u.wr?, u.rd?);
            sees_new(&p.rd[r], w).then_some(r)
        })
        .collect()
}

/// Every way the memory's ports go onto a block's two.
fn roles(p: &Ports) -> Vec<[Use; 2]> {
    let paired = |w: usize, r: usize| {
        let (w, r) = (&p.wr[w], &p.rd[r]);
        w.addr == r.addr && w.clk == r.clk && w.factor == r.factor
    };
    let rd = |r: usize| Use {
        wr: None,
        rd: Some(r),
    };
    let wr = |w: usize| Use {
        wr: Some(w),
        rd: None,
    };
    let both = |w: usize, r: usize| Use {
        wr: Some(w),
        rd: Some(r),
    };
    let none = Use::default();
    let mut out = Vec::new();
    match (p.wr.len(), p.rd.len()) {
        (0, 1) => out.push([rd(0), none]),
        (0, 2) => out.push([rd(0), rd(1)]),
        (1, 1) => {
            if paired(0, 0) {
                out.push([both(0, 0), none]);
            }
            // Vivado's simple dual port writes on A and reads on B; an
            // SDP-wide one reads on A.
            out.push([wr(0), rd(0)]);
            out.push([rd(0), wr(0)]);
        }
        (1, 2) => {
            for (i, j) in [(0, 1), (1, 0)] {
                if paired(0, i) {
                    out.push([both(0, i), rd(j)]);
                }
            }
        }
        (2, n) if n <= 2 => {
            // Each read shares its block port with the write at its address.
            let mut sides = [wr(0), wr(1)];
            let mut ok = true;
            for r in 0..n {
                match (0..2).find(|&w| paired(w, r) && sides[w].rd.is_none()) {
                    Some(w) => sides[w].rd = Some(r),
                    None => ok = false,
                }
            }
            if ok {
                out.push(sides);
            }
        }
        _ => {}
    }
    out
}

/// How a memory goes into blocks: which port does what, the blocks' size
/// and widths, and how many blocks deep (`rows`) and wide (`cols`).
///
/// The memory's words map to *base words* of width `base`, each holding
/// `slice` bits; a port of width P sees `depth(base) / depth(P)`
/// consecutive base words as one of its words, so ports of different
/// widths agree on where every bit is. With `parity`, a base word's bits
/// fill its parity bits too (bit 8 of each nine); without, only its data
/// bits.
#[derive(Debug, Clone)]
struct Layout {
    size: Size,
    sdp: bool,
    sides: [Use; 2],
    widths: [Option<Width>; 2],
    modes: [(WriteMode, bool); 2],
    base: Width,
    parity: bool,
    slice: usize,
    /// Address bits of one block's base words.
    block_bits: usize,
    /// Address bits of the memory (base words).
    abits: usize,
    rows: usize,
    cols: usize,
    /// Whether a writing port needs its byte enables.
    byteena: [bool; 2],
    /// Soft logic, roughly in LUTs: the tie-breaker.
    soft: usize,
}

/// Where a port column sits on the block's pins: a data or parity pin of
/// the port's (an SDP side's pins run over both ports, `{B, A}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pin {
    Data(usize),
    Parity(usize),
}

impl Layout {
    fn blocks(&self) -> usize {
        self.rows * self.cols
    }

    /// Bits a base word's slice can hold.
    fn capacity(base: Width, parity: bool) -> usize {
        if parity {
            base.bits()
        } else {
            base.data()
        }
    }

    /// Port column `c`'s pin, for a port of width `w`.
    fn pin(&self, w: Width, c: usize) -> Pin {
        if self.parity && w >= Width::X9 {
            match c % 9 {
                8 => Pin::Parity(c / 9),
                j => Pin::Data(c / 9 * 8 + j),
            }
        } else {
            let _ = w;
            Pin::Data(c)
        }
    }

    /// Port column of port bit `k` (port word sub-word `k / slice`).
    fn column(&self, k: usize) -> usize {
        let cap = Layout::capacity(self.base, self.parity);
        k / self.slice * cap + k % self.slice
    }

    /// Memory bits a port of width `w` reads or writes of one block: its
    /// base words' slices.
    fn port_bits(&self, w: Width) -> usize {
        self.slice * (self.base.depth(self.size) / w.depth(self.size))
    }

    /// Whether port width `w` is SDP's wide side, its pins `{B, A}`.
    fn wide(&self, w: Width) -> bool {
        self.sdp && w == self.size.sdp()
    }

    /// The memory bit (word-relative: port word sub-word `k / slice`) that
    /// port bit `k` of a block in column `col` holds, if any.
    fn bit(&self, w: usize, col: usize, k: usize) -> Option<usize> {
        let (sub, b) = (k / self.slice, k % self.slice);
        let bit = col * self.slice + b;
        (bit < w).then_some(sub * w + bit)
    }

    /// The block's contents index of base word `bw`'s slice bit `b`.
    fn linear(&self, bw: usize, b: usize) -> usize {
        let s = self.size;
        if self.parity && self.base >= Width::X9 {
            let (lane, j) = (b / 9, b % 9);
            match j {
                8 => s.data_bits() + bw * self.base.parity() + lane,
                j => bw * self.base.data() + lane * 8 + j,
            }
        } else {
            bw * self.base.data() + b
        }
    }

    /// The byte enable (lane) of port column `c`, for a port of width `w`.
    fn lane(&self, w: Width, c: usize) -> usize {
        if w < Width::X9 {
            0
        } else if self.parity {
            c / 9
        } else {
            c / 8
        }
    }
}

fn factor(p: &Ports, u: Use) -> Option<usize> {
    match (u.wr, u.rd) {
        (Some(w), _) => Some(p.wr[w].factor),
        (None, Some(r)) => Some(p.rd[r].factor),
        (None, None) => None,
    }
}

/// RAMB18 halves, then primitives, then soft logic.
fn cost(l: &Layout) -> (usize, usize, usize) {
    (l.blocks() * l.size.halves(), l.blocks(), l.soft)
}

/// Two write ports whose addresses differ in a constant bit.
fn constant_apart(p: &Ports) -> bool {
    let [a, b] = &p.wr[..] else { return false };
    a.addr
        .iter()
        .zip(&b.addr)
        .any(|(x, y)| matches!((x, y), (SigBit::Const(s), SigBit::Const(t)) if s != t))
}

/// The cheapest layout, if a block can take the memory.
fn layout(mem: &Mem, p: &Ports) -> Option<Layout> {
    let abits = (usize::BITS - (mem.size - 1).leading_zeros()) as usize;
    let mut best: Option<Layout> = None;
    for sides in roles(p) {
        if collision(mem, p, &sides).is_err() {
            continue;
        }
        let Some(modes) = modes(p, &sides) else {
            continue;
        };
        let factors = sides.map(|u| factor(p, u));
        for size in SIZES {
            for base in Width::ALL {
                for parity in [true, false] {
                    if parity && base < Width::X9 {
                        continue;
                    }
                    let slice = mem.width.min(Layout::capacity(base, parity));
                    let widths = factors.map(|f| {
                        f.and_then(|f| {
                            Width::ALL
                                .into_iter()
                                .find(|w| w.depth(size) * f == base.depth(size))
                        })
                    });
                    if factors
                        .iter()
                        .zip(&widths)
                        .any(|(f, w)| f.is_some() != w.is_some())
                    {
                        continue;
                    }
                    // A width past TDP's is SDP: read on A, written on B.
                    let wide = widths.iter().flatten().any(|&w| w > size.tdp_max());
                    if widths.iter().flatten().any(|&w| w > size.sdp()) {
                        continue;
                    }
                    let sdp = wide;
                    if sdp && (sides[0].wr.is_some() || sides[1].rd.is_some()) {
                        continue;
                    }
                    // An SDP block's write modes are one, READ_FIRST.
                    if sdp && modes.iter().any(|m| m.0 != WriteMode::ReadFirst) {
                        continue;
                    }
                    let block_bits = log2(base.depth(size));
                    let rows = if abits > block_bits {
                        mem.size.div_ceil(base.depth(size))
                    } else {
                        1
                    };
                    let mut l = Layout {
                        size,
                        sdp,
                        sides,
                        widths,
                        modes,
                        base,
                        parity,
                        slice,
                        block_bits,
                        abits,
                        rows,
                        cols: mem.width.div_ceil(slice),
                        byteena: [false; 2],
                        soft: 0,
                    };
                    let mut ok = true;
                    for s in 0..2 {
                        let (Some(w), Some(width)) = (sides[s].wr, widths[s]) else {
                            continue;
                        };
                        let w = &p.wr[w];
                        for col in 0..l.cols {
                            let mut lanes: Vec<Option<SigBit>> = vec![None; width.lanes()];
                            for k in 0..l.slice * w.factor {
                                let Some(bit) = l.bit(mem.width, col, k) else {
                                    continue;
                                };
                                let g = &mut lanes[l.lane(width, l.column(k))];
                                match *g {
                                    Some(e) if e != w.en[bit] => ok = false,
                                    _ => *g = Some(w.en[bit]),
                                }
                            }
                            let mut sigs: Vec<SigBit> = lanes.into_iter().flatten().collect();
                            sigs.dedup();
                            if sigs.len() > 1 {
                                l.byteena[s] = true;
                            }
                        }
                    }
                    // A READ_FIRST write of some of its bytes, read on the
                    // other port narrower: Vivado's unisim model gives that
                    // read the new data, UG473 the old; the board would
                    // settle it, so it isn't relied on.
                    for s in 0..2 {
                        if let (true, Some(w), Some(r), Some(ww), Some(rw)) = (
                            l.byteena[s],
                            sides[s].wr,
                            sides[1 - s].rd,
                            widths[s],
                            widths[1 - s],
                        ) {
                            ok &= !(rw < ww && sees_old(&p.rd[r], w));
                        }
                    }
                    if !ok {
                        continue;
                    }
                    let bypass =
                        modes.iter().filter(|m| m.1).count() + emulated_reads(p, &sides).len();
                    l.soft =
                        if rows > 1 { mem.width * rows + rows } else { 0 } + bypass * mem.width;
                    if best.as_ref().is_none_or(|b| cost(&l) < cost(b)) {
                        best = Some(l);
                    }
                }
            }
        }
    }
    best
}

/// Whether the blocks take this memory, as it is or one copy per read
/// port ([`per_read_port`]).
pub fn mappable(mem: &Mem) -> bool {
    fits(mem) || per_read_port(mem).is_some()
}

fn fits(mem: &Mem) -> bool {
    ports(mem, true).is_some_and(|p| layout(mem, &p).is_some())
        || split(mem).is_some_and(|h| ports(&h, false).is_some_and(|p| layout(&h, &p).is_some()))
}

/// A memory with more read ports than its blocks have, as one copy per
/// read port, each with every write: how a register file read at two
/// addresses is built (two blocks, each written alike). None if the memory
/// fits as it is, or a copy doesn't.
fn per_read_port(mem: &Mem) -> Option<Vec<Mem>> {
    if fits(mem) || mem.wr.is_empty() {
        return None;
    }
    let mut groups: Vec<Vec<MemRd>> = Vec::new();
    for r in &mem.rd {
        match groups.last_mut() {
            Some(g) if r.wide_continuation => g.push(r.clone()),
            _ => groups.push(vec![r.clone()]),
        }
    }
    if groups.len() < 2 {
        return None;
    }
    let copies: Vec<Mem> = groups
        .into_iter()
        .enumerate()
        .map(|(k, rd)| Mem {
            memid: format!("{}$rd{k}", mem.memid),
            rd,
            ..mem.clone()
        })
        .collect();
    copies.iter().all(fits).then_some(copies)
}

/// A single port memory as one of half the width and twice the depth,
/// its port split over the block's two at adjacent words (`{a, 0}` the
/// low half, `{a, 1}` the high): how Vivado puts a 512 x 36 single port
/// into one RAMB18E1 (x18 per port) rather than a RAMB36E1. The halves
/// never touch one word, so they never collide.
fn split(mem: &Mem) -> Option<Mem> {
    if !mem.width.is_multiple_of(2) || mem.wr.len() > 1 || mem.rd.len() != 1 {
        return None;
    }
    let h = mem.width / 2;
    let at = |addr: &SigSpec, k: usize| -> SigSpec {
        std::iter::once(SigBit::Const(State::from(k == 1)))
            .chain(addr.iter().copied())
            .collect()
    };
    let halve = |c: &Const, k: usize| Const(c.0[k * h..(k + 1) * h].to_vec());
    let w = mem.wr.first();
    if w.is_some_and(|w| w.wide_continuation || w.addr != mem.rd[0].addr) {
        return None;
    }
    let r = &mem.rd[0];
    if r.wide_continuation {
        return None;
    }
    let wr: Vec<MemWr> = w
        .map(|w| {
            (0..2)
                .map(|k| MemWr {
                    clk: w.clk,
                    en: w.en[k * h..(k + 1) * h].to_vec(),
                    addr: at(&w.addr, k),
                    data: w.data[k * h..(k + 1) * h].to_vec(),
                    wide_continuation: false,
                    priority: vec![false; 2],
                })
                .collect()
        })
        .unwrap_or_default();
    let rd: Vec<MemRd> = (0..2)
        .map(|k| MemRd {
            addr: at(&r.addr, k),
            data: r.data[k * h..(k + 1) * h].to_vec(),
            wide_continuation: false,
            sync: r.sync.as_ref().map(|s| MemRdSync {
                init_value: halve(&s.init_value, k),
                arst_value: halve(&s.arst_value, k),
                srst_value: halve(&s.srst_value, k),
                transparent: (0..wr.len()).map(|j| j == k && s.transparent[0]).collect(),
                collision_x: (0..wr.len()).map(|j| j != k || s.collision_x[0]).collect(),
                ..s.clone()
            }),
        })
        .collect();
    Some(Mem {
        memid: mem.memid.clone(),
        size: 2 * mem.size,
        offset: 0,
        width: h,
        abits: mem.abits + 1,
        init: Const(
            mem.init
                .0
                .chunks(h)
                .flat_map(|c| c.iter().copied())
                .collect(),
        ),
        rd,
        wr,
    })
}

/// A memory's declaration asks for logic (`ram_style` and friends).
pub fn wants_logic(attrs: &chipsmith_netlist::Attributes) -> bool {
    [
        "ramstyle",
        "ram_style",
        "syn_ramstyle",
        "romstyle",
        "rom_style",
        "syn_romstyle",
    ]
    .iter()
    .filter_map(|k| attrs.get(*k))
    .any(|v| {
        let s = match v {
            ParamValue::Str(s) => s.to_ascii_lowercase(),
            _ => return false,
        };
        matches!(s.trim(), "logic" | "registers" | "distributed")
    }) || attrs.contains_key("logic_block")
}

// ---------------------------------------------------------------------
// The logic around: what the module says about the ports' signals.

/// Who drives and who reads each net.
struct Context {
    driver: Vec<Option<usize>>,
    /// (cell, register D bit) per reader.
    readers: Vec<Vec<(usize, Option<usize>)>>,
    /// Nets a module port reads.
    exported: Vec<bool>,
    inits: rustc_hash::FxHashMap<chipsmith_netlist::Net, State>,
}

impl Context {
    fn new(m: &Module) -> Context {
        let n = m.net_count();
        let mut driver = vec![None; n];
        let mut readers: Vec<Vec<(usize, Option<usize>)>> = vec![Vec::new(); n];
        for (i, c) in m.cells.iter().enumerate() {
            if let CellKind::Ff(ff) = &c.kind {
                if let FfKind::Dff { d, .. } = &ff.kind {
                    for (k, b) in d.iter().enumerate() {
                        if let SigBit::Net(net) = b {
                            readers[net.index()].push((i, Some(k)));
                        }
                    }
                }
            }
            c.kind.for_each_port(|p| {
                for b in p.bits {
                    let SigBit::Net(net) = b else { continue };
                    match p.dir {
                        Some(Dir::Out) => driver[net.index()] = Some(i),
                        _ => {
                            let is_d = matches!(&c.kind, CellKind::Ff(_)) && p.name == "D";
                            if !is_d {
                                readers[net.index()].push((i, None));
                            }
                        }
                    }
                }
            });
        }
        let mut exported = vec![false; n];
        for w in &m.wires {
            if matches!(w.port, Some(Dir::Out | Dir::InOut)) {
                for b in &w.bits {
                    if let SigBit::Net(net) = b {
                        exported[net.index()] = true;
                    }
                }
            }
        }
        Context {
            driver,
            readers,
            exported,
            inits: inits(m),
        }
    }

    /// Whether `x` can only be 1 while `e` is: `x` is `e`, or an AND (or a
    /// `s ? b : 0`) of it, a few levels deep.
    fn implies(&self, m: &Module, x: SigBit, e: SigBit, depth: u32) -> bool {
        if x == e || x == SigBit::ZERO {
            return true;
        }
        let SigBit::Net(n) = x else { return false };
        if depth == 0 {
            return false;
        }
        let Some(ci) = self.driver[n.index()] else {
            return false;
        };
        let next = |a: SigBit| self.implies(m, a, e, depth - 1);
        match &m.cells[ci].kind {
            CellKind::Binary(b)
                if matches!(
                    b.op,
                    chipsmith_netlist::BinaryOp::And | chipsmith_netlist::BinaryOp::LogicAnd
                ) && b.y.len() == 1
                    && b.a.sig.len() == 1
                    && b.b.sig.len() == 1 =>
            {
                next(b.a.sig[0]) || next(b.b.sig[0])
            }
            CellKind::Mux(mx) => {
                let Some(j) = mx.y.iter().position(|&y| y == x) else {
                    return false;
                };
                (mx.a[j] == SigBit::ZERO && (next(mx.s) || next(mx.b[j])))
                    || (mx.b[j] == SigBit::ZERO && next(mx.a[j]))
            }
            CellKind::Gate(g) if g.op() == chipsmith_netlist::GateOp::And => {
                g.inputs().iter().any(|&a| next(a))
            }
            _ => false,
        }
    }

    /// Whether `x` is `!e`: an inverter, or `e ? 0 : 1`.
    fn negates(&self, m: &Module, x: SigBit, e: SigBit) -> bool {
        let SigBit::Net(n) = x else { return false };
        let Some(ci) = self.driver[n.index()] else {
            return false;
        };
        match &m.cells[ci].kind {
            CellKind::Gate(g) if g.op() == chipsmith_netlist::GateOp::Not => g.inputs() == [e],
            CellKind::Unary(u)
                if matches!(
                    u.op,
                    chipsmith_netlist::UnaryOp::Not | chipsmith_netlist::UnaryOp::LogicNot
                ) && u.y.len() == 1 =>
            {
                u.a.sig == [e]
            }
            CellKind::Mux(mx) => {
                let Some(j) = mx.y.iter().position(|&y| y == x) else {
                    return false;
                };
                mx.s == e && mx.a[j] == SigBit::ONE && mx.b[j] == SigBit::ZERO
            }
            _ => false,
        }
    }

    /// Whether the two write ports can't write one word on one edge: every
    /// enable bit of one of them is 1 only while their addresses differ
    /// above the wider port's span.
    fn disjoint(&self, m: &Module, p: &Ports) -> bool {
        let [a, b] = &p.wr[..] else { return false };
        let lg = log2(a.factor.max(b.factor));
        let top = a.addr.len().max(b.addr.len());
        let bit = |x: &WrPort, t: usize| x.addr.get(t).copied().unwrap_or(SigBit::ZERO);
        let pairs: Vec<(SigBit, SigBit)> = (lg..top).map(|t| (bit(a, t), bit(b, t))).collect();
        [a, b].iter().any(|w| {
            w.en.iter()
                .all(|&e| self.requires_difference(m, e, &pairs, 6))
        })
    }

    /// Whether `x` can only be 1 while some pair differs: an AND (or a
    /// `s ? b : 0`) of an OR of XORs of pairs, as `!=` lowers.
    fn requires_difference(
        &self,
        m: &Module,
        x: SigBit,
        pairs: &[(SigBit, SigBit)],
        depth: u32,
    ) -> bool {
        if x == SigBit::ZERO {
            return true;
        }
        let SigBit::Net(n) = x else { return false };
        if depth == 0 {
            return false;
        }
        let Some(ci) = self.driver[n.index()] else {
            return false;
        };
        let next = |a: SigBit| self.requires_difference(m, a, pairs, depth - 1);
        let is_pair = |a: SigBit, b: SigBit| pairs.iter().any(|&p| p == (a, b) || p == (b, a));
        fn differs(
            ctx: &Context,
            m: &Module,
            x: SigBit,
            is_pair: &dyn Fn(SigBit, SigBit) -> bool,
            depth: u32,
        ) -> bool {
            let SigBit::Net(n) = x else { return false };
            let Some(ci) = ctx.driver[n.index()].filter(|_| depth > 0) else {
                return false;
            };
            match &m.cells[ci].kind {
                CellKind::Gate(g) => match g.op() {
                    chipsmith_netlist::GateOp::Xor => {
                        let i = g.inputs();
                        i.len() == 2 && is_pair(i[0], i[1])
                    }
                    chipsmith_netlist::GateOp::Or => g
                        .inputs()
                        .iter()
                        .all(|&a| differs(ctx, m, a, is_pair, depth - 1)),
                    _ => false,
                },
                _ => false,
            }
        }
        if differs(self, m, x, &is_pair, 16) {
            return true;
        }
        match &m.cells[ci].kind {
            CellKind::Mux(mx) => {
                let Some(j) = mx.y.iter().position(|&y| y == x) else {
                    return false;
                };
                (mx.a[j] == SigBit::ZERO && (next(mx.s) || next(mx.b[j])))
                    || (mx.b[j] == SigBit::ZERO && next(mx.a[j]))
            }
            CellKind::Gate(g) if g.op() == chipsmith_netlist::GateOp::And => {
                g.inputs().iter().any(|&a| next(a))
            }
            _ => false,
        }
    }

    /// The same signal for every enable bit that computes the same thing:
    /// `proc` gives each bit of a word written under nested conditions its
    /// own multiplexer output, alike but for constants.
    fn same_enables(&self, m: &Module, mem: &mut Mem) {
        fn key(
            ctx: &Context,
            m: &Module,
            b: SigBit,
            depth: u32,
            memo: &mut HashMap<SigBit, String>,
        ) -> String {
            if let Some(k) = memo.get(&b) {
                return k.clone();
            }
            let k = match b {
                SigBit::Net(n) if depth > 0 => {
                    match ctx.driver[n.index()].map(|c| &m.cells[c].kind) {
                        Some(CellKind::Mux(mx)) => match mx.y.iter().position(|&y| y == b) {
                            Some(j) => format!(
                                "({:?} ? {} : {})",
                                mx.s,
                                key(ctx, m, mx.b[j], depth - 1, memo),
                                key(ctx, m, mx.a[j], depth - 1, memo)
                            ),
                            None => format!("{b:?}"),
                        },
                        _ => format!("{b:?}"),
                    }
                }
                _ => format!("{b:?}"),
            };
            memo.insert(b, k.clone());
            k
        }
        let mut memo = HashMap::new();
        for w in &mut mem.wr {
            let mut first: HashMap<String, SigBit> = HashMap::new();
            for e in &mut w.en {
                let k = key(self, m, *e, 6, &mut memo);
                *e = *first.entry(k).or_insert(*e);
            }
        }
    }

    /// The register a read port's data all go into and nothing else, if
    /// its clocking is the block's output register's: the port's clock, an
    /// enable (REGCE, active high) and a synchronous reset (RSTREG).
    fn output_register(&self, m: &Module, r: &RdPort) -> Option<Absorb> {
        let mut bits = Vec::new();
        let mut clocking: Option<(Ctrl, Option<SigBit>, Option<Ctrl>, bool)> = None;
        let mut srval = Vec::new();
        let mut init = Vec::new();
        for b in &r.data {
            let SigBit::Net(n) = b else { return None };
            if self.exported[n.index()] {
                return None;
            }
            let [(cell, Some(bit))] = self.readers[n.index()][..] else {
                return None;
            };
            let CellKind::Ff(Ff {
                q,
                kind: FfKind::Dff { clk, control, .. },
            }) = &m.cells[cell].kind
            else {
                return None;
            };
            if *clk != r.clk {
                return None;
            }
            let pos = |c: &Option<Ctrl>| match c {
                None => Some(None),
                Some(c) if c.polarity == Polarity::Pos => Some(Some(c.sig)),
                Some(_) => None,
            };
            let (en, rst, rst_first, value) = match control {
                DffControl::Plain { en } => (pos(en)?, None, true, State::X),
                DffControl::Sync { srst, value, en } => {
                    let (en, first) = match en {
                        SyncEnable::Always => (None, true),
                        SyncEnable::ResetOverEnable(e) => (pos(&Some(*e))?, true),
                        SyncEnable::EnableOverReset(e) => (pos(&Some(*e))?, false),
                    };
                    (en, Some(*srst), first, value.0[bit])
                }
                _ => return None,
            };
            let this = (r.clk, en, rst, rst_first);
            if clocking.is_some_and(|c| c != this) {
                return None;
            }
            clocking = Some(this);
            srval.push(value);
            init.push(
                q[bit]
                    .net()
                    .and_then(|n| self.inits.get(&n))
                    .copied()
                    .unwrap_or(State::X),
            );
            bits.push((cell, bit, q[bit]));
        }
        let (_, en, rst, rst_first) = clocking?;
        Some(Absorb {
            bits,
            en,
            rst,
            rst_first,
            srval: Const(srval),
            init: Const(init),
        })
    }
}

/// A register on a read port's data that the blocks' output registers
/// replace.
#[derive(Debug, Clone)]
struct Absorb {
    /// Per read data bit: the register cell, its bit, its output.
    bits: Vec<(usize, usize, SigBit)>,
    en: Option<SigBit>,
    rst: Option<Ctrl>,
    rst_first: bool,
    /// Per read data bit.
    srval: Const,
    init: Const,
}

/// How a read port is built.
#[derive(Debug, Clone)]
struct ReadImpl {
    /// The block port's enable is the read enable (else logic holds what
    /// the block read).
    hw_en: bool,
    /// The read is held while its port writes (NO_CHANGE), the enable 1.
    no_change: bool,
    /// The read's synchronous reset is the latch's (RSTRAM).
    hw_srst: bool,
    outreg: Option<Absorb>,
}

/// Two values agree where both are defined; the defined one of each bit.
fn merge(a: &Const, b: &Const) -> Option<Const> {
    a.0.iter()
        .zip(&b.0)
        .map(|(&x, &y)| match (x, y) {
            (State::X, v) | (v, State::X) => Some(v),
            (x, y) if x == y => Some(x),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(Const)
}

fn realize(m: &Module, ctx: &Context, mem: &Mem, p: &Ports, l: &Layout) -> Vec<ReadImpl> {
    let mut bypassed: Vec<usize> = match collision(mem, p, &l.sides) {
        Ok(Some(c)) => bypassed_reads(p, &l.sides, c),
        _ => Vec::new(),
    };
    bypassed.extend(emulated_reads(p, &l.sides).into_iter().map(|(r, _)| r));
    for s in 0..2 {
        if l.modes[s].1 {
            bypassed.extend(l.sides[s].rd);
        }
    }
    let mut reads = vec![
        ReadImpl {
            hw_en: false,
            no_change: false,
            hw_srst: false,
            outreg: None,
        };
        p.rd.len()
    ];
    for s in 0..2 {
        let Some(r) = l.sides[s].rd else { continue };
        let rd = &p.rd[r];
        let w = l.sides[s].wr;
        // The block port's enable stops its write too: the write must
        // imply the read enable. Or the read is off exactly while the port
        // writes (NO_CHANGE).
        let (hw_en, no_change) = match w {
            None => (true, false),
            Some(w) => {
                let wr = &p.wr[w];
                if wr.en.iter().all(|&x| ctx.implies(m, x, rd.en, 4)) {
                    (true, false)
                } else if !partial(wr)
                    && l.modes[s] == (WriteMode::ReadFirst, false)
                    && ctx.negates(m, rd.en, wr.en[0])
                {
                    (false, true)
                } else {
                    (false, false)
                }
            }
        };
        // RSTRAM acts on enabled edges: the enable is the read's (a reset
        // over the enable widens it to `en | srst`, which still stops no
        // write the read enable allows). Not beside NO_CHANGE, whose
        // writes would reset too.
        let hw_srst = rd.srst != SigBit::ZERO && !no_change && (hw_en || rd.en == SigBit::ONE);
        let soft = (!hw_en && !no_change && rd.en != SigBit::ONE)
            || (rd.srst != SigBit::ZERO && !hw_srst)
            || bypassed.contains(&r);
        let mut outreg = None;
        if !soft {
            if let Some(abs) = ctx.output_register(m, rd) {
                // The latch and the register share INIT and SRVAL.
                let srval_ok =
                    !hw_srst || abs.rst.is_none() || merge(&rd.srst_value, &abs.srval).is_some();
                let init_ok = merge(&rd.init_value, &abs.init).is_some();
                if srval_ok && init_ok {
                    outreg = Some(abs);
                }
            }
        }
        reads[r] = ReadImpl {
            hw_en,
            no_change,
            hw_srst,
            outreg,
        };
    }
    reads
}

// ---------------------------------------------------------------------
// Building it.

/// `a == value`, as gates.
fn equals(g: &mut Gates, a: &[SigBit], value: usize) -> SigBit {
    let lits: Vec<SigBit> = a
        .iter()
        .enumerate()
        .map(|(i, &b)| if value >> i & 1 == 1 { b } else { g.not(b) })
        .collect();
    g.reduce(chipsmith_netlist::GateOp::And, &lits, SigBit::ONE)
}

fn add_ff(g: &mut Gates, clk: Ctrl, en: Option<SigBit>, d: SigSpec) -> SigSpec {
    let q = g.m.new_sig(d.len());
    g.add(CellKind::Ff(Ff {
        q: q.clone(),
        kind: FfKind::Dff {
            clk,
            d,
            control: DffControl::Plain {
                en: en.map(Ctrl::pos),
            },
        },
    }));
    q
}

fn mux_word(g: &mut Gates, a: &[SigBit], b: &[SigBit], s: SigBit) -> SigSpec {
    a.iter().zip(b).map(|(&a, &b)| g.mux(a, b, s)).collect()
}

/// Removes bits `drop` from a word-level register.
fn drop_bits(ff: &mut Ff<Word>, drop: &[bool]) {
    let keep = |s: &mut SigSpec| {
        let mut it = drop.iter();
        s.retain(|_| !*it.next().expect("one flag per bit"));
    };
    let keepc = |c: &mut Const| {
        let mut it = drop.iter();
        c.0.retain(|_| !*it.next().expect("one flag per bit"));
    };
    keep(&mut ff.q);
    if let FfKind::Dff { d, control, .. } = &mut ff.kind {
        keep(d);
        if let DffControl::Sync { value, .. } | DffControl::Async { value, .. } = control {
            keepc(value);
        }
    }
}

/// A value per port pin (data then parity), from a value per memory bit
/// of the port's word and the block column `col`.
fn pin_value(l: &Layout, w: Width, mem_width: usize, col: usize, per_bit: &Const) -> u128 {
    let mut v = 0u128;
    for k in 0..l.port_bits(w) {
        let Some(bit) = l.bit(mem_width, col, k) else {
            continue;
        };
        if per_bit.0.get(bit) == Some(&State::One) {
            let i = match l.pin(w, l.column(k)) {
                Pin::Data(i) => i,
                Pin::Parity(i) => w.data() + i,
            };
            v |= 1 << i;
        }
    }
    v
}

/// One memory's blocks and logic. Returns the blocks (to go where the
/// memory was), the read data's new drivers, and power-up values of the
/// registers made.
fn build(
    g: &mut Gates,
    mem: &Mem,
    p: &Ports,
    l: &Layout,
    reads: &[ReadImpl],
) -> (
    Vec<chipsmith_netlist::Cell>,
    Vec<(SigBit, SigBit)>,
    Vec<(SigBit, State)>,
) {
    let w = mem.width;
    let name = mem.memid.trim_start_matches('\\');
    let size = l.size;
    let mut powerup = Vec::new();

    // Per block port: its address pins, row bits and range check.
    struct SideSigs {
        pins: SigSpec,
        row_bits: SigSpec,
        in_range: SigBit,
    }
    let sides: Vec<Option<SideSigs>> = (0..2)
        .map(|s| {
            let u = l.sides[s];
            let width = l.widths[s]?;
            let (addr, f) = match (u.wr, u.rd) {
                (Some(i), _) => (&p.wr[i].addr, p.wr[i].factor),
                (None, Some(i)) => (&p.rd[i].addr, p.rd[i].factor),
                _ => return None,
            };
            let lg = log2(f);
            let bit = |t: usize| addr.get(t).copied().unwrap_or(SigBit::ZERO);
            // The port's word address on the pins above its width's
            // `shift`; the pins below (and a RAMB36E1's cascade bit 15)
            // tied high, as Vivado ties them; a shallow memory's bits above
            // its own 0.
            let shift = width.shift();
            let mut pins = vec![SigBit::ONE; size.addr_pins()];
            for i in 0..log2(width.depth(size)) {
                let t = lg + i;
                pins[shift + i] = if t < l.abits { bit(t) } else { SigBit::ZERO };
            }
            let row_bits = if l.rows > 1 {
                (l.block_bits..l.abits).map(bit).collect()
            } else {
                Vec::new()
            };
            let high: Vec<SigBit> = addr
                .iter()
                .skip(l.abits)
                .copied()
                .filter(|&b| b != SigBit::ZERO)
                .collect();
            let in_range = if high.is_empty() || u.wr.is_none() {
                SigBit::ONE
            } else {
                let any = g.reduce_or(&high);
                g.not(any)
            };
            Some(SideSigs {
                pins,
                row_bits,
                in_range,
            })
        })
        .collect();

    // Write collisions (`Resolve`): enables and data as the block gets
    // them, and per read the bits it gets wrong, with what the RTL reads
    // there instead (registered below like the block's read).
    let mut wr_en: Vec<SigSpec> = p.wr.iter().map(|x| x.en.clone()).collect();
    let mut wr_data: Vec<SigSpec> = p.wr.iter().map(|x| x.data.clone()).collect();
    let mut fixes: HashMap<usize, (SigSpec, SigSpec)> = HashMap::new();
    if let Ok(Some(c)) = collision(mem, p, &l.sides) {
        let (ph, pl) = (&p.wr[c.hi], &p.wr[c.lo]);
        let (lg_h, lg_l) = (log2(ph.factor), log2(pl.factor));
        let top = ph.addr.len().max(pl.addr.len());
        let eq: Vec<SigBit> = (lg_h.max(lg_l)..top)
            .map(|t| {
                let bit = |x: &WrPort| x.addr.get(t).copied().unwrap_or(SigBit::ZERO);
                g.xnor(bit(ph), bit(pl))
            })
            .collect();
        let same = g.reduce(chipsmith_netlist::GateOp::And, &eq, SigBit::ONE);
        let reads_of: Vec<(usize, usize)> = l
            .sides
            .iter()
            .filter_map(|u| Some((u.wr?, u.rd?)))
            .filter(|&(_, r)| bypassed_reads(p, &l.sides, c).contains(&r))
            .collect();
        let read_of = |wi: usize| reads_of.iter().find(|&&(w, _)| w == wi).map(|&(_, r)| r);
        match c.how {
            Resolve::MaskLoser => {
                let clash = g.and(ph.en[0], same);
                let keep = g.not(clash);
                let lost: SigSpec = pl.en.iter().map(|&e| g.and(e, clash)).collect();
                wr_en[c.lo] = pl.en.iter().map(|&e| g.and(e, keep)).collect();
                if let Some(r) = read_of(c.lo) {
                    fixes.insert(r, (lost, pl.data.clone()));
                }
            }
            Resolve::Redirect => {
                let clash = g.and(pl.en[0], same);
                let keep = g.not(clash);
                let lost: SigSpec = ph.en.iter().map(|&e| g.and(e, clash)).collect();
                wr_en[c.hi] = ph.en.iter().map(|&e| g.and(e, keep)).collect();
                let sel: SigSpec = (lg_h..lg_l)
                    .map(|t| ph.addr.get(t).copied().unwrap_or(SigBit::ZERO))
                    .collect();
                let mut taken = vec![SigBit::ZERO; pl.data.len()];
                for sub in 0..pl.factor {
                    let here = equals(g, &sel, sub >> lg_h);
                    let here = g.and(clash, here);
                    for b in 0..w {
                        let (k, j) = (sub * w + b, (sub % ph.factor) * w + b);
                        taken[k] = g.and(here, ph.en[j]);
                        wr_data[c.lo][k] = g.mux(pl.data[k], ph.data[j], taken[k]);
                    }
                }
                if let Some(r) = read_of(c.lo) {
                    fixes.insert(r, (taken, pl.data.clone()));
                }
                if let Some(r) = read_of(c.hi) {
                    fixes.insert(r, (lost, ph.data.clone()));
                }
            }
        }
    }

    // Reads that see the other port's new data, in logic.
    for (r, wi) in emulated_reads(p, &l.sides) {
        let (rd, wr) = (&p.rd[r], &p.wr[wi]);
        let eq: Vec<SigBit> = rd
            .addr
            .iter()
            .zip(&wr.addr)
            .map(|(&a, &b)| g.xnor(a, b))
            .collect();
        let same = g.reduce(chipsmith_netlist::GateOp::And, &eq, SigBit::ONE);
        let hit: SigSpec = wr_en[wi].iter().map(|&e| g.and(e, same)).collect();
        fixes.insert(r, (hit, wr_data[wi].clone()));
    }
    // Reads of their own port's new data on a READ_FIRST port, likewise.
    for s in 0..2 {
        if let (true, Some(r), Some(wi)) = (l.modes[s].1, l.sides[s].rd, l.sides[s].wr) {
            // The write as the RTL has it, before any collision masking:
            // this read's own new data, whichever port won the word.
            fixes.insert(r, (p.wr[wi].en.clone(), p.wr[wi].data.clone()));
        }
    }

    let mut blocks = Vec::new();
    // Per row and column: each side's read pins, {parity, data} by pin.
    let mut douts: Vec<Vec<[Vec<SigBit>; 2]>> = Vec::new();
    let hits: Vec<Vec<SigBit>> = (0..2)
        .map(|s| {
            (0..l.rows)
                .map(|row| match &sides[s] {
                    Some(sg) if l.rows > 1 && l.sides[s].wr.is_some() => {
                        equals(g, &sg.row_bits, row)
                    }
                    _ => SigBit::ONE,
                })
                .collect()
        })
        .collect();
    let read_width = |s: usize| l.sides[s].rd.and(l.widths[s]);
    let write_width = |s: usize| l.sides[s].wr.and(l.widths[s]);
    for (row, (&hit_a, &hit_b)) in hits[0].iter().zip(&hits[1]).enumerate() {
        let hit = [hit_a, hit_b];
        let mut row_douts = Vec::new();
        for col in 0..l.cols {
            let mut contents = vec![false; size.data_bits() + size.parity_bits()];
            let base_words = l.base.depth(size);
            for bw in 0..base_words {
                let word = if l.abits >= l.block_bits {
                    row * base_words + bw
                } else {
                    bw
                };
                if word >= mem.size {
                    continue;
                }
                for b in 0..l.slice {
                    let bit = col * l.slice + b;
                    if bit < w && mem.init.0[word * w + bit] == State::One {
                        contents[l.linear(bw, b)] = true;
                    }
                }
            }
            // Port configurations; SDP's ports' values run over both
            // ports' pins (`{B, A}`), each port's half its own.
            let mut ports = [BlockPort::unused(), BlockPort::unused()];
            let mut inv = Inversions::default();
            for s in 0..2 {
                ports[s].read = read_width(s);
                ports[s].write = write_width(s);
                ports[s].mode = match l.sides[s].rd {
                    Some(r) if reads[r].no_change => WriteMode::NoChange,
                    _ => l.modes[s].0,
                };
                let clk = match (l.sides[s].wr, l.sides[s].rd) {
                    (Some(wi), _) => Some(p.wr[wi].clk),
                    (None, Some(r)) => Some(p.rd[r].clk),
                    _ => None,
                };
                inv.clk[s] = clk.is_some_and(|c| c.polarity == Polarity::Neg);
            }
            if l.sdp {
                // Both READ_FIRST (`layout`).
                ports = ports.map(|p| BlockPort {
                    mode: WriteMode::ReadFirst,
                    ..p
                });
            }
            for s in 0..2 {
                let Some(r) = l.sides[s].rd else { continue };
                let (rd, ri) = (&p.rd[r], &reads[r]);
                let width = l.widths[s].expect("a reading port has a width");
                let mut init = rd.init_value.clone();
                let mut srval = rd.srst_value.clone();
                if let Some(abs) = &ri.outreg {
                    ports[s].out_reg = true;
                    ports[s].rstreg_first = abs.rst_first;
                    init = merge(&init, &abs.init).expect("checked in realize");
                    if abs.rst.is_some() {
                        srval = if ri.hw_srst {
                            merge(&srval, &abs.srval).expect("checked in realize")
                        } else {
                            abs.srval.clone()
                        };
                    }
                }
                let iv = pin_value(l, width, w, col, &init);
                let sv = pin_value(l, width, w, col, &srval);
                if l.wide(width) {
                    // The wide read's halves: A's pins below, B's above, each
                    // {parity, data} of an x18 (x36).
                    let half = size.tdp_max();
                    let (dw, pw) = (half.data(), half.parity());
                    let split = |v: u128| -> (u64, u64) {
                        let data = v & ((1u128 << (2 * dw)) - 1);
                        let par = v >> (2 * dw);
                        let a = (data & ((1 << dw) - 1)) | (par & ((1 << pw) - 1)) << dw;
                        let b = (data >> dw) | (par >> pw) << dw;
                        (a as u64, b as u64)
                    };
                    (ports[0].init, ports[1].init) = split(iv);
                    (ports[0].srval, ports[1].srval) = split(sv);
                } else {
                    ports[s].init = iv as u64;
                    ports[s].srval = sv as u64;
                }
            }
            let config = Config {
                size,
                sdp: l.sdp,
                ports,
                inv,
                contents,
            };
            debug_assert!(config.check().is_ok(), "{name}: {:?}", config.check());
            let dp = size.data_pins();
            let dout = [g.m.new_sig(dp), g.m.new_sig(dp)];
            let doutp = [g.m.new_sig(dp / 8), g.m.new_sig(dp / 8)];
            let mut cell = Ramb::new(config, dout.clone(), doutp.clone());
            for s in 0..2 {
                let Some(sg) = &sides[s] else { continue };
                let u = l.sides[s];
                let clk = match (u.wr, u.rd) {
                    (Some(wi), _) => p.wr[wi].clk.sig,
                    (None, Some(r)) => p.rd[r].clk.sig,
                    _ => continue,
                };
                cell.clk[s] = clk;
                cell.addr[s] = sg.pins.clone();
                cell.en[s] = SigBit::ONE;
                if let Some(r) = u.rd {
                    let (rd, ri) = (&p.rd[r], &reads[r]);
                    if ri.hw_en {
                        cell.en[s] = rd.en;
                    }
                    if ri.hw_srst {
                        if ri.hw_en && !rd.ce_over_srst && rd.en != SigBit::ONE {
                            cell.en[s] = g.or(rd.en, rd.srst);
                        }
                        cell.rstram[s] = rd.srst;
                    }
                    if let Some(abs) = &ri.outreg {
                        cell.regce[s] = abs.en.unwrap_or(SigBit::ONE);
                        if let Some(rst) = abs.rst {
                            cell.rstreg[s] = rst.sig;
                            if rst.polarity == Polarity::Neg {
                                cell.config.inv.rstreg[s] = true;
                            }
                        }
                    }
                }
                // SDP: the read side's controls are port A's.
                let Some(wi) = u.wr else { continue };
                let width = l.widths[s].expect("a writing port has a width");
                let en = &wr_en[wi];
                let mut lanes: Vec<Option<SigBit>> = vec![None; width.lanes()];
                let (di, dip) = (&mut cell.di, &mut cell.dip);
                for k in 0..l.port_bits(width) {
                    let Some(bit) = l.bit(w, col, k) else {
                        continue;
                    };
                    let c = l.column(k);
                    let data = wr_data[wi][bit];
                    match (l.pin(width, c), l.wide(width)) {
                        (Pin::Data(i), false) => di[s][i] = data,
                        (Pin::Parity(i), false) => dip[s][i] = data,
                        // SDP's wide write: A's pins hold the low half.
                        (Pin::Data(i), true) => di[i / dp][i % dp] = data,
                        (Pin::Parity(i), true) => dip[i / (dp / 8)][i % (dp / 8)] = data,
                    }
                    lanes[l.lane(width, c)] = Some(en[bit]);
                }
                let gate = g.and(hit[s], sg.in_range);
                let we: Vec<SigBit> = lanes
                    .iter()
                    .map(|e| match e {
                        Some(e) => g.and(*e, gate),
                        None => SigBit::ZERO,
                    })
                    .collect();
                // WEA/WEBWE as Vivado connects them: the lanes' enables
                // repeated over the port's pins (two on a RAMB18E1, four on a
                // RAMB36E1; SDP's wide write all of WEBWE), so a port of one
                // lane has its enable on every pin; WEBWE's others 0.
                let used = if l.wide(width) {
                    cell.we[s].len()
                } else {
                    size.data_pins() / 8
                };
                for (i, pin) in cell.we[s].iter_mut().enumerate() {
                    *pin = if i < used {
                        we[i % we.len()]
                    } else {
                        SigBit::ZERO
                    };
                }
            }
            blocks.push(chipsmith_netlist::Cell {
                name: format!("{name}.{row}.{col}"),
                kind: cell.cell(),
                attributes: Default::default(),
            });
            row_douts.push([
                [dout[0].clone(), doutp[0].clone()].concat(),
                [dout[1].clone(), doutp[1].clone()].concat(),
            ]);
        }
        douts.push(row_douts);
    }

    // Read data: the blocks' columns, multiplexed over the rows by the
    // registered top address bits, then whatever the block can't do.
    let mut subst = Vec::new();
    for (s, side_sigs) in sides.iter().enumerate() {
        let Some(ri) = l.sides[s].rd else { continue };
        let rd = &p.rd[ri];
        let r_impl = &reads[ri];
        let width = l.widths[s].expect("a reading port has a width");
        let n = rd.data.len();
        let dp = size.data_pins();
        let mut per_row: Vec<SigSpec> = vec![vec![SigBit::X; n]; l.rows];
        for (row, row_douts) in douts.iter().enumerate() {
            for (col, pins) in row_douts.iter().enumerate() {
                for k in 0..l.port_bits(width) {
                    let Some(bit) = l.bit(w, col, k) else {
                        continue;
                    };
                    // pins[p] is port p's {data, parity}.
                    per_row[row][bit] = match (l.pin(width, l.column(k)), l.wide(width)) {
                        (Pin::Data(i), false) => pins[s][i],
                        (Pin::Parity(i), false) => pins[s][dp + i],
                        (Pin::Data(i), true) => pins[i / dp][i % dp],
                        (Pin::Parity(i), true) => pins[i / (dp / 8)][dp + i % (dp / 8)],
                    };
                }
            }
        }
        let domain_en = if r_impl.hw_en && rd.en != SigBit::ONE {
            Some(rd.en)
        } else {
            None
        };
        let mut data = if l.rows == 1 {
            per_row.pop().expect("one row")
        } else {
            let sg = side_sigs.as_ref().expect("a reading port's signals");
            let mut sel = add_ff(g, rd.clk, domain_en, sg.row_bits.clone());
            if let Some(abs) = &r_impl.outreg {
                sel = add_ff(g, rd.clk, abs.en, sel);
            }
            let words = 1 << sel.len();
            per_row.resize(words, vec![SigBit::X; n]);
            let y = g.m.new_sig(n);
            g.add(CellKind::Bmux(chipsmith_netlist::Bmux {
                a: per_row.concat(),
                s: sel,
                y: y.clone(),
            }));
            y
        };
        if let Some((flags, want)) = fixes.get(&ri) {
            // Where the block read something else than the RTL's read of a
            // write's new data: that data, registered as the block's read
            // is (same clock, same enable).
            let flag = add_ff(g, rd.clk, domain_en, flags.clone());
            let dq = add_ff(g, rd.clk, domain_en, want.clone());
            for &f in &flag {
                powerup.push((f, State::Zero));
            }
            data = data
                .iter()
                .zip(flag.iter().zip(&dq))
                .map(|(&b, (&f, &d))| g.mux(b, d, f))
                .collect();
        }
        if !r_impl.hw_en && !r_impl.no_change && rd.en != SigBit::ONE {
            // The block reads on every edge; registers hold what it read
            // when the read was enabled.
            let en_q = add_ff(g, rd.clk, None, vec![rd.en])[0];
            powerup.push((en_q, State::Zero));
            let hold = g.m.new_sig(n);
            let out = mux_word(g, &hold, &data, en_q);
            g.add(CellKind::Ff(Ff {
                q: hold.clone(),
                kind: FfKind::Dff {
                    clk: rd.clk,
                    d: out.clone(),
                    control: DffControl::Plain { en: None },
                },
            }));
            for (h, &v) in hold.iter().zip(&rd.init_value.0) {
                powerup.push((*h, v));
            }
            data = out;
        }
        if rd.srst != SigBit::ZERO && !r_impl.hw_srst {
            // A flag that the last enabled edge reset the read data.
            let f = g.m.new_sig(1)[0];
            let d = if rd.ce_over_srst {
                g.mux(f, rd.srst, rd.en)
            } else {
                let ne = g.not(rd.en);
                let hold = g.and(ne, f);
                g.or(rd.srst, hold)
            };
            g.add(CellKind::Ff(Ff {
                q: vec![f],
                kind: FfKind::Dff {
                    clk: rd.clk,
                    d: vec![d],
                    control: DffControl::Plain { en: None },
                },
            }));
            powerup.push((f, State::Zero));
            let v: SigSpec = rd.srst_value.0.iter().map(|&s| SigBit::Const(s)).collect();
            data = mux_word(g, &data, &v, f);
        }
        let targets: Vec<SigBit> = match &r_impl.outreg {
            Some(abs) => abs.bits.iter().map(|&(_, _, q)| q).collect(),
            None => rd.data.clone(),
        };
        subst.extend(targets.into_iter().zip(data));
    }
    (blocks, subst, powerup)
}

/// Maps every memory the blocks take; returns how many RAMB18 halves.
pub fn map(m: &mut Module) -> usize {
    // Copies first, so each is a memory of its own below.
    let mut i = 0;
    while i < m.cells.len() {
        let copies = match &m.cells[i].kind {
            CellKind::Mem(mem) if !wants_logic(&m.cells[i].attributes) => per_read_port(mem),
            _ => None,
        };
        let Some(copies) = copies else {
            i += 1;
            continue;
        };
        let cell = m.cells.remove(i);
        let n = copies.len();
        for (k, mem) in copies.into_iter().enumerate() {
            let mut c = cell.clone();
            c.name = format!("{}$rd{k}", cell.name);
            c.kind = CellKind::Mem(mem);
            m.cells.insert(i + k, c);
        }
        i += n;
    }
    let ctx = Context::new(m);
    let mut work = Vec::new();
    for (i, c) in m.cells.iter().enumerate() {
        let CellKind::Mem(mem) = &c.kind else {
            continue;
        };
        if wants_logic(&c.attributes) {
            continue;
        }
        let mut whole = mem.clone();
        ctx.same_enables(m, &mut whole);
        let debug = std::env::var_os("CHIPSMITH_BRAM_DEBUG").is_some();
        // The memory as it is, or split over both block ports: the cheaper.
        let mut best: Option<(Mem, Ports, Layout)> = None;
        for (mem, widened) in
            std::iter::once((whole.clone(), true)).chain(split(&whole).map(|h| (h, false)))
        {
            let Some(mut p) = ports(&mem, widened) else {
                if debug {
                    eprintln!("bram: {} ({} wide): ports don't fit", mem.memid, mem.width);
                }
                continue;
            };
            p.disjoint = ctx.disjoint(m, &p) || constant_apart(&p);
            let Some(l) = layout(&mem, &p) else {
                if debug {
                    eprintln!(
                        "bram: {} ({} wide): no layout of {:?} / {:?}",
                        mem.memid, mem.width, p.wr, p.rd
                    );
                }
                continue;
            };
            if debug {
                eprintln!("bram: {} ({} wide): {:?}", mem.memid, mem.width, cost(&l));
            }
            if best.as_ref().is_none_or(|(_, _, b)| cost(&l) < cost(b)) {
                best = Some((mem, p, l));
            }
        }
        let Some((mem, p, l)) = best else {
            if debug {
                eprintln!("bram: {}: no layout", whole.memid);
            }
            continue;
        };
        if debug {
            eprintln!("bram: {}: {l:?}", mem.memid);
        }
        let r = realize(m, &ctx, &mem, &p, &l);
        work.push((i, mem, p, l, r));
    }
    // The output registers the blocks take over go first, while cell
    // indices hold.
    let mut drops: HashMap<usize, Vec<bool>> = HashMap::new();
    let mut absorbed = Vec::new();
    for (_, _, _, _, r) in &work {
        for abs in r.iter().filter_map(|x| x.outreg.as_ref()) {
            for &(cell, bit, q) in &abs.bits {
                let CellKind::Ff(ff) = &m.cells[cell].kind else {
                    unreachable!("an output register is a register")
                };
                drops.entry(cell).or_insert_with(|| vec![false; ff.width()])[bit] = true;
                absorbed.extend(q.net());
            }
        }
    }
    for (cell, drop) in &drops {
        if let CellKind::Ff(ff) = &mut m.cells[*cell].kind {
            drop_bits(ff, drop);
        }
    }
    // Their outputs' power-up values go with them (the block's INIT).
    let absorbed: std::collections::HashSet<_> = absorbed.into_iter().collect();
    for wire in &mut m.wires {
        let Some(ParamValue::Bits(c)) = wire.attributes.get_mut(attr::INIT) else {
            continue;
        };
        for (b, s) in wire.bits.iter().zip(c.0.iter_mut()) {
            if b.net().is_some_and(|n| absorbed.contains(&n)) {
                *s = State::X;
            }
        }
        if c.0.iter().all(|s| *s == State::X) {
            wire.attributes.remove(attr::INIT);
        }
    }
    let mut halves = 0;
    let mut subst_all = Vec::new();
    let mut powerup_all = Vec::new();
    let mut g = Gates::new(m, "bram");
    let mut built = Vec::new();
    for (i, mem, p, l, r) in work {
        let (blocks, subst, powerup) = build(&mut g, &mem, &p, &l, &r);
        halves += blocks.len() * l.size.halves();
        built.push((i, blocks));
        subst_all.extend(subst);
        powerup_all.extend(powerup);
    }
    for (i, blocks) in built.into_iter().rev() {
        g.m.cells.splice(i..i + 1, blocks);
    }
    m.cells
        .retain(|c| !matches!(&c.kind, CellKind::Ff(ff) if ff.width() == 0));
    let mut values = inits(m);
    for (b, v) in powerup_all {
        if let (SigBit::Net(n), State::Zero | State::One) = (b, v) {
            values.insert(n, v);
        }
    }
    set_inits(m, &values);
    let mut subst = Subst::new(m);
    for (from, to) in subst_all {
        if let SigBit::Net(n) = from {
            subst.set(n, to);
        }
    }
    subst.apply(m);
    halves
}
