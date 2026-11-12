//! SDM design-section assembly (README (g2), (l)): 4 KiB pages, a header
//! block, a block holding the header's SHA-384, and SHA-384 chain tables
//! listing every hashed page with a flag (stream id).

use anyhow::{ensure, Result};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha384};

pub const PAGE: usize = 4096;

/// Chain entry flags.
pub const FLAG_MODULE: u32 = 0x8000_0002;
pub const FLAG_ACTION: u32 = 2;
pub const FLAG_LINK: u32 = 4;

pub enum Page {
    Hashed(Vec<u8>, u32),
    Plain(Vec<u8>),
}

/// A chain table holds at most 78 entries. A longer chain puts the link to
/// the next table at entry 57, with the table's page in that place in the
/// file, and 20 more entries after it.
const TABLE_MAX: usize = 78;
const LINK_AT: usize = 57;

fn table_page(entries: &[([u8; 48], u32)]) -> Vec<u8> {
    let mut p = vec![0u8; PAGE];
    for (i, w) in [
        1u32,
        16 + 52 * entries.len() as u32,
        entries.len() as u32,
        0,
    ]
    .iter()
    .enumerate()
    {
        p[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
    }
    for (i, (h, f)) in entries.iter().enumerate() {
        let o = 16 + 52 * i;
        p[o..o + 48].copy_from_slice(h);
        p[o + 48..o + 52].copy_from_slice(&f.to_le_bytes());
    }
    p
}

fn sha384(b: &[u8]) -> [u8; 48] {
    Sha384::digest(b).into()
}

/// CRC-32/BZIP2 (MSB first, poly 0x04C11DB7, init and xorout 0xFFFFFFFF).
pub fn crc32_bzip2(b: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &x in b {
        c ^= (x as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04c1_1db7
            } else {
                c << 1
            };
        }
    }
    !c
}

fn put(b: &mut [u8], at: usize, v: &[u8]) {
    b[at..at + v.len()].copy_from_slice(v);
}

/// Pages from +0x2000 on (the first chain table, then everything in file
/// order with the link tables placed), built back to front because each
/// table holds the hash of the next.
pub(crate) fn chain(pages: Vec<Page>) -> Vec<u8> {
    let hashed: Vec<usize> = pages
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Page::Hashed(..)))
        .map(|(i, _)| i)
        .collect();
    // Split the hashed pages into tables: (first item, item count).
    let mut tables = Vec::new();
    let mut at = 0;
    while hashed.len() - at > TABLE_MAX {
        tables.push((at, TABLE_MAX - 1));
        at += TABLE_MAX - 1;
    }
    tables.push((at, hashed.len() - at));
    let entry = |i: usize| match &pages[i] {
        Page::Hashed(b, f) => (sha384(b), *f),
        Page::Plain(_) => unreachable!(),
    };
    // Link table pages, last first.
    let mut links: Vec<Vec<u8>> = vec![Vec::new(); tables.len()];
    for t in (0..tables.len()).rev() {
        let (first, n) = tables[t];
        let mut e: Vec<([u8; 48], u32)> = (first..first + n).map(|k| entry(hashed[k])).collect();
        if t + 1 < tables.len() {
            e.insert(LINK_AT, (sha384(&links[t + 1]), FLAG_LINK));
        }
        links[t] = table_page(&e);
    }
    // Where each link page goes: before the hashed item that follows the link.
    let mut before: std::collections::HashMap<usize, usize> = Default::default();
    for t in 1..tables.len() {
        let (first, _) = tables[t - 1];
        before.insert(hashed[first + LINK_AT], t);
    }
    let mut out = links[0].clone();
    for (i, p) in pages.into_iter().enumerate() {
        if let Some(&t) = before.get(&i) {
            out.extend_from_slice(&links[t]);
        }
        match p {
            Page::Hashed(b, _) | Page::Plain(b) => out.extend_from_slice(&b),
        }
    }
    out
}

pub enum Kind {
    Io,
    Core,
}

pub struct Fields<'a> {
    pub kind: Kind,
    pub design: &'a super::Design,
    /// Byte sum of the image's object headers and both sectors' CRAM.
    pub sum: u32,
    /// Sections after this one (+0x1F00).
    pub following: u32,
}

/// Header block constants, the same in every specimen (README (l)).
const COMMON: &[(usize, &str)] = &[
    (0x000, "93655697"),
    (
        0x021,
        "01011a4d41494e050000053f0000009ee61ab40001011a82000000000000000e000000ffffffff",
    ),
    (0x070, "8511"),
    (0x07c, "ddc062c3ddc062c3ffffff7f"),
    (
        0x160,
        "633625e59d43a4a39e2cb282a2998fe549742695defbc45ec77f80ff06ad4173",
    ),
];
const IO_HDR: &[(usize, &str)] = &[
    (0x00c, "494f"),
    (
        0x090,
        "ffffffff405973070014000900e1f50560020000000000000002000020023003400301",
    ),
    (0x144, "23000000c00670030f00000078"),
    (0x201, "020202020202020202000000000002"),
    (0x220, "02000a0a00000e"),
    (0x23d, "02f400dd0ad40e000000ff0008"),
    (0x330, "10"),
    (0x340, "2c2c"),
];
const CORE_HDR: &[(usize, &str)] = &[
    (0x00c, "434f5245"),
    (0x090, "ffffffff0000000000140009"),
    (0x0b1, "0201002c0201"),
    (0x200, "2c2c0001ffffffff"),
    (0x22c, "2c2c"),
];

pub fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Assemble a design section from its pages (everything after the first
/// chain table, in file order).
pub fn assemble(pages: Vec<Page>, f: &Fields, keyed_hash_key: &[u8]) -> Result<Vec<u8>> {
    let body = chain(pages);
    let size = 2 * PAGE + body.len();
    let mut h = vec![0u8; PAGE];
    let consts = match f.kind {
        Kind::Io => IO_HDR,
        Kind::Core => CORE_HDR,
    };
    for (at, v) in COMMON.iter().chain(consts) {
        put(&mut h, *at, &hex(v));
    }
    put(&mut h, 0x78, &f.sum.to_le_bytes());
    let d = f.design;
    match f.kind {
        Kind::Io => {
            put(&mut h, 0x9c, &d.hdr_clk_hz.to_le_bytes());
            put(&mut h, 0x348, &d.io_hash);
        }
        Kind::Core => {
            put(&mut h, 0x1e0, &d.design_hash);
            put(&mut h, 0x234, &d.io_hash);
        }
    }
    if let super::Boot::HpsFirst { hps_io_hash } = &d.boot {
        // pfg's HPS-first core.rbf (designs/hpsf; README, "HPS-first").
        // +0xB8, +0x184, IO +0x20B/+0x210 are the same in the FPGA-first
        // build of that design, +0x190 is 3 there: they mark an HPS, +0x190
        // bit 3 HPS-first. IO +0xB4 points at the record at +0x36C, which
        // holds phase 1's HPS IO hash. Their meaning is [inf].
        put(&mut h, 0xb8, &0x002f_0000u32.to_le_bytes());
        put(&mut h, 0x184, &0x0000_0400u32.to_le_bytes());
        put(&mut h, 0x190, &0x0bu32.to_le_bytes());
        if let Kind::Io = f.kind {
            put(&mut h, 0xb4, &0x0001_036cu32.to_le_bytes());
            put(&mut h, 0x20b, &[0xc1]);
            put(&mut h, 0x210, &[0x80]);
            put(&mut h, 0x36c, &hex("2c2c0003"));
            put(&mut h, 0x374, hps_io_hash);
        }
    }
    let out = seal(h, &body, f.following, keyed_hash_key)?;
    ensure!(out.len() == size);
    Ok(out)
}

/// A section from its header page (everything but what depends on the
/// body) and its body: the body's size and hashes and the keyed hash go
/// into the header, then the block holding the header's hash.
pub fn seal(mut h: Vec<u8>, body: &[u8], following: u32, keyed_hash_key: &[u8]) -> Result<Vec<u8>> {
    ensure!(h.len() == PAGE, "a section header is one page");
    ensure!(
        body.len() >= PAGE,
        "a section body starts with a chain table page"
    );
    put(&mut h, 0x8, &((2 * PAGE + body.len()) as u32).to_le_bytes());
    put(&mut h, 0x3c, &Sha256::digest(body)[..4]);
    put(&mut h, 0xc0, &sha384(&body[..PAGE]));
    let mut mac = Hmac::<Sha256>::new_from_slice(keyed_hash_key)?;
    mac.update(&h[..0xbb0]);
    put(&mut h, 0xbb0, &mac.finalize().into_bytes());
    put(&mut h, 0xbd0, &sha384(body));
    let crc = crc32_bzip2(&h[..0xffc]);
    put(&mut h, 0xffc, &crc.to_le_bytes());
    let mut b1 = vec![0u8; PAGE];
    put(&mut b1, 0, &sha384(&h));
    put(&mut b1, 0xf00, &following.to_le_bytes());
    let crc = crc32_bzip2(&b1[..0xffc]);
    put(&mut b1, 0xffc, &crc.to_le_bytes());
    let mut out = h;
    out.extend_from_slice(&b1);
    out.extend_from_slice(body);
    Ok(out)
}

/// A firmware module as the sections carry it: its pages without the last
/// (`MFST` manifest) page; the first is hashed as a module header.
pub fn module(fw: &[u8]) -> Result<Vec<Page>> {
    ensure!(
        fw.len().is_multiple_of(PAGE) && fw.len() >= 2 * PAGE,
        "firmware module is not whole pages"
    );
    let n = fw.len() / PAGE - 1;
    Ok((0..n)
        .map(|k| {
            let p = fw[k * PAGE..(k + 1) * PAGE].to_vec();
            if k == 0 {
                Page::Hashed(p, FLAG_MODULE)
            } else {
                Page::Plain(p)
            }
        })
        .collect())
}

/// An action page from a record table; `streams` gives, for each 0x17
/// record in order, the page count and the per-page bitmap bits.
pub fn action_page(recs: &[(u32, &[u32])], streams: &[(u32, Vec<bool>)]) -> Result<Vec<u8>> {
    let mut p = Vec::new();
    let mut s = streams.iter();
    for (t, words) in recs {
        let mut w = words.to_vec();
        if *t == 0x17 {
            let (n, bits) = s
                .next()
                .ok_or_else(|| anyhow::anyhow!("more 0x17 records than streams"))?;
            w[0] = *n;
            let mut bm = vec![0u32; bits.len().div_ceil(32).max(1)];
            for (i, b) in bits.iter().enumerate() {
                bm[i / 32] |= (*b as u32) << (i % 32);
            }
            w.extend(bm);
        }
        p.extend_from_slice(&t.to_le_bytes());
        p.extend_from_slice(&(8 + 4 * w.len() as u32).to_le_bytes());
        for x in w {
            p.extend_from_slice(&x.to_le_bytes());
        }
    }
    ensure!(s.next().is_none(), "fewer 0x17 records than streams");
    ensure!(p.len() <= PAGE, "action page overflows");
    p.resize(PAGE, 0);
    Ok(p)
}

/// A deflated stream as zero-padded hashed pages with one flag.
pub fn stream_pages(deflated: &[u8]) -> Vec<Vec<u8>> {
    deflated
        .chunks(PAGE)
        .map(|c| {
            let mut p = c.to_vec();
            p.resize(PAGE, 0);
            p
        })
        .collect()
}
