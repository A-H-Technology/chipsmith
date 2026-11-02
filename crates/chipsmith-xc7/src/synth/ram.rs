//! Small memories onto RAM32Ms, distributed RAM in a SLICEM's LUTs, as
//! yosys' `memory_libmap` picks them for `synth_xilinx` (lutrams_xcv.txt)
//! where a memory has one write port and reads asynchronously.
//!
//! A RAM32M is four 32 x 2 memories written together at one address
//! (ADDRD) and read each at its own: A, B and C at any address, D at the
//! write address. Each read port reads every 2-bit column of the memory
//! from a memory of its own, so a memory with R read ports and W-bit words
//! needs R * W / 2 of them, three to a RAM32M (D's read port is the write
//! address, which no read port here is taken to share).
//!
//! Everything else stays a `$mem_v2` for `memory::map_to_logic`.

use chipsmith_netlist::{CellKind, Mem, Module, Polarity, SigBit, State};
use chipsmith_synth::rewrite::Gates;

use super::cells::Ram32m;

const DEPTH: usize = 32;
const ABITS: usize = 5;

/// Whether `mem` fits the RAM32M's shape, with its one write enable.
fn mappable(mem: &Mem) -> Option<SigBit> {
    let [wr] = mem.wr.as_slice() else {
        return None;
    };
    let en = *wr.en.first()?;
    let fits = mem.size <= DEPTH
        && mem.offset == 0
        && !mem.rd.is_empty()
        && wr.clk.is_some()
        && !wr.wide_continuation
        && wr.en.iter().all(|&e| e == en)
        && mem.rd.iter().all(|r| r.sync.is_none() && !r.wide_continuation);
    fits.then_some(en)
}

/// An address cut or zero-padded to the RAM32M's five bits.
fn address(addr: &[SigBit]) -> Vec<SigBit> {
    (0..ABITS)
        .map(|i| addr.get(i).copied().unwrap_or(SigBit::ZERO))
        .collect()
}

/// Maps every memory that fits; returns how many RAM32Ms.
pub fn map(m: &mut Module) -> usize {
    let mut g = Gates::new(m, "ram");
    let cells = std::mem::take(&mut g.m.cells);
    let mut rams = 0;
    for c in cells {
        match c.kind {
            CellKind::Mem(mem) if mappable(&mem).is_some() => {
                rams += map_one(&mut g, &mem, mappable(&mem).expect("checked"));
            }
            kind => g.m.cells.push(chipsmith_netlist::Cell { kind, ..c }),
        }
    }
    rams
}

fn map_one(g: &mut Gates, mem: &Mem, we: SigBit) -> usize {
    let wr = &mem.wr[0];
    let clk = wr.clk.expect("checked");
    let waddr = address(&wr.addr);
    let init_bit = |word: usize, bit: usize| {
        mem.init.0.get(word * mem.width + bit) == Some(&State::One)
    };
    // (read port, first bit of the column) for every column of every port.
    let columns: Vec<(usize, usize)> = (0..mem.rd.len())
        .flat_map(|r| (0..mem.width).step_by(2).map(move |b| (r, b)))
        .collect();
    let mut n = 0;
    for slots in columns.chunks(3) {
        let mut raddr: [Vec<SigBit>; 3] = std::array::from_fn(|_| address(&[]));
        let mut di = [[SigBit::ZERO; 2]; 4];
        let mut dout: [[SigBit; 2]; 4] =
            std::array::from_fn(|_| std::array::from_fn(|_| SigBit::Net(g.m.new_net())));
        let mut init = [0u64; 4];
        for (k, &(r, b)) in slots.iter().enumerate() {
            raddr[k] = address(&mem.rd[r].addr);
            for i in 0..2 {
                if b + i >= mem.width {
                    continue;
                }
                di[k][i] = wr.data[b + i];
                // The memory's read data, driven by the RAM now.
                if let data @ SigBit::Net(_) = mem.rd[r].data[b + i] {
                    dout[k][i] = data;
                }
                for word in 0..mem.size {
                    if init_bit(word, b + i) {
                        init[k] |= 1 << (2 * word + i);
                    }
                }
            }
        }
        g.add(
            Ram32m {
                raddr,
                waddr: waddr.clone(),
                di,
                dout,
                wclk: clk.sig,
                wclk_inv: clk.polarity == Polarity::Neg,
                we,
                init,
            }
            .cell(),
        );
        n += 1;
    }
    n
}
