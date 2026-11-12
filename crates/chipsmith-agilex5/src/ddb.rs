//! The PDB v25 `.ddb` container of Quartus Prime Pro's device databases
//! (agilex5-re's `tools/ddb`), as far as the image writer reads them: the
//! header and the decompressed body. A fixed header, a compressed body, then
//! a compressed footer with the class table, which nothing here needs. Field
//! names are the ones Quartus prints in its `PDB_ASCII_DUMP` trace.

use std::io::Read;
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};

/// `m_compression_info.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    /// `is_enabled == 0`: stored as is.
    None,
    /// Type 0: zlib header and deflate, ended by a sync flush and no final
    /// block (the same quirk as Quartus 13).
    Zlib,
    /// Type 2: raw LZMA1, a 5-byte properties header and the range-coded
    /// stream, with no size field and no end marker. The size comes from the
    /// PDB header.
    Lzma1,
}

const MAGIC: u16 = 0x0bad;
/// 0x1bad marks a file whose writer died before committing (Quartus 13).
const MAGIC_TRUNCATED: u16 = 0x1bad;
const VERSION: i32 = 25;

pub struct Ddb {
    /// Quartus' build string, e.g. "Version 26.1.1 Build 130 ...".
    pub version_string: String,
    pub body: Vec<u8>,
}

impl Ddb {
    pub fn open(path: &Path) -> Result<Self> {
        let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&raw).with_context(|| format!("{}", path.display()))
    }

    fn parse(raw: &[u8]) -> Result<Self> {
        let mut c = Cursor { buf: raw, pos: 0 };
        let version_number = c.i32()?;
        let magic = c.u16()?;
        ensure!(
            magic != MAGIC_TRUNCATED,
            "truncated database (header id 0x1bad)"
        );
        ensure!(magic == MAGIC, "bad header id {magic:#x}");
        ensure!(
            version_number == VERSION,
            "unsupported header version {version_number}"
        );
        let (u0, u1) = (c.i32()?, c.i16()?);
        ensure!(
            u0 == 0 && u1 == 0,
            "unused header fields are {u0:#x} {u1:#x}"
        );
        let size_of_header = c.u16()?;
        let end_of_header_pos = c.u16()?;
        let version_string = c.c_str()?;
        let size_of_body = c.u64()?;
        let end_of_body_pos = c.u64()?;
        let _size_of_footer = c.u64()?;
        let (is_enabled, ty) = (c.i32()?, c.i32()?);
        let compression_chunk_size = c.i32()?;
        let _compression_level = c.i32()?;
        let compression = match (is_enabled, ty) {
            (0, _) => Compression::None,
            (1, 0) => Compression::Zlib,
            (1, 2) => Compression::Lzma1,
            (e, t) => bail!("unknown compression is_enabled={e} type={t}"),
        };
        // Every file seen is one stream; a nonzero chunk size would mean a
        // chunked layout nobody has looked at yet.
        ensure!(
            compression_chunk_size == 0,
            "chunked compression ({compression_chunk_size}) not supported"
        );
        let _pdb_core_signature = c.c_str()?;
        let end_of_footer_pos = c.u64()?;
        let _num_user_sections = c.u64()?;
        ensure!(
            c.pos == end_of_header_pos as usize,
            "header ends at {:#x}, m_end_of_header_pos is {end_of_header_pos:#x}",
            c.pos
        );
        ensure!(
            size_of_header == end_of_header_pos,
            "m_size_of_header {size_of_header} != m_end_of_header_pos {end_of_header_pos}"
        );
        ensure!(
            end_of_header_pos as u64 <= end_of_body_pos && end_of_body_pos <= end_of_footer_pos,
            "section bounds out of order"
        );
        ensure!(
            end_of_footer_pos as usize == raw.len(),
            "m_end_of_footer_pos {end_of_footer_pos} != file size {}",
            raw.len()
        );
        let body = &raw[end_of_header_pos as usize..end_of_body_pos as usize];
        Ok(Ddb {
            version_string,
            body: decompress(compression, body, size_of_body).context("body")?,
        })
    }
}

fn decompress(kind: Compression, src: &[u8], expect: u64) -> Result<Vec<u8>> {
    let out = match kind {
        Compression::None => src.to_vec(),
        Compression::Zlib => {
            let mut out = Vec::with_capacity(expect as usize);
            // No final block, so read_to_end reports UnexpectedEof once the
            // input is exhausted; everything decoded up to then is valid.
            match flate2::read::ZlibDecoder::new(src).read_to_end(&mut out) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
                Err(e) => return Err(e).context("zlib"),
            }
            out
        }
        Compression::Lzma1 => {
            let mut out = Vec::with_capacity(expect as usize);
            let opts = lzma_rs::decompress::Options {
                unpacked_size: lzma_rs::decompress::UnpackedSize::UseProvided(Some(expect)),
                ..Default::default()
            };
            lzma_rs::lzma_decompress_with_options(
                &mut std::io::BufReader::new(src),
                &mut out,
                &opts,
            )
            .map_err(|e| anyhow::anyhow!("lzma1: {e:?}"))?;
            out
        }
    };
    ensure!(
        out.len() as u64 == expect,
        "decompressed {} bytes, header says {expect}",
        out.len()
    );
    Ok(out)
}

/// `PDB_SEGMENT_READER`'s primitives: little-endian, unaligned, untagged.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn arr<const N: usize>(&mut self) -> Result<[u8; N]> {
        let b = self
            .buf
            .get(self.pos..self.pos + N)
            .with_context(|| format!("read of {N} bytes at {:#x} runs past the end", self.pos))?;
        self.pos += N;
        Ok(b.try_into().unwrap())
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.arr()?))
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    /// `get_c_str`: an i32 length that counts a NUL which isn't stored; 0 is
    /// a null pointer, read here as "".
    fn c_str(&mut self) -> Result<String> {
        let n = self.i32()?;
        if n == 0 {
            return Ok(String::new());
        }
        ensure!(n > 0, "negative c_str length {n} at {:#x}", self.pos - 4);
        let b = self
            .buf
            .get(self.pos..self.pos + n as usize - 1)
            .context("c_str runs past the end")?;
        self.pos += n as usize - 1;
        Ok(String::from_utf8_lossy(b).into_owned())
    }
}
