//! Memories onto M4K blocks: yosys' `memory_libmap` with cyclone2-re's
//! `synth/m4k.txt` and its logic-cost rule, then `m4k_map.v`.
//!
//! What the open flow supports (README "M4K RAM blocks"): one clock,
//! synchronous reads; single port (one address; a read sees the write on
//! the same edge, new data) x1 to x18, or simple dual port (one write and
//! one read port; a read of the word being written returns the old data)
//! x1 to x36; a ROM is either without its write. No byte enables, read
//! enables, resets or output registers. A memory that doesn't fit, or is so
//! small that its logic costs less than a block, stays logic.

use chipsmith_netlist::{
    CellKind, Const, Dir, Generic, GenericPort, Mem, Module, ParamValue, Polarity, SigBit, SigSpec,
    State,
};

/// What a block costs against a bit of logic (m4k.txt's `cost` and the
/// script's `-logic-cost-ram`/`-logic-cost-rom`).
const SP_COST: f64 = 64.0;
const SDP_COST: f64 = 65.0;
const LOGIC_COST_PER_BIT: f64 = 0.025;

/// The block's widths and how deep each makes it.
const CONFIGS: [(usize, usize); 6] = [(1, 4096), (2, 2048), (4, 1024), (9, 512), (18, 256), (36, 128)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// One address for reading and writing (or a ROM).
    Single,
    /// A write port and a read port.
    SimpleDual,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Single => "SINGLE",
            Mode::SimpleDual => "SIMPLE_DUAL",
        }
    }

    fn max_width(self) -> usize {
        match self {
            Mode::Single => 18,
            Mode::SimpleDual => 36,
        }
    }

    fn cost(self) -> f64 {
        match self {
            Mode::Single => SP_COST,
            Mode::SimpleDual => SDP_COST,
        }
    }
}

/// How a memory goes into blocks.
struct Plan {
    mode: Mode,
    /// The configuration's width (1, 2, 4, 9, 18, 36) and depth.
    width: usize,
    depth: usize,
    blocks: usize,
}

/// The modes a memory's ports allow, cheapest first.
fn modes(mem: &Mem) -> Vec<Mode> {
    let [rd] = &mem.rd[..] else {
        return Vec::new();
    };
    let Some(sync) = &rd.sync else {
        return Vec::new();
    };
    let plain = sync.clk.polarity == Polarity::Pos
        && sync.en == SigBit::ONE
        && sync.arst == SigBit::ZERO
        && sync.srst == SigBit::ZERO
        && !rd.wide_continuation
        // The block's output powers up 0.
        && sync.init_value.0.iter().all(|s| matches!(s, State::X | State::Zero));
    if !plain {
        return Vec::new();
    }
    match &mem.wr[..] {
        [] => vec![Mode::Single, Mode::SimpleDual],
        [wr] => {
            let one_enable = wr.en.iter().all(|&e| e == wr.en[0]);
            if wr.clk != Some(sync.clk) || !one_enable || wr.wide_continuation {
                return Vec::new();
            }
            let transparent = sync.transparent[0];
            if transparent && wr.addr == rd.addr {
                vec![Mode::Single]
            } else if !transparent {
                vec![Mode::SimpleDual]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn plan(mem: &Mem) -> Option<Plan> {
    let logic = LOGIC_COST_PER_BIT * (mem.size * mem.width) as f64;
    let mut best: Option<(f64, Plan)> = None;
    for mode in modes(mem) {
        for &(width, depth) in &CONFIGS {
            // Deeper than a block would need address decoding across
            // blocks, which isn't supported.
            if width > mode.max_width() || mem.size > depth {
                continue;
            }
            let blocks = mem.width.div_ceil(width);
            let cost = mode.cost() * blocks as f64;
            if best.as_ref().is_none_or(|(c, _)| cost < *c) {
                best = Some((
                    cost,
                    Plan {
                        mode,
                        width,
                        depth,
                        blocks,
                    },
                ));
            }
        }
    }
    best.filter(|(cost, _)| *cost < logic).map(|(_, p)| p)
}

/// The physical position of data bit `bit` of word `word` in a block of
/// this width: x9/x18/x36 words in order, x1/x2/x4 skipping each ninth bit
/// (the parity column those modes don't have).
fn init_index(width: usize, word: usize, bit: usize) -> usize {
    let p = word * width + bit;
    if width >= 9 {
        p
    } else {
        p + p / 8
    }
}

fn port(name: &str, dir: Dir, sig: SigSpec) -> GenericPort {
    GenericPort {
        name: name.into(),
        dir: Some(dir),
        sig,
    }
}

/// An address on the block's 12 pins: an n-bit address on the top n.
fn pins(addr: &[SigBit], depth: usize) -> SigSpec {
    let n = depth.trailing_zeros() as usize;
    let mut a = vec![SigBit::ZERO; 12 - n];
    a.extend((0..n).map(|i| addr.get(i).copied().unwrap_or(SigBit::ZERO)));
    a
}

/// Whether the blocks take this memory.
pub fn mappable(mem: &Mem) -> bool {
    plan(mem).is_some()
}

/// Maps every memory the blocks take; returns how many blocks.
pub fn map(m: &mut Module) -> usize {
    let mut count = 0;
    let mut read_data: Vec<(SigBit, SigBit)> = Vec::new();
    let cells = std::mem::take(&mut m.cells);
    let mut out = Vec::with_capacity(cells.len());
    for c in cells {
        let CellKind::Mem(mem) = &c.kind else {
            out.push(c);
            continue;
        };
        let Some(p) = plan(mem) else {
            out.push(c);
            continue;
        };
        let rd = &mem.rd[0];
        let clk = rd.sync.as_ref().expect("planned ports are synchronous").clk.sig;
        let wr = mem.wr.first();
        let name = mem.memid.trim_start_matches('\\');
        for b in 0..p.blocks {
            let cols = b * p.width..((b + 1) * p.width).min(mem.width);
            let mut init = vec![State::X; 4608];
            for word in 0..mem.size.min(p.depth) {
                for (k, col) in cols.clone().enumerate() {
                    init[init_index(p.width, word, k)] = mem.init.0[word * mem.width + col];
                }
            }
            let mut din: SigSpec = match wr {
                Some(w) => cols.clone().map(|col| w.data[col]).collect(),
                None => Vec::new(),
            };
            din.resize(36, SigBit::ZERO);
            let dout = m.new_sig(36);
            // The read data are now the block's outputs.
            for (k, col) in cols.clone().enumerate() {
                read_data.push((rd.data[col], dout[k]));
            }
            let (a_addr, b_addr, we) = match (p.mode, wr) {
                (Mode::Single, Some(w)) => (pins(&rd.addr, p.depth), vec![SigBit::ZERO; 12], w.en[0]),
                (Mode::Single, None) => (pins(&rd.addr, p.depth), vec![SigBit::ZERO; 12], SigBit::ZERO),
                (Mode::SimpleDual, Some(w)) => (pins(&w.addr, p.depth), pins(&rd.addr, p.depth), w.en[0]),
                (Mode::SimpleDual, None) => (vec![SigBit::ZERO; 12], pins(&rd.addr, p.depth), SigBit::ZERO),
            };
            let params = [
                ("MODE".to_string(), ParamValue::Str(p.mode.name().into())),
                ("WIDTH".to_string(), ParamValue::int(p.width as i64)),
                ("INIT".to_string(), ParamValue::Bits(Const(init))),
            ]
            .into_iter()
            .collect();
            out.push(chipsmith_netlist::Cell {
                name: format!("{name}.0.{b}"),
                kind: CellKind::Other(Generic {
                    ty: super::cells::M4K.into(),
                    params,
                    ports: vec![
                        port("CLK", Dir::In, vec![clk]),
                        port("A_ADDR", Dir::In, a_addr),
                        port("A_DATAIN", Dir::In, din),
                        port("A_WE", Dir::In, vec![we]),
                        port("B_ADDR", Dir::In, b_addr),
                        port("DATAOUT", Dir::Out, dout),
                    ],
                }),
                attributes: Default::default(),
            });
            count += 1;
        }
    }
    m.cells = out;
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    for (from, to) in read_data {
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
    fn narrow_modes_skip_the_parity_column() {
        assert_eq!(init_index(1, 7, 0), 7);
        assert_eq!(init_index(1, 8, 0), 9);
        assert_eq!(init_index(2, 4, 1), 10);
        assert_eq!(init_index(9, 1, 8), 17);
    }

    #[test]
    fn addresses_sit_on_the_top_pins() {
        let a = vec![SigBit::ONE; 8];
        let p = pins(&a, 256);
        assert_eq!(&p[..4], &[SigBit::ZERO; 4]);
        assert_eq!(&p[4..], &a[..]);
    }
}
