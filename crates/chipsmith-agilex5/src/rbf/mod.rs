//! `.rbf` writer: a `.sof` image to the bitstream quartus_pfg would make,
//! without Quartus (README (l), (o)). The CMF section is the install's
//! signed `main.cmf` with one word and its block CRC patched; the IO and
//! CORE sections are pfg's byte for byte (CORE through pfg's own 2D
//! compression, S4 flags and page interleave).

pub mod core;
pub mod deflate;
pub mod install;
pub mod io;
pub mod phase1;
pub mod section;
mod tables;

use anyhow::{ensure, Context, Result};

use crate::db::Db;
use install::{pb_parse, Install, Pb};
use section::{Fields, Kind, Page, FLAG_ACTION, PAGE};

pub const RBF_MAGIC: u32 = 0x6229_4895;

/// How the image boots, from OPTIONS 1.9 (README, "HPS-first").
pub enum Boot {
    /// No HPS: the SDM configures IO and CORE at power-on.
    FpgaOnly,
    /// `HPS_INITIALIZATION "HPS FIRST"`: phase 1 (HPS I/O, HPS EMIF, SPL)
    /// is loaded on its own, and this is phase 2, the `core.rbf` the HPS
    /// hands to the SDM. Its IO section leaves out the HPS EMIF's IO96 bank
    /// (io96_1) and names phase 1 by its HPS IO hash.
    HpsFirst { hps_io_hash: [u8; 32] },
}

/// What the section headers take from the .sof's OPTIONS record (the
/// 0x3a record after the image, behind its u32 index; README (g2)).
pub struct Design {
    /// "IO hash", hex text in field 1.20 (both sections).
    pub io_hash: [u8; 32],
    /// "Design hash", raw in record 0x36 field 2.10 (CORE).
    pub design_hash: [u8; 32],
    /// Field 1.2, header +0x9C: 100 MHz without an HPS, 125 MHz with the
    /// GHRD's (both builds of designs/hpsf). What it clocks is [unk].
    pub hdr_clk_hz: u32,
    pub boot: Boot,
}

fn hex32(t: &[u8]) -> Result<[u8; 32]> {
    ensure!(
        t.len() == 64 && t.iter().all(u8::is_ascii_hexdigit),
        "not 64 hex digits"
    );
    let s = std::str::from_utf8(t)?;
    let mut h = [0u8; 32];
    for (i, b) in h.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)?;
    }
    Ok(h)
}

pub fn sof_design(records: &[(u16, &[u8])]) -> Result<Design> {
    fn msg(b: &[u8], f: u64) -> Option<Vec<(u64, Pb<'_>)>> {
        let fs = pb_parse(b).or_else(|| pb_parse(b.get(4..)?))?;
        fs.into_iter().find_map(|(g, v)| match v {
            Pb::Bytes(x) if g == f => pb_parse(x),
            _ => None,
        })
    }
    let bytes = |m: &[(u64, Pb<'_>)], f: u64| -> Option<Vec<u8>> {
        m.iter().find_map(|(g, v)| match v {
            Pb::Bytes(x) if *g == f => Some(x.to_vec()),
            _ => None,
        })
    };
    let int = |m: &[(u64, Pb<'_>)], f: u64| -> Option<u64> {
        m.iter().find_map(|(g, v)| match v {
            Pb::Int(x) if *g == f => Some(*x),
            _ => None,
        })
    };
    let opts = records
        .iter()
        .skip(1)
        .filter(|(t, _)| *t == 0x3a)
        .find_map(|(_, d)| msg(d, 1).filter(|m| bytes(m, 20).is_some_and(|t| t.len() == 64)))
        .context("no OPTIONS record (0x3a with field 1.20)")?;
    let design = records
        .iter()
        .filter(|(t, _)| *t == 0x36)
        .find_map(|(_, d)| msg(d, 2).and_then(|m| bytes(&m, 10)))
        .context("no design hash (record 0x36 field 2.10)")?;
    let boot_opts = bytes(&opts, 9)
        .and_then(|b| pb_parse(&b).map(|m| (int(&m, 1), int(&m, 3))))
        .context("no OPTIONS 1.9")?;
    // 1.9.1: the design has an HPS; 1.9.3: the SDM configures the fabric
    // before the HPS (1 in FPGA-only designs too).
    let boot = match boot_opts {
        (Some(0) | None, _) => Boot::FpgaOnly,
        (Some(_), Some(0) | None) => Boot::HpsFirst {
            hps_io_hash: hex32(
                &bytes(&opts, 21).context("HPS-first, but no HPS IO hash (OPTIONS 1.21)")?,
            )?,
        },
        (Some(_), Some(_)) => {
            anyhow::bail!("FPGA-first with an HPS: a5db does not write the HPS section")
        }
    };
    Ok(Design {
        io_hash: hex32(&bytes(&opts, 20).unwrap())?,
        design_hash: design
            .try_into()
            .ok()
            .context("design hash is not 32 bytes")?,
        hdr_clk_hz: int(&opts, 2).context("no OPTIONS 1.2")? as u32,
        boot,
    })
}

/// The CMF section: main.cmf with +0x1F00 = number of sections after it and
/// block 1's CRC recomputed.
fn cmf(inst: &Install, following: u32) -> Result<Vec<u8>> {
    let mut c = inst.fw("main.cmf")?.to_vec();
    c[0x1f00..0x1f04].copy_from_slice(&following.to_le_bytes());
    let crc = section::crc32_bzip2(&c[0x1000..0x1ffc]);
    c[0x1ffc..0x2000].copy_from_slice(&crc.to_le_bytes());
    Ok(c)
}

fn hashed(pages: Vec<Vec<u8>>, flag: u32) -> impl Iterator<Item = Page> {
    pages.into_iter().map(move |p| Page::Hashed(p, flag))
}

fn io_section(db: &mut Db, inst: &Install, img: &[u8], f: &Fields, key: &[u8]) -> Result<Vec<u8>> {
    let ranges = io::pd_ranges(db)?;
    let hps_first = matches!(f.design.boot, Boot::HpsFirst { .. });
    // HPS-first leaves the HPS EMIF's bank (io96_1) and io_config.fw to phase 1.
    let banks = if hps_first { 1 } else { 2 };
    let streams = io::streams(img, &ranges, banks);
    let pages: Vec<Vec<Vec<u8>>> = streams
        .iter()
        .map(|s| Ok(section::stream_pages(&deflate::deflate(s)?)))
        .collect::<Result<_>>()?;
    let count = |i: usize| (pages[i].len() as u32, vec![false; pages[i].len()]);
    let act1 = section::action_page(tables::IO_ACT1, &[count(0), count(1), count(2)])?;
    let act2 = section::action_page(
        if hps_first {
            tables::IO_ACT2_HPS_FIRST
        } else {
            tables::IO_ACT2
        },
        &[count(3), count(4), count(5)],
    )?;
    let mut out: Vec<Page> = Vec::new();
    out.extend(section::module(inst.fw("enable_pm.fw")?)?);
    out.extend(section::module(inst.fw("cssm.fw")?)?);
    out.push(Page::Hashed(act1, FLAG_ACTION));
    let mut pages = pages.into_iter();
    for _ in 0..3 {
        out.extend(hashed(pages.next().unwrap(), 0));
    }
    out.extend(section::module(inst.fw("cssm_reset_release.fw")?)?);
    out.extend(section::module(inst.fw("cssm_ux_phy.fw")?)?);
    if !hps_first {
        out.extend(section::module(inst.fw("io_config.fw")?)?);
    }
    out.push(Page::Hashed(act2, FLAG_ACTION));
    for p in pages {
        out.extend(hashed(p, 0));
    }
    section::assemble(out, f, key)
}

fn core_section(inst: &Install, img: &[u8], f: &Fields, key: &[u8]) -> Result<Vec<u8>> {
    let lay = core::Layout::new(&inst.s4)?;
    let (s0, n) = io::part("sector0", "cram");
    let (s1, _) = io::part("sector1", "cram");
    ensure!(n == core::NF * core::FB);
    let enc = core::encode(&lay, [&img[s0..s0 + n], &img[s1..s1 + n]], &inst.s4_flags);
    let offs = enc.offs;
    let lib_pages: Vec<Vec<u8>> = enc.libs.chunks(PAGE).map(<[u8]>::to_vec).collect();
    // Per sector: codebook, M20K contents, second codebook, BBM, then the
    // frame stream. The contents' pages go in the frame pages' interleave.
    let ram: Vec<Vec<u8>> = ["sector0", "sector1"]
        .iter()
        .map(|o| {
            let (s, n) = io::part(o, "ram");
            deflate::deflate(&core::ram_block(&img[s..s + n]))
        })
        .collect::<Result<_>>()?;
    let rpages: Vec<Vec<Vec<u8>>> = ram.iter().map(|d| section::stream_pages(d)).collect();
    let deflated: Vec<Vec<u8>> = enc
        .frames
        .iter()
        .map(|f| deflate::deflate(f))
        .collect::<Result<_>>()?;
    let fpages: Vec<Vec<Vec<u8>>> = deflated.iter().map(|d| section::stream_pages(d)).collect();
    let frames = [fpages[0].len(), fpages[1].len()];
    // Lib pages sector by sector, the setup pages alternating, then the frame
    // pages in pfg's interleave.
    let mut data: Vec<Page> = Vec::new();
    for s in 0..2 {
        data.extend(hashed(lib_pages.clone(), s));
    }
    for s in 0..2 {
        data.push(Page::Hashed(inst.cb1.clone(), s as u32));
    }
    let mut ram_bitmap = Vec::new();
    let mut next = [0usize; 2];
    for s in interleave([ram[0].len(), ram[1].len()]) {
        ram_bitmap.push(s == 1);
        data.push(Page::Hashed(rpages[s][next[s]].clone(), s as u32));
        next[s] += 1;
    }
    ensure!(
        next == [rpages[0].len(), rpages[1].len()],
        "interleave placed {next:?} contents pages"
    );
    for s in 0..2 {
        data.push(Page::Hashed(inst.cb2.clone(), s as u32));
    }
    for s in 0..2 {
        data.push(Page::Hashed(enc.bbm[s].clone(), s as u32));
    }
    let mut bitmap = Vec::new();
    let mut next = [0usize; 2];
    for s in interleave([deflated[0].len(), deflated[1].len()]) {
        bitmap.push(s == 1);
        data.push(Page::Hashed(fpages[s][next[s]].clone(), s as u32));
        next[s] += 1;
    }
    ensure!(
        next == frames,
        "interleave placed {next:?} of {frames:?} frame pages"
    );
    ensure!(bitmap.len() == frames[0] + frames[1]);
    // With no libs pfg sends no lib pages and drops their 0x15 records.
    let recs: Vec<(u32, Vec<u32>)> = tables::CORE_ACT1
        .iter()
        .filter(|(t, w)| !(*t == 0x15 && w[2] == 4 && lib_pages.is_empty()))
        .map(|(t, w)| {
            let mut w = w.to_vec();
            if *t == 0x1f {
                w[1..4].copy_from_slice(&offs);
            }
            if *t == 0x15 && w[2] == 4 {
                w[0] = lib_pages.len() as u32;
            }
            (*t, w)
        })
        .collect();
    let recs: Vec<(u32, &[u32])> = recs.iter().map(|(t, w)| (*t, w.as_slice())).collect();
    let act1 = section::action_page(
        &recs,
        &[
            (ram_bitmap.len() as u32, ram_bitmap),
            (bitmap.len() as u32, bitmap),
        ],
    )?;
    let (h0s, h0n) = io::part("sector0", "hdr");
    let (h1s, h1n) = io::part("sector1", "hdr");
    let shdr = section::stream_pages(&deflate::deflate(&core::sector_header_stream(
        &img[h0s..h0s + h0n],
        &img[h1s..h1s + h1n],
    ))?);
    let act2 = section::action_page(
        tables::CORE_ACT2,
        &[(shdr.len() as u32, vec![false; shdr.len()])],
    )?;
    let mut out: Vec<Page> = Vec::new();
    out.extend(section::module(inst.fw("lsm_init.fw")?)?);
    out.push(Page::Hashed(act1, FLAG_ACTION));
    out.extend(data);
    out.extend(section::module(inst.fw("unfreeze_routing_fw.fw")?)?);
    out.push(Page::Hashed(act2, FLAG_ACTION));
    out.extend(hashed(shdr, 0));
    out.extend(section::module(inst.fw("enter_user_mode_fw.fw")?)?);
    section::assemble(out, f, key)
}

/// The order in which pfg sends the two sectors' frame pages
/// (`BITASM_FAMILY_VAR1::get_group_data`, README (o)). The sector with more
/// pages (R; sector 0 on a tie) sends on every turn; the other sends only
/// while its bytes sent, scaled by the page-count ratio, stay within 4 KiB
/// of R's. Turns alternate 0, 1, 0, … starting at R, for as many rounds as
/// R has pages.
fn interleave(sizes: [usize; 2]) -> Vec<usize> {
    let n = sizes.map(|s| s.div_ceil(PAGE));
    let r = if n[0] >= n[1] {
        n[0] as f64 / n[1] as f64
    } else {
        n[1] as f64 / n[0] as f64
    };
    let big = usize::from(n[1] > n[0]);
    let rounds = sizes[0].max(sizes[1]).div_ceil(PAGE);
    let mut off = [0usize; 2];
    let mut out = Vec::new();
    let mut emit = |t: usize, off: &mut [usize; 2]| {
        if off[t] < sizes[t] {
            out.push(t);
            off[t] = (off[t] + PAGE).min(sizes[t]);
        }
    };
    let (mut t, mut round) = (big, 0);
    emit(t, &mut off);
    loop {
        loop {
            t += 1;
            if t == 2 {
                t = 0;
                round += 1;
                if round == rounds {
                    return out;
                }
            }
            if t == big || (off[t] as f64) * r - off[big] as f64 <= PAGE as f64 {
                break;
            }
        }
        emit(t, &mut off);
    }
}

/// Byte sum of every object header and both sectors' CRAM and M20K
/// contents (header +0x78; the contents were always zero before README (y)).
fn header_sum(img: &[u8]) -> u32 {
    let mut s = 0u32;
    for (o, p, start, n) in crate::db::PARTS {
        if *p == "hdr" || ((*p == "cram" || *p == "ram") && o.starts_with("sector")) {
            s = img[*start..start + n]
                .iter()
                .fold(s, |a, &b| a.wrapping_add(b as u32));
        }
    }
    s
}

/// SHA-256 over IO96 objects (README (n)): the IO hash covers both banks
/// without an HPS; with one, io96_1 is the HPS EMIF's, and the IO hash
/// covers io96_0 while the HPS IO hash is io96_1's [obs, designs/hpsf in
/// both boot orders].
pub fn io96_hash(img: &[u8], objs: &[&str]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for obj in objs {
        for (o, _, start, n) in crate::db::PARTS {
            if o == obj {
                h.update(&img[*start..start + n]);
            }
        }
    }
    h.finalize().into()
}

/// The GHRD HPS's OPTIONS 1.2 (header +0x9C) [obs, designs/hpsf].
const HPS_HDR_CLK_HZ: u32 = 125_000_000;

/// The board's phase 1, for an image a5db built (its own .sof container
/// says FPGA-only): the HPS-first .sof it was made from, or its HPS IO hash
/// as 64 hex digits (`quartus_pfg -i`'s "HPS IO hash").
pub enum Phase1 {
    Sof(crate::sof::Sof),
    HpsIoHash([u8; 32]),
}

impl Phase1 {
    pub fn parse(arg: &str) -> Result<Phase1> {
        if arg.len() == 64 && arg.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(Phase1::HpsIoHash(hex32(arg.as_bytes())?));
        }
        Ok(Phase1::Sof(crate::sof::Sof::read(arg)?))
    }
}

pub fn write(
    db: &mut Db,
    inst: &Install,
    sof: &crate::sof::Sof,
    key: &[u8],
    phase1: Option<&Phase1>,
) -> Result<Vec<u8>> {
    let img = sof.image();
    let mut design = sof_design(&sof.records())?;
    if let Some(p) = phase1 {
        let (hps_io_hash, clk) = match p {
            Phase1::HpsIoHash(h) => (*h, HPS_HDR_CLK_HZ),
            Phase1::Sof(r) => match sof_design(&r.records())? {
                Design {
                    boot: Boot::HpsFirst { hps_io_hash },
                    hdr_clk_hz,
                    ..
                } => (hps_io_hash, hdr_clk_hz),
                _ => anyhow::bail!("the phase-1 .sof is not an HPS-first build"),
            },
        };
        design.boot = Boot::HpsFirst { hps_io_hash };
        design.hdr_clk_hz = clk;
        design.io_hash = io96_hash(img, &["io96_0"]);
    }
    let sum = header_sum(img);
    let io_f = Fields {
        kind: Kind::Io,
        design: &design,
        sum,
        following: 1,
    };
    let core_f = Fields {
        kind: Kind::Core,
        design: &design,
        sum,
        following: 0,
    };
    let mut out = cmf(inst, 2)?;
    ensure!(
        u32::from_le_bytes(out[0..4].try_into().unwrap()) == RBF_MAGIC,
        "main.cmf does not start with the .rbf magic"
    );
    out.extend(io_section(db, inst, img, &io_f, key)?);
    out.extend(core_section(inst, img, &core_f, key)?);
    Ok(out)
}
