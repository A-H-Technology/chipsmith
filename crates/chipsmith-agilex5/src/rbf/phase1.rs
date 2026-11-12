//! Phase 1 of an HPS-first boot (`quartus_pfg -o hps=1`'s `.hps.rbf`) with
//! another U-Boot SPL in it.
//!
//! Phase 1 is three sections: CMF (the SDM firmware), HPIO (the HPS's I/O
//! and EMIF) and HPS. The HPS section holds a module page, an action page,
//! one deflated stream (the SPL zero-padded to 16 bytes, then the HPS
//! handoff data U-Boot reads) and a second module page. In two Quartus
//! builds of one design that differ only in the SPL, everything but that
//! stream, the action record giving its page count and the SPL's length, and
//! the hashes over them is the same bytes. So a phase 1 for a board's HPS
//! settings, built once by Quartus, takes any SPL.

use anyhow::{bail, ensure, Context, Result};

use super::{
    deflate,
    section::{self, Page, FLAG_ACTION, FLAG_MODULE, PAGE},
};

const SECTION_MAGIC: u32 = 0x9756_6593;
/// The action record that places the stream in the HPS's on-chip RAM:
/// [pages, 0, 2, 0, 0, SPL bytes (padded), 0x7F000, handoff bytes].
const REC_HPS_LOAD: u32 = 0x2b;

fn u32_at(b: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        b.get(o..o + 4).context("truncated .rbf")?.try_into()?,
    ))
}

/// (start, size, name) of each section, in file order.
fn sections(rbf: &[u8]) -> Result<Vec<(usize, usize, [u8; 4])>> {
    let mut out = Vec::new();
    let mut base = 0;
    while base < rbf.len() {
        // The first (CMF) section's header block starts at +0x400.
        let h = base + if base == 0 { 0x400 } else { 0 };
        ensure!(
            u32_at(rbf, h)? == SECTION_MAGIC,
            "no section header at {h:#x}"
        );
        let size = u32_at(rbf, h + 8)? as usize;
        ensure!(
            size > 0 && base + size <= rbf.len(),
            "bad section size at {h:#x}"
        );
        out.push((base, size, rbf[h + 12..h + 16].try_into()?));
        base += size;
    }
    Ok(out)
}

/// The action page's records as (type, words).
fn records(page: &[u8]) -> Result<Vec<(u32, Vec<u32>)>> {
    let mut out = Vec::new();
    let mut o = 0;
    while o + 8 <= page.len() {
        let (t, len) = (u32_at(page, o)?, u32_at(page, o + 4)? as usize);
        if t == 0 && len == 0 {
            break;
        }
        ensure!(
            len >= 8 && len % 4 == 0 && o + len <= page.len(),
            "bad action record at {o:#x}"
        );
        let words = (o + 8..o + len)
            .step_by(4)
            .map(|w| u32_at(page, w))
            .collect::<Result<_>>()?;
        out.push((t, words));
        o += len;
    }
    Ok(out)
}

/// `base`, a Quartus phase 1, with `spl` (`u-boot-spl-dtb.bin`) in place of
/// its SPL. The keyed hash is the one every section carries (`keys/rbf_keyed_hash.key`).
pub fn with_spl(base: &[u8], spl: &[u8], keyed_hash_key: &[u8]) -> Result<Vec<u8>> {
    let secs = sections(base)?;
    let names: Vec<&[u8; 4]> = secs.iter().map(|(_, _, n)| n).collect();
    ensure!(
        names == [b"CMF\0", b"HPIO", b"HPS\0"],
        "not a phase 1 (.hps.rbf): sections {:?}",
        names
            .iter()
            .map(|n| String::from_utf8_lossy(&n[..]))
            .collect::<Vec<_>>()
    );
    let (start, size, _) = secs[2];
    let hps = &base[start..start + size];
    let pages: Vec<&[u8]> = hps.chunks(PAGE).collect();
    // Header, header hash, one chain table, then the hashed pages: a stream
    // long enough to need a second table would not fit the SPL's RAM.
    let table = pages[2];
    let n = u32_at(table, 8)? as usize;
    ensure!(
        n == pages.len() - 3,
        "the HPS section's chain has {n} entries for {} pages",
        pages.len() - 3
    );
    let flags: Vec<u32> = (0..n)
        .map(|i| u32_at(table, 16 + 52 * i + 48))
        .collect::<Result<_>>()?;
    ensure!(
        n >= 4
            && flags[0] == FLAG_MODULE
            && flags[1] == FLAG_ACTION
            && flags[n - 1] == FLAG_MODULE
            && flags[2..n - 1].iter().all(|&f| f == 0),
        "unexpected HPS section layout: flags {flags:x?}"
    );
    let (first, action, last) = (pages[3], pages[4], pages[pages.len() - 1]);

    let mut recs = records(action)?;
    let load = match recs.iter_mut().find(|(t, _)| *t == REC_HPS_LOAD) {
        Some((_, w)) if w.len() == 8 => w,
        Some(_) => bail!("HPS load record of unexpected length"),
        None => bail!("no HPS load record in the action page"),
    };
    let (old_spl, handoff_len) = (load[5] as usize, load[7] as usize);
    let stream: Vec<u8> = pages[5..pages.len() - 1].concat();
    let (old, _) = deflate::inflate(&stream)?;
    ensure!(
        old.len() == old_spl + handoff_len,
        "HPS stream is not SPL + handoff"
    );
    let handoff = &old[old_spl..];

    let mut data = spl.to_vec();
    data.resize(spl.len().next_multiple_of(16), 0);
    load[5] = data.len() as u32;
    data.extend_from_slice(handoff);
    let stream = section::stream_pages(&deflate::deflate(&data)?);
    load[0] = stream.len() as u32;

    let recs: Vec<(u32, &[u32])> = recs.iter().map(|(t, w)| (*t, &w[..])).collect();
    let mut body = vec![
        Page::Hashed(first.to_vec(), FLAG_MODULE),
        Page::Hashed(section::action_page(&recs, &[])?, FLAG_ACTION),
    ];
    body.extend(stream.into_iter().map(|p| Page::Hashed(p, 0)));
    body.push(Page::Hashed(last.to_vec(), FLAG_MODULE));
    let following = u32_at(pages[1], 0xf00)?;
    let hps = section::seal(
        pages[0].to_vec(),
        &section::chain(body),
        following,
        keyed_hash_key,
    )?;

    let mut out = base[..start].to_vec();
    out.extend_from_slice(&hps);
    Ok(out)
}
