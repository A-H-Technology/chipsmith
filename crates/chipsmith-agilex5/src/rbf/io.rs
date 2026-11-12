//! The IO section's six design streams from the image (README (g), "IO
//! section"; a port of `tools/io_decode.py encode`).

use anyhow::Result;

use crate::db::Db;

/// (object, part) offsets and sizes in the image.
pub fn part(obj: &str, part: &str) -> (usize, usize) {
    crate::db::PARTS
        .iter()
        .find(|(o, p, _, _)| *o == obj && *p == part)
        .map(|&(_, _, s, n)| (s, n))
        .unwrap()
}

fn img_part<'a>(img: &'a [u8], obj: &str, p: &str) -> &'a [u8] {
    let (s, n) = part(obj, p);
    &img[s..s + n]
}

const MAX_N: usize = 509; // words in one send: at most 2048 bytes with its header
const CTL_PD: [(u32, u32); 2] = [(0x0400_000d, 0), (0x3c00_000c, 0)];
const CTL_IOHDR: [(u32, u32); 1] = [(0x00c8_000c, 0)];

fn send(out: &mut Vec<u8>, target: u32, flags: u32, body: &[u8]) {
    let n = body.len() / 4;
    assert!(body.len() == 4 * n && n < 0x10000);
    out.extend_from_slice(&(target | flags << 8 | (n as u32) << 16).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(body);
    if n & 1 == 1 {
        out.extend_from_slice(&[0; 4]);
    }
}

fn ctl(out: &mut Vec<u8>, pairs: &[(u32, u32)]) {
    for (a, b) in pairs {
        out.extend_from_slice(&a.to_le_bytes());
        out.extend_from_slice(&b.to_le_bytes());
    }
}

fn packet(records: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (i, r) in records {
        body.extend_from_slice(&(r.len() as u32 + 8).to_le_bytes());
        body.extend_from_slice(&i.to_le_bytes());
        body.extend_from_slice(r);
    }
    let mut out = Vec::new();
    for w in [
        32 + body.len() as u32,
        0x20,
        0,
        records.len() as u32,
        0,
        0,
        0,
        0,
    ] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out.extend(body);
    out
}

// ---- perictrl0 data: the register write list --------------------------

/// The perictrl0 registers pfg sends: every 32-bit word holding an
/// `sm_hssi_top` named bit, except eight control words the firmware owns
/// (README (l)). Returned as merged (byte address, length) ranges.
const PD_SKIP: [usize; 8] = [
    0xa1000, 0x20f000, 0x20f004, 0x20f034, 0x20f114, 0x20f120, 0x20f124, 0x20f128,
];

pub fn pd_ranges(db: &mut Db) -> Result<Vec<(usize, usize)>> {
    db.load("sm_hssi_top")?;
    let words = db.map("sm_hssi_top").byte_offsets("hssi_mono_0_0", "data");
    let (base, _) = part("perictrl0", "data");
    let mut w: Vec<usize> = words.into_iter().map(|b| (b - base) / 4 * 4).collect();
    w.sort_unstable();
    w.dedup();
    w.retain(|a| !PD_SKIP.contains(a));
    let mut out: Vec<(usize, usize)> = Vec::new();
    for a in w {
        match out.last_mut() {
            Some((s, l)) if *s + *l == a => *l += 4,
            _ => out.push((a, 4)),
        }
    }
    Ok(out)
}

fn pd_stream(data: &[u8], ranges: &[(usize, usize)]) -> Vec<u8> {
    let cap = 4 * MAX_N - 8;
    let (mut out, mut cur) = (Vec::new(), Vec::new());
    let close = |out: &mut Vec<u8>, cur: &mut Vec<u8>| {
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&(8 + cur.len() as u32).to_le_bytes());
        b.append(cur);
        send(out, 0, 0, &b);
        ctl(out, &CTL_PD);
    };
    for &(a0, l0) in ranges {
        let (mut a, mut l) = (a0, l0);
        while l > 0 {
            if cap - cur.len() < 12 {
                close(&mut out, &mut cur);
            }
            let take = l.min((cap - cur.len() - 8) / 4 * 4);
            cur.extend_from_slice(&(a as u32).to_le_bytes());
            cur.extend_from_slice(&(take as u32).to_le_bytes());
            cur.extend_from_slice(&data[a..a + take]);
            a += take;
            l -= take;
        }
    }
    close(&mut out, &mut cur);
    out
}

// ---- perictrl headers ---------------------------------------------------

const PC_TAIL: usize = 24; // the last 24 header bytes are not sent

fn rev8(b: u8) -> u8 {
    b.reverse_bits()
}

fn pc_hdr_sends(out: &mut Vec<u8>, target: u32, hdr: &[u8]) {
    let mut pl = vec![0u8; 8];
    pl.extend(hdr[..hdr.len() - PC_TAIL].iter().rev().map(|&b| rev8(b)));
    pl.extend_from_slice(&[0, 0]);
    let pk = packet(&[(0, pl)]);
    let mut off = 0;
    while off < pk.len() {
        let end = (off + 4 * MAX_N).min(pk.len());
        let mut b = (off as u32).to_le_bytes().to_vec();
        b.extend_from_slice(&pk[off..end]);
        send(out, target, (end == pk.len()) as u32, &b);
        off = end;
    }
}

// ---- io96 headers: 10 blocks of bit fields --------------------------------

const IO_C: [usize; 10] = [1472, 1472, 1472, 1472, 1472, 1472, 1472, 1472, 744, 202];
const IO_D: [Option<usize>; 10] = [
    Some(1401),
    Some(1467),
    Some(1401),
    Some(1401),
    Some(1401),
    Some(1467),
    Some(1401),
    Some(1401),
    None,
    None,
];

/// [kind A, B, C, D][block] → (start bit, bits)
fn io_fields() -> [[Option<(usize, usize)>; 10]; 4] {
    let mut f = [[None; 10]; 4];
    let mut pos = 0;
    for k in 0..10 {
        for (kind, bits) in [
            (0, Some(800)),
            (1, Some(1472)),
            (2, Some(IO_C[k])),
            (3, IO_D[k]),
        ] {
            if let Some(bits) = bits {
                f[kind][k] = Some((pos, bits));
                pos += bits;
            }
        }
        pos = pos.div_ceil(8) * 8;
    }
    f
}

fn io_field_payload(hdr: &[u8], field: Option<(usize, usize)>, rlen: usize) -> Vec<u8> {
    let mut pl = vec![0u8; rlen - 8];
    let Some((s, bits)) = field else { return pl };
    let nw = bits.div_ceil(32);
    let bit = |i: usize| (hdr[(s + i) / 8] >> ((s + i) % 8)) & 1;
    let words: Vec<u32> = (0..nw)
        .map(|j| {
            (0..32)
                .filter(|&b| 32 * j + b < bits)
                .fold(0u32, |w, b| w | (bit(32 * j + b) as u32) << b)
        })
        .collect();
    let at = pl.len() - 4 * nw;
    for (j, w) in words.iter().rev().enumerate() {
        pl[at + 4 * j..at + 4 * j + 4].copy_from_slice(&w.to_le_bytes());
    }
    pl
}

fn io_hdr_stream(hdrs: &[&[u8]]) -> Vec<u8> {
    let f = io_fields();
    let mut out = Vec::new();
    for (kind, rlen) in [(3, 200), (2, 200), (1, 200), (0, 116)] {
        for (t, hdr) in hdrs.iter().enumerate() {
            let recs: Vec<(u32, Vec<u8>)> = (0..10)
                .map(|k| (k as u32, io_field_payload(hdr, f[kind][k], rlen)))
                .collect();
            send(&mut out, t as u32, 0, &packet(&recs));
            ctl(&mut out, &CTL_IOHDR);
        }
    }
    out
}

// ---- io96 t40/t41 -----------------------------------------------------

const T41_HEAD: usize = 1024;
const T40_ADDR: usize = 0x47b000;

fn t41_blocks() -> Vec<(usize, usize, usize)> {
    let mut out = vec![(0, 0, T41_HEAD)];
    for a in (0x400000..0x480000).step_by(0x800) {
        if (T40_ADDR..0x47f000).contains(&a) {
            continue;
        }
        out.push((
            a,
            T41_HEAD + a - 0x400000 + if a >= 0x47f000 { 0x1000 } else { 0 },
            0x800,
        ));
    }
    out
}

fn t40_blocks() -> Vec<(usize, usize, usize)> {
    (0..part("io96_0", "t40").1)
        .step_by(0x800)
        .map(|o| (T40_ADDR + o, o, 0x800))
        .collect()
}

fn mem_stream(blocks: &[(usize, usize, usize)], datas: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, &(a, o, l)) in blocks.iter().enumerate() {
        for (t, d) in datas.iter().enumerate() {
            let mut b = (a as u32).to_le_bytes().to_vec();
            b.extend_from_slice(&d[o..o + l]);
            send(&mut out, t as u32, (i == blocks.len() - 1) as u32, &b);
        }
    }
    out
}

/// The six inflated design streams, in chain order. The IO96 streams cover
/// the first `banks` banks (2, or 1 when io96_1 is the HPS EMIF's).
pub fn streams(img: &[u8], ranges: &[(usize, usize)], banks: usize) -> [Vec<u8>; 6] {
    let p = |o: &str, q: &str| img_part(img, o, q);
    let mut pc21 = Vec::new();
    pc_hdr_sends(&mut pc21, 0, p("perictrl2", "hdr"));
    pc_hdr_sends(&mut pc21, 1, p("perictrl1", "hdr"));
    let mut pc0 = Vec::new();
    pc_hdr_sends(&mut pc0, 0, p("perictrl0", "hdr"));
    [
        pd_stream(p("perictrl0", "data"), ranges),
        pc21,
        pc0,
        io_hdr_stream(&[p("io96_0", "hdr"), p("io96_1", "hdr")][..banks]),
        mem_stream(
            &t41_blocks(),
            &[p("io96_0", "t41"), p("io96_1", "t41")][..banks],
        ),
        mem_stream(
            &t40_blocks(),
            &[p("io96_0", "t40"), p("io96_1", "t40")][..banks],
        ),
    ]
}
