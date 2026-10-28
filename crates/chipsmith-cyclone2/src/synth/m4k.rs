//! Memories onto M4K blocks, after yosys' `memory_libmap`: which memories a
//! block can take (its ports' semantics against the block's), the cheapest
//! arrangement of blocks, and the soft logic around them.
//!
//! A memory's ports are first widened (`memory_share`): write or read ports
//! on one clock whose addresses differ only in their low bits, constant and
//! covering every value, are one port that many words wide, which is how a
//! mixed-width memory is written in Verilog. Then the ports go to the
//! block's ports A and B:
//!
//! - a ROM (one read port): single port, or for a x36 a read-only port B;
//! - one write and one read port at the same address, the read seeing the
//!   write (new data): single port;
//! - one write and one read port reading the old data: simple dual port,
//!   on one clock or two;
//! - two ports reading and writing, or a write with two reads one of which
//!   shares its address: true dual port, on one clock, each port seeing its
//!   own write and the old data of the other's.
//!
//! The block takes per-port widths (x1-x36; x18 at most in single and true
//! dual port), byte enables, clock enables, a second register on the read
//! data (the block's output register, with an asynchronous clear), and two
//! clocks. What the block can't do is soft logic: a memory deeper than a
//! block is several blocks, with the write enable decoded and the read data
//! multiplexed by the address's top bits; one wider than a block is
//! several side by side; a read enable the block's clock enable can't carry
//! (it would also stop a write) holds the read data in LE registers, and so
//! does a synchronous reset. A memory shallower than its blocks repeats its
//! top address bit on the block's unused address pins (the block reads an
//! unconnected pin as some level, which is not known for every pin), so
//! every word has an address of its own.
//!
//! Every memory a block can take goes into blocks, as Quartus does, unless
//! its declaration says `ramstyle`/`ram_style` "logic" (or "registers").
//! The cheapest arrangement is the fewest blocks, then the least soft logic.

use std::collections::HashMap;

use chipsmith_netlist::{
    attr, CellKind, Const, Ctrl, DffControl, Dir, Ff, FfKind, Generic, GenericPort, Mem, MemRd,
    MemRdSync, Module, ParamValue, Polarity, SigBit, SigSpec, State, Word,
};
use chipsmith_synth::rewrite::{Gates, Subst};

use crate::m4k::{
    depth, OutReg, Packing, Param, Ports as BlockPorts, ReadPort, Sel, Shape, Side, Width,
};

/// What a block costs, by mode (`synth/m4k.txt`'s costs): the arrangement
/// with the fewest blocks wins, a simpler mode on a tie.
const SP_COST: u32 = 64;
const SDP_COST: u32 = 65;
const TDP_COST: u32 = 66;

const WIDTHS: [Width; 6] = [
    Width::X1,
    Width::X2,
    Width::X4,
    Width::X9,
    Width::X18,
    Width::X36,
];

fn width_of_depth(d: usize) -> Option<Width> {
    WIDTHS.into_iter().find(|&w| depth(w) == d)
}

fn log2(n: usize) -> usize {
    n.trailing_zeros() as usize
}

/// A write port, widened: `factor` consecutive words at `addr` (whose bits
/// below log2(factor) are ignored), word 0 first in `data` and `en`.
#[derive(Debug, Clone)]
struct WrPort {
    clk: SigBit,
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
    clk: SigBit,
    en: SigBit,
    srst: SigBit,
    ce_over_srst: bool,
    srst_value: Const,
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
    /// requires their addresses to differ (Quartus' true dual port
    /// template masks collisions so). Without it, collisions get logic
    /// (`Resolve`), which Quartus would find dead and sweep.
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

/// The memory's ports as the blocks see them, if they are ports a block
/// can have: synchronous, on rising edges, no asynchronous reset, a read
/// register that powers up 0.
fn ports(mem: &Mem) -> Option<Ports> {
    if mem.offset != 0 || mem.size < 2 || mem.width == 0 {
        return None;
    }
    let mut wr = Vec::new();
    for (i, w) in mem.wr.iter().enumerate() {
        let clk = w.clk?;
        if clk.polarity != Polarity::Pos || w.wide_continuation {
            return None;
        }
        wr.push(WrPort {
            clk: clk.sig,
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
    let wr = widen(
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
    );
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
        // The block's read data powers up 0.
        let zero_init = init_value
            .0
            .iter()
            .all(|s| matches!(s, State::X | State::Zero));
        if clk.polarity != Polarity::Pos || *arst != SigBit::ZERO || !zero_init {
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
        let per = |flags: &Vec<bool>| -> Option<Vec<bool>> {
            wr.iter()
                .map(|w| {
                    let mut it = w
                        .ids
                        .iter()
                        .filter(|&&i| !disjoint(&mem.wr[i].addr, addr))
                        .map(|&i| flags[i]);
                    let f = it.next().unwrap_or(false);
                    it.all(|x| x == f).then_some(f)
                })
                .collect()
        };
        rd.push(RdPort {
            clk: clk.sig,
            en: *en,
            srst: *srst,
            ce_over_srst: *ce_over_srst,
            srst_value: srst_value.clone(),
            addr: addr.clone(),
            factor: 1,
            data: data.clone(),
            transparent: per(transparent)?,
            collision_x: per(collision_x)?,
        });
    }
    let rd = widen(
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
            ..p.clone()
        },
    );
    Some(Ports {
        wr,
        rd,
        disjoint: false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Single,
    SimpleDual,
    TrueDual,
}

impl Kind {
    fn cost(self) -> u32 {
        match self {
            Kind::Single => SP_COST,
            Kind::SimpleDual => SDP_COST,
            Kind::TrueDual => TDP_COST,
        }
    }
}

/// What one block port does: a write port, a read port, or both at the
/// same address.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Use {
    wr: Option<usize>,
    rd: Option<usize>,
}

/// A read that sees a write on its own block port gets the new data; the
/// block can't mask what a byte enable leaves out of it.
fn same_port_ok(r: &RdPort, w: usize, partial: bool) -> bool {
    r.collision_x[w] || (r.transparent[w] && !partial)
}

/// A read of the word the other block port writes gets the old data.
fn cross_port_ok(r: &RdPort, w: usize) -> bool {
    r.collision_x[w] || !r.transparent[w]
}

fn partial(w: &WrPort) -> bool {
    w.en.iter().any(|&e| e != w.en[0])
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
            (p.rd[r].transparent[w] && !p.rd[r].collision_x[w]).then_some(r)
        })
        .collect()
}

/// Every way the memory's ports go onto a block's two.
fn roles(p: &Ports) -> Vec<(Kind, [Use; 2])> {
    let paired = |w: usize, r: usize| {
        let (w, r) = (&p.wr[w], &p.rd[r]);
        w.addr == r.addr && w.clk == r.clk && w.factor == r.factor
    };
    let mut out = Vec::new();
    match (p.wr.len(), p.rd.len()) {
        (0, 1) => {
            out.push((
                Kind::Single,
                [
                    Use {
                        wr: None,
                        rd: Some(0),
                    },
                    Use::default(),
                ],
            ));
            out.push((
                Kind::SimpleDual,
                [
                    Use::default(),
                    Use {
                        wr: None,
                        rd: Some(0),
                    },
                ],
            ));
        }
        (0, 2) => out.push((
            Kind::TrueDual,
            [
                Use {
                    wr: None,
                    rd: Some(0),
                },
                Use {
                    wr: None,
                    rd: Some(1),
                },
            ],
        )),
        (1, 1) => {
            let r = &p.rd[0];
            if paired(0, 0) && same_port_ok(r, 0, partial(&p.wr[0])) {
                out.push((
                    Kind::Single,
                    [
                        Use {
                            wr: Some(0),
                            rd: Some(0),
                        },
                        Use::default(),
                    ],
                ));
            }
            if cross_port_ok(r, 0) && r.clk == p.wr[0].clk {
                out.push((
                    Kind::SimpleDual,
                    [
                        Use {
                            wr: Some(0),
                            rd: None,
                        },
                        Use {
                            wr: None,
                            rd: Some(0),
                        },
                    ],
                ));
            }
            // On two clocks Quartus 13.0 has no simple dual port: a true
            // dual port reading on port A (clock 0) and writing on port B
            // (clock 1), as it builds one itself.
            if cross_port_ok(r, 0) && r.clk != p.wr[0].clk {
                out.push((
                    Kind::TrueDual,
                    [
                        Use {
                            wr: None,
                            rd: Some(0),
                        },
                        Use {
                            wr: Some(0),
                            rd: None,
                        },
                    ],
                ));
            }
        }
        (1, 2) => {
            for (i, j) in [(0, 1), (1, 0)] {
                if paired(0, i)
                    && same_port_ok(&p.rd[i], 0, partial(&p.wr[0]))
                    && cross_port_ok(&p.rd[j], 0)
                {
                    out.push((
                        Kind::TrueDual,
                        [
                            Use {
                                wr: Some(0),
                                rd: Some(i),
                            },
                            Use {
                                wr: None,
                                rd: Some(j),
                            },
                        ],
                    ));
                }
            }
        }
        (2, n) if n <= 2 => {
            // Each read shares its block port with the write at its address.
            let mut sides = [
                Use {
                    wr: Some(0),
                    rd: None,
                },
                Use {
                    wr: Some(1),
                    rd: None,
                },
            ];
            let mut ok = true;
            for r in 0..n {
                match (0..2).find(|&w| paired(w, r) && sides[w].rd.is_none()) {
                    Some(w) => {
                        let rd = &p.rd[r];
                        ok &= same_port_ok(rd, w, partial(&p.wr[w])) && cross_port_ok(rd, 1 - w);
                        sides[w].rd = Some(r);
                    }
                    None => ok = false,
                }
            }
            if ok {
                out.push((Kind::TrueDual, sides));
            }
        }
        _ => {}
    }
    // A true dual port whose port A writes is on one clock.
    out.retain(|(kind, sides)| {
        *kind != Kind::TrueDual || sides[0].wr.is_none() || {
            let mut clks = sides.iter().flat_map(|u| {
                u.wr.map(|w| p.wr[w].clk)
                    .into_iter()
                    .chain(u.rd.map(|r| p.rd[r].clk))
            });
            let first = clks.next();
            clks.all(|c| Some(c) == first)
        }
    });
    out
}

/// How a memory goes into blocks: which port does what, the blocks' shape
/// (output registers aside, which depend on the logic around), and how
/// many blocks deep (`rows`) and wide (`cols`).
#[derive(Debug, Clone)]
struct Layout {
    kind: Kind,
    sides: [Use; 2],
    widths: [Option<Width>; 2],
    base: Width,
    slice: usize,
    packing: Packing,
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

impl Layout {
    fn ports(&self) -> BlockPorts {
        let rp = |w: Option<Width>| ReadPort {
            width: w.expect("a used port has a width"),
            out_reg: None,
        };
        match self.kind {
            Kind::Single => BlockPorts::Single {
                a: rp(self.widths[0]),
            },
            Kind::SimpleDual => BlockPorts::SimpleDual {
                // A ROM's port A doesn't write; its width is the read's.
                write: self.widths[0].or(self.widths[1]).expect("a width"),
                read: rp(self.widths[1]),
                read_clock: Sel::Zero,
            },
            Kind::TrueDual => BlockPorts::TrueDual {
                a: rp(self.widths[0]),
                b: rp(self.widths[1]),
                b_clock: Sel::Zero,
            },
        }
    }

    fn shape(&self, ports: BlockPorts) -> Shape {
        Shape::new(ports, self.base, self.slice as u8, self.packing)
            .expect("a layout's shape was checked")
    }

    fn blocks(&self) -> usize {
        self.rows * self.cols
    }

    /// The memory bit (word-relative: port word sub-word `k / slice`) that
    /// port bit `k` of a block in column `col` holds, if any.
    fn bit(&self, w: usize, col: usize, k: usize) -> Option<usize> {
        let (sub, b) = (k / self.slice, k % self.slice);
        let bit = col * self.slice + b;
        (bit < w).then_some(sub * w + bit)
    }
}

fn factor(p: &Ports, u: Use) -> Option<usize> {
    match (u.wr, u.rd) {
        (Some(w), _) => Some(p.wr[w].factor),
        (None, Some(r)) => Some(p.rd[r].factor),
        (None, None) => None,
    }
}

/// For a writing block port in one block column: the enable of each byte
/// enable's columns, if each has one; `None` if bits under one byte enable
/// differ. Bits the column doesn't hold have none.
fn byte_groups(
    l: &Layout,
    shape: Shape,
    w: &WrPort,
    width: Width,
    mem_width: usize,
    col: usize,
) -> Option<Vec<Option<SigBit>>> {
    let mut groups: Vec<Option<SigBit>> = vec![None; width.bits().div_ceil(9)];
    for k in 0..shape.data_bits(width) {
        let Some(bit) = l.bit(mem_width, col, k) else {
            continue;
        };
        let g = &mut groups[shape.column(width, k) / 9];
        match *g {
            Some(e) if e != w.en[bit] => return None,
            _ => *g = Some(w.en[bit]),
        }
    }
    Some(groups)
}

/// The cheapest layout, if a block can take the memory.
fn layout(mem: &Mem, p: &Ports) -> Option<Layout> {
    let abits = (usize::BITS - (mem.size - 1).leading_zeros()) as usize;
    let mut best: Option<Layout> = None;
    for (kind, sides) in roles(p) {
        if collision(mem, p, &sides).is_err() {
            continue;
        }
        let factors = sides.map(|u| factor(p, u));
        for base in WIDTHS {
            for packing in [Packing::Dense, Packing::Bytes] {
                if packing == Packing::Bytes && base < Width::X9 {
                    continue;
                }
                let cap = match packing {
                    Packing::Dense => base.bits(),
                    Packing::Bytes => base.bits() / 9 * 8,
                };
                let slice = mem.width.min(cap);
                let widths = factors.map(|f| f.and_then(|f| width_of_depth(depth(base) / f)));
                if factors
                    .iter()
                    .zip(&widths)
                    .any(|(f, w)| f.is_some() != w.is_some())
                {
                    continue;
                }
                let block_bits = log2(depth(base));
                // A shallow memory repeats its top address bit, which each
                // port must have.
                if abits < block_bits && factors.iter().flatten().any(|&f| abits <= log2(f)) {
                    continue;
                }
                let rows = if abits > block_bits {
                    mem.size.div_ceil(depth(base))
                } else {
                    1
                };
                let mut l = Layout {
                    kind,
                    sides,
                    widths,
                    base,
                    slice,
                    packing,
                    block_bits,
                    abits,
                    rows,
                    cols: mem.width.div_ceil(slice),
                    byteena: [false; 2],
                    soft: 0,
                };
                // Quartus reads a x36 ROM on port B; narrower ones are
                // single ports.
                if kind == Kind::SimpleDual
                    && sides[0].wr.is_none()
                    && widths[1] != Some(Width::X36)
                {
                    continue;
                }
                let Ok(shape) = Shape::new(l.ports(), base, slice as u8, packing) else {
                    continue;
                };
                let mut ok = true;
                for (s, side) in [Side::A, Side::B].into_iter().enumerate() {
                    let (Some(w), Some(width)) = (sides[s].wr, widths[s]) else {
                        continue;
                    };
                    let w = &p.wr[w];
                    for col in 0..l.cols {
                        match byte_groups(&l, shape, w, width, mem.width, col) {
                            Some(g) => {
                                let mut sigs: Vec<SigBit> = g.into_iter().flatten().collect();
                                sigs.dedup();
                                if sigs.len() > 1 {
                                    // Port B's byte enables are BE2-3; x9
                                    // and narrower have one group.
                                    let max = match side {
                                        Side::A => Width::X36,
                                        Side::B => Width::X18,
                                    };
                                    ok &= width >= Width::X18 && width <= max;
                                    l.byteena[s] = true;
                                }
                                // An enable tied low or high isn't
                                // routable (an unconnected write enable
                                // reads as no write).
                                ok &= !sigs.contains(&SigBit::ZERO) && !sigs.contains(&SigBit::ONE);
                            }
                            None => ok = false,
                        }
                    }
                }
                if !ok {
                    continue;
                }
                l.soft = if rows > 1 { mem.width * rows + rows } else { 0 }
                    + usize::from(l.byteena.iter().any(|&b| b));
                let better = best.as_ref().is_none_or(|b| {
                    let key = |l: &Layout| (l.blocks() as u32 * l.kind.cost(), l.soft);
                    key(&l) < key(b)
                });
                if better {
                    best = Some(l);
                }
            }
        }
    }
    best
}

/// Whether the blocks take this memory.
pub fn mappable(mem: &Mem) -> bool {
    ports(mem).is_some_and(|p| layout(mem, &p).is_some())
}

/// A memory's declaration asks for logic (Quartus' `ramstyle`, yosys'
/// `ram_style` and friends).
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
            inits: chipsmith_synth::rewrite::inits(m),
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
        // An OR of differences: every input is one.
        let differs = |a: SigBit| -> bool {
            fn go(
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
                            .all(|&a| go(ctx, m, a, is_pair, depth - 1)),
                        _ => false,
                    },
                    _ => false,
                }
            }
            go(self, m, a, &is_pair, 16)
        };
        if differs(x) {
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

    /// The register a read port's data all go into and nothing else: the
    /// block's output register, if its clocking is one the block has.
    fn output_register(&self, m: &Module, r: &RdPort) -> Option<Absorb> {
        let mut bits = Vec::new();
        let mut clocking: Option<(SigBit, Option<SigBit>, Option<SigBit>)> = None;
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
            if clk.polarity != Polarity::Pos {
                return None;
            }
            let pos = |c: &Option<Ctrl>| match c {
                None => Some(None),
                Some(c) if c.polarity == Polarity::Pos => Some(Some(c.sig)),
                Some(_) => None,
            };
            let (en, clr) = match control {
                DffControl::Plain { en } => (pos(en)?, None),
                // The block's output register clears to 0.
                DffControl::Async { arst, value, en } if value.0[bit] != State::One => {
                    (pos(en)?, Some(pos(&Some(*arst))??))
                }
                _ => return None,
            };
            // It powers up 0.
            let init = q[bit].net().and_then(|n| self.inits.get(&n)).copied();
            if init == Some(State::One) {
                return None;
            }
            let this = (clk.sig, en, clr);
            if clocking.is_some_and(|c| c != this) {
                return None;
            }
            clocking = Some(this);
            bits.push((cell, bit, q[bit]));
        }
        let (clk, en, clr) = clocking?;
        Some(Absorb { bits, clk, en, clr })
    }
}

/// A register on a read port's data that the blocks' output registers
/// replace.
#[derive(Debug, Clone)]
struct Absorb {
    /// Per read data bit: the register cell, its bit, its output.
    bits: Vec<(usize, usize, SigBit)>,
    clk: SigBit,
    en: Option<SigBit>,
    clr: Option<SigBit>,
}

/// A clock and its clock enable: the block's clock 0 or 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Domain {
    clk: SigBit,
    en: Option<SigBit>,
}

/// How a read port is built: whether the block's clock enable is its read
/// enable (else LE registers hold the data), and the register the block's
/// output register takes over.
#[derive(Debug, Clone)]
struct ReadImpl {
    hw_en: bool,
    outreg: Option<(Absorb, OutReg)>,
}

/// The blocks' clocks, enables and clears, and each read port's build.
struct Realized {
    domains: Vec<Domain>,
    clears: Vec<SigBit>,
    side_clock: [Sel; 2],
    reads: Vec<ReadImpl>,
}

fn realize(m: &Module, ctx: &Context, mem: &Mem, p: &Ports, l: &Layout) -> Realized {
    let bypassed: Vec<usize> = match collision(mem, p, &l.sides) {
        Ok(Some(c)) => bypassed_reads(p, &l.sides, c),
        _ => Vec::new(),
    };
    let side_domain = |s: usize, hw: bool| -> Option<Domain> {
        let u = l.sides[s];
        let clk = match (u.wr, u.rd) {
            (Some(w), _) => p.wr[w].clk,
            (None, Some(r)) => p.rd[r].clk,
            _ => return None,
        };
        let en = u.rd.filter(|_| hw).and_then(|r| {
            let e = p.rd[r].en;
            (e != SigBit::ONE).then_some(e)
        });
        Some(Domain { clk, en })
    };
    // Whether a block port's read enable can be its clock enable: the
    // enable stops the port's write too, so the write must imply it.
    let hw_ok = |s: usize| -> bool {
        let u = l.sides[s];
        match (u.wr, u.rd) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(w), Some(r)) => {
                let e = p.rd[r].en;
                p.wr[w].en.iter().all(|&x| ctx.implies(m, x, e, 4))
            }
        }
    };
    let mut choices = Vec::new();
    for a in [true, false] {
        for b in [true, false] {
            choices.push([a && hw_ok(0), b && hw_ok(1)]);
        }
    }
    for hw in choices {
        let mut domains: Vec<Domain> = Vec::new();
        let mut side_clock = [Sel::Zero; 2];
        let mut ok = true;
        let mut sd = [side_domain(0, hw[0]), side_domain(1, hw[1])];
        // A simple dual port's write under the read's enable shares its
        // clock: one clock enable for both, as Quartus has it.
        if let (Kind::SimpleDual, Some(w), Some(a), Some(b)) = (l.kind, l.sides[0].wr, sd[0], sd[1])
        {
            if let Some(e) = b.en.filter(|_| a.clk == b.clk && a.en.is_none()) {
                if p.wr[w].en.iter().all(|&x| ctx.implies(m, x, e, 4)) {
                    sd[0] = Some(b);
                }
            }
        }
        for s in 0..2 {
            let Some(d) = sd[s] else {
                continue;
            };
            // A simple dual port's read side is on clock 1 only beside a
            // write; a true dual port is on one clock.
            match domains.iter().position(|&x| x == d) {
                Some(i) => side_clock[s] = if i == 0 { Sel::Zero } else { Sel::One },
                None if domains.is_empty() => domains.push(d),
                // Port B on clock 1: a simple dual port's read, or a true
                // dual port's write while port A only reads.
                None if s == 1
                    && (l.kind == Kind::SimpleDual
                        || (l.kind == Kind::TrueDual && l.sides[0].wr.is_none())) =>
                {
                    domains.push(d);
                    side_clock[s] = Sel::One;
                }
                None => ok = false,
            }
        }
        if !ok {
            continue;
        }
        let mut clears: Vec<SigBit> = Vec::new();
        let mut reads = vec![
            ReadImpl {
                hw_en: false,
                outreg: None,
            };
            p.rd.len()
        ];
        for (u, &hw_s) in l.sides.iter().zip(&hw) {
            let Some(r) = u.rd else { continue };
            let rd = &p.rd[r];
            reads[r].hw_en = hw_s;
            // The output register follows the read data straight out of
            // the block: not with LE logic between.
            let soft =
                (!hw_s && rd.en != SigBit::ONE) || rd.srst != SigBit::ZERO || bypassed.contains(&r);
            if soft {
                continue;
            }
            let Some(abs) = ctx.output_register(m, rd) else {
                continue;
            };
            let d = Domain {
                clk: abs.clk,
                en: abs.en,
            };
            let clock = match domains.iter().position(|&x| x == d) {
                Some(0) => Sel::Zero,
                Some(_) => Sel::One,
                None if domains.len() < 2 => {
                    domains.push(d);
                    Sel::One
                }
                None => continue,
            };
            let clear = match abs.clr {
                None => None,
                Some(c) => match clears.iter().position(|&x| x == c) {
                    Some(i) => Some(if i == 0 { Sel::Zero } else { Sel::One }),
                    None if clears.len() < 2 => {
                        clears.push(c);
                        Some(if clears.len() == 1 {
                            Sel::Zero
                        } else {
                            Sel::One
                        })
                    }
                    None => continue,
                },
            };
            reads[r].outreg = Some((abs, OutReg { clock, clear }));
        }
        return Realized {
            domains,
            clears,
            side_clock,
            reads,
        };
    }
    unreachable!("without clock enables a layout's ports always fit two clocks")
}

// ---------------------------------------------------------------------
// Building it.

fn port(name: &str, dir: Dir, sig: SigSpec) -> GenericPort {
    GenericPort {
        name: name.into(),
        dir: Some(dir),
        sig,
    }
}

/// `a == value`, as gates.
fn equals(g: &mut Gates, a: &[SigBit], value: usize) -> SigBit {
    let lits: Vec<SigBit> = a
        .iter()
        .enumerate()
        .map(|(i, &b)| if value >> i & 1 == 1 { b } else { g.not(b) })
        .collect();
    g.reduce(chipsmith_netlist::GateOp::And, &lits, SigBit::ONE)
}

fn add_ff(g: &mut Gates, clk: SigBit, en: Option<SigBit>, d: SigSpec) -> SigSpec {
    let q = g.m.new_sig(d.len());
    g.add(CellKind::Ff(Ff {
        q: q.clone(),
        kind: FfKind::Dff {
            clk: Ctrl::pos(clk),
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

/// One memory's blocks and logic. Returns the blocks (to go where the
/// memory was) and the read data's new drivers.
fn build(
    g: &mut Gates,
    mem: &Mem,
    p: &Ports,
    l: &Layout,
    r: &Realized,
) -> (Vec<chipsmith_netlist::Cell>, Vec<(SigBit, SigBit)>) {
    let w = mem.width;
    let name = mem.memid.trim_start_matches('\\');
    let mut ports = l.ports();
    let out_reg = |side: usize| -> Option<OutReg> {
        let rd = l.sides[side].rd?;
        r.reads[rd].outreg.as_ref().map(|(_, o)| *o)
    };
    match &mut ports {
        BlockPorts::Single { a } => a.out_reg = out_reg(0),
        BlockPorts::SimpleDual {
            read, read_clock, ..
        } => {
            read.out_reg = out_reg(1);
            *read_clock = r.side_clock[1];
        }
        BlockPorts::TrueDual { a, b, b_clock } => {
            a.out_reg = out_reg(0);
            b.out_reg = out_reg(1);
            *b_clock = r.side_clock[1];
        }
    }
    let shape = l.shape(ports);

    // Per block port: its address bits, row and range checks.
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
            let n = width.addr_bits() as usize;
            let bit = |t: usize| addr.get(t).copied().unwrap_or(SigBit::ZERO);
            let mut pins = vec![SigBit::ZERO; 12 - n];
            // Base address bit lg + i; a shallow memory's top bit again
            // above its own.
            pins.extend((0..n).map(|i| bit((lg + i).min(l.abits - 1))));
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
        // The two ports' spans (each port's word) overlap: the address
        // above the wider span agrees.
        let top = ph.addr.len().max(pl.addr.len());
        let eq: Vec<SigBit> = (lg_h.max(lg_l)..top)
            .map(|t| {
                let bit = |x: &WrPort| x.addr.get(t).copied().unwrap_or(SigBit::ZERO);
                g.xnor(bit(ph), bit(pl))
            })
            .collect();
        let same = g.reduce(chipsmith_netlist::GateOp::And, &eq, SigBit::ONE);
        let reads: Vec<(usize, usize)> = l
            .sides
            .iter()
            .filter_map(|u| Some((u.wr?, u.rd?)))
            .filter(|&(_, r)| bypassed_reads(p, &l.sides, c).contains(&r))
            .collect();
        let read_of = |wi: usize| reads.iter().find(|&&(w, _)| w == wi).map(|&(_, r)| r);
        match c.how {
            Resolve::MaskLoser => {
                // The winner writes its whole span, uniformly.
                let clash = g.and(ph.en[0], same);
                let keep = g.not(clash);
                let lost: SigSpec = pl.en.iter().map(|&e| g.and(e, clash)).collect();
                wr_en[c.lo] = pl.en.iter().map(|&e| g.and(e, keep)).collect();
                if let Some(r) = read_of(c.lo) {
                    fixes.insert(r, (lost, pl.data.clone()));
                }
            }
            Resolve::Redirect => {
                // The loser writes its whole span, uniformly, and takes the
                // winner's data on the bits the winner writes in it.
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

    let mut blocks = Vec::new();
    let mut douts: Vec<Vec<SigSpec>> = Vec::new();
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
    for (row, (&hit_a, &hit_b)) in hits[0].iter().zip(&hits[1]).enumerate() {
        let hit = [hit_a, hit_b];
        let mut row_douts = Vec::new();
        for col in 0..l.cols {
            let mut init = vec![State::X; crate::m4k::BITS];
            // Contents: each base word's slice of its memory word.
            let base_words = depth(l.base);
            for bw in 0..base_words {
                let word = if l.abits >= l.block_bits {
                    row * base_words + bw
                } else {
                    // A shallow memory's word at this base address, if the
                    // repeated top bits agree.
                    let top = bw >> (l.abits - 1) & 1;
                    let rep = bw >> l.abits;
                    let all = (1usize << (l.block_bits - l.abits)) - 1;
                    if rep != if top == 1 { all } else { 0 } {
                        continue;
                    }
                    bw & ((1 << l.abits) - 1)
                };
                if word >= mem.size {
                    continue;
                }
                for b in 0..l.slice {
                    let bit = col * l.slice + b;
                    if bit < w {
                        let lin = shape.linear(l.base, bw, b);
                        init[lin] = mem.init.0[word * w + bit];
                    }
                }
            }
            let dout = g.m.new_sig(36);
            let mut ports = Vec::new();
            ports.push(port("CLK", Dir::In, vec![r.domains[0].clk]));
            let mut addr = [vec![SigBit::ZERO; 12], vec![SigBit::ZERO; 12]];
            // Columns no memory bit sits in take x: nothing writes there, so
            // nothing drives them (a constant memory bit is driven, `pack`).
            let mut din_a = vec![SigBit::X; 36];
            let mut din_b = vec![SigBit::X; 18];
            let mut we = [SigBit::ZERO; 2];
            let mut be: [Option<SigSpec>; 2] = [None, None];
            for s in 0..2 {
                let Some(sg) = &sides[s] else { continue };
                addr[s] = sg.pins.clone();
                let (Some(wi), Some(width)) = (l.sides[s].wr, l.widths[s]) else {
                    continue;
                };
                let en = &wr_en[wi];
                let mut groups: Vec<Option<SigBit>> = vec![None; width.bits().div_ceil(9)];
                for k in 0..shape.data_bits(width) {
                    let Some(bit) = l.bit(w, col, k) else {
                        continue;
                    };
                    let c = shape.column(width, k);
                    match s {
                        0 => din_a[c] = wr_data[wi][bit],
                        _ => din_b[c] = wr_data[wi][bit],
                    }
                    groups[c / 9] = Some(en[bit]);
                }
                let gate = g.and(hit[s], sg.in_range);
                if l.byteena[s] {
                    // Unused bytes are enabled: nothing is written there.
                    let bes: SigSpec = groups.iter().map(|e| e.unwrap_or(SigBit::ONE)).collect();
                    let used: Vec<SigBit> = groups.iter().flatten().copied().collect();
                    let any = g.reduce_or(&used);
                    we[s] = g.and(any, gate);
                    be[s] = Some(bes);
                } else {
                    let e = groups
                        .iter()
                        .flatten()
                        .next()
                        .copied()
                        .unwrap_or(SigBit::ZERO);
                    we[s] = g.and(e, gate);
                }
            }
            let [a_addr, b_addr] = addr;
            ports.push(port("A_ADDR", Dir::In, a_addr));
            ports.push(port("A_DATAIN", Dir::In, din_a));
            ports.push(port("A_WE", Dir::In, vec![we[0]]));
            ports.push(port("B_ADDR", Dir::In, b_addr));
            ports.push(port("DATAOUT", Dir::Out, dout.clone()));
            if let Some(d) = r.domains.get(1) {
                ports.push(port("CLK1", Dir::In, vec![d.clk]));
            }
            for (k, d) in r.domains.iter().enumerate() {
                if let Some(e) = d.en {
                    ports.push(port(&format!("ENA{k}"), Dir::In, vec![e]));
                }
            }
            for (k, c) in r.clears.iter().enumerate() {
                ports.push(port(&format!("CLR{k}"), Dir::In, vec![*c]));
            }
            if let Some(b) = &be[0] {
                ports.push(port("A_BYTEENA", Dir::In, b.clone()));
            }
            if l.kind == Kind::TrueDual {
                ports.push(port("B_DATAIN", Dir::In, din_b));
                ports.push(port("B_WE", Dir::In, vec![we[1]]));
                if let Some(b) = &be[1] {
                    ports.push(port("B_BYTEENA", Dir::In, b.clone()));
                }
            }
            let mut params: std::collections::BTreeMap<String, ParamValue> = shape
                .params()
                .into_iter()
                .map(|(k, v)| {
                    let v = match v {
                        Param::Str(s) => ParamValue::Str(s.into()),
                        Param::Int(i) => ParamValue::int(i as i64),
                    };
                    (k.to_string(), v)
                })
                .collect();
            params.insert("INIT".into(), ParamValue::Bits(Const(init)));
            blocks.push(chipsmith_netlist::Cell {
                name: format!("{name}.{row}.{col}"),
                kind: CellKind::Other(Generic {
                    ty: super::cells::M4K.into(),
                    params,
                    ports,
                }),
                attributes: Default::default(),
            });
            row_douts.push(dout);
        }
        douts.push(row_douts);
    }

    // Read data: the blocks' columns, multiplexed over the rows by the
    // registered top address bits, then whatever the block can't do.
    let mut subst = Vec::new();
    for (s, side_sigs) in sides.iter().enumerate() {
        let Some(ri) = l.sides[s].rd else { continue };
        let rd = &p.rd[ri];
        let ri_impl = &r.reads[ri];
        let width = l.widths[s].expect("a reading port has a width");
        let offset = if l.kind == Kind::TrueDual && s == 1 {
            18
        } else {
            0
        };
        let n = rd.data.len();
        let mut per_row: Vec<SigSpec> = vec![vec![SigBit::X; n]; l.rows];
        for (row, row_douts) in douts.iter().enumerate() {
            for (col, dout) in row_douts.iter().enumerate() {
                for k in 0..shape.data_bits(width) {
                    if let Some(bit) = l.bit(w, col, k) {
                        per_row[row][bit] = dout[offset + shape.column(width, k)];
                    }
                }
            }
        }
        let domain_en = if ri_impl.hw_en && rd.en != SigBit::ONE {
            Some(rd.en)
        } else {
            None
        };
        let mut data = if l.rows == 1 {
            per_row.pop().expect("one row")
        } else {
            let sg = side_sigs.as_ref().expect("a reading port's signals");
            let mut sel = add_ff(g, rd.clk, domain_en, sg.row_bits.clone());
            if let Some((abs, _)) = &ri_impl.outreg {
                sel = add_ff(g, abs.clk, abs.en, sel);
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
            // A read beside a write the collision changed: where the block
            // read something else, the RTL's read of its own write's data.
            // Registered as the block's read is (same clock, same enable).
            let flag = add_ff(g, rd.clk, domain_en, flags.clone());
            let dq = add_ff(g, rd.clk, domain_en, want.clone());
            data = data
                .iter()
                .zip(flag.iter().zip(&dq))
                .map(|(&b, (&f, &d))| g.mux(b, d, f))
                .collect();
        }
        if !ri_impl.hw_en && rd.en != SigBit::ONE {
            // The block reads on every edge; LE registers hold what it read
            // when the read was enabled.
            let en_q = add_ff(g, rd.clk, None, vec![rd.en])[0];
            let hold = g.m.new_sig(n);
            let out = mux_word(g, &hold, &data, en_q);
            g.add(CellKind::Ff(Ff {
                q: hold,
                kind: FfKind::Dff {
                    clk: Ctrl::pos(rd.clk),
                    d: out.clone(),
                    control: DffControl::Plain { en: None },
                },
            }));
            data = out;
        }
        if rd.srst != SigBit::ZERO {
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
                    clk: Ctrl::pos(rd.clk),
                    d: vec![d],
                    control: DffControl::Plain { en: None },
                },
            }));
            let v: SigSpec = rd.srst_value.0.iter().map(|&s| SigBit::Const(s)).collect();
            data = mux_word(g, &data, &v, f);
        }
        let targets: Vec<SigBit> = match &ri_impl.outreg {
            Some((abs, _)) => abs.bits.iter().map(|&(_, _, q)| q).collect(),
            None => rd.data.clone(),
        };
        subst.extend(targets.into_iter().zip(data));
    }
    (blocks, subst)
}

/// Maps every memory the blocks take; returns how many blocks.
pub fn map(m: &mut Module) -> usize {
    let ctx = Context::new(m);
    let mut work = Vec::new();
    for (i, c) in m.cells.iter().enumerate() {
        let CellKind::Mem(mem) = &c.kind else {
            continue;
        };
        if wants_logic(&c.attributes) {
            continue;
        }
        let mut mem = mem.clone();
        ctx.same_enables(m, &mut mem);
        let Some(mut p) = ports(&mem) else { continue };
        p.disjoint = ctx.disjoint(m, &p);
        let Some(l) = layout(&mem, &p) else { continue };
        let r = realize(m, &ctx, &mem, &p, &l);
        work.push((i, mem, p, l, r));
    }
    // The output registers the blocks take over go first, while cell
    // indices hold.
    let mut drops: HashMap<usize, Vec<bool>> = HashMap::new();
    let mut absorbed = Vec::new();
    for (_, _, _, _, r) in &work {
        for (abs, _) in r.reads.iter().filter_map(|x| x.outreg.as_ref()) {
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
    // Their outputs' power-up values go with them (the block's is 0).
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
    let mut count = 0;
    let mut subst_all = Vec::new();
    // Built in order (the nets they make are numbered so), then each
    // memory replaced by its blocks from the last back, so the earlier
    // ones' indices hold.
    let mut g = Gates::new(m, "m4k");
    let mut built = Vec::new();
    for (i, mem, p, l, r) in work {
        let (blocks, subst) = build(&mut g, &mem, &p, &l, &r);
        count += blocks.len();
        built.push((i, blocks));
        subst_all.extend(subst);
    }
    for (i, blocks) in built.into_iter().rev() {
        g.m.cells.splice(i..i + 1, blocks);
    }
    m.cells
        .retain(|c| !matches!(&c.kind, CellKind::Ff(ff) if ff.width() == 0));
    let mut subst = Subst::new(m);
    for (from, to) in subst_all {
        if let SigBit::Net(n) = from {
            subst.set(n, to);
        }
    }
    subst.apply(m);
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_by_depth() {
        assert_eq!(width_of_depth(512), Some(Width::X9));
        assert_eq!(width_of_depth(4096), Some(Width::X1));
        assert_eq!(width_of_depth(64), None);
    }
}
