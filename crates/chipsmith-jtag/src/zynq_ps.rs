//! The Zynq's processor side as the host reaches it through the DAP: the
//! boot mode, configuring the PL from QSPI flash through the PCAP (the
//! sequence chipsmith's first-stage loader runs, chipsmith-xc7's
//! `zynq_loader.S`), and a PS reset that reruns the BootROM.

use anyhow::{ensure, Result};
use std::time::{Duration, Instant};

use super::adiv5::MemAp;
use super::zynq_qspi::LINEAR_BASE;
use super::Cable;

const SLCR_UNLOCK: u32 = 0xf800_0008;
const SLCR_UNLOCK_KEY: u32 = 0xdf0d;
const PSS_RST_CTRL: u32 = 0xf800_0200;
const BOOT_MODE: u32 = 0xf800_025c;

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
pub const PCFG_DONE_INT: u32 = 1 << 2;
const DMA_DONE_INT: u32 = 1 << 13;
/// AXI, DMA and PCAP error bits of INT_STS (UG585 B.16).
pub const INT_ERRORS: u32 = 0x00f4_8860;

/// The boot device the mode pins (MIO 5:3) selected at power-on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    Jtag,
    Qspi,
    Nor,
    Nand,
    Sd,
    Reserved(u8),
}

pub fn boot_mode<C: Cable>(ahb: &mut MemAp<'_, '_, C>) -> Result<BootMode> {
    // UG585 table 6-4.
    Ok(match ahb.read32(BOOT_MODE)? & 7 {
        0b000 => BootMode::Jtag,
        0b001 => BootMode::Qspi,
        0b010 => BootMode::Nor,
        0b100 => BootMode::Nand,
        0b101 => BootMode::Sd,
        m => BootMode::Reserved(m as u8),
    })
}

/// devcfg's INT_STS: after a configuration, PCFG_DONE_INT set and no error
/// bits mean the PL took its bitstream.
pub fn devcfg_status<C: Cable>(ahb: &mut MemAp<'_, '_, C>) -> Result<u32> {
    ahb.read32(INT_STS)
}

fn wait<C: Cable>(
    ahb: &mut MemAp<'_, '_, C>,
    reg: u32,
    mask: u32,
    want: u32,
    what: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let v = ahb.read32(reg)?;
        if v & mask == want {
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "{what}: {reg:#010x} = {v:#010x}");
    }
}

/// Configure the PL from a byte-swapped `.bin` at `offset` in QSPI flash,
/// which must be in linear mode (`Qspi::into_linear`).
pub fn pcap_from_flash<C: Cable>(
    ahb: &mut MemAp<'_, '_, C>,
    offset: u32,
    bytes: u32,
) -> Result<()> {
    ensure!(bytes.is_multiple_of(4), "a .bin is whole words");
    ahb.write32(UNLOCK, 0x757b_df0d)?;
    ahb.write32(INT_STS, u32::MAX)?;
    let ctrl = ahb.read32(CTRL)? | PCAP_PR | PCAP_MODE;
    ahb.write32(CTRL, ctrl)?;
    let mctrl = ahb.read32(MCTRL)?;
    ahb.write32(MCTRL, mctrl & !PCAP_LPBK)?;
    // Pulse PROG_B: clears the PL, then INIT rises when it's ready.
    ahb.write32(CTRL, ctrl | PCFG_PROG_B)?;
    ahb.write32(CTRL, ctrl & !PCFG_PROG_B)?;
    wait(ahb, STATUS, PCFG_INIT, 0, "INIT never fell")?;
    ahb.write32(CTRL, ctrl | PCFG_PROG_B)?;
    wait(ahb, STATUS, PCFG_INIT, PCFG_INIT, "INIT never rose")?;
    ahb.write32(INT_STS, u32::MAX)?;
    // Bit 0 of both addresses marks the DMA's last transfer, which ends the
    // PCAP's configuration (UG585 6.4.2).
    ahb.write32(DMA_SRC_ADDR, (LINEAR_BASE + offset) | 1)?;
    ahb.write32(DMA_DST_ADDR, u32::MAX)?;
    ahb.write32(DMA_SRC_LEN, bytes / 4)?;
    ahb.write32(DMA_DST_LEN, bytes / 4)?;
    wait(
        ahb,
        INT_STS,
        DMA_DONE_INT | INT_ERRORS,
        DMA_DONE_INT,
        "PCAP DMA",
    )?;
    wait(
        ahb,
        INT_STS,
        PCFG_DONE_INT | INT_ERRORS,
        PCFG_DONE_INT,
        "PL DONE",
    )
}

/// A PS system reset (SLCR PSS_RST_CTRL): the BootROM runs again from the
/// boot mode latched at power-on. The DAP's AHB-AP is reset with the rest
/// of the PS, so the write itself may not be acknowledged, and the caller
/// opens a new `Dap` afterwards.
pub fn soft_reset<C: Cable>(ahb: &mut MemAp<'_, '_, C>) -> Result<()> {
    ahb.write32(SLCR_UNLOCK, SLCR_UNLOCK_KEY)?;
    // Errors here are the reset cutting the access short.
    let _ = ahb.write32(PSS_RST_CTRL, 1);
    Ok(())
}

/// After `soft_reset`: wait for the BootROM and the loader it starts to
/// configure the PL, reconnecting to `chain[dap]` until devcfg answers.
/// Right after the reset the AHB-AP's accesses fault (seen on the Zybo:
/// still at 2 s on one try, fine on the next), so faults retry until
/// `timeout`. Returns devcfg's INT_STS once PCFG_DONE is set.
pub fn wait_configured<C: Cable>(
    cable: &mut C,
    chain: &[super::xc7::Tap],
    dap: usize,
    timeout: Duration,
) -> Result<u32> {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        let status = (|| {
            let mut d = super::adiv5::Dap::new(cable, chain, dap)?;
            let mut ahb = MemAp::new(&mut d, 0)?;
            devcfg_status(&mut ahb)
        })();
        match status {
            Ok(s) if s & INT_ERRORS != 0 => {
                anyhow::bail!("the PL's configuration failed (devcfg INT_STS {s:#010x})")
            }
            Ok(s) if s & PCFG_DONE_INT != 0 => return Ok(s),
            Ok(s) => last = Some(format!("INT_STS {s:#010x}")),
            Err(e) => last = Some(format!("{e:#}")),
        }
    }
    anyhow::bail!(
        "the PL wasn't configured {timeout:?} after the reset (last: {})",
        last.unwrap_or_default()
    )
}
