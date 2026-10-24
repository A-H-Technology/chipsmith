//! The DE2's EPCS16 configuration flash, driven through chipsmith's serial
//! flash loader (`loader/top.v`) over the USB-Blaster.
//!
//! With the loader in the FPGA and USER0 in the instruction register, one
//! Shift-DR is one SPI transaction (nCSO low for the shift, one DCLK per
//! TCK). The EPCS takes commands, addresses and data MSB first; the cable
//! shifts LSB first, so every byte is bit-reversed on the way in and out.
//!
//! Commands are the EPCS family's (Altera's "Serial Configuration Devices"
//! data sheet, the same set as an M25P16's).

use anyhow::{bail, ensure, Context, Result};
use std::time::{Duration, Instant};

use chipsmith_jtag::{Cable, TapState};

/// The EP2C35's instruction register is 10 bits; USER0 selects the
/// loader's data path.
const IR_LEN: usize = 10;
const USER0: u16 = 0x00c;

const WRITE_ENABLE: u8 = 0x06;
const READ_STATUS: u8 = 0x05;
const READ_BYTES: u8 = 0x03;
const PAGE_PROGRAM: u8 = 0x02;
const SECTOR_ERASE: u8 = 0xd8;
const READ_SILICON_ID: u8 = 0xab;
/// Status register: write in progress.
const WIP: u8 = 1 << 0;

/// The EPCS16's silicon ID (`READ_SILICON_ID`).
const EPCS16_ID: u8 = 0x14;
pub const SIZE: usize = 2 << 20;
pub const SECTOR: usize = 64 << 10;
pub const PAGE: usize = 256;

/// Reads this long go in one transaction; the cable splits them into USB
/// round trips anyway, so longer ones only cost memory.
const READ_CHUNK: usize = 64 << 10;

pub struct Epcs<'c, C: Cable> {
    cable: &'c mut C,
}

impl<'c, C: Cable> Epcs<'c, C> {
    /// Select the loader's data path and check an EPCS16 answers. The
    /// loader must already be configured into the FPGA.
    pub fn open(cable: &'c mut C) -> Result<Self> {
        cable.tms(&TapState::Reset.path_to(TapState::Reset))?;
        cable.tms(&TapState::Reset.path_to(TapState::IrShift))?;
        let ir = USER0.to_le_bytes();
        cable.shift(&ir, IR_LEN, true, false)?;
        cable.tms(&TapState::IrExit1.path_to(TapState::Idle))?;
        let mut epcs = Epcs { cable };
        let id = epcs.transaction(&[READ_SILICON_ID, 0, 0, 0], 2)?;
        ensure!(
            id == [EPCS16_ID; 2],
            "no EPCS16 behind the serial flash loader (silicon ID {id:02x?}, expected 14 14); \
             is the loader configured?"
        );
        Ok(epcs)
    }

    /// One SPI transaction: send `out`, then read `read` bytes.
    fn transaction(&mut self, out: &[u8], read: usize) -> Result<Vec<u8>> {
        // DATA0 reaches TDO within the same TCK: on the DE2 the silicon
        // ID's first bit is the one sampled right after the command's last
        // (bit 32 of a `READ_SILICON_ID` shift). So a transaction is whole
        // bytes, which commands without a read need anyway: the flash
        // ignores a page program or erase that doesn't end on a byte.
        let bits = (out.len() + read) * 8;
        let mut tdi: Vec<u8> = out.iter().map(|b| b.reverse_bits()).collect();
        tdi.resize(bits.div_ceil(8), 0);
        self.cable.tms(&TapState::Idle.path_to(TapState::DrShift))?;
        let tdo = self.cable.shift(&tdi, bits, true, read > 0)?;
        self.cable.tms(&TapState::DrExit1.path_to(TapState::Idle))?;
        let Some(tdo) = tdo else {
            return Ok(Vec::new());
        };
        let start = out.len() * 8;
        Ok((0..read)
            .map(|k| {
                (0..8).fold(0u8, |byte, i| {
                    let at = start + 8 * k + i;
                    // MSB first on the wire.
                    byte | ((tdo[at / 8] >> (at % 8) & 1) << (7 - i))
                })
            })
            .collect())
    }

    fn status(&mut self) -> Result<u8> {
        Ok(self.transaction(&[READ_STATUS], 1)?[0])
    }

    fn wait_ready(&mut self, timeout: Duration, what: &str) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while self.status()? & WIP != 0 {
            ensure!(
                Instant::now() < deadline,
                "the EPCS is still busy after {what}"
            );
        }
        Ok(())
    }

    fn write_enable(&mut self) -> Result<()> {
        self.transaction(&[WRITE_ENABLE], 0)?;
        Ok(())
    }

    fn command_at(command: u8, address: usize) -> [u8; 4] {
        [
            command,
            (address >> 16) as u8,
            (address >> 8) as u8,
            address as u8,
        ]
    }

    /// `len` bytes from `address`, as stored.
    pub fn read(&mut self, address: usize, len: usize) -> Result<Vec<u8>> {
        ensure!(address + len <= SIZE, "read past the end of the EPCS16");
        let mut out = Vec::with_capacity(len);
        let mut at = address;
        while out.len() < len {
            let n = (len - out.len()).min(READ_CHUNK);
            out.extend(self.transaction(&Self::command_at(READ_BYTES, at), n)?);
            at += n;
        }
        Ok(out)
    }

    /// Erase the sectors `[address, address + len)` touches.
    pub fn erase(&mut self, address: usize, len: usize) -> Result<()> {
        ensure!(address + len <= SIZE, "erase past the end of the EPCS16");
        let first = address / SECTOR;
        let last = (address + len).div_ceil(SECTOR);
        for s in first..last {
            self.write_enable()?;
            self.transaction(&Self::command_at(SECTOR_ERASE, s * SECTOR), 0)?;
            // The data sheet's maximum sector erase time is 3 s.
            self.wait_ready(Duration::from_secs(6), "a sector erase")?;
        }
        Ok(())
    }

    /// Program erased flash with `data` from `address`, page by page.
    pub fn program(&mut self, address: usize, data: &[u8]) -> Result<()> {
        ensure!(
            address + data.len() <= SIZE,
            "write past the end of the EPCS16"
        );
        let mut at = address;
        let mut rest = data;
        while !rest.is_empty() {
            // A page program wraps within its 256-byte page.
            let n = rest.len().min(PAGE - at % PAGE);
            let (page, tail) = rest.split_at(n);
            // All-ones is what erased flash holds already.
            if page.iter().any(|&b| b != 0xff) {
                self.write_enable()?;
                let mut out = Self::command_at(PAGE_PROGRAM, at).to_vec();
                out.extend_from_slice(page);
                self.transaction(&out, 0)?;
                // Page program: 5 ms maximum.
                self.wait_ready(Duration::from_millis(50), "a page program")?;
            }
            at += n;
            rest = tail;
        }
        Ok(())
    }

    /// The whole flash, read twice: a dump that two reads agree on.
    pub fn dump(&mut self) -> Result<Vec<u8>> {
        let first = self.read(0, SIZE)?;
        let second = self.read(0, SIZE)?;
        if let Some(at) = first.iter().zip(&second).position(|(a, b)| a != b) {
            bail!("two reads of the EPCS differ (first at {at:#x}); not trusting either");
        }
        Ok(first)
    }

    /// Replace `[address, address + data.len())` with `data` and read it
    /// back.
    pub fn write_verified(&mut self, address: usize, data: &[u8]) -> Result<()> {
        self.erase(address, data.len())?;
        self.program(address, data)?;
        let back = self.read(address, data.len())?;
        if let Some(at) = back.iter().zip(data).position(|(a, b)| a != b) {
            bail!(
                "the EPCS reads back {:#04x} at {:#x}, written {:#04x}",
                back[at],
                address + at,
                data[at]
            );
        }
        Ok(())
    }
}

/// Run `f` with the loader's flash, after checking `cable` reaches it.
pub fn with<C: Cable, T>(cable: &mut C, f: impl FnOnce(&mut Epcs<C>) -> Result<T>) -> Result<T> {
    let mut epcs = Epcs::open(cable).context("opening the EPCS through the serial flash loader")?;
    let out = f(&mut epcs)?;
    epcs.cable.flush()?;
    Ok(out)
}
