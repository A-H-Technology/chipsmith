//! Bench: configure the Zynq's PL from its QSPI flash through the PCAP,
//! every register written from the host over the DAP: the sequence
//! chipsmith's first-stage loader runs.
//! `cargo run --release -p chipsmith-jtag --example zynq_pcap -- OFFSET BYTES`
//! (a byte-swapped `.bin` already in flash at OFFSET, hex).
use std::time::Instant;

use anyhow::{bail, Context};
use chipsmith_jtag::{
    adiv5::{Dap, MemAp},
    ftdi::Ftdi,
    xc7, zynq_ps,
    zynq_qspi::{Qspi, LINEAR_BASE},
};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [offset, bytes] = args.as_slice() else {
        bail!("usage: zynq_pcap OFFSET BYTES");
    };
    let offset = u32::from_str_radix(offset.trim_start_matches("0x"), 16)?;
    let bytes: u32 = bytes.parse()?;

    let mut cable = Ftdi::open()?;
    let chain = xc7::scan(&mut cable)?;
    let target = chain
        .iter()
        .position(|t| t.ir_len == 4)
        .context("no ARM DAP")?;
    let mut dap = Dap::new(&mut cable, &chain, target)?;
    let mut ahb = MemAp::new(&mut dap, 0)?;
    Qspi::new(&mut ahb)?.into_linear()?;
    let start = Instant::now();
    zynq_ps::pcap_from_flash(&mut ahb, offset, bytes)?;
    println!(
        "PL configured from flash {:#x}, {bytes} bytes, in {:.0} ms (INT_STS {:#010x})",
        LINEAR_BASE + offset,
        start.elapsed().as_secs_f64() * 1e3,
        zynq_ps::devcfg_status(&mut ahb)?
    );
    Ok(())
}
