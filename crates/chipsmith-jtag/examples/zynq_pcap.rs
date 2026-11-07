//! Bench: configure the Zynq's PL from its QSPI flash through the PCAP,
//! every register written from the host over the DAP: the sequence a
//! first-stage loader runs, tried before it's ARM code.
//! `cargo run --release -p chipsmith-jtag --example zynq_pcap -- OFFSET BYTES`
//! (a byte-swapped `.bin` already in flash at OFFSET, hex).
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use chipsmith_jtag::{
    adiv5::{Dap, MemAp},
    ftdi::Ftdi,
    xc7,
    zynq_qspi::{Qspi, LINEAR_BASE},
};

// devcfg (UG585 appendix B.16).
const DEVCFG: u32 = 0xf800_7000;
const CTRL: u32 = DEVCFG;
const INT_STS: u32 = DEVCFG + 0x0c;
const STATUS: u32 = DEVCFG + 0x14;
const DMA_SRC_ADDR: u32 = DEVCFG + 0x18;
const DMA_DST_ADDR: u32 = DEVCFG + 0x1c;
const DMA_SRC_LEN: u32 = DEVCFG + 0x20;
const DMA_DST_LEN: u32 = DEVCFG + 0x24;
const UNLOCK: u32 = DEVCFG + 0x34;
const MCTRL: u32 = DEVCFG + 0x80;

const PCFG_PROG_B: u32 = 1 << 30;
const PCAP_PR: u32 = 1 << 27;
const PCAP_MODE: u32 = 1 << 26;
const PCAP_LPBK: u32 = 1 << 4;
const PCFG_INIT: u32 = 1 << 4;
const PCFG_DONE_INT: u32 = 1 << 2;
const DMA_DONE_INT: u32 = 1 << 13;
/// AXI/DMA/PCAP error bits of INT_STS (UG585 B.16).
const ERRORS: u32 = 0x00f4_8860;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [offset, bytes] = args.as_slice() else {
        bail!("usage: zynq_pcap OFFSET BYTES");
    };
    let offset = u32::from_str_radix(offset.trim_start_matches("0x"), 16)?;
    let bytes: u32 = bytes.parse()?;
    ensure!(bytes.is_multiple_of(4), "a .bin is whole words");

    let mut cable = Ftdi::open()?;
    let chain = xc7::scan(&mut cable)?;
    let target = chain
        .iter()
        .position(|t| t.ir_len == 4)
        .context("no ARM DAP")?;
    let mut dap = Dap::new(&mut cable, &chain, target)?;
    let mut ahb = MemAp::new(&mut dap, 0)?;
    Qspi::new(&mut ahb)?.into_linear()?;

    let wait = |ahb: &mut MemAp<'_, '_, Ftdi>,
                reg: u32,
                mask: u32,
                want: u32,
                what: &str|
     -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let v = ahb.read32(reg)?;
            if v & mask == want {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "{what}: {reg:#010x} = {v:#010x}");
        }
    };

    ahb.write32(UNLOCK, 0x757b_df0d)?;
    ahb.write32(INT_STS, u32::MAX)?;
    let ctrl = ahb.read32(CTRL)? | PCAP_PR | PCAP_MODE;
    ahb.write32(CTRL, ctrl)?;
    let mctrl = ahb.read32(MCTRL)?;
    ahb.write32(MCTRL, mctrl & !PCAP_LPBK)?;
    // Pulse PROG_B: clears the PL, then INIT rises when it's ready.
    ahb.write32(CTRL, ctrl | PCFG_PROG_B)?;
    ahb.write32(CTRL, ctrl & !PCFG_PROG_B)?;
    wait(&mut ahb, STATUS, PCFG_INIT, 0, "INIT never fell")?;
    ahb.write32(CTRL, ctrl | PCFG_PROG_B)?;
    wait(&mut ahb, STATUS, PCFG_INIT, PCFG_INIT, "INIT never rose")?;
    ahb.write32(INT_STS, u32::MAX)?;

    let start = Instant::now();
    // Bit 0 of both addresses marks the last transfer of a DMA, which ends
    // the PCAP's configuration (UG585 6.4.2).
    ahb.write32(DMA_SRC_ADDR, (LINEAR_BASE + offset) | 1)?;
    ahb.write32(DMA_DST_ADDR, u32::MAX)?;
    ahb.write32(DMA_SRC_LEN, bytes / 4)?;
    ahb.write32(DMA_DST_LEN, bytes / 4)?;
    wait(
        &mut ahb,
        INT_STS,
        DMA_DONE_INT | ERRORS,
        DMA_DONE_INT,
        "DMA",
    )?;
    wait(
        &mut ahb,
        INT_STS,
        PCFG_DONE_INT | ERRORS,
        PCFG_DONE_INT,
        "DONE",
    )?;
    println!(
        "PL configured from flash {:#x}, {bytes} bytes, in {:.0} ms (INT_STS {:#010x})",
        LINEAR_BASE + offset,
        start.elapsed().as_secs_f64() * 1e3,
        ahb.read32(INT_STS)?
    );
    Ok(())
}
