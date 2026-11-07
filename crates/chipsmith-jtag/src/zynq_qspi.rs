//! The Zynq PS's QSPI controller (UG585 chapter 12, registers in appendix
//! B.24) driven from the host through the DAP, in I/O mode: the host is the
//! only master, so the ARM needs no code. Single-bit SPI only; the flash's
//! clock and MIO pins must already be set up (the BootROM and U-Boot both
//! leave them so; `check_ready` says if not).

use anyhow::{ensure, Context, Result};
use std::time::{Duration, Instant};

use super::adiv5::MemAp;
use super::Cable;

const BASE: u32 = 0xe000_d000;
const CONFIG: u32 = BASE;
const INTR_STATUS: u32 = BASE + 0x04;
const ENABLE: u32 = BASE + 0x14;
const TXD0: u32 = BASE + 0x1c;
const RX_DATA: u32 = BASE + 0x20;
const LQSPI_CFG: u32 = BASE + 0xa0;
const MOD_ID: u32 = BASE + 0xfc;

const APER_CLK_CTRL: u32 = 0xf800_012c;
const LQSPI_CPU_1XCLKACT: u32 = 1 << 23;
const LQSPI_CLK_CTRL: u32 = 0xf800_014c;
const MIO_PIN_01: u32 = 0xf800_0704;

/// TX FIFO below its threshold, which is 1: empty.
const TX_EMPTY: u32 = 1 << 2;
/// The RX FIFO's depth: reading this many words empties it, whatever is in it.
const RX_FIFO_WORDS: usize = 63;

/// Flash mode, holdb driven, manual CS, auto start, CS deasserted,
/// 32-bit FIFO words, SCK = ref/8, master (UG585 B.24 Config_reg).
/// /8 keeps SCK at 25 MHz from the 200 MHz reference: far under any flash's
/// limit, and the host's scans are the bottleneck anyway.
const CONFIG_OWNED: u32 = 1 << 31 | 1 << 19 | 1 << 16 | 1 << 15 | 1 << 14 | 1 << 10 | 0xff;
/// Bit 17 is reserved but resets to 1 and must stay 1: with it cleared
/// (as an earlier version of this wrote) nothing reaches the flash.
const CONFIG_IDLE: u32 =
    1 << 31 | 1 << 19 | 1 << 17 | 1 << 14 | 1 << 10 | 0b11 << 6 | 0b010 << 3 | 1;
const PCS: u32 = 1 << 10;

pub struct Qspi<'a, 'd, 'c, C: Cable> {
    ahb: &'a mut MemAp<'d, 'c, C>,
    /// Config with CS deasserted; reserved bits as the controller had them.
    idle: u32,
}

impl<'a, 'd, 'c, C: Cable> Qspi<'a, 'd, 'c, C> {
    pub fn new(ahb: &'a mut MemAp<'d, 'c, C>) -> Result<Self> {
        let mut q = Qspi { ahb, idle: 0 };
        q.check_ready()?;
        q.ahb.write32(ENABLE, 0)?;
        // Linear mode would hand the bus to the AXI window; I/O mode is ours.
        q.ahb.write32(LQSPI_CFG, 0)?;
        q.idle = q.ahb.read32(CONFIG)? & !CONFIG_OWNED | CONFIG_IDLE;
        q.ahb.write32(CONFIG, q.idle)?;
        q.ahb.write32(ENABLE, 1)?;
        q.drain_rx()?;
        Ok(q)
    }

    fn check_ready(&mut self) -> Result<()> {
        let id = self.ahb.read32(MOD_ID)?;
        ensure!(
            id == 0x0109_0101,
            "QSPI module ID {id:#010x}, not the Zynq's"
        );
        let aper = self.ahb.read32(APER_CLK_CTRL)?;
        ensure!(
            aper & LQSPI_CPU_1XCLKACT != 0,
            "the QSPI controller's clock is gated (APER_CLK_CTRL {aper:#010x})"
        );
        let clk = self.ahb.read32(LQSPI_CLK_CTRL)?;
        ensure!(
            clk & 1 != 0,
            "the QSPI reference clock is off (LQSPI_CLK_CTRL {clk:#010x})"
        );
        for pin in 1..=6 {
            let mio = self.ahb.read32(MIO_PIN_01 + 4 * (pin - 1))?;
            // L0_SEL, bit 1: the pin is the QSPI's.
            ensure!(
                mio & 0xfe == 0x02,
                "MIO {pin} isn't routed to QSPI ({mio:#010x})"
            );
        }
        Ok(())
    }

    /// The RX FIFO's not-empty flag (Intr_status bit 4) never sets when
    /// read over the DAP, even with words in the FIFO (seen on the Zybo
    /// 2026-11-07), so nothing here trusts it. Reading an empty RX FIFO is
    /// a bus error instead, which is what ends the drain; after that each
    /// word sent is read back exactly once.
    fn drain_rx(&mut self) -> Result<()> {
        for _ in 0..=RX_FIFO_WORDS {
            if self.ahb.try_read32(RX_DATA)?.is_none() {
                return Ok(());
            }
        }
        anyhow::bail!("the QSPI RX FIFO never emptied")
    }

    /// One chip-select-framed transfer: `tx` out, as many bytes back.
    /// Whole 4-byte words only, so every word in the FIFO is a full one.
    pub fn transfer(&mut self, tx: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            tx.len().is_multiple_of(4),
            "transfer of {} bytes, not whole words",
            tx.len()
        );
        self.ahb.write32(CONFIG, self.idle & !PCS)?;
        let mut rx = Vec::with_capacity(tx.len());
        for word in tx.as_chunks::<4>().0 {
            // The FIFO sends bits 7:0 first.
            self.ahb.write32(TXD0, u32::from_le_bytes(*word))?;
            // Empty means the word is in the shift register; its 32 SCKs
            // (1.3 us) are over long before the next DAP access lands, which
            // takes a USB round trip.
            let deadline = Instant::now() + Duration::from_millis(100);
            while self.ahb.read32(INTR_STATUS)? & TX_EMPTY == 0 {
                ensure!(Instant::now() < deadline, "QSPI TX FIFO never drained");
            }
            rx.extend_from_slice(&self.ahb.read32(RX_DATA)?.to_le_bytes());
        }
        self.ahb.write32(CONFIG, self.idle)?;
        Ok(rx)
    }

    /// JEDEC RDID (0x9f): manufacturer, memory type, capacity.
    pub fn read_id(&mut self) -> Result<[u8; 3]> {
        let rx = self.transfer(&[0x9f, 0, 0, 0])?;
        rx[1..4].try_into().context("short RDID")
    }
}
