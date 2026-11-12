//! What the writer takes from Quartus Prime Pro: the signed SDM firmware
//! (`main.cmf` and the `.fw` modules), the CORE codebooks from the
//! programming database, the CRAM `s4` group list from `sof_meta.pb`, and
//! the S4 flag lists from `fm_sector_bnxplv_rif_t*.ddb` (agilex5-re README
//! (l), (o)). Read from an install, or from agilex5-re's `quartus/`, which
//! holds just these files in the install's layout, the firmware modules
//! unzipped (chipsmith docs/legal.md).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};

pub struct Install {
    /// `devices/programmer/firmware/agilex5e.zip` entries under `sm4revb/SHA_384/`.
    pub fw: HashMap<String, Vec<u8>>,
    /// GZIP_ALTERA_LITERALS and the second 256-entry codebook, as pfg stores
    /// them: one u64 per entry in a 4 KiB page.
    pub cb1: Vec<u8>,
    pub cb2: Vec<u8>,
    /// Bit offsets of the `s4` groups of `sectorbnxplv_dr`.
    pub s4: Vec<usize>,
    /// `devices/10nm/sm4revb/sof_meta.pb` as is.
    pub sof_meta: Vec<u8>,
    /// `programming_sequence.pb.enc` inflated: the .sof's PROGRAMMING_SEQUENCE.
    pub programming_sequence: Vec<u8>,
    /// "Quartus Prime Version 26.1.1 Build 130 …", from a database header.
    pub version: String,
    /// Per image frame, the S4 keys that flag a pair: (key bit, nibble).
    pub s4_flags: Vec<Vec<(u16, u8)>>,
}

impl Install {
    pub fn open(root: &Path) -> Result<Install> {
        let fw = firmware(root)?;
        let npcf = crate::ddb::Ddb::open(&root.join("devices/programmer/sm4revb_npcf.ddb"))?;
        let version = format!("Quartus Prime {}", npcf.version_string);
        let (cb1, cb2) = codebooks(&npcf.body)?;
        let meta = std::fs::read(root.join("devices/10nm/sm4revb/sof_meta.pb"))
            .context("reading sof_meta.pb")?;
        // An 8-byte `ac 0f` wrapper, then zlib.
        let enc = std::fs::read(root.join("devices/10nm/sm4revb/programming_sequence.pb.enc"))
            .context("reading programming_sequence.pb.enc")?;
        ensure!(
            enc.len() > 10 && enc[..2] == [0xac, 0x0f],
            "programming_sequence.pb.enc: unexpected wrapper"
        );
        let mut programming_sequence = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::ZlibDecoder::new(&enc[8..]),
            &mut programming_sequence,
        )?;
        let mut s4_flags = vec![Vec::new(); RIF_FILES * RIF_SLOTS];
        for t in 0..RIF_FILES {
            let body = crate::ddb::Ddb::open(
                &root.join(format!("devices/programmer/fm_sector_bnxplv_rif_t{t}.ddb")),
            )?
            .body;
            rif_lists(&body, t, &mut s4_flags)?;
        }
        Ok(Install {
            fw,
            cb1,
            cb2,
            s4: s4_positions(&meta)?,
            s4_flags,
            sof_meta: meta,
            programming_sequence,
            version,
        })
    }

    pub fn fw(&self, name: &str) -> Result<&[u8]> {
        self.fw
            .get(name)
            .map(Vec::as_slice)
            .with_context(|| format!("{name} not in the firmware zip"))
    }
}

/// The firmware modules: an install's zip, or the modules unzipped beside
/// where it would be.
fn firmware(root: &Path) -> Result<HashMap<String, Vec<u8>>> {
    let dir = root.join("devices/programmer/firmware");
    let zip = dir.join("agilex5e.zip");
    if zip.is_file() {
        let zip = std::fs::read(&zip).context("reading the firmware zip")?;
        return unzip(&zip, "sm4revb/SHA_384/");
    }
    let modules = dir.join("sm4revb/SHA_384");
    let mut fw = HashMap::new();
    for e in std::fs::read_dir(&modules)
        .with_context(|| format!("no firmware zip, and no modules in {}", modules.display()))?
    {
        let p = e?.path();
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .context("module name")?
            .to_owned();
        fw.insert(
            name,
            std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?,
        );
    }
    Ok(fw)
}

fn u16le(b: &[u8], p: usize) -> usize {
    u16::from_le_bytes([b[p], b[p + 1]]) as usize
}
fn u32le(b: &[u8], p: usize) -> usize {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap()) as usize
}

/// Entries of a zip under a directory prefix, by file name.
fn unzip(z: &[u8], prefix: &str) -> Result<HashMap<String, Vec<u8>>> {
    let eocd = (0..z.len().saturating_sub(21))
        .rev()
        .find(|&p| z[p..p + 4] == [0x50, 0x4b, 5, 6])
        .context("no zip directory")?;
    let (n, mut p) = (u16le(z, eocd + 10), u32le(z, eocd + 16));
    let mut out = HashMap::new();
    for _ in 0..n {
        ensure!(
            z[p..p + 4] == [0x50, 0x4b, 1, 2],
            "bad zip central directory"
        );
        let (method, csize, usize_) = (u16le(z, p + 10), u32le(z, p + 20), u32le(z, p + 24));
        let (nlen, xlen, clen) = (u16le(z, p + 28), u16le(z, p + 30), u16le(z, p + 32));
        let local = u32le(z, p + 42);
        let name = String::from_utf8_lossy(&z[p + 46..p + 46 + nlen]).into_owned();
        p += 46 + nlen + xlen + clen;
        let Some(file) = name.strip_prefix(prefix) else {
            continue;
        };
        let data = local + 30 + u16le(z, local + 26) + u16le(z, local + 28);
        let raw = &z[data..data + csize];
        let bytes = match method {
            0 => raw.to_vec(),
            8 => super::deflate::inflate(raw)?.0,
            m => bail!("{name}: zip method {m}"),
        };
        ensure!(
            bytes.len() == usize_,
            "{name}: inflated {} bytes, expected {usize_}",
            bytes.len()
        );
        out.insert(file.to_owned(), bytes);
    }
    Ok(out)
}

/// The NPCF body stores each 256-entry codebook sparsely as 12-byte
/// `(value, offset, 0)` records; sm4revb has three such tables in a row.
/// pfg's first codebook page is the second table, its second page the first.
fn codebooks(body: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut tables = Vec::new();
    let mut p = 0;
    while p + 12 * 256 <= body.len() {
        let run = (0..256).all(|k| {
            let q = p + 12 * k;
            u32le(body, q + 4) == k && u32le(body, q + 8) == 0 && u32le(body, q) < 256
        });
        if run {
            let mut page = vec![0u8; 4096];
            for k in 0..256 {
                page[8 * k] = body[p + 12 * k];
            }
            tables.push(page);
            p += 12 * 256;
        } else {
            p += 1;
        }
    }
    ensure!(
        tables.len() == 3,
        "expected 3 codebooks in sm4revb_npcf.ddb, found {}",
        tables.len()
    );
    for t in &tables[..2] {
        let mut seen = [false; 256];
        for k in 0..256 {
            seen[t[8 * k] as usize] = true;
        }
        ensure!(
            seen.iter().all(|&s| s),
            "a codebook is not a permutation of 0..255"
        );
    }
    Ok((tables[1].clone(), tables[0].clone()))
}

/// Minimal protobuf: [(field, wire value)] where length-delimited fields are
/// byte slices and varints/fixed are numbers.
pub enum Pb<'a> {
    Int(u64),
    Bytes(&'a [u8]),
}

pub fn pb_parse(b: &[u8]) -> Option<Vec<(u64, Pb<'_>)>> {
    fn varint(b: &[u8], p: &mut usize) -> Option<u64> {
        let (mut v, mut s) = (0u64, 0);
        loop {
            let c = *b.get(*p)?;
            *p += 1;
            v |= ((c & 0x7f) as u64) << s;
            s += 7;
            if c < 0x80 {
                return Some(v);
            }
        }
    }
    let (mut out, mut p) = (Vec::new(), 0);
    while p < b.len() {
        let k = varint(b, &mut p)?;
        let (f, wt) = (k >> 3, k & 7);
        if f == 0 {
            return None;
        }
        let v = match wt {
            0 => Pb::Int(varint(b, &mut p)?),
            1 => {
                p += 8;
                Pb::Int(u64::from_le_bytes(b.get(p - 8..p)?.try_into().ok()?))
            }
            5 => {
                p += 4;
                Pb::Int(u32::from_le_bytes(b.get(p - 4..p)?.try_into().ok()?) as u64)
            }
            2 => {
                let n = varint(b, &mut p)? as usize;
                p += n;
                Pb::Bytes(b.get(p - n..p)?)
            }
            _ => return None,
        };
        out.push((f, v));
    }
    Some(out)
}

const RIF_FILES: usize = 3;
/// Frame slots per RIF file: file t covers image frames 1,568·t onwards.
const RIF_SLOTS: usize = 1568;

/// The `S4_SI (frame, bit, flag, value)` records of one RIF body. They are
/// stored raw, 10 bytes each, in one run per frame slot with the bits
/// ascending; the `S4_SII`/`S4_SII_BIT` objects around them only index the
/// runs (README (o)). Every record's frame is its slot and its bit its own
/// key, so its condition (that bit set in that frame) always holds and a
/// key's presence alone flags the pair. The nibble pfg writes is
/// `value << 2 | 1 << flag | 1` (`FBGEN_ND_CRAM_FRAME_MGR::fill_core_frame_data`).
fn rif_lists(b: &[u8], t: usize, out: &mut [Vec<(u16, u8)>]) -> Result<()> {
    let rec = |p: usize| (u32le(b, p), u32le(b, p + 4), b[p + 8], b[p + 9]);
    let ok = |(f, bit, fl, v): (usize, usize, u8, u8)| {
        f < RIF_FILES * RIF_SLOTS && bit < 4320 && fl <= 1 && v <= 3
    };
    let mut p = 0;
    while p + 10 <= b.len() {
        let first = rec(p);
        if ok(first) {
            let (mut q, mut last, mut run) = (p, None, Vec::new());
            while q + 10 <= b.len() {
                let r = rec(q);
                if !ok(r) || r.0 != first.0 || last.is_some_and(|l| r.1 <= l) {
                    break;
                }
                run.push(r);
                last = Some(r.1);
                q += 10;
            }
            // Shorter runs are BIT records read out of step.
            if run.len() >= 8 {
                let f = first.0;
                ensure!(
                    (RIF_SLOTS * t..RIF_SLOTS * (t + 1)).contains(&f),
                    "RIF t{t}: frame {f} outside the file's slots"
                );
                ensure!(out[f].is_empty(), "RIF t{t}: frame {f} listed twice");
                out[f] = run
                    .iter()
                    .map(|&(_, bit, fl, v)| (bit as u16, v << 2 | 1 << fl | 1))
                    .collect();
                p = q;
                continue;
            }
        }
        p += 1;
    }
    Ok(())
}

/// sof_meta.pb field 4 → 2 (`sectorbnxplv_dr`) → each field 3 entry's field 2.
fn s4_positions(meta: &[u8]) -> Result<Vec<usize>> {
    for (f, v) in pb_parse(meta).context("sof_meta.pb")? {
        let (4, Pb::Bytes(v)) = (f, v) else { continue };
        for (f2, v2) in pb_parse(v).unwrap_or_default() {
            let (2, Pb::Bytes(v2)) = (f2, v2) else {
                continue;
            };
            let fields = pb_parse(v2).unwrap_or_default();
            if !fields
                .iter()
                .any(|(f, v)| *f == 1 && matches!(v, Pb::Bytes(b) if *b == b"sectorbnxplv_dr"))
            {
                continue;
            }
            let mut out = Vec::new();
            for (g, e) in &fields {
                if let (3, Pb::Bytes(e)) = (g, e) {
                    for (h, x) in pb_parse(e).unwrap_or_default() {
                        if let (2, Pb::Int(x)) = (h, x) {
                            out.push(x as usize);
                        }
                    }
                }
            }
            return Ok(out);
        }
    }
    bail!("sectorbnxplv_dr not found in sof_meta.pb")
}
