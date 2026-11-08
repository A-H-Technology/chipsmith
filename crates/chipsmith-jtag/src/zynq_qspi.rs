//! The Zynq PS's QSPI controller (UG585 chapter 12, registers in appendix
//! B.24) driven from the host through the DAP, in I/O mode: the host is the
//! only master, so the ARM needs no code. Single-bit SPI only; the flash's
//! clock and MIO pins must already be set up (the BootROM and U-Boot both
//! leave them so; `check_ready` says if not).

use anyhow::{ensure, Context, Result};
use std::time::{Duration, Instant};

use super::adiv5::{Access, MemAp};
use super::Cable;

const BASE: u32 = 0xe000_d000;
const CONFIG: u32 = BASE;
const ENABLE: u32 = BASE + 0x14;
const TXD0: u32 = BASE + 0x1c;
const TXD1: u32 = BASE + 0x80;
const RX_DATA: u32 = BASE + 0x20;
const LQSPI_CFG: u32 = BASE + 0xa0;
const MOD_ID: u32 = BASE + 0xfc;
/// Where linear mode maps the flash (UG585 table 4-1).
pub const LINEAR_BASE: u32 = 0xfc00_0000;
const LQ_MODE: u32 = 1 << 31;
const READ: u32 = 0x03;

const APER_CLK_CTRL: u32 = 0xf800_012c;
const LQSPI_CPU_1XCLKACT: u32 = 1 << 23;
const LQSPI_CLK_CTRL: u32 = 0xf800_014c;
const MIO_PIN_01: u32 = 0xf800_0704;
const LPBK_DLY_ADJ: u32 = BASE + 0x38;
const SLCR_UNLOCK: u32 = 0xf800_0008;
const IO_PLL_CTRL: u32 = 0xf800_0108;
const PLL_STATUS: u32 = 0xf800_010c;
const IO_PLL_LOCK: u32 = 1 << 2;
/// QSPI on MIO 1-6 as U-Boot leaves it: LVCMOS33, fast slew, L0_SEL.
/// The BootROM leaves them slow.
const MIO_QSPI: u32 = 0x702;
/// The highest PS_CLK a Zynq board feeds (UG585: 30-60 MHz); the clock is
/// set for this so it is never too fast, whatever the board's crystal.
const PS_CLK_MAX_HZ: u64 = 60_000_000;
const LQSPI_REF_MAX_HZ: u64 = 200_000_000;

/// The RX FIFO's depth: reading this many words empties it, whatever is in it.
const RX_FIFO_WORDS: usize = 63;
/// TX words per batch in a frame: well under the RX FIFO's depth, since
/// every word sent leaves one there until the batch's reads take it.
const FRAME_WORDS: usize = 32;
/// TCKs between batched scans: at 15 MHz, 32 of them (with the scan) are
/// about 5 us, past the 1.3 us a word takes on the wire at 25 MHz, so each
/// RX read finds its word.
const SCAN_IDLE: u32 = 32;
const PAGE: usize = 256;

// S25FL-S commands and status bits (Spansion S25FL128S datasheet).
const SE: u8 = 0xd8;
const P4E: u8 = 0x20;
const PP: u8 = 0x02;
const SR_WIP: u8 = 1 << 0;
const SR_BP: u8 = 0b111 << 2;
const SR_E_ERR: u8 = 1 << 5;
const SR_P_ERR: u8 = 1 << 6;

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
        q.set_clock()?;
        // Linear mode would hand the bus to the AXI window; I/O mode is ours.
        q.ahb.write32(LQSPI_CFG, 0)?;
        q.idle = q.ahb.read32(CONFIG)? & !CONFIG_OWNED | CONFIG_IDLE;
        q.ahb.write32(CONFIG, q.idle)?;
        q.ahb.write32(ENABLE, 1)?;
        q.drain_rx()?;
        Ok(q)
    }

    /// The batched frames below assume SCK of 13-25 MHz (`SCAN_IDLE`), but
    /// what the controller runs at depends on who booted the chip: U-Boot
    /// leaves the IO PLL / 5 (25 MHz SCK), the BootROM after a QSPI boot
    /// the ARM PLL / 40 (about 3 MHz), at which RX reads came before their
    /// words and pages programmed as zeros (seen on the Zybo, 2026-11-08).
    /// So the reference is set here: the IO PLL, divided to at most
    /// 200 MHz for any PS_CLK up to 60 MHz; SCK is that / 8. Also loopback
    /// sampling off (the BootROM turns it on without a feedback pin) and
    /// the pins at fast slew.
    fn set_clock(&mut self) -> Result<()> {
        let status = self.ahb.read32(PLL_STATUS)?;
        ensure!(
            status & IO_PLL_LOCK != 0,
            "the IO PLL isn't locked (PLL_STATUS {status:#010x})"
        );
        let fdiv = (self.ahb.read32(IO_PLL_CTRL)? >> 12 & 0x7f) as u64;
        let div = (PS_CLK_MAX_HZ * fdiv)
            .div_ceil(LQSPI_REF_MAX_HZ)
            .clamp(1, 63) as u32;
        self.ahb.write32(SLCR_UNLOCK, 0xdf0d)?;
        // SRCSEL 0: the IO PLL; CLKACT.
        self.ahb.write32(LQSPI_CLK_CTRL, div << 8 | 1)?;
        for pin in 0..6 {
            self.ahb.write32(MIO_PIN_01 + 4 * pin, MIO_QSPI)?;
        }
        self.ahb.write32(LPBK_DLY_ADJ, 0)
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

    /// One chip-select-framed transfer: `tx` out, the bytes clocked in
    /// back (those of a trailing partial word are dropped). Batched: TX
    /// words then as many RX reads per batch, few enough that the RX FIFO
    /// can't overflow; CS stays low across batches.
    pub fn frame(&mut self, tx: &[u8]) -> Result<Vec<u8>> {
        self.ahb.write32(CONFIG, self.idle & !PCS)?;
        let (words, tail) = tx.as_chunks::<4>();
        let mut rx = Vec::with_capacity(tx.len());
        for chunk in words.chunks(FRAME_WORDS) {
            let mut acc: Vec<Access> = chunk
                .iter()
                .map(|w| Access::Write(TXD0, u32::from_le_bytes(*w)))
                .collect();
            acc.extend(std::iter::repeat_n(Access::Read(RX_DATA), chunk.len()));
            for w in self.ahb.run(&acc, SCAN_IDLE)? {
                rx.extend_from_slice(&w.to_le_bytes());
            }
        }
        if !tail.is_empty() {
            // TXD1..3 send 1..3 bytes (UG585 B.24), bits 7:0 first.
            let reg = TXD1 + 4 * (tail.len() as u32 - 1);
            let word = tail.iter().rev().fold(0u32, |a, &b| a << 8 | b as u32);
            self.ahb.run(
                &[Access::Write(reg, word), Access::Read(RX_DATA)],
                SCAN_IDLE,
            )?;
        }
        self.ahb.write32(CONFIG, self.idle)?;
        Ok(rx)
    }

    /// Hand the controller to its linear (memory-mapped) mode: the flash
    /// reads at `LINEAR_BASE + offset` with plain READ (0x03), so the host
    /// can pull it through the AHB-AP in batches instead of one I/O-mode
    /// word per round trip.
    pub fn into_linear(self) -> Result<()> {
        self.ahb.write32(ENABLE, 0)?;
        // CS and start back under the controller's control, CS released.
        let auto = self.idle & !(1 << 16 | 1 << 15 | 1 << 14 | PCS);
        self.ahb.write32(CONFIG, auto)?;
        self.ahb.write32(LQSPI_CFG, LQ_MODE | READ)?;
        self.ahb.write32(ENABLE, 1)
    }

    /// JEDEC RDID (0x9f): manufacturer, memory type, capacity, then (on
    /// Spansion parts) the CFI length and the sector architecture.
    pub fn read_id(&mut self) -> Result<[u8; 5]> {
        let rx = self.frame(&[0x9f, 0, 0, 0, 0, 0, 0, 0])?;
        rx[1..6].try_into().context("short RDID")
    }

    /// Status register 1 (0x05). The flash repeats it for as long as it's
    /// clocked, so a whole word's worth is fine.
    pub fn status(&mut self) -> Result<u8> {
        Ok(self.frame(&[0x05, 0, 0, 0])?[1])
    }

    fn write_enable(&mut self) -> Result<()> {
        // WREN only latches if CS rises right after its 8 bits.
        self.frame(&[0x06])?;
        Ok(())
    }

    fn wait_ready(&mut self, what: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let sr = self.status()?;
            ensure!(
                sr & (SR_E_ERR | SR_P_ERR) == 0,
                "{what}: the flash reports an error (SR1 {sr:#04x})"
            );
            if sr & SR_WIP == 0 {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "{what}: still busy after {timeout:?}"
            );
        }
    }

    /// Erase what covers `offset..offset + len` and program `data` there,
    /// page by page. `progress` gets the bytes done so far. Read back
    /// through `into_linear` to verify.
    pub fn write(
        &mut self,
        offset: u32,
        data: &[u8],
        mut progress: impl FnMut(usize),
    ) -> Result<()> {
        let id = self.read_id()?;
        ensure!(
            id[0] == 0x01 && id[1] == 0x20,
            "only Spansion S25FL-S flash is known (RDID {id:02x?})"
        );
        let sr = self.status()?;
        ensure!(
            sr & SR_BP == 0,
            "the flash's block protection is on (SR1 {sr:#04x}); not touching it"
        );
        ensure!(
            (offset as usize).is_multiple_of(PAGE) && data.len().is_multiple_of(4),
            "write at {offset:#x} isn't page-aligned whole words"
        );
        let end = offset + data.len() as u32;
        ensure!(end <= 1 << id[2], "write past the flash's end");
        // Sector architecture 0: uniform 256 KiB; 1: 4 KiB parameter
        // sectors over the first 128 KiB, 64 KiB sectors after.
        let mut at = offset & !0xfff;
        while at < end {
            let (cmd, size) = match id[4] {
                0x00 => (SE, 256 << 10),
                0x01 if at < 128 << 10 => (P4E, 4 << 10),
                0x01 => (SE, 64 << 10),
                arch => anyhow::bail!("unknown sector architecture {arch:#04x}"),
            };
            let at_sector = at & !(size - 1);
            self.write_enable()?;
            let [_, a2, a1, a0] = at_sector.to_be_bytes();
            self.frame(&[cmd, a2, a1, a0])?;
            self.wait_ready(&format!("erasing {at_sector:#x}"), Duration::from_secs(5))?;
            at = at_sector + size;
        }
        for (k, page) in data.chunks(PAGE).enumerate() {
            let addr = offset + (k * PAGE) as u32;
            self.write_enable()?;
            let [_, a2, a1, a0] = addr.to_be_bytes();
            let mut tx = vec![PP, a2, a1, a0];
            tx.extend_from_slice(page);
            self.frame(&tx)?;
            self.wait_ready(
                &format!("programming {addr:#x}"),
                Duration::from_millis(100),
            )?;
            progress(((k + 1) * PAGE).min(data.len()));
        }
        Ok(())
    }
}

/// `bytes` of flash from `offset`, through the linear window (the
/// controller must be in linear mode: `Qspi::into_linear`), in batched
/// AHB-AP reads. `progress` gets the bytes read so far.
pub fn read_linear<C: Cable>(
    ahb: &mut MemAp<'_, '_, C>,
    offset: u32,
    bytes: usize,
    mut progress: impl FnMut(usize),
) -> Result<Vec<u8>> {
    // Big enough to amortize each step's setup, small enough to report.
    const STEP: usize = 256 * 1024;
    let mut data = Vec::with_capacity(bytes);
    while data.len() < bytes {
        let n = STEP.min(bytes - data.len());
        let words = ahb.read_block(LINEAR_BASE + offset + data.len() as u32, n.div_ceil(4))?;
        data.extend(words.iter().flat_map(|w| w.to_le_bytes()));
        data.truncate(bytes.min(data.len()));
        progress(data.len());
    }
    Ok(data)
}
