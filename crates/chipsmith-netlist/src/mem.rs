//! `$mem_v2`: a memory with all its ports, what `memory_collect` builds
//! and RAM mapping consumes.
//!
//! RTLIL packs each port kind's signals and settings into concatenated
//! ports and per-port bit vectors (`RD_ADDR` is every read port's address,
//! bit `i` of `RD_CLK_ENABLE` is read port `i`'s). Here each port is a
//! struct, the way yosys' `Mem` helper unpacks it.

use crate::comb::expect_width;
use crate::ports::{CellError, Decoder, Dir, Params, Ports};
use crate::sig::{Const, Ctrl, Polarity, SigBit, SigSpec, State};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mem {
    /// The memory's RTLIL name (`\ram`), which `$memrd_v2` and friends
    /// refer to it by.
    pub memid: String,
    /// Words.
    pub size: usize,
    /// The address of the first word.
    pub offset: i64,
    /// Bits per word.
    pub width: usize,
    pub abits: usize,
    /// `size * width` bits, word 0 first; x where uninitialized.
    pub init: Const,
    pub rd: Vec<MemRd>,
    pub wr: Vec<MemWr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemRd {
    pub addr: SigSpec,
    pub data: SigSpec,
    /// This port is the next `width` bits of the previous one (a wide
    /// port, split into word-sized pieces).
    pub wide_continuation: bool,
    /// `None` for an asynchronous read.
    pub sync: Option<MemRdSync>,
}

/// A synchronous read port's clocking. RTLIL ties these signals to
/// constants on an asynchronous port, so they only exist here for a
/// synchronous one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemRdSync {
    pub clk: Ctrl,
    pub en: SigBit,
    pub arst: SigBit,
    pub srst: SigBit,
    /// The enable gates the synchronous reset.
    pub ce_over_srst: bool,
    pub init_value: Const,
    pub arst_value: Const,
    pub srst_value: Const,
    /// Per write port: a write on the same edge is visible in this read.
    pub transparent: Vec<bool>,
    /// Per write port: reading a word written on the same edge gives x.
    pub collision_x: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemWr {
    /// `None` for an asynchronous write.
    pub clk: Option<Ctrl>,
    /// Per data bit.
    pub en: SigSpec,
    pub addr: SigSpec,
    pub data: SigSpec,
    pub wide_continuation: bool,
    /// Per write port: this one wins over that one when both write a word
    /// on the same edge.
    pub priority: Vec<bool>,
}

/// What an asynchronous read port's clocking signals are tied to, as
/// yosys' `Mem::emit` writes them.
static ASYNC_CLK: [SigBit; 1] = [SigBit::X];
static ASYNC_EN: [SigBit; 1] = [SigBit::ONE];
static ASYNC_RST: [SigBit; 1] = [SigBit::ZERO];

impl Ports for Mem {
    fn for_each_port<'a>(&'a self, f: &mut dyn FnMut(&'a str, Dir, &'a [SigBit])) {
        let one = std::slice::from_ref;
        for r in &self.rd {
            f(
                "RD_CLK",
                Dir::In,
                r.sync.as_ref().map_or(&ASYNC_CLK[..], |s| one(&s.clk.sig)),
            );
        }
        for r in &self.rd {
            f(
                "RD_EN",
                Dir::In,
                r.sync.as_ref().map_or(&ASYNC_EN[..], |s| one(&s.en)),
            );
        }
        for r in &self.rd {
            f(
                "RD_ARST",
                Dir::In,
                r.sync.as_ref().map_or(&ASYNC_RST[..], |s| one(&s.arst)),
            );
        }
        for r in &self.rd {
            f(
                "RD_SRST",
                Dir::In,
                r.sync.as_ref().map_or(&ASYNC_RST[..], |s| one(&s.srst)),
            );
        }
        for r in &self.rd {
            f("RD_ADDR", Dir::In, &r.addr);
        }
        for r in &self.rd {
            f("RD_DATA", Dir::Out, &r.data);
        }
        for w in &self.wr {
            f(
                "WR_CLK",
                Dir::In,
                w.clk.as_ref().map_or(&ASYNC_CLK[..], |c| one(&c.sig)),
            );
        }
        for w in &self.wr {
            f("WR_EN", Dir::In, &w.en);
        }
        for w in &self.wr {
            f("WR_ADDR", Dir::In, &w.addr);
        }
        for w in &self.wr {
            f("WR_DATA", Dir::In, &w.data);
        }
        // A memory without ports of a kind still has the (empty) ports.
        if self.rd.is_empty() {
            for name in [
                "RD_CLK", "RD_EN", "RD_ARST", "RD_SRST", "RD_ADDR", "RD_DATA",
            ] {
                let dir = if name == "RD_DATA" { Dir::Out } else { Dir::In };
                f(name, dir, &[]);
            }
        }
        if self.wr.is_empty() {
            for name in ["WR_CLK", "WR_EN", "WR_ADDR", "WR_DATA"] {
                f(name, Dir::In, &[]);
            }
        }
    }

    fn for_each_bit_mut(&mut self, f: &mut dyn FnMut(&mut SigBit)) {
        for r in &mut self.rd {
            if let Some(s) = &mut r.sync {
                for b in [&mut s.clk.sig, &mut s.en, &mut s.arst, &mut s.srst] {
                    f(b);
                }
            }
            r.addr.iter_mut().chain(&mut r.data).for_each(&mut *f);
        }
        for w in &mut self.wr {
            if let Some(c) = &mut w.clk {
                f(&mut c.sig);
            }
            w.en.iter_mut()
                .chain(&mut w.addr)
                .chain(&mut w.data)
                .for_each(&mut *f);
        }
    }
}

/// Bit `i` of a per-port parameter.
fn bit(c: &Const, i: usize) -> bool {
    c.0[i] == State::One
}

fn flags(bits: impl IntoIterator<Item = bool>) -> Const {
    let c = Const(bits.into_iter().map(State::from).collect());
    // yosys writes a per-port parameter with no ports as a single 0.
    if c.is_empty() {
        Const(vec![State::Zero])
    } else {
        c
    }
}

fn concat<'a>(parts: impl IntoIterator<Item = &'a Const>) -> Const {
    let c = Const(
        parts
            .into_iter()
            .flat_map(|c| c.0.iter().copied())
            .collect(),
    );
    if c.is_empty() {
        Const(vec![State::Zero])
    } else {
        c
    }
}

impl Mem {
    pub(crate) fn decode(d: &mut Decoder) -> Result<Mem, CellError> {
        let memid = match d.param("MEMID")? {
            crate::ParamValue::Str(s) => s,
            crate::ParamValue::Bits(_) => return Err(d.error("MEMID is not a string")),
        };
        let size = d.uint("SIZE")?;
        let offset = d.int("OFFSET")?;
        let width = d.uint("WIDTH")?;
        let abits = d.uint("ABITS")?;
        let init = d.konst("INIT")?;
        let nr = d.uint("RD_PORTS")?;
        let nw = d.uint("WR_PORTS")?;

        let rd_clk = d.port_w("RD_CLK", Dir::In, nr)?;
        let rd_en = d.port_w("RD_EN", Dir::In, nr)?;
        let rd_arst = d.port_w("RD_ARST", Dir::In, nr)?;
        let rd_srst = d.port_w("RD_SRST", Dir::In, nr)?;
        let rd_addr = d.port_w("RD_ADDR", Dir::In, nr * abits)?;
        let rd_data = d.port_w("RD_DATA", Dir::Out, nr * width)?;
        let rd_wide = d.konst_w("RD_WIDE_CONTINUATION", nr)?;
        let rd_clk_enable = d.konst_w("RD_CLK_ENABLE", nr)?;
        let rd_clk_polarity = d.konst_w("RD_CLK_POLARITY", nr)?;
        let rd_transparency = d.konst_w("RD_TRANSPARENCY_MASK", nr * nw)?;
        let rd_collision_x = d.konst_w("RD_COLLISION_X_MASK", nr * nw)?;
        let rd_ce_over_srst = d.konst_w("RD_CE_OVER_SRST", nr)?;
        let rd_init = d.konst_w("RD_INIT_VALUE", nr * width)?;
        let rd_arst_value = d.konst_w("RD_ARST_VALUE", nr * width)?;
        let rd_srst_value = d.konst_w("RD_SRST_VALUE", nr * width)?;

        let wr_clk = d.port_w("WR_CLK", Dir::In, nw)?;
        let wr_en = d.port_w("WR_EN", Dir::In, nw * width)?;
        let wr_addr = d.port_w("WR_ADDR", Dir::In, nw * abits)?;
        let wr_data = d.port_w("WR_DATA", Dir::In, nw * width)?;
        let wr_wide = d.konst_w("WR_WIDE_CONTINUATION", nw)?;
        let wr_clk_enable = d.konst_w("WR_CLK_ENABLE", nw)?;
        let wr_clk_polarity = d.konst_w("WR_CLK_POLARITY", nw)?;
        let wr_priority = d.konst_w("WR_PRIORITY_MASK", nw * nw)?;

        let word = |c: &Const, i: usize| Const(c.0[i * width..(i + 1) * width].to_vec());
        let rd = (0..nr)
            .map(|i| MemRd {
                addr: rd_addr[i * abits..(i + 1) * abits].to_vec(),
                data: rd_data[i * width..(i + 1) * width].to_vec(),
                wide_continuation: bit(&rd_wide, i),
                sync: bit(&rd_clk_enable, i).then(|| MemRdSync {
                    clk: Ctrl {
                        sig: rd_clk[i],
                        polarity: Polarity::from_bool(bit(&rd_clk_polarity, i)),
                    },
                    en: rd_en[i],
                    arst: rd_arst[i],
                    srst: rd_srst[i],
                    ce_over_srst: bit(&rd_ce_over_srst, i),
                    init_value: word(&rd_init, i),
                    arst_value: word(&rd_arst_value, i),
                    srst_value: word(&rd_srst_value, i),
                    transparent: (0..nw).map(|j| bit(&rd_transparency, i * nw + j)).collect(),
                    collision_x: (0..nw).map(|j| bit(&rd_collision_x, i * nw + j)).collect(),
                }),
            })
            .collect();
        let wr = (0..nw)
            .map(|i| MemWr {
                clk: bit(&wr_clk_enable, i).then(|| Ctrl {
                    sig: wr_clk[i],
                    polarity: Polarity::from_bool(bit(&wr_clk_polarity, i)),
                }),
                en: wr_en[i * width..(i + 1) * width].to_vec(),
                addr: wr_addr[i * abits..(i + 1) * abits].to_vec(),
                data: wr_data[i * width..(i + 1) * width].to_vec(),
                wide_continuation: bit(&wr_wide, i),
                priority: (0..nw).map(|j| bit(&wr_priority, i * nw + j)).collect(),
            })
            .collect();
        Ok(Mem {
            memid,
            size,
            offset,
            width,
            abits,
            init,
            rd,
            wr,
        })
    }

    pub(crate) fn params(&self, p: &mut Params) {
        let x = Const::filled(State::X, self.width);
        let rd_sync = |f: &dyn Fn(&MemRdSync) -> bool| {
            flags(self.rd.iter().map(|r| r.sync.as_ref().is_some_and(f)))
        };
        let rd_value = |f: fn(&MemRdSync) -> &Const| {
            concat(self.rd.iter().map(|r| r.sync.as_ref().map_or(&x, f)))
        };
        let rd_mask = |f: fn(&MemRdSync) -> &Vec<bool>| {
            flags(self.rd.iter().flat_map(|r| match &r.sync {
                Some(s) => f(s).clone(),
                None => vec![false; self.wr.len()],
            }))
        };
        p.0.push(("MEMID", crate::ParamValue::Str(self.memid.clone())));
        p.int("SIZE", self.size)
            .konst("OFFSET", Const::from_int(self.offset, 32))
            .int("WIDTH", self.width)
            .int("ABITS", self.abits)
            .konst("INIT", self.init.clone())
            .int("RD_PORTS", self.rd.len())
            .konst(
                "RD_WIDE_CONTINUATION",
                flags(self.rd.iter().map(|r| r.wide_continuation)),
            )
            .konst(
                "RD_CLK_ENABLE",
                flags(self.rd.iter().map(|r| r.sync.is_some())),
            )
            .konst("RD_CLK_POLARITY", rd_sync(&|s| s.clk.polarity.is_pos()))
            .konst("RD_TRANSPARENCY_MASK", rd_mask(|s| &s.transparent))
            .konst("RD_COLLISION_X_MASK", rd_mask(|s| &s.collision_x))
            .konst("RD_CE_OVER_SRST", rd_sync(&|s| s.ce_over_srst))
            .konst("RD_INIT_VALUE", rd_value(|s| &s.init_value))
            .konst("RD_ARST_VALUE", rd_value(|s| &s.arst_value))
            .konst("RD_SRST_VALUE", rd_value(|s| &s.srst_value))
            .int("WR_PORTS", self.wr.len())
            .konst(
                "WR_WIDE_CONTINUATION",
                flags(self.wr.iter().map(|w| w.wide_continuation)),
            )
            .konst(
                "WR_CLK_ENABLE",
                flags(self.wr.iter().map(|w| w.clk.is_some())),
            )
            .konst(
                "WR_CLK_POLARITY",
                flags(
                    self.wr
                        .iter()
                        .map(|w| w.clk.is_some_and(|c| c.polarity.is_pos())),
                ),
            )
            .konst(
                "WR_PRIORITY_MASK",
                flags(self.wr.iter().flat_map(|w| w.priority.iter().copied())),
            );
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        let nw = self.wr.len();
        for (i, r) in self.rd.iter().enumerate() {
            let at = |e: String| format!("read port {i}: {e}");
            expect_width("addr", &r.addr, self.abits).map_err(at)?;
            expect_width("data", &r.data, self.width).map_err(at)?;
            if let Some(s) = &r.sync {
                for (name, c) in [
                    ("init value", &s.init_value),
                    ("arst value", &s.arst_value),
                    ("srst value", &s.srst_value),
                ] {
                    if c.len() != self.width {
                        return Err(at(format!("{name} is {} bits", c.len())));
                    }
                }
                if s.transparent.len() != nw || s.collision_x.len() != nw {
                    return Err(at(format!("masks aren't one per write port ({nw})")));
                }
            }
        }
        for (i, w) in self.wr.iter().enumerate() {
            let at = |e: String| format!("write port {i}: {e}");
            expect_width("addr", &w.addr, self.abits).map_err(at)?;
            expect_width("data", &w.data, self.width).map_err(at)?;
            expect_width("en", &w.en, self.width).map_err(at)?;
            if w.priority.len() != nw {
                return Err(at(format!("priority isn't one per write port ({nw})")));
            }
        }
        Ok(())
    }
}
