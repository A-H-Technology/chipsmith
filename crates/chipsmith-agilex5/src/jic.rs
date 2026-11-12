//! A phase 1 (`.hps.rbf`) as the DE25-Nano's QSPI image, in the `.jic`
//! `quartus_pgm` writes (`quartus_pfg -o device=MT25QU128
//! -o flash_loader=A5EB013BB23B -o mode=ASX4 -o hps=1`).
//!
//! The flash holds the CMF section four times, at 0, 512 KiB, 1 MiB and
//! 1.5 MiB, then the rest of the file from 2 MiB on; every other byte is
//! erased (0xFF). The `.jic` is tagged records, the flash in record 0x1C with
//! each byte's bits reversed, and a CRC-16 over the file closing it. Records
//! other than 0x1C and the CRC are the same bytes for any phase 1.

use anyhow::{ensure, Result};

/// MT25QU128: 16 MiB.
const FLASH: usize = 16 << 20;
const CMF_COPIES: [usize; 4] = [0, 0x8_0000, 0x10_0000, 0x18_0000];
const REST_AT: usize = 0x20_0000;
/// The CMF section's size, from its header (at +0x400, size at +8).
fn cmf_size(rbf: &[u8]) -> Option<usize> {
    let w = |o: usize| Some(u32::from_le_bytes(rbf.get(o..o + 4)?.try_into().ok()?));
    (w(0x400)? == 0x9756_6593).then_some(w(0x408)? as usize)
}

fn record(out: &mut Vec<u8>, tag: u16, data: &[u8]) {
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

/// CRC-16/X-25 (reflected 0x1021, init and xorout 0xFFFF): the `.jic` trailer.
fn crc16(b: &[u8]) -> u16 {
    let mut c = 0xffffu16;
    for &x in b {
        c ^= x as u16;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0x8408
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// The `.jic` that puts `phase1` into the DE25-Nano's QSPI.
pub fn jic(phase1: &[u8]) -> Result<Vec<u8>> {
    let cmf = cmf_size(phase1)
        .filter(|&n| n <= CMF_COPIES[1] && n <= phase1.len())
        .ok_or_else(|| anyhow::anyhow!("not an .rbf with a CMF section of at most 512 KiB"))?;
    let rest = &phase1[cmf..];
    ensure!(
        REST_AT + rest.len() <= FLASH,
        "phase 1 does not fit the 16 MiB flash"
    );
    let mut flash = vec![0xffu8; FLASH];
    for at in CMF_COPIES {
        flash[at..at + cmf].copy_from_slice(&phase1[..cmf]);
    }
    flash[REST_AT..REST_AT + rest.len()].copy_from_slice(rest);

    let mut out = b"JIC\0\0\0\0\0\x08\0\0\0".to_vec();
    record(
        &mut out,
        0x01,
        b"Quartus Prime Programmer Version 26.1.1 Build 130 08/06/2026 SC Pro Edition\0",
    );
    record(&mut out, 0x02, b"A5EB013BB23B\0");
    record(&mut out, 0x03, b"Untitled\0");
    record(&mut out, 0x1b, b"MT25QU128\0");
    let mut data = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x01, 0x00];
    data.extend(flash.iter().map(|b| b.reverse_bits()));
    record(&mut out, 0x1c, &data);
    // [inf] constant in every Quartus .jic of a phase 1 (two SPLs).
    record(&mut out, 0x1e, &[0, 0, 0, 0, 0x4d, 0x1e, 0, 0]);
    let mut part = vec![0, 0, 0, 0, 0, 0, 0xe8, 0, 0, 0, 0x01, 0x00, b'm'];
    part.extend_from_slice(b"MT25QU128 00000000 00000000;");
    record(&mut out, 0x1a, &part);
    // The CRC covers its own record's tag and length.
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    let crc = crc16(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}
