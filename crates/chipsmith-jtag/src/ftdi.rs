//! FTDI FT2232 in MPSSE mode as a JTAG cable: Digilent's onboard
//! programmers (the Zybo's PROG UART, the "digilent" cable of
//! openFPGALoader), channel A.
//!
//! MPSSE is a byte-command engine in the FTDI: clock out N bytes or bits on
//! TDI (optionally reading TDO), clock TMS bits, clock idle cycles. Command
//! values, per FTDI AN_108:
//!   0x19/0x39 bytes out (/in and out), LSB first, -ve edge out, +ve edge in
//!   0x1B/0x3B bits out (/in and out)
//!   0x4B/0x6B TMS bits out (/with TDO read), TDI held at bit 7
//!   0x8E/0x8F clock bits/bytes with no data
//!   0x80/0x82 set low/high GPIO byte (value, direction)
//!   0x86 clock divisor, 0x87 send immediate
//! Protocol as implemented by openFPGALoader (src/ftdiJtagMPSSE.cpp) and
//! OpenOCD (src/jtag/drivers/mpsse.c).

use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use rusb::{DeviceHandle, GlobalContext};

use super::{bit, Cable};

const VID: u16 = 0x0403;
const PID: u16 = 0x6010;
/// Channel A: interface 0, endpoints 0x02/0x81, FTDI control index 1.
const INTERFACE: u8 = 0;
const EP_OUT: u8 = 0x02;
const EP_IN: u8 = 0x81;
const CONTROL_INDEX: u16 = 1;
const TIMEOUT: Duration = Duration::from_secs(5);
/// Every IN packet starts with two modem-status bytes.
const STATUS_BYTES: usize = 2;

/// Digilent's GPIO setup for channel A (openFPGALoader's "digilent" cable,
/// src/cable.hpp): low byte value/direction, high byte value/direction.
/// The low nibble is TCK/TDI/TDO/TMS (TCK, TDI, TMS out); the high bits
/// enable Digilent's level shifters.
const LOW: (u8, u8) = (0xe8, 0xeb);
const HIGH: (u8, u8) = (0x00, 0x60);

/// 30 MHz / (1 + divisor) with the divide-by-5 prescaler off: 15 MHz.
/// Digilent's cables are rated to 30 MHz; half that leaves margin.
const DIVISOR: u16 = 1;

/// Largest data payload of one byte-shift command (length field is 16 bits).
const MAX_SHIFT_BYTES: usize = 65536;
/// Bytes to queue before a write when nothing is read back.
const WRITE_CHUNK: usize = 64 * 1024;
/// TDO bytes to request per round trip: below the FT2232H's 4 KiB receive
/// buffer, so it never stalls while we are still writing.
const READ_CHUNK: usize = 2048;

pub struct Ftdi {
    handle: DeviceHandle<GlobalContext>,
    out: Vec<u8>,
    /// IN endpoint packet size, for stripping status bytes.
    packet: usize,
}

/// How to turn the bytes one queued read returns into TDO bits.
enum Readback {
    /// `n` whole bytes, LSB first.
    Bytes(usize),
    /// A bit-shift of `n` bits: one byte, shifted in from the top.
    Bits(u8),
}

impl Ftdi {
    pub fn open() -> Result<Self> {
        let handle = rusb::open_device_with_vid_pid(VID, PID).with_context(|| {
            format!("no FT2232 ({VID:04x}:{PID:04x}) found, or no permission to open it; is the board connected?")
        })?;
        let _ = handle.set_auto_detach_kernel_driver(true);
        handle
            .claim_interface(INTERFACE)
            .context("claiming the FTDI's JTAG channel")?;
        let packet = handle
            .device()
            .active_config_descriptor()?
            .interfaces()
            .flat_map(|i| i.descriptors())
            .flat_map(|d| d.endpoint_descriptors().collect::<Vec<_>>())
            .find(|e| e.address() == EP_IN)
            .map_or(512, |e| e.max_packet_size() as usize);

        let ftdi = |request: u8, value: u16| {
            handle.write_control(0x40, request, value, CONTROL_INDEX, &[], TIMEOUT)
        };
        ftdi(0, 0).context("resetting the FTDI")?;
        ftdi(9, 2).context("setting the latency timer")?;
        // Bit mode: reset, then MPSSE.
        ftdi(0x0b, 0x0000).context("resetting the bit mode")?;
        ftdi(0x0b, 0x0200).context("entering MPSSE mode")?;
        ftdi(0, 1).context("purging the receive buffer")?;
        ftdi(0, 2).context("purging the transmit buffer")?;

        let mut cable = Ftdi {
            handle,
            out: Vec::new(),
            packet,
        };
        let [div_lo, div_hi] = DIVISOR.to_le_bytes();
        cable.out.extend_from_slice(&[
            0x8a, // 60 MHz base clock (divide-by-5 off)
            0x97, // adaptive clocking off
            0x8d, // three-phase clocking off
            0x85, // loopback off
            0x86, div_lo, div_hi, 0x80, LOW.0, LOW.1, 0x82, HIGH.0, HIGH.1,
        ]);
        cable.write_all()?;
        Ok(cable)
    }

    fn write_all(&mut self) -> Result<()> {
        let mut sent = 0;
        while sent < self.out.len() {
            sent += self
                .handle
                .write_bulk(EP_OUT, &self.out[sent..], TIMEOUT)
                .context("writing to the FTDI")?;
        }
        self.out.clear();
        Ok(())
    }

    fn read_exact(&mut self, want: usize) -> Result<Vec<u8>> {
        let mut got = Vec::with_capacity(want);
        let mut buf = vec![0u8; self.packet * 8];
        let deadline = Instant::now() + TIMEOUT;
        while got.len() < want {
            ensure!(
                Instant::now() < deadline,
                "FTDI returned {} of {want} TDO bytes",
                got.len()
            );
            let n = self
                .handle
                .read_bulk(EP_IN, &mut buf, TIMEOUT)
                .context("reading from the FTDI")?;
            for packet in buf[..n].chunks(self.packet) {
                got.extend_from_slice(packet.get(STATUS_BYTES..).unwrap_or(&[]));
            }
        }
        ensure!(
            got.len() == want,
            "FTDI returned {} TDO bytes, expected {want}",
            got.len()
        );
        Ok(got)
    }

    /// Send what's queued plus "send immediate", and turn the replies to
    /// `reads` into TDO bits appended to `tdo`.
    fn round_trip(&mut self, reads: &[Readback], tdo: &mut Vec<bool>) -> Result<()> {
        self.out.push(0x87);
        self.write_all()?;
        let want = reads
            .iter()
            .map(|r| match r {
                Readback::Bytes(n) => *n,
                Readback::Bits(_) => 1,
            })
            .sum();
        let bytes = self.read_exact(want)?;
        let mut at = 0;
        for r in reads {
            match *r {
                Readback::Bytes(n) => {
                    for b in &bytes[at..at + n] {
                        tdo.extend((0..8).map(|i| b >> i & 1 == 1));
                    }
                    at += n;
                }
                Readback::Bits(n) => {
                    let b = bytes[at] >> (8 - n);
                    tdo.extend((0..n).map(|i| b >> i & 1 == 1));
                    at += 1;
                }
            }
        }
        Ok(())
    }
}

impl Cable for Ftdi {
    fn tms(&mut self, tms: &[bool]) -> Result<()> {
        // Up to 6 TMS bits per command (7 would set bit 6 of the length
        // byte's companion; 6 is what OpenOCD uses), TDI held low.
        for chunk in tms.chunks(6) {
            let bits = chunk
                .iter()
                .enumerate()
                .fold(0u8, |acc, (i, &t)| acc | (t as u8) << i);
            self.out
                .extend_from_slice(&[0x4b, chunk.len() as u8 - 1, bits]);
        }
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
        let body = if exit { bits - 1 } else { bits };
        let whole = body / 8;
        let rest = (body % 8) as u8;
        let mut tdo: Vec<bool> = Vec::with_capacity(bits);
        let mut reads: Vec<Readback> = Vec::new();
        let mut pending = 0;

        for chunk in tdi[..whole].chunks(if capture { READ_CHUNK } else { MAX_SHIFT_BYTES }) {
            let [lo, hi] = ((chunk.len() - 1) as u16).to_le_bytes();
            self.out
                .extend_from_slice(&[if capture { 0x39 } else { 0x19 }, lo, hi]);
            self.out.extend_from_slice(chunk);
            if capture {
                reads.push(Readback::Bytes(chunk.len()));
                pending += chunk.len();
                if pending >= READ_CHUNK {
                    self.round_trip(&reads, &mut tdo)?;
                    reads.clear();
                    pending = 0;
                }
            } else if self.out.len() >= WRITE_CHUNK {
                self.write_all()?;
            }
        }
        if rest > 0 {
            let byte =
                (0..rest as usize).fold(0u8, |acc, i| acc | (bit(tdi, whole * 8 + i) as u8) << i);
            self.out
                .extend_from_slice(&[if capture { 0x3b } else { 0x1b }, rest - 1, byte]);
            if capture {
                reads.push(Readback::Bits(rest));
            }
        }
        if exit {
            // The last bit goes out with TMS high (to Exit1): TDI in bit 7.
            let last = (bit(tdi, bits - 1) as u8) << 7 | 1;
            self.out
                .extend_from_slice(&[if capture { 0x6b } else { 0x4b }, 0, last]);
            if capture {
                reads.push(Readback::Bits(1));
            }
        }

        if !capture {
            return Ok(None);
        }
        self.round_trip(&reads, &mut tdo)?;
        if tdo.len() != bits {
            bail!("FTDI returned {} TDO bits, expected {bits}", tdo.len());
        }
        let mut packed = vec![0u8; bits.div_ceil(8)];
        for (i, &b) in tdo.iter().enumerate() {
            packed[i / 8] |= (b as u8) << (i % 8);
        }
        Ok(Some(packed))
    }

    fn idle(&mut self, clocks: u32) -> Result<()> {
        // 0x8F clocks 8 * (n + 1) cycles, up to 65536 bytes' worth.
        let mut left = clocks as usize;
        while left >= 8 {
            let bytes = (left / 8).min(MAX_SHIFT_BYTES);
            let [lo, hi] = ((bytes - 1) as u16).to_le_bytes();
            self.out.extend_from_slice(&[0x8f, lo, hi]);
            left -= bytes * 8;
        }
        if left > 0 {
            self.out.extend_from_slice(&[0x8e, left as u8 - 1]);
        }
        if self.out.len() >= WRITE_CHUNK {
            self.write_all()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.write_all()
    }
}

impl Drop for Ftdi {
    fn drop(&mut self) {
        let _ = self.write_all();
        let _ = self.handle.release_interface(INTERFACE);
    }
}
