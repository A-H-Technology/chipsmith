//! Bench: reach the Zynq's processor side through its ARM DAP over the
//! Digilent FT2232, with no code on the ARM: the AHB-AP's IDR, the PS
//! IDCODE and the boot mode the board's jumpers select.
//! `cargo run --release -p chipsmith-jtag --example zynq_dap`
use anyhow::Context;
use chipsmith_jtag::{
    adiv5::{Dap, MemAp},
    ftdi::Ftdi,
    xc7,
};

/// UG585 appendix B, SLCR.
const PSS_IDCODE: u32 = 0xf800_0530;
const BOOT_MODE: u32 = 0xf800_025c;

fn main() -> anyhow::Result<()> {
    let mut cable = Ftdi::open()?;
    let chain = xc7::scan(&mut cable)?;
    for (i, t) in chain.iter().enumerate() {
        println!("TAP {i}: IDCODE {:#010x}, IR {}", t.idcode, t.ir_len);
    }
    let target = chain
        .iter()
        .position(|t| t.ir_len == 4)
        .context("no ARM DAP on the chain")?;
    let mut dap = Dap::new(&mut cable, &chain, target)?;
    let mut ahb = MemAp::new(&mut dap, 0)?;
    println!("AP 0 IDR {:#010x}", ahb.idr()?);
    println!("PSS_IDCODE {:#010x}", ahb.read32(PSS_IDCODE)?);
    let mode = ahb.read32(BOOT_MODE)?;
    // UG585 table 6-4: the mode pins MIO[5:3] as BOOT_MODE[2:0].
    let name = match mode & 7 {
        0b000 => "JTAG",
        0b001 => "Quad-SPI",
        0b010 => "NOR",
        0b100 => "NAND",
        0b101 => "SD card",
        _ => "reserved",
    };
    println!("BOOT_MODE {mode:#010x}: {name}");
    Ok(())
}
