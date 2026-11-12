//! Small memories onto MLAB RAM (README (z9)), as yosys' `memory_libmap`
//! with tools/a5pnr/yosys/mlab.txt maps them: one `$__A5_MLAB_` per read
//! port and data bit, a 32 x 1 RAM written on the rising clock when its
//! enable is high and read with no clock. A memory fits with at most 32
//! words, one clocked write port with one enable, and asynchronous reads
//! (a register on a read stays the RTL's own flop). Everything else stays
//! a `$mem_v2` for `memory::map_to_logic`.

use std::collections::BTreeMap;

use chipsmith_netlist::{
    CellKind, Const, Ctrl, DffControl, Dir, Ff, FfKind, Generic, GenericPort, Mem, MemRdSync,
    Module, ParamValue, Polarity, SigBit, SigSpec, State, Word,
};

pub const MLAB: &str = "$__A5_MLAB_";

fn fits(mem: &Mem, same: &dyn Fn(SigBit) -> SigBit) -> Option<SigBit> {
    let [wr] = mem.wr.as_slice() else { return None };
    let en = *wr.en.first()?;
    let en0 = same(en);
    let ok = mem.size <= 32
        && mem.offset == 0
        && !mem.rd.is_empty()
        && wr.clk.is_some_and(|c| c.polarity == Polarity::Pos)
        && !wr.wide_continuation
        && wr.en.iter().all(|&e| same(e) == en0)
        && mem
            .rd
            .iter()
            .all(|r| !r.wide_continuation && r.sync.as_ref().is_none_or(splittable));
    ok.then_some(en)
}

/// A synchronous read that is an asynchronous one and a register: no
/// reset, and no write on the same edge visible in it.
fn splittable(s: &MemRdSync) -> bool {
    s.arst == SigBit::ZERO && s.srst == SigBit::ZERO && !s.transparent.iter().any(|&t| t)
}

fn addr(a: &[SigBit]) -> Vec<SigBit> {
    (0..5)
        .map(|i| a.get(i).copied().unwrap_or(SigBit::ZERO))
        .collect()
}

/// Maps every memory that fits; returns how many RAM bits.
pub fn map(m: &mut Module) -> usize {
    // A write enable per data bit, each bit `s ? e : 0` of one word mux
    // (proc's per-bit enables; the CPU's register file): equal bits are one
    // enable.
    let mut muxed: std::collections::HashMap<SigBit, SigBit> = Default::default();
    for c in &m.cells {
        if let CellKind::Mux(x) = &c.kind {
            for i in 0..x.y.len() {
                if x.a[i] == SigBit::ZERO
                    && x.b.iter().all(|&b| b == x.b[0])
                    && x.a.iter().all(|&a| a == SigBit::ZERO)
                {
                    muxed.insert(x.y[i], x.y[0]);
                }
            }
        }
    }
    let same = |e: SigBit| muxed.get(&e).copied().unwrap_or(e);
    let cells = std::mem::take(&mut m.cells);
    let mut bits = 0;
    let mut add = Vec::new();
    for c in cells {
        let CellKind::Mem(mem) = &c.kind else {
            m.cells.push(c);
            continue;
        };
        let Some(we) = fits(mem, &same) else {
            if std::env::var_os("A5SYNTH_DEBUG").is_some() {
                eprintln!(
                    "ram: {} stays logic: {} x {}, {} writes {:?}, reads {:?}",
                    mem.memid,
                    mem.size,
                    mem.width,
                    mem.wr.len(),
                    mem.wr
                        .iter()
                        .map(|w| {
                            let mut e = w.en.to_vec();
                            e.dedup();
                            e
                        })
                        .collect::<Vec<_>>(),
                    mem.rd.iter().map(|r| &r.sync).collect::<Vec<_>>()
                );
            }
            m.cells.push(c);
            continue;
        };
        let wr = &mem.wr[0];
        let memid = mem.memid.trim_start_matches('\\');
        for (r, rd) in mem.rd.iter().enumerate() {
            // A synchronous read: the RAM's read data into a register of
            // its own, which keeps the port's data nets (the RTL's register).
            let data: SigSpec = match &rd.sync {
                None => rd.data.clone(),
                Some(s) => {
                    let raw: SigSpec = (0..mem.width).map(|_| SigBit::Net(m.new_net())).collect();
                    let en = (s.en != SigBit::ONE).then(|| Ctrl::pos(s.en));
                    let ff = Ff::<Word> {
                        q: rd.data.clone(),
                        kind: FfKind::Dff {
                            clk: s.clk,
                            d: raw.clone(),
                            control: DffControl::Plain { en },
                        },
                    };
                    add.push((format!("{memid}.{r}.rdreg"), CellKind::Ff(ff)));
                    raw
                }
            };
            // b indexes the write data, the read data and the init bits alike.
            #[allow(clippy::needless_range_loop)]
            for b in 0..mem.width {
                let init: u32 = (0..mem.size)
                    .filter(|&w| mem.init.0.get(w * mem.width + b) == Some(&State::One))
                    .fold(0, |v, w| v | 1 << w);
                let port = |name: &str, dir, sig: Vec<SigBit>| GenericPort {
                    name: name.into(),
                    dir: Some(dir),
                    sig,
                };
                let ports = vec![
                    port("PORT_W_CLK", Dir::In, vec![wr.clk.expect("checked").sig]),
                    port("PORT_W_WR_EN", Dir::In, vec![we]),
                    port("PORT_W_ADDR", Dir::In, addr(&wr.addr)),
                    port("PORT_W_WR_DATA", Dir::In, vec![wr.data[b]]),
                    port("PORT_R_ADDR", Dir::In, addr(&rd.addr)),
                    port("PORT_R_RD_DATA", Dir::Out, vec![data[b]]),
                ];
                let params = BTreeMap::from([(
                    "INIT".to_string(),
                    ParamValue::Bits(Const(
                        (0..32).map(|w| State::from(init >> w & 1 == 1)).collect(),
                    )),
                )]);
                // a5pnr's ffmap reads `<memory>.<read port>.<bit>` (yosys' naming).
                add.push((
                    format!("{memid}.{r}.{b}"),
                    CellKind::Other(Generic {
                        ty: MLAB.into(),
                        params,
                        ports,
                    }),
                ));
                bits += 1;
            }
        }
    }
    for (name, kind) in add {
        m.add_cell(name, kind);
    }
    bits
}
