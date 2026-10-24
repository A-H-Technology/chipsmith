//! Altera USB-Blaster (the original, and the DE2's onboard copy): an FTDI
//! FT245 FIFO in front of a CPLD that turns bytes into JTAG. TCK is fixed at
//! 24 MHz (the CPLD's clock), so there is nothing to configure beyond the FTDI.
//!
//! A byte with bit 7 clear sets the pins directly (bit-bang): one TCK takes
//! two bytes, TCK low then high. A byte with bit 7 set starts byte-shift
//! mode: its low 6 bits count the data bytes that follow, each shifted LSB
//! first with TMS held at its last bit-banged value. Bit 6 asks for TDO back:
//! one byte per bit-banged clock (TDO in bit 0), or one per shifted byte.
//!
//! Protocol as implemented by openFPGALoader (src/usbBlaster.cpp) and OpenOCD
//! (src/jtag/drivers/usb_blaster).

use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use rusb::{DeviceHandle, GlobalContext};

use super::{bit, Cable};

const VID: u16 = 0x09fb;
const PID: u16 = 0x6001;
const EP_OUT: u8 = 0x02;
const EP_IN: u8 = 0x81;
/// Every IN packet starts with two FTDI modem-status bytes.
const IN_PACKET: usize = 64;
const STATUS_BYTES: usize = 2;
const TIMEOUT: Duration = Duration::from_secs(5);

const TCK: u8 = 1 << 0;
const TMS: u8 = 1 << 1;
const TDI: u8 = 1 << 4;
const READ: u8 = 1 << 6;
const SHIFT: u8 = 1 << 7;
/// nCE, nCS and the LED, which every bit-bang byte carries.
const IDLE: u8 = (1 << 2) | (1 << 3) | (1 << 5);
const MAX_SHIFT_BYTES: usize = 63;

/// How much to buffer before writing when nothing needs reading back.
const WRITE_CHUNK: usize = 16 * 1024;
/// TDO bytes to request per round trip. The FT245's transmit FIFO holds a
/// few hundred bytes; staying under one packet means it never fills while
/// we are still writing.
const READ_CHUNK: usize = IN_PACKET - STATUS_BYTES;

pub struct UsbBlaster {
    handle: DeviceHandle<GlobalContext>,
    out: Vec<u8>,
    /// TMS as last bit-banged; byte-shift mode holds it.
    tms: bool,
}

/// One piece of a captured shift: the bytes to send and what comes back.
enum Readback {
    /// One TDO byte per shifted byte.
    Bytes(usize),
    /// One byte per clock, TDO in bit 0.
    Bits(usize),
}

impl UsbBlaster {
    pub fn open() -> Result<Self> {
        let handle = rusb::open_device_with_vid_pid(VID, PID).with_context(|| {
            format!(
                "no USB-Blaster ({VID:04x}:{PID:04x}) found, or no permission to open it; \
                 is the board connected and powered?"
            )
        })?;
        // ftdi_sio may have bound the interface; take it back. Not every
        // platform supports detaching, and then there's nothing to detach.
        let _ = handle.set_auto_detach_kernel_driver(true);
        handle
            .claim_interface(0)
            .context("claiming the USB-Blaster")?;

        // FTDI vendor requests, as libftdi sends them: reset, then a 2 ms
        // latency timer so short TDO reads come back promptly.
        let ftdi =
            |request: u8, value: u16| handle.write_control(0x40, request, value, 1, &[], TIMEOUT);
        ftdi(0, 0).context("resetting the FTDI")?;
        ftdi(9, 2).context("setting the FTDI latency timer")?;
        // Bytes left in the FIFOs by an interrupted session, or by a board
        // power-cycled mid-transfer, would otherwise arrive as our TDO
        // ("returned 276 TDO bytes, expected 63").
        ftdi(0, 1).context("purging the FTDI receive buffer")?;
        ftdi(0, 2).context("purging the FTDI transmit buffer")?;

        let mut blaster = UsbBlaster {
            handle,
            out: Vec::new(),
            tms: false,
        };
        // Whatever a previous user left half-written in the CPLD gets
        // completed by a run of bit-bang clocks (TMS high, which also resets
        // the TAP), the way openFPGALoader starts.
        blaster.tms(&[true; 2048])?;
        blaster.flush()?;
        Ok(blaster)
    }

    fn clock(&mut self, tms: bool, tdi: bool, read: bool) {
        let pins = IDLE | if tms { TMS } else { 0 } | if tdi { TDI } else { 0 };
        self.out.push(pins);
        self.out.push(pins | TCK | if read { READ } else { 0 });
        self.tms = tms;
    }

    /// TCK low again after a run of clocks.
    fn settle(&mut self) {
        self.out.push(IDLE | if self.tms { TMS } else { 0 });
    }

    fn write_all(&mut self) -> Result<()> {
        let mut sent = 0;
        while sent < self.out.len() {
            let n = self
                .handle
                .write_bulk(EP_OUT, &self.out[sent..], TIMEOUT)
                .context("writing to the USB-Blaster")?;
            sent += n;
        }
        self.out.clear();
        Ok(())
    }

    fn read_exact(&mut self, want: usize) -> Result<Vec<u8>> {
        let mut got = Vec::with_capacity(want);
        let mut buf = [0u8; IN_PACKET * 8];
        let deadline = Instant::now() + TIMEOUT;
        while got.len() < want {
            ensure!(
                Instant::now() < deadline,
                "USB-Blaster returned {} of {want} TDO bytes",
                got.len()
            );
            let n = self
                .handle
                .read_bulk(EP_IN, &mut buf, TIMEOUT)
                .context("reading from the USB-Blaster")?;
            for packet in buf[..n].chunks(IN_PACKET) {
                got.extend_from_slice(packet.get(STATUS_BYTES..).unwrap_or(&[]));
            }
        }
        ensure!(
            got.len() == want,
            "USB-Blaster returned {} TDO bytes, expected {want}",
            got.len()
        );
        Ok(got)
    }

    /// Send what is queued, then collect `readback`'s TDO bits.
    fn round_trip(&mut self, readback: &[Readback], tdo: &mut Vec<bool>) -> Result<()> {
        self.write_all()?;
        let want = readback
            .iter()
            .map(|r| match r {
                Readback::Bytes(n) | Readback::Bits(n) => *n,
            })
            .sum();
        let bytes = self.read_exact(want)?;
        let mut at = 0;
        for r in readback {
            match *r {
                Readback::Bytes(n) => {
                    for byte in &bytes[at..at + n] {
                        tdo.extend((0..8).map(|i| byte >> i & 1 == 1));
                    }
                    at += n;
                }
                Readback::Bits(n) => {
                    tdo.extend(bytes[at..at + n].iter().map(|b| b & 1 == 1));
                    at += n;
                }
            }
        }
        Ok(())
    }
}

impl Cable for UsbBlaster {
    fn tms(&mut self, tms: &[bool]) -> Result<()> {
        if tms.is_empty() {
            return Ok(());
        }
        for &t in tms {
            self.clock(t, false, false);
        }
        self.settle();
        if self.out.len() >= WRITE_CHUNK {
            self.write_all()?;
        }
        Ok(())
    }

    fn shift(
        &mut self,
        tdi: &[u8],
        bits: usize,
        exit: bool,
        capture: bool,
    ) -> Result<Option<Vec<u8>>> {
        if bits == 0 {
            return Ok(capture.then(Vec::new));
        }
        if self.tms {
            bail!("shift with TMS high: the TAP is not in a shift state");
        }
        // Everything up to the final bit (which may need TMS high) as
        // whole bytes in byte-shift mode, the rest bit-banged.
        let body = if exit { bits - 1 } else { bits };
        let whole = body / 8;
        let read = if capture { READ } else { 0 };

        let mut tdo: Vec<bool> = Vec::with_capacity(bits);
        let mut pending: Vec<Readback> = Vec::new();
        let mut pending_bytes = 0;

        for chunk in tdi[..whole].chunks(MAX_SHIFT_BYTES) {
            self.out.push(SHIFT | read | chunk.len() as u8);
            self.out.extend_from_slice(chunk);
            if capture {
                pending.push(Readback::Bytes(chunk.len()));
                pending_bytes += chunk.len();
                if pending_bytes >= READ_CHUNK {
                    self.round_trip(&pending, &mut tdo)?;
                    pending.clear();
                    pending_bytes = 0;
                }
            } else if self.out.len() >= WRITE_CHUNK {
                self.write_all()?;
            }
        }

        let tail = (whole * 8..bits).map(|i| (i, exit && i == bits - 1));
        for (i, last) in tail {
            self.clock(last, bit(tdi, i), capture);
            if capture {
                pending.push(Readback::Bits(1));
                pending_bytes += 1;
                if pending_bytes >= READ_CHUNK {
                    self.round_trip(&pending, &mut tdo)?;
                    pending.clear();
                    pending_bytes = 0;
                }
            }
        }
        self.settle();

        if !capture {
            return Ok(None);
        }
        self.round_trip(&pending, &mut tdo)?;
        let mut packed = vec![0u8; bits.div_ceil(8)];
        for (i, &b) in tdo.iter().enumerate() {
            packed[i / 8] |= (b as u8) << (i % 8);
        }
        Ok(Some(packed))
    }

    fn idle(&mut self, clocks: u32) -> Result<()> {
        let clocks = clocks as usize;
        let zeros = vec![0u8; clocks / 8];
        if !zeros.is_empty() {
            if self.tms {
                bail!("idle clocks with TMS high would leave the state");
            }
            for chunk in zeros.chunks(MAX_SHIFT_BYTES) {
                self.out.push(SHIFT | chunk.len() as u8);
                self.out.extend_from_slice(chunk);
            }
        }
        for _ in 0..clocks % 8 {
            self.clock(false, false, false);
        }
        self.settle();
        if self.out.len() >= WRITE_CHUNK {
            self.write_all()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.write_all()
    }
}

impl Drop for UsbBlaster {
    fn drop(&mut self) {
        let _ = self.write_all();
        let _ = self.handle.release_interface(0);
    }
}
