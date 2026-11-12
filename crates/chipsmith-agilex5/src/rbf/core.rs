//! The CORE section's data streams: both sectors' CRAM as physical frames
//! with pfg's S4 flag nibbles, compressed the way pfg's 2D engine does it
//! (libs, BBM codes, residual columns; README (g) "CORE section: 2D
//! compression", (l), (o)).

use anyhow::{ensure, Result};

pub const NF: usize = 4704; // frames per sector
pub const FB: usize = 540; // dr bytes per frame
pub const PF: usize = 656; // physical frame bytes
const GROUP: usize = 98;
const HDR: [u8; 8] = [4, 0, 0xa4, 0, 0, 0, 0, 0];

/// Frame groups the stream carries: group 1 is never stored.
pub fn stored_groups() -> impl Iterator<Item = usize> {
    std::iter::once(0).chain(2..48)
}

/// dr bits with no physical bit.
const DROPPED: [(usize, usize); 7] = [
    (256, 275),
    (1664, 1675),
    (2080, 2099),
    (2496, 2515),
    (3392, 3403),
    (3808, 3827),
    (4192, 4215),
];
/// 4-bit dr groups stored as two pad bits, (b+2, b+3), two pad bits, (b, b+1).
const PADDED_PAIRS: [usize; 3] = [4264, 3488, 1760];
/// 32 always-zero physical bits.
const PAD32_AT: usize = 2528;

#[derive(Clone, Copy)]
enum Phys {
    Copy(usize),
    /// An S4 pair (a, b) as a nibble starting at this bit: `0 0 a b`, or
    /// the flag nibble when one side is set and its key is listed. The keys
    /// are the pair's bits in pfg's swizzled frame order (ka for a, kb for b).
    Nib {
        a: usize,
        b: usize,
        ka: u16,
        kb: u16,
    },
}

/// Physical bit → dr bit (README (g), "Physical frame ↔ dr").
pub struct Layout(Vec<Option<Phys>>);

impl Layout {
    pub fn new(s4: &[usize]) -> Result<Layout> {
        let dropped = |d: usize| DROPPED.iter().any(|&(a, b)| (a..=b).contains(&d));
        let groups: std::collections::HashSet<usize> = s4.iter().copied().collect();
        let mut out: Vec<Option<Phys>> = Vec::new();
        let mut d = 4319i64;
        while d >= 0 {
            let u = d as usize;
            if dropped(u) {
                d -= 1;
            } else if u >= 3 && PADDED_PAIRS.contains(&(u - 3)) {
                let b = u - 3;
                out.extend([
                    None,
                    None,
                    Some(Phys::Copy(b + 2)),
                    Some(Phys::Copy(b + 3)),
                    None,
                    None,
                    Some(Phys::Copy(b)),
                    Some(Phys::Copy(b + 1)),
                ]);
                d = b as i64 - 1;
            } else if u >= 7 && groups.contains(&(u - 7)) {
                let g = u - 7;
                out.extend([
                    Some(Phys::Copy(g + 5)),
                    Some(Phys::Copy(g + 1)),
                    Some(Phys::Copy(g + 4)),
                    Some(Phys::Copy(g)),
                ]);
                let nib = |a, b, ka: usize| {
                    Some(Phys::Nib {
                        a,
                        b,
                        ka: ka as u16,
                        kb: ka as u16 + 1,
                    })
                };
                out.extend([
                    nib(g + 3, g + 7, g + 2),
                    None,
                    None,
                    None,
                    nib(g + 2, g + 6, g),
                    None,
                    None,
                    None,
                ]);
                d = g as i64 - 1;
            } else {
                out.push(Some(Phys::Copy(u)));
                d -= 1;
            }
        }
        out.splice(PAD32_AT..PAD32_AT, std::iter::repeat_n(None, 32));
        ensure!(
            out.len() == PF * 8,
            "physical layout has {} bits, expected {}",
            out.len(),
            PF * 8
        );
        Ok(Layout(out))
    }

    /// dr frame → physical frame; `flags` is the frame's S4 key list.
    pub fn to_phys(&self, dr: &[u8], flags: &[(u16, u8)]) -> Vec<u8> {
        let bit = |d: usize| (dr[d >> 3] >> (d & 7)) & 1;
        let mut out = vec![0u8; PF];
        let mut set = |pb: usize, v: u8| out[pb >> 3] |= v << (pb & 7);
        for (pb, m) in self.0.iter().enumerate() {
            match *m {
                Some(Phys::Copy(d)) => set(pb, bit(d)),
                Some(Phys::Nib { a, b, ka, kb }) => {
                    let (va, vb) = (bit(a), bit(b));
                    let key = if va == vb {
                        None
                    } else {
                        Some(if va == 1 { ka } else { kb })
                    };
                    match key.and_then(|k| flags.binary_search_by_key(&k, |&(k, _)| k).ok()) {
                        Some(i) => (0..4).for_each(|j| set(pb + j, flags[i].1 >> j & 1)),
                        None => {
                            set(pb + 2, va);
                            set(pb + 3, vb);
                        }
                    }
                }
                None => {}
            }
        }
        out
    }
}

/// Bytes per frame group of one IP column, and the lib stride pfg pads a
/// column to (4 bytes under 12, else 8).
fn lib_stride(w: usize) -> usize {
    if w < 12 {
        w.div_ceil(4) * 4
    } else {
        w.div_ceil(8) * 8
    }
}

/// The BBM page header: the 15 IP types (id, width / 2, 98, 0), a u16, then
/// row 0 (u16 0, 60 nibbles low first): each column's IP id.
const BBM_HEAD: &str = "0105620002056200030c62000408620005086200060462000700620008006200090062000a0062000b0062000c0062000d0062000e0062000f0462000000\
                        0000252141121223212121212141121246121223212121212121212121411212";
const COLS: usize = 60;
const BBM_ROW0: usize = 62;

/// (IP id, byte width) of each of the 60 physical-frame columns.
fn columns() -> Vec<(u8, usize)> {
    let h = super::section::hex(BBM_HEAD);
    let width: std::collections::HashMap<u8, usize> = h[..60]
        .chunks(4)
        .map(|r| (r[0], r[1] as usize * 2))
        .collect();
    (0..COLS)
        .map(|j| {
            let id = h[BBM_ROW0 + 2 + j / 2] >> (4 * (j & 1)) & 15;
            (id, width[&id])
        })
        .collect()
}

/// Row-history classes (README (g)): the column bytes that codes 8 (class 1)
/// and 9 (class 2) XOR with the frame 18 back, for frames 26..97.
fn row_bytes(id: u8, code: u8) -> &'static [usize] {
    match (id, code) {
        (1, 8) => &[8, 9],
        (1, 9) => &[6, 7],
        (2, 8) => &[0, 1],
        (2, 9) => &[2, 3],
        (3, 8) => &[10, 11],
        (3, 9) => &[12, 13],
        (4, 8) => &[4, 5],
        (4, 9) => &[6, 7],
        _ => &[],
    }
}
const ROW_HISTORY: usize = 18;
const ROW_FIRST: usize = 26;

/// `FBGEN_UTIL::lsb_calculate_crc32` with pfg's 0x88010000 table: a
/// reflected CRC with no inversion, chained over a group's frames.
fn crc_8801(data: &[u8], mut c: u32) -> u32 {
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                c >> 1 ^ 0x8801_0000
            } else {
                c >> 1
            };
        }
    }
    c
}

/// What pfg stores for both sectors' CRAM (README (o)): the lib pages, the
/// 0x1f record's lib offsets, and per sector the BBM page and the inflated
/// frame stream.
pub struct Encoded {
    pub libs: Vec<u8>,
    pub offs: [u32; 3],
    pub bbm: [Vec<u8>; 2],
    pub frames: [Vec<u8>; 2],
}

/// pfg's 2D compression (`FBGEN_ND_2D_ENGINE::run`, `FBGEN_GEN::
/// merge_and_create_2d_libs`, `update_with_libs`, `run_combined_target`).
/// A block is one column over one stored 98-frame group. An all-zero block
/// gets code 1. For IP ids 1..4, the libs are the two blocks whose CRC is
/// most frequent over both sectors (ties to the smaller CRC); a block equal
/// to lib k gets 2 + k. Every other block takes whichever of literal (0),
/// XOR lib 0 or 1 (10, 11) and row history class 1 or 2 (8, 9) leaves the
/// most zero bytes, an earlier one winning ties. Ids 5, 6 and 15 stay literal
/// unless the column has been zero since the sector's first stored group.
pub fn encode(lay: &Layout, cram: [&[u8]; 2], s4_flags: &[Vec<(u16, u8)>]) -> Encoded {
    let cols = columns();
    let start: Vec<usize> = cols
        .iter()
        .scan(0, |a, &(_, w)| {
            let s = *a;
            *a += w;
            Some(s)
        })
        .collect();
    let groups: Vec<usize> = stored_groups().collect();
    // phys[s][stored group][frame in group]
    let phys: Vec<Vec<Vec<Vec<u8>>>> = (0..2)
        .map(|s| {
            groups
                .iter()
                .map(|&g| {
                    (GROUP * g..GROUP * (g + 1))
                        .map(|f| lay.to_phys(&cram[s][f * FB..(f + 1) * FB], &s4_flags[f]))
                        .collect()
                })
                .collect()
        })
        .collect();
    let block = |s: usize, g: usize, j: usize| -> Vec<u8> {
        let (a, w) = (start[j], cols[j].1);
        phys[s][g]
            .iter()
            .flat_map(|fr| fr[a..a + w].iter().copied())
            .collect()
    };
    let blocks: Vec<Vec<Vec<Vec<u8>>>> = (0..2)
        .map(|s| {
            (0..groups.len())
                .map(|g| (0..COLS).map(|j| block(s, g, j)).collect())
                .collect()
        })
        .collect();
    let mut code = vec![vec![vec![0u8; COLS]; groups.len()]; 2];
    let all = || {
        (0..2).flat_map(|s| (0..groups.len()).flat_map(move |g| (0..COLS).map(move |j| (s, g, j))))
    };
    // pfg resets a column's zero flag after each group only for ids 1..4;
    // for the others it sticks, so their code 1 needs every earlier group of
    // the column zero too.
    for s in 0..2 {
        for j in 0..COLS {
            let mut zero_so_far = true;
            for g in 0..groups.len() {
                let z = blocks[s][g][j].iter().all(|&b| b == 0);
                zero_so_far &= z;
                if (1..=4).contains(&cols[j].0) && z || zero_so_far {
                    code[s][g][j] = 1;
                }
            }
        }
    }
    let mut libs = Vec::new();
    let mut offs = vec![0usize];
    for id in 1..=4u8 {
        let w = cols.iter().find(|c| c.0 == id).unwrap().1;
        let stride = lib_stride(w);
        let mut count: std::collections::BTreeMap<u32, (usize, (usize, usize, usize))> =
            Default::default();
        for (s, g, j) in all() {
            if cols[j].0 == id && code[s][g][j] != 1 {
                let c = crc_8801(&blocks[s][g][j], 0);
                count.entry(c).or_insert((0, (s, g, j))).0 += 1;
            }
        }
        let mut found: Vec<Vec<u8>> = Vec::new();
        for _ in 0..2 {
            // BTreeMap order is CRC order, so max_by_key's last-wins needs a reversed walk.
            let Some((&c, &(n, at))) = count.iter().rev().max_by_key(|(_, (n, _))| *n) else {
                break;
            };
            if n == 0 {
                break;
            }
            count.get_mut(&c).unwrap().0 = 0;
            let lib = blocks[at.0][at.1][at.2].clone();
            for (s, g, j) in all() {
                if cols[j].0 == id && code[s][g][j] == 0 && blocks[s][g][j] == lib {
                    code[s][g][j] = 2 + found.len() as u8;
                }
            }
            found.push(lib);
        }
        for (s, g, j) in all() {
            if cols[j].0 != id || code[s][g][j] != 0 {
                continue;
            }
            let b = &blocks[s][g][j];
            let zeros = |x: &[u8]| x.iter().filter(|&&v| v == 0).count();
            let (mut best, mut cand) = (zeros(b), 0u8);
            for (k, l) in found.iter().enumerate() {
                let z = b.iter().zip(l).filter(|(x, y)| x == y).count();
                if z > best {
                    (best, cand) = (z, 10 + k as u8);
                }
            }
            for cl in [8u8, 9] {
                let mut r = b.clone();
                for i in ROW_FIRST..GROUP {
                    for &k in row_bytes(id, cl) {
                        r[i * w + k] ^= b[(i - ROW_HISTORY) * w + k];
                    }
                }
                let z = zeros(&r);
                if z > best {
                    (best, cand) = (z, cl);
                }
            }
            code[s][g][j] = cand;
        }
        for lib in &found {
            for fr in lib.chunks(w) {
                libs.extend_from_slice(fr);
                libs.resize(libs.len() + stride - w, 0);
            }
        }
        offs.push(libs.len() / 8);
    }
    let lib_of = |j: usize, k: usize| -> Vec<u8> {
        // lib k of column j's id, unpadded, from the packed lib area
        let id = cols[j].0 as usize;
        let (w, stride) = (cols[j].1, lib_stride(cols[j].1));
        let base = offs[id - 1] * 8 + k * GROUP * stride;
        (0..GROUP)
            .flat_map(|i| {
                libs[base + i * stride..base + i * stride + w]
                    .iter()
                    .copied()
            })
            .collect()
    };
    let mut bbm: [Vec<u8>; 2] = Default::default();
    let mut frames: [Vec<u8>; 2] = Default::default();
    for s in 0..2 {
        let mut p = super::section::hex(BBM_HEAD);
        for group in &code[s][..groups.len()] {
            p.extend_from_slice(&[0, 0]);
            for pair in group.chunks(2) {
                p.push(pair[0] | pair[1] << 4);
            }
        }
        p.resize(4096, 0);
        bbm[s] = p;
        let mut out = Vec::with_capacity(groups.len() * GROUP * (8 + PF));
        let resid: Vec<Vec<Vec<u8>>> = (0..groups.len())
            .map(|g| {
                (0..COLS)
                    .map(|j| {
                        let (b, w) = (&blocks[s][g][j], cols[j].1);
                        match code[s][g][j] {
                            c @ (10 | 11) => b
                                .iter()
                                .zip(lib_of(j, c as usize - 10))
                                .map(|(x, y)| x ^ y)
                                .collect(),
                            c @ (8 | 9) => {
                                let mut r = b.clone();
                                for i in ROW_FIRST..GROUP {
                                    for &k in row_bytes(cols[j].0, c) {
                                        r[i * w + k] ^= b[(i - ROW_HISTORY) * w + k];
                                    }
                                }
                                r
                            }
                            _ => b.clone(),
                        }
                    })
                    .collect()
            })
            .collect();
        for g in 0..groups.len() {
            for i in 0..GROUP {
                out.extend_from_slice(&HDR);
                for j in 0..COLS {
                    let c = code[s][g][j];
                    if c == 0 || c >= 8 {
                        let w = cols[j].1;
                        out.extend_from_slice(&resid[g][j][i * w..(i + 1) * w]);
                    }
                }
            }
        }
        frames[s] = out;
    }
    let bytes = libs.len();
    libs.resize(bytes.div_ceil(4096) * 4096, 0);
    Encoded {
        libs,
        offs: [
            (offs[1] << 16 | offs[0]) as u32,
            (offs[3] << 16 | offs[2]) as u32,
            offs[4] as u32,
        ],
        bbm,
        frames,
    }
}

/// A sector's M20K contents as the CORE stream carries them (README (y)):
/// one record per M20K in the stored row groups (0, 2, 3, …, 47; group 1
/// isn't stored, as for the frames) and the sector's four M20K columns,
/// each `(8, w, 128, 2)`, four zero bytes (w = 12, 16, 20, 25 by column)
/// and 2,560 bytes: address a's 40-bit word at bits 40 (511 − a) .. +39,
/// LSB first. In the `.sof`'s ram part a group is 256 rows of 320 bits,
/// row r holding addresses 2r (bits 40c ..) and 2r + 1 (bits 160 + 40c ..)
/// of column c (`eram_array_r<r>`, db/sm4revb/bits/cw_mm_m20k). With no
/// contents this is the all-zero block every phase-2 specimen had; checked
/// byte for byte against pfg on mq (512 words) and blinky.
pub fn ram_block(ram: &[u8]) -> Vec<u8> {
    let bit = |o: usize| ram[o >> 3] >> (o & 7) & 1;
    let mut out = Vec::with_capacity(482_784);
    for g in std::iter::once(0).chain(2..48) {
        for (c, w) in [12u8, 16, 20, 25].into_iter().enumerate() {
            out.extend_from_slice(&[8, w, 128, 2, 0, 0, 0, 0]);
            let mut rec = vec![0u8; 2560];
            for a in 0..512usize {
                let base = g * 81_920 + (a >> 1) * 320 + (a & 1) * 160 + 40 * c;
                for i in 0..40 {
                    if bit(base + i) == 1 {
                        let k = 40 * (511 - a) + i;
                        rec[k >> 3] |= 1 << (k & 7);
                    }
                }
            }
            out.extend(rec);
        }
    }
    out
}

/// The stream after the second action page: both sector headers, twice,
/// each as a send of the header bits reversed and shifted by 24 bits with a
/// command word in front. pfg overwrites the first `forced` bits of each
/// body: all zero but the listed ones, 224 bits in the first pass (plus a
/// few set bits higher up) and 32 in the second (README (o); measured on
/// random, all-zero and all-one headers).
pub fn sector_header_stream(h0: &[u8], h1: &[u8]) -> Vec<u8> {
    fn body(h: &[u8], forced: usize, set: &[usize]) -> Vec<u8> {
        let n = h.len() * 8;
        let bit = |i: usize| (h[i >> 3] >> (i & 7)) & 1;
        let mut out = vec![0u8; h.len()];
        for j in forced..n {
            if n >= 25 + j && bit(n - 25 - j) == 1 {
                out[j >> 3] |= 1 << (j & 7);
            }
        }
        for &j in set {
            out[j >> 3] |= 1 << (j & 7);
        }
        out
    }
    /// One send: the header, how many bits pfg forces, which of those it
    /// sets, and the command word's flags.
    type Send<'a> = (&'a [u8], usize, &'a [usize], u32);
    let passes: [[Send; 2]; 2] = [
        [
            (
                h0,
                224,
                &[2, 3, 6, 19, 21, 23, 26, 27, 832, 1885, 2134, 3183],
                0,
            ),
            (h1, 224, &[2, 3, 6, 19, 21, 23, 27, 768, 921, 1494, 2095], 1),
        ],
        [
            (h0, 32, &[6, 19, 21, 23, 26, 27], 0),
            (h1, 32, &[6, 19, 21, 23, 27], 1),
        ],
    ];
    let mut out = Vec::new();
    for pass in passes {
        for (h, forced, set, flags) in pass {
            let b = body(h, forced, set);
            let n = (b.len() / 4) as u32;
            out.extend_from_slice(&(flags << 8 | n << 16).to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&b);
            if n & 1 == 1 {
                out.extend_from_slice(&[0; 4]);
            }
        }
        for w in [0x0064_000du32, 0, 0x0004_000c, 0] {
            out.extend_from_slice(&w.to_le_bytes());
        }
    }
    out
}
