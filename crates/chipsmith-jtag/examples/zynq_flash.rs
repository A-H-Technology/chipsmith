//! Bench: the Zybo's QSPI flash from the host, through the Zynq's DAP over
//! the Digilent FT2232, with no code on the ARM.
//! `cargo run --release -p chipsmith-jtag --example zynq_flash -- dump FILE [BYTES]`
//! `... -- write FILE [OFFSET]`: erase, program, read back and compare.
//! `... -- reset`: rerun the BootROM (a PS reset) and report the PL's state.
use std::time::Instant;

use anyhow::{bail, Context};
use chipsmith_jtag::{
    adiv5::{Dap, MemAp},
    ftdi::Ftdi,
    xc7, zynq_ps,
    zynq_qspi::{self, Qspi},
};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cable = Ftdi::open()?;
    let chain = xc7::scan(&mut cable)?;
    let target = chain
        .iter()
        .position(|t| t.ir_len == 4)
        .context("no ARM DAP on the chain")?;
    let mut dap = Dap::new(&mut cable, &chain, target)?;
    let mut ahb = MemAp::new(&mut dap, 0)?;
    let mut qspi = Qspi::new(&mut ahb)?;
    let id = qspi.read_id()?;
    println!(
        "flash JEDEC ID {:02x} {:02x} {:02x}, sector architecture {:02x}, SR1 {:#04x}",
        id[0],
        id[1],
        id[2],
        id[4],
        qspi.status()?
    );
    // JEDEC capacity byte: 2^n bytes (0x18 = 16 MiB).
    let size = 1usize << id[2];

    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["dump", file, rest @ ..] => {
            let bytes = match rest {
                [] => size,
                [n] => n.parse()?,
                _ => bail!("dump FILE [BYTES]"),
            };
            qspi.into_linear()?;
            let start = Instant::now();
            let data = read_linear(&mut ahb, 0, bytes)?;
            std::fs::write(file, &data)?;
            println!(
                "{} bytes to {file} in {:.1} s",
                data.len(),
                start.elapsed().as_secs_f64()
            );
        }
        ["write", file, rest @ ..] => {
            let offset = match rest {
                [] => 0,
                [o] => u32::from_str_radix(o.trim_start_matches("0x"), 16)?,
                _ => bail!("write FILE [OFFSET]"),
            };
            let mut data = std::fs::read(file)?;
            // Whole words; erased flash reads 0xff, so padding with it is a no-op.
            data.resize(data.len().next_multiple_of(4), 0xff);
            let start = Instant::now();
            qspi.write(offset, &data, |done| {
                let s = start.elapsed().as_secs_f64();
                eprint!(
                    "\r{} KiB, {:.1} KiB/s",
                    done / 1024,
                    done as f64 / 1024.0 / s
                );
            })?;
            eprintln!();
            println!(
                "programmed {} bytes in {:.1} s",
                data.len(),
                start.elapsed().as_secs_f64()
            );
            qspi.into_linear()?;
            let back = read_linear(&mut ahb, offset, data.len())?;
            match back.iter().zip(&data).position(|(a, b)| a != b) {
                None => println!("read back {} bytes: identical", back.len()),
                Some(k) => bail!(
                    "read back differs at {:#x}: {:#04x}, wrote {:#04x}",
                    offset as usize + k,
                    back[k],
                    data[k]
                ),
            }
        }
        ["reset"] => {
            println!("boot mode {:?}", zynq_ps::boot_mode(&mut ahb)?);
            zynq_ps::soft_reset(&mut ahb)?;
            let start = Instant::now();
            let sts = zynq_ps::wait_configured(
                &mut cable,
                &chain,
                target,
                std::time::Duration::from_secs(15),
            )?;
            println!(
                "PL configured {:.1} s after the reset (devcfg INT_STS {sts:#010x})",
                start.elapsed().as_secs_f64()
            );
        }
        _ => bail!("usage: zynq_flash dump FILE [BYTES] | write FILE [OFFSET] | reset"),
    }
    Ok(())
}

fn read_linear(
    ahb: &mut MemAp<'_, '_, Ftdi>,
    offset: u32,
    bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let start = Instant::now();
    let data = zynq_qspi::read_linear(ahb, offset, bytes, |done| {
        let s = start.elapsed().as_secs_f64();
        eprint!(
            "\rread {} KiB, {:.0} KiB/s",
            done / 1024,
            done as f64 / 1024.0 / s
        );
    })?;
    eprintln!();
    Ok(data)
}
