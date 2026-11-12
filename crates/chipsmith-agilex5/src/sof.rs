//! `.sof` container: gzip of `SOF\0`, u32 0, u32 record count, then
//! `(u16 tag, u32 len, data)` records. Record 0x3a (first) is the
//! configuration image behind a u32 0; 0x41 is 8 zero bytes and an
//! HMAC-SHA256 over everything before the HMAC; 0x08 is a CRC-16/X-25 over
//! everything before its data (README (g)).

use std::io::{Read, Write};

use anyhow::{bail, ensure, Context, Result};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const IMAGE_TAG: u16 = 0x3a;
const HMAC_TAG: u16 = 0x41;
const CRC_TAG: u16 = 0x08;

pub struct Sof {
    raw: Vec<u8>,
    image: std::ops::Range<usize>,
}

struct Record {
    tag: u16,
    data: std::ops::Range<usize>,
}

fn records(raw: &[u8]) -> Result<Vec<Record>> {
    ensure!(raw.starts_with(b"SOF\0"), "not a SOF");
    let mut out = Vec::new();
    let mut p = 12;
    while p < raw.len() {
        ensure!(p + 6 <= raw.len(), "truncated record header at {p:#x}");
        let tag = u16::from_le_bytes([raw[p], raw[p + 1]]);
        let len = u32::from_le_bytes(raw[p + 2..p + 6].try_into().unwrap()) as usize;
        ensure!(
            p + 6 + len <= raw.len(),
            "record {tag:#x} overruns the file"
        );
        out.push(Record {
            tag,
            data: p + 6..p + 6 + len,
        });
        p += 6 + len;
    }
    Ok(out)
}

fn crc16_x25(data: &[u8]) -> u16 {
    let mut c: u16 = 0xffff;
    for &b in data {
        c ^= b as u16;
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

impl Sof {
    pub fn read(path: &str) -> Result<Sof> {
        let file = std::fs::read(path).with_context(|| format!("reading {path}"))?;
        let raw = if file.starts_with(&[0x1f, 0x8b]) {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&file[..]).read_to_end(&mut out)?;
            out
        } else {
            file
        };
        let image = records(&raw)?
            .into_iter()
            .find(|r| r.tag == IMAGE_TAG)
            .context("no image record")?
            .data;
        Ok(Sof {
            image: image.start + 4..image.end,
            raw,
        })
    }

    /// A `.sof` from its uncompressed bytes (`sofgen::build`).
    pub fn from_raw(raw: Vec<u8>) -> Result<Sof> {
        let image = records(&raw)?
            .into_iter()
            .find(|r| r.tag == IMAGE_TAG)
            .context("no image record")?
            .data;
        Ok(Sof {
            image: image.start + 4..image.end,
            raw,
        })
    }

    /// Every record as (tag, data).
    pub fn records(&self) -> Vec<(u16, &[u8])> {
        records(&self.raw)
            .map(|rs| rs.into_iter().map(|r| (r.tag, &self.raw[r.data])).collect())
            .unwrap_or_default()
    }

    pub fn image(&self) -> &[u8] {
        &self.raw[self.image.clone()]
    }

    pub fn image_mut(&mut self) -> &mut [u8] {
        &mut self.raw[self.image.clone()]
    }

    /// Where OPTIONS (the 0x3a record with index 3) keeps the IO hash, as
    /// 64 hex digits in field 1.20.
    fn io_hash_span(&self) -> Option<std::ops::Range<usize>> {
        let base = self.raw.as_ptr() as usize;
        let rec = records(&self.raw)
            .ok()?
            .into_iter()
            .find(|r| r.tag == IMAGE_TAG && self.raw[r.data.clone()].starts_with(&[3, 0, 0, 0]))?;
        let pb = &self.raw[rec.data.start + 4..rec.data.end];
        let opts = crate::rbf::install::pb_parse(pb)?
            .into_iter()
            .find_map(|(f, v)| match v {
                crate::rbf::install::Pb::Bytes(b) if f == 1 => Some(b),
                _ => None,
            })?;
        let h = crate::rbf::install::pb_parse(opts)?
            .into_iter()
            .find_map(|(f, v)| match v {
                crate::rbf::install::Pb::Bytes(b) if f == 20 && b.len() == 64 => Some(b),
                _ => None,
            })?;
        let start = h.as_ptr() as usize - base;
        Some(start..start + 64)
    }

    /// Recompute the IO hash (a function of the image, README (n)), the
    /// HMAC and the CRC, then write gzip.
    pub fn write(&mut self, path: &str, hmac_key: &[u8]) -> Result<()> {
        if let Some(span) = self.io_hash_span() {
            let hex: String = crate::sofgen::io_hash(self.image())
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect();
            self.raw[span].copy_from_slice(hex.as_bytes());
        }
        let recs = records(&self.raw)?;
        let h = recs
            .iter()
            .find(|r| r.tag == HMAC_TAG)
            .context("no HMAC record")?;
        ensure!(h.data.len() == 40, "HMAC record is not 8 + 32 bytes");
        let mut mac = Hmac::<Sha256>::new_from_slice(hmac_key)?;
        mac.update(&self.raw[..h.data.start + 8]);
        let tag = mac.finalize().into_bytes();
        let at = h.data.start + 8;
        self.raw[at..at + 32].copy_from_slice(&tag);
        let c = recs.last().context("no records")?;
        if c.tag != CRC_TAG || c.data.len() != 2 {
            bail!("last record is not the CRC");
        }
        let crc = crc16_x25(&self.raw[..c.data.start]);
        self.raw[c.data.clone()].copy_from_slice(&crc.to_le_bytes());
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        gz.write_all(&self.raw)?;
        std::fs::write(path, gz.finish()?).with_context(|| format!("writing {path}"))?;
        Ok(())
    }
}
