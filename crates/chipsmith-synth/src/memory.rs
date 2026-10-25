//! Memories: `proc`'s `$memrd`/`$memwr`/`$meminit` cells collected into
//! one `$mem_v2` each (yosys' `memory_collect`), registers on a read port
//! merged into it (`memory_dff`), and memories no RAM block takes turned
//! into registers and multiplexers (`memory_map`).

use std::collections::HashMap;

use anyhow::{bail, ensure, Context, Result};
use chipsmith_netlist::{
    attr, Binary, BinaryOp, Bmux, Cell, CellKind, Const, Ctrl, DffControl, Ff, FfKind, Generic,
    Mem, MemRd, MemRdSync, MemWr, Module, Mux, Net, Operand, ParamValue, Polarity, SigBit, SigSpec,
    State, SyncEnable, Wire, Word,
};

use crate::rewrite::{inits, Namer, Subst};
use crate::techmap::extend;

fn param<'a>(g: &'a Generic, name: &str) -> Result<&'a ParamValue> {
    g.params
        .get(name)
        .with_context(|| format!("{} without {name}", g.ty))
}

fn int(g: &Generic, name: &str) -> Result<i64> {
    param(g, name)?
        .bits()
        .and_then(Const::as_i64)
        .with_context(|| format!("{}: {name} is not a number", g.ty))
}

fn flag(g: &Generic, name: &str) -> Result<bool> {
    Ok(param(g, name)?.bits().is_some_and(Const::as_bool))
}

fn konst(g: &Generic, name: &str) -> Result<Const> {
    param(g, name)?
        .bits()
        .cloned()
        .with_context(|| format!("{}: {name} is not bits", g.ty))
}

fn sig(g: &Generic, name: &str) -> Result<SigSpec> {
    Ok(g.port(name)
        .with_context(|| format!("{} without port {name}", g.ty))?
        .sig
        .clone())
}

fn memid(g: &Generic) -> Option<&str> {
    g.params.get("MEMID").and_then(ParamValue::str)
}

fn konst_of(s: &[SigBit]) -> Option<Const> {
    s.iter()
        .map(|b| match b {
            SigBit::Const(s) => Some(*s),
            SigBit::Net(_) => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(Const)
}

fn clock(g: &Generic) -> Result<Option<Ctrl>> {
    Ok(if flag(g, "CLK_ENABLE")? {
        Some(Ctrl {
            sig: sig(g, "CLK")?[0],
            polarity: Polarity::from_bool(flag(g, "CLK_POLARITY")?),
        })
    } else {
        None
    })
}

/// `memory_collect`: every RTLIL memory and its port cells into one
/// `$mem_v2`.
pub fn collect(m: &mut Module) -> Result<()> {
    if m.memories.is_empty() {
        return Ok(());
    }
    let mut by_mem: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, c) in m.cells.iter().enumerate() {
        if let CellKind::Other(g) = &c.kind {
            if g.ty.starts_with("$mem") && g.ty != "$mem_v2" {
                let id = memid(g).with_context(|| format!("{} without MEMID", c.name))?;
                by_mem.entry(id.to_string()).or_default().push(i);
            }
        }
    }
    let mut remove = vec![false; m.cells.len()];
    let mut mems = Vec::new();
    for memory in std::mem::take(&mut m.memories) {
        let id = format!("\\{}", memory.name.trim_start_matches('\\'));
        let cells = by_mem.remove(&id).unwrap_or_default();
        let (mut rd, mut wr, mut init) = (Vec::new(), Vec::new(), Vec::new());
        for &i in &cells {
            remove[i] = true;
            let CellKind::Other(g) = &m.cells[i].kind else {
                unreachable!()
            };
            match g.ty.as_str() {
                "$memrd" | "$memrd_v2" => rd.push((m.cells[i].name.clone(), g.clone())),
                "$memwr" | "$memwr_v2" => {
                    let key = if g.ty == "$memwr" { "PRIORITY" } else { "PORTID" };
                    wr.push((int(g, key)?, g.clone()));
                }
                "$meminit" | "$meminit_v2" => init.push((int(g, "PRIORITY")?, g.clone())),
                other => bail!("memory {id}: {other} cells are not supported"),
            }
        }
        wr.sort_by_key(|(k, _)| *k);
        init.sort_by_key(|(k, _)| *k);
        let width = memory.width;
        let size = memory.size;
        let abits = rd
            .iter()
            .map(|(_, g)| sig(g, "ADDR").map(|a| a.len()))
            .chain(wr.iter().map(|(_, g)| sig(g, "ADDR").map(|a| a.len())))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .max()
            .unwrap_or(0);

        let mut contents = vec![State::X; size * width];
        for (_, g) in &init {
            let addr = konst_of(&sig(g, "ADDR")?)
                .and_then(|c| c.as_i64())
                .with_context(|| format!("memory {id}: non-constant init address"))?;
            let data = konst_of(&sig(g, "DATA")?)
                .with_context(|| format!("memory {id}: non-constant init data"))?;
            let en = if g.ty == "$meminit_v2" {
                konst_of(&sig(g, "EN")?)
                    .with_context(|| format!("memory {id}: non-constant init enable"))?
            } else {
                Const::filled(State::One, width)
            };
            let first = (addr - memory.start_offset) as usize * width;
            for (k, s) in data.0.iter().enumerate() {
                let i = first + k;
                if i < contents.len() && en.0[k % width] == State::One {
                    contents[i] = *s;
                }
            }
        }

        let nw = wr.len();
        let wr_ports: Vec<MemWr> = wr
            .iter()
            .enumerate()
            .map(|(i, (_, g))| -> Result<MemWr> {
                let clk = clock(g)?;
                let priority = if g.ty == "$memwr" {
                    // Old-style: a later port wins over earlier ones on the
                    // same clock.
                    (0..nw)
                        .map(|j| {
                            j < i
                                && clock(&wr[j].1).ok().flatten() == clk
                                && clk.is_some()
                        })
                        .collect()
                } else {
                    let mask = konst(g, "PRIORITY_MASK")?;
                    wr.iter()
                        .map(|(portid, _)| {
                            mask.0.get(*portid as usize) == Some(&State::One)
                        })
                        .collect()
                };
                ensure!(
                    sig(g, "DATA")?.len() == width,
                    "memory {id}: wide write ports are not supported"
                );
                Ok(MemWr {
                    clk,
                    en: sig(g, "EN")?,
                    addr: extend(&sig(g, "ADDR")?, abits, false),
                    data: sig(g, "DATA")?,
                    wide_continuation: false,
                    priority,
                })
            })
            .collect::<Result<_>>()?;
        let mut rd_ports = Vec::new();
        rd.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, g) in &rd {
            let data = sig(g, "DATA")?;
            ensure!(
                data.len() == width,
                "memory {id}: wide read port {name} is not supported"
            );
            let clk = clock(g)?;
            let sync = match clk {
                None => None,
                Some(clk) => {
                    let transparent = if g.ty == "$memrd" {
                        let t = flag(g, "TRANSPARENT")?;
                        wr_ports.iter().map(|w| t && w.clk == Some(clk)).collect()
                    } else {
                        bail!("memory {id}: synchronous $memrd_v2 port {name} is not supported");
                    };
                    let x = Const::filled(State::X, width);
                    Some(MemRdSync {
                        clk,
                        en: sig(g, "EN")?[0],
                        arst: SigBit::ZERO,
                        srst: SigBit::ZERO,
                        ce_over_srst: false,
                        init_value: x.clone(),
                        arst_value: x.clone(),
                        srst_value: x,
                        transparent,
                        collision_x: vec![false; nw],
                    })
                }
            };
            rd_ports.push(MemRd {
                addr: extend(&sig(g, "ADDR")?, abits, false),
                data,
                wide_continuation: false,
                sync,
            });
        }
        mems.push((
            memory.attributes.clone(),
            Mem {
                memid: id,
                size,
                offset: memory.start_offset,
                width,
                abits,
                init: Const(contents),
                rd: rd_ports,
                wr: wr_ports,
            },
        ));
    }
    if let Some(id) = by_mem.keys().next() {
        bail!("memory cells for {id}, which isn't a memory of the module");
    }
    let mut it = remove.into_iter();
    m.cells.retain(|_| !it.next().expect("one flag per cell"));
    let mut namer = Namer::new(m, "mem");
    for (attributes, mem) in mems {
        let name = namer.fresh();
        let id = m.add_cell(name, CellKind::Mem(mem));
        m.cells[id].attributes = attributes;
    }
    Ok(())
}

/// Who reads each net: (cell, is a register's D input, bit within D).
fn readers(m: &Module) -> Vec<Vec<(usize, Option<usize>)>> {
    let mut out = vec![Vec::new(); m.net_count()];
    for (i, c) in m.cells.iter().enumerate() {
        if let CellKind::Ff(_) = &c.kind {
            c.kind.for_each_port(|p| {
                if p.dir == Some(chipsmith_netlist::Dir::In) {
                    for (k, b) in p.bits.iter().enumerate() {
                        if let SigBit::Net(n) = b {
                            out[n.index()].push((i, (p.name == "D").then_some(k)));
                        }
                    }
                }
            });
            continue;
        }
        c.kind.for_each_port(|p| {
            if p.dir.is_none_or(|d| d.reads()) {
                for b in p.bits {
                    if let SigBit::Net(n) = b {
                        out[n.index()].push((i, None));
                    }
                }
            }
        });
    }
    for (_, w, dir) in m.ports() {
        if dir.drives() {
            for b in &w.bits {
                if let SigBit::Net(n) = b {
                    out[n.index()].push((usize::MAX, None));
                }
            }
        }
    }
    out
}

/// A register's clocking, if it is a plain or synchronously reset
/// edge-triggered one: what a read port can take over.
#[derive(Clone, PartialEq)]
struct Clocking {
    clk: Ctrl,
    en: Option<Ctrl>,
    srst: Option<(Ctrl, bool)>,
}

fn clocking(ff: &Ff<Word>) -> Option<Clocking> {
    let FfKind::Dff { clk, control, .. } = &ff.kind else {
        return None;
    };
    match control {
        DffControl::Plain { en } => Some(Clocking {
            clk: *clk,
            en: *en,
            srst: None,
        }),
        DffControl::Sync { srst, en, .. } => Some(Clocking {
            clk: *clk,
            en: match en {
                SyncEnable::Always => None,
                SyncEnable::EnableOverReset(c) => Some(*c),
                SyncEnable::ResetOverEnable(_) => return None,
            },
            srst: Some((*srst, matches!(en, SyncEnable::EnableOverReset(_)))),
        }),
        _ => None,
    }
}

/// Removes bits `drop` from a word-level register.
pub(crate) fn drop_bits(ff: &mut Ff<Word>, drop: &[bool]) {
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

/// `memory_dff`: a register on every bit of an asynchronous read port's
/// data (and nothing else reading the data) becomes the port's clock:
/// the read happens at the edge, before any write on it, so it returns the
/// old data. Failing that, a register on its whole address does: then the
/// read sees writes on the same edge (new data).
pub fn merge_registers(m: &mut Module) -> Result<()> {
    let init = inits(m);
    let readers = readers(m);
    let mut ff_drop: HashMap<usize, Vec<bool>> = HashMap::new();
    let mut updates: Vec<(usize, usize, MemRd)> = Vec::new();
    // Driver of each net among registers' Q: (cell, bit).
    let mut q_of = vec![None; m.net_count()];
    for (i, c) in m.cells.iter().enumerate() {
        if let CellKind::Ff(ff) = &c.kind {
            for (k, b) in ff.q.iter().enumerate() {
                if let SigBit::Net(n) = b {
                    q_of[n.index()] = Some((i, k));
                }
            }
        }
    }
    for (ci, c) in m.cells.iter().enumerate() {
        let CellKind::Mem(mem) = &c.kind else {
            continue;
        };
        for (pi, rd) in mem.rd.iter().enumerate() {
            if rd.sync.is_some() {
                continue;
            }
            if let Some(port) = output_register(m, &readers, &init, mem, rd, &mut ff_drop) {
                updates.push((ci, pi, port));
                continue;
            }
            if let Some(port) = address_register(m, &q_of, mem, rd) {
                updates.push((ci, pi, port));
            }
        }
    }
    for (ci, pi, port) in updates {
        if let CellKind::Mem(mem) = &mut m.cells[ci].kind {
            mem.rd[pi] = port;
        }
    }
    for (ci, drop) in ff_drop {
        if let CellKind::Ff(ff) = &mut m.cells[ci].kind {
            drop_bits(ff, &drop);
        }
    }
    m.cells.retain(|c| !matches!(&c.kind, CellKind::Ff(ff) if ff.width() == 0));
    Ok(())
}

fn output_register(
    m: &Module,
    readers: &[Vec<(usize, Option<usize>)>],
    init: &rustc_hash::FxHashMap<Net, State>,
    mem: &Mem,
    rd: &MemRd,
    ff_drop: &mut HashMap<usize, Vec<bool>>,
) -> Option<MemRd> {
    let mut found: Vec<(usize, usize)> = Vec::new();
    let mut clk: Option<Clocking> = None;
    for b in &rd.data {
        let SigBit::Net(n) = b else { return None };
        let [(cell, Some(bit))] = readers[n.index()][..] else {
            return None;
        };
        if ff_drop.get(&cell).is_some_and(|d| d[bit]) {
            return None;
        }
        let CellKind::Ff(ff) = &m.cells[cell].kind else {
            return None;
        };
        let this = clocking(ff)?;
        if clk.as_ref().is_some_and(|c| *c != this) {
            return None;
        }
        clk = Some(this);
        found.push((cell, bit));
    }
    let clk = clk?;
    // The enable and reset must be active high for the port's signals.
    if clk.en.is_some_and(|e| e.polarity == Polarity::Neg)
        || clk.srst.is_some_and(|(r, _)| r.polarity == Polarity::Neg)
    {
        return None;
    }
    let mut data = Vec::new();
    let (mut init_value, mut srst_value) = (Vec::new(), Vec::new());
    for &(cell, bit) in &found {
        let CellKind::Ff(ff) = &m.cells[cell].kind else {
            unreachable!()
        };
        let q = ff.q[bit];
        data.push(q);
        init_value.push(q.net().and_then(|n| init.get(&n)).copied().unwrap_or(State::X));
        srst_value.push(match &ff.kind {
            FfKind::Dff {
                control: DffControl::Sync { value, .. },
                ..
            } => value.0[bit],
            _ => State::X,
        });
        ff_drop
            .entry(cell)
            .or_insert_with(|| vec![false; ff.width()])[bit] = true;
    }
    Some(MemRd {
        addr: rd.addr.clone(),
        data,
        wide_continuation: false,
        sync: Some(MemRdSync {
            clk: clk.clk,
            en: clk.en.map_or(SigBit::ONE, |e| e.sig),
            arst: SigBit::ZERO,
            srst: clk.srst.map_or(SigBit::ZERO, |(r, _)| r.sig),
            ce_over_srst: clk.srst.is_some_and(|(_, ce)| ce),
            init_value: Const(init_value),
            arst_value: Const::filled(State::X, mem.width),
            srst_value: Const(srst_value),
            transparent: vec![false; mem.wr.len()],
            collision_x: vec![false; mem.wr.len()],
        }),
    })
}

fn address_register(
    m: &Module,
    q_of: &[Option<(usize, usize)>],
    mem: &Mem,
    rd: &MemRd,
) -> Option<MemRd> {
    let mut clk: Option<Ctrl> = None;
    let mut addr = Vec::new();
    for b in &rd.addr {
        let n = match b {
            SigBit::Net(n) => *n,
            // A constant address bit stays constant.
            SigBit::Const(_) => {
                addr.push(*b);
                continue;
            }
        };
        let (cell, bit) = q_of[n.index()]?;
        let CellKind::Ff(ff) = &m.cells[cell].kind else {
            return None;
        };
        let FfKind::Dff {
            clk: c,
            d,
            control: DffControl::Plain { en: None },
        } = &ff.kind
        else {
            return None;
        };
        if clk.is_some_and(|k| k != *c) {
            return None;
        }
        clk = Some(*c);
        addr.push(d[bit]);
    }
    let clk = clk?;
    // Every write port on this clock is seen; one on another clock can't
    // be ordered against the read.
    if mem.wr.iter().any(|w| w.clk != Some(clk)) {
        return None;
    }
    let x = Const::filled(State::X, mem.width);
    Some(MemRd {
        addr,
        data: rd.data.clone(),
        wide_continuation: false,
        sync: Some(MemRdSync {
            clk,
            en: SigBit::ONE,
            arst: SigBit::ZERO,
            srst: SigBit::ZERO,
            ce_over_srst: false,
            init_value: x.clone(),
            arst_value: x.clone(),
            srst_value: x,
            transparent: vec![true; mem.wr.len()],
            collision_x: vec![false; mem.wr.len()],
        }),
    })
}

/// `memory_map`: the memories `select` picks become a register per word
/// (named `memid[i]`, as yosys names them), write logic per word and a
/// multiplexer tree per read port.
pub fn map_to_logic(m: &mut Module, select: impl Fn(&Mem) -> bool) -> Result<()> {
    let mut namer = Namer::new(m, "memmap");
    let cells = std::mem::take(&mut m.cells);
    let mut out = Vec::new();
    let mut new_wires = Vec::new();
    let mut subst = Subst::new(m);
    for c in cells {
        match c.kind {
            CellKind::Mem(mem) if select(&mem) => {
                let mut add = |kind: CellKind| {
                    out.push(Cell {
                        name: namer.fresh(),
                        kind,
                        attributes: Default::default(),
                    })
                };
                map_mem(m, &mem, &mut add, &mut new_wires, &mut subst)
                    .with_context(|| format!("memory {}", mem.memid))?;
            }
            kind => out.push(Cell { kind, ..c }),
        }
    }
    m.cells = out;
    m.wires.extend(new_wires);
    subst.apply(m);
    Ok(())
}

fn konst_sig(v: usize, w: usize) -> SigSpec {
    Const::from_int(v as i64, w)
        .0
        .into_iter()
        .map(SigBit::Const)
        .collect()
}

fn map_mem(
    m: &mut Module,
    mem: &Mem,
    add: &mut impl FnMut(CellKind),
    wires: &mut Vec<Wire>,
    subst: &mut Subst,
) -> Result<()> {
    ensure!(mem.offset == 0, "a memory starting at a nonzero address is not supported");
    let w = mem.width;
    let clk = match mem.wr.first() {
        Some(p) => Some(p.clk.context("asynchronous write ports are not supported")?),
        None => None,
    };
    ensure!(
        mem.wr.iter().all(|p| p.clk == clk),
        "write ports on different clocks are not supported"
    );
    let name = mem.memid.trim_start_matches('\\');
    let words: Vec<SigSpec> = (0..mem.size)
        .map(|i| {
            let q = m.new_sig(w);
            let mut wire = Wire::new(format!("{name}[{i}]"), q.clone());
            let word = &mem.init.0[i * w..(i + 1) * w];
            if word.iter().any(|s| *s != State::X) {
                wire.attributes
                    .insert(attr::INIT.into(), ParamValue::Bits(Const(word.to_vec())));
            }
            wires.push(wire);
            q
        })
        .collect();

    match clk {
        // A ROM: the words are their contents.
        None => {
            for (i, q) in words.iter().enumerate() {
                for (k, b) in q.iter().enumerate() {
                    if let SigBit::Net(n) = b {
                        subst.set(*n, SigBit::Const(mem.init.0[i * w + k]));
                    }
                }
            }
        }
        Some(clk) => {
            for (i, q) in words.iter().enumerate() {
                let mut data: SigSpec = q.clone();
                for p in &mem.wr {
                    let hit = m.new_sig(1)[0];
                    add(CellKind::Binary(Binary {
                        op: BinaryOp::Eq,
                        a: Operand {
                            sig: p.addr.clone(),
                            signed: false,
                        },
                        b: Operand {
                            sig: konst_sig(i, p.addr.len().max(1)),
                            signed: false,
                        },
                        y: vec![hit],
                    }));
                    // One multiplexer per run of bits sharing an enable.
                    let mut k = 0;
                    while k < w {
                        let en = p.en[k];
                        let mut end = k + 1;
                        while end < w && p.en[end] == en {
                            end += 1;
                        }
                        let s = m.new_sig(1)[0];
                        add(CellKind::Binary(Binary {
                            op: BinaryOp::And,
                            a: Operand {
                                sig: vec![hit],
                                signed: false,
                            },
                            b: Operand {
                                sig: vec![en],
                                signed: false,
                            },
                            y: vec![s],
                        }));
                        let y = m.new_sig(end - k);
                        add(CellKind::Mux(Mux {
                            a: data[k..end].to_vec(),
                            b: p.data[k..end].to_vec(),
                            s,
                            y: y.clone(),
                        }));
                        data.splice(k..end, y);
                        k = end;
                    }
                }
                add(CellKind::Ff(Ff {
                    q: q.clone(),
                    kind: FfKind::Dff {
                        clk,
                        d: data,
                        control: DffControl::Plain { en: None },
                    },
                }));
            }
        }
    }

    // Read ports: a multiplexer tree on the address over the words.
    let abits = (usize::BITS - mem.size.saturating_sub(1).leading_zeros()) as usize;
    let mut all: SigSpec = Vec::with_capacity(w << abits);
    for i in 0..1usize << abits {
        all.extend(words.get(i).cloned().unwrap_or_else(|| vec![SigBit::X; w]));
    }
    for rd in &mem.rd {
        let mut addr = extend(&rd.addr, abits, false);
        let data_out = rd.data.clone();
        let read = |m: &mut Module, add: &mut dyn FnMut(CellKind), addr: SigSpec| -> SigSpec {
            let y = m.new_sig(w);
            if abits == 0 {
                return all.clone();
            }
            add(CellKind::Bmux(Bmux {
                a: all.clone(),
                s: addr,
                y: y.clone(),
            }));
            y
        };
        match &rd.sync {
            None => {
                let y = read(m, add, addr);
                for (o, v) in data_out.iter().zip(y) {
                    if let SigBit::Net(n) = o {
                        subst.set(*n, v);
                    }
                }
            }
            Some(s) => {
                ensure!(
                    s.arst == SigBit::ZERO,
                    "read ports with an asynchronous reset are not supported"
                );
                let all_t = s.transparent.iter().all(|&t| t);
                let none_t = s.transparent.iter().all(|&t| !t);
                if all_t && !s.transparent.is_empty() {
                    // Register the address: the words are read after the
                    // edge that wrote them.
                    ensure!(
                        s.en == SigBit::ONE && s.srst == SigBit::ZERO,
                        "transparent read ports with an enable or reset are not supported"
                    );
                    let q = m.new_sig(addr.len());
                    add(CellKind::Ff(Ff {
                        q: q.clone(),
                        kind: FfKind::Dff {
                            clk: s.clk,
                            d: addr,
                            control: DffControl::Plain { en: None },
                        },
                    }));
                    addr = q;
                    let y = read(m, add, addr);
                    for (o, v) in data_out.iter().zip(y) {
                        if let SigBit::Net(n) = o {
                            subst.set(*n, v);
                        }
                    }
                } else {
                    ensure!(
                        none_t,
                        "read ports transparent to only some write ports are not supported"
                    );
                    let y = read(m, add, addr);
                    let en = (s.en != SigBit::ONE).then_some(Ctrl::pos(s.en));
                    let control = if s.srst == SigBit::ZERO {
                        DffControl::Plain { en }
                    } else {
                        DffControl::Sync {
                            srst: Ctrl::pos(s.srst),
                            value: s.srst_value.clone(),
                            en: match en {
                                None => SyncEnable::Always,
                                Some(e) if s.ce_over_srst => SyncEnable::EnableOverReset(e),
                                Some(e) => SyncEnable::ResetOverEnable(e),
                            },
                        }
                    };
                    add(CellKind::Ff(Ff {
                        q: data_out.clone(),
                        kind: FfKind::Dff {
                            clk: s.clk,
                            d: y,
                            control,
                        },
                    }));
                    if s.init_value.0.iter().any(|v| *v != State::X) {
                        wires.push({
                            let mut wire = Wire::new(
                                format!("$chipsmith$memrd${}", wires.len()),
                                data_out.clone(),
                            );
                            wire.attributes.insert(
                                attr::INIT.into(),
                                ParamValue::Bits(s.init_value.clone()),
                            );
                            wire
                        });
                    }
                }
            }
        }
    }
    Ok(())
}
