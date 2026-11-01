//! FASM from the physical design and its routes, as openXC7's nextpnr
//! writes it (`himbaechel/uarch/xilinx/fasm.cc`), feature for feature where
//! chipsmith configures the same thing, so that prjxray's `fasm2frames`
//! makes the same frames of it:
//!
//! - slices: LUT tables (A1 the lowest index bit; a RAM LUT's 5LUT half in
//!   bits 0-31 and its 6LUT half in 32-63), the register and output muxes,
//!   ZINI and ZRST, the slice's clock inversion (NOCLKINV on every half of
//!   a used tile), SR mode and CE/SR use, the carry-in mux;
//! - pins at LVCMOS33: outputs 16 mA (nextpnr's `I12_I16`) and slow,
//!   inputs with `IN_ONLY` unless also driven, no pull;
//! - each pip `<tile>.<dst>.<src>`, a pseudo pip its site's bits instead
//!   (`crate::xray::pseudo_pip_features`), with nextpnr's renaming in
//!   single-IOB (`_SING`) I/O tiles;
//! - the clock tree's buffer enables, from the pips the clocks use.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::{Context, Result};
use chipsmith_route::chipdb::{ChipDb, Edge};

use crate::phys::{Physical, PreCyInit};
use crate::xray::pseudo_pip_features;

const LETTERS: [char; 4] = ['A', 'B', 'C', 'D'];

/// Whether a single-IOB I/O tile (`RIOB33_SING`, `RIOI3_SING`) is the top
/// one of its bank (sites `Y49`, `Y99`, ...), whose bits nextpnr names as
/// the second site's.
fn top_sing(db: &ChipDb, tile: u32) -> bool {
    db.sites
        .iter()
        .filter(|(_, s)| s.tile == tile)
        .filter_map(|(name, _)| name.rsplit_once('Y')?.1.parse::<u32>().ok())
        .any(|y| y % 50 == 49)
}

fn tile_name(db: &ChipDb, tile: u32) -> &str {
    &db.tile_names[tile as usize]
}

/// The FASM lines of one pip, or none (pips nextpnr writes nothing for).
pub(crate) fn pip_lines(db: &ChipDb, e: &Edge) -> Vec<String> {
    let tt = db.tile_type(e.tile);
    let (s, d) = tt.pips[e.pip as usize];
    let (src, dst) = (tt.wires.get(s), tt.wires.get(d));
    let tile = tile_name(db, e.tile);
    let sing = ["RIOI3_SING", "LIOI3_SING", "RIOI_SING"]
        .iter()
        .any(|p| tile.starts_with(p));
    if let Some(features) = pseudo_pip_features(&tt.name, dst, src) {
        let top = sing && top_sing(db, e.tile);
        return features
            .into_iter()
            .map(|f| {
                let f = if top { f.replacen("Y0", "Y1", 1) } else { f };
                format!("{tile}.{f}")
            })
            .collect();
    }
    if tile.starts_with("DSP_L") || tile.starts_with("DSP_R") {
        return Vec::new();
    }
    let (mut src, mut dst) = (src.to_string(), dst.to_string());
    if sing {
        // nextpnr: prjxray-db has no ppips for these.
        if (src.contains("IMUX") || src.contains("CTRL0")) && !dst.contains("CLK") {
            return Vec::new();
        }
        if let Some(i) = src.find("_SING_") {
            src.replace_range(i..i + 5, "");
        }
        if top_sing(db, e.tile) {
            if let Some(i) = dst.find("_0") {
                dst.replace_range(i..i + 2, "_1");
            }
            if let Some(i) = dst.find("OLOGIC0") {
                dst.replace_range(i..i + 7, "OLOGIC1");
                if let Some(i) = src.find("_0") {
                    src.replace_range(i..i + 2, "_1");
                }
            }
        }
    }
    if tile.contains("IOI") && dst.contains("OCLKB") && src.contains("IOI_OCLKM_") {
        return Vec::new();
    }
    vec![format!("{tile}.{dst}.{src}")]
}

/// The design's FASM; `pips` every routed pip.
pub(crate) fn write(db: &ChipDb, phys: &Physical, pips: &[Edge]) -> Result<String> {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };

    // Slices, by tile: both halves of a used tile get their clock
    // inversion bit.
    let mut by_tile: BTreeMap<u32, Vec<&crate::phys::SliceConf>> = BTreeMap::new();
    for s in phys.slices.values() {
        by_tile.entry(s.tile).or_default().push(s);
    }
    for (&tile, halves) in &by_tile {
        let name = tile_name(db, tile);
        let mut written: BTreeSet<&str> = BTreeSet::new();
        for s in halves {
            let p = format!("{name}.{}", s.half);
            written.insert(s.half.as_str());
            for k in 0..4 {
                let l = LETTERS[k];
                if let Some(init) = s.lut_init[k] {
                    line(format!("{p}.{l}LUT.INIT[63:0] = 64'b{init:064b}"));
                }
                if s.lut_ram[k] {
                    if k < 3 {
                        line(format!("{p}.{l}LUT.DI1MUX.{l}I"));
                    }
                    line(format!("{p}.{l}LUT.SMALL"));
                    line(format!("{p}.{l}LUT.RAM"));
                }
                if let Some(m) = s.outmux[k] {
                    line(format!("{p}.{l}OUTMUX.{m}"));
                }
                if let Some(ff) = s.ff[k] {
                    if ff.zini {
                        line(format!("{p}.{l}FF.ZINI"));
                    }
                    if ff.zrst {
                        line(format!("{p}.{l}FF.ZRST"));
                    }
                }
                if let Some(m) = s.ffmux[k] {
                    line(format!("{p}.{l}FFMUX.{m}"));
                }
            }
            let has_ff = s.ff.iter().any(Option::is_some);
            if has_ff && s.ffsync {
                line(format!("{p}.FFSYNC"));
            }
            line(format!("{p}.{}", if s.clkinv { "CLKINV" } else { "NOCLKINV" }));
            if has_ff && s.srused {
                line(format!("{p}.SRUSEDMUX"));
            }
            if has_ff && s.ceused {
                line(format!("{p}.CEUSEDMUX"));
            }
            if let Some(pre) = s.carry {
                let input = match pre {
                    PreCyInit::Cin => "CIN",
                    PreCyInit::Ax => "AX",
                    PreCyInit::Zero => "C0",
                    PreCyInit::One => "C1",
                };
                line(format!("{p}.PRECYINIT.{input}"));
            }
        }
        let m = name.starts_with("CLBLM");
        for half in [if m { "SLICEM_X0" } else { "SLICEL_X0" }, "SLICEL_X1"] {
            if !written.contains(half) {
                line(format!("{name}.{half}.NOCLKINV"));
            }
        }
    }

    // Pins.
    for pad in &phys.pads {
        let s = db
            .sites
            .get(&pad.site)
            .with_context(|| format!("no site {}", pad.site))?;
        let name = tile_name(db, s.tile);
        let y = if name.contains("_SING_") {
            u32::from(top_sing(db, s.tile))
        } else {
            let rel: u32 = s
                .relative
                .rsplit_once('Y')
                .and_then(|(_, y)| y.parse().ok())
                .with_context(|| format!("site {} at {}", pad.site, s.relative))?;
            1 - rel
        };
        let p = format!("{name}.IOB_Y{y}");
        if pad.output {
            line(format!("{p}.LVCMOS33_LVTTL.DRIVE.I12_I16"));
            line(format!(
                "{p}.LVCMOS12_LVCMOS15_LVCMOS18_LVCMOS25_LVCMOS33_LVTTL_SSTL135_SSTL15.SLEW.SLOW"
            ));
        }
        if pad.input {
            line(format!("{p}.LVCMOS25_LVCMOS33_LVTTL.IN"));
            if !pad.output {
                line(format!(
                    "{p}.LVCMOS12_LVCMOS15_LVCMOS18_LVCMOS25_LVCMOS33_LVDS_25_LVTTL_SSTL135_SSTL15_TMDS_33.IN_ONLY"
                ));
            }
        }
        line(format!("{p}.PULLTYPE.NONE"));
    }

    for site in &phys.bufgs {
        let s = &db.sites[site];
        let p = format!("{}.BUFGCTRL.{}", tile_name(db, s.tile), s.relative);
        for f in ["IN_USE", "IS_IGNORE1_INVERTED", "ZINV_CE0", "ZINV_S0"] {
            line(format!("{p}.{f}"));
        }
    }
    for (site, chain) in &phys.bscans {
        let s = &db.sites[site];
        line(format!("{}.BSCAN.JTAG_CHAIN_{chain}", tile_name(db, s.tile)));
    }

    // Routing.
    for e in pips {
        for l in pip_lines(db, e) {
            line(l);
        }
    }

    // The clock tree's buffers, from the clock pips (nextpnr's
    // `write_clocking`).
    let mut sources: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    for e in pips {
        let tt = db.tile_type(e.tile);
        let (s, _) = tt.pips[e.pip as usize];
        sources
            .entry(e.tile)
            .or_default()
            .insert(tt.wires.get(s).to_string());
    }
    let mut gclks: BTreeSet<String> = BTreeSet::new();
    let mut hclk_rows: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    for (&tile, used) in &sources {
        let ty = db.tile_type(tile).name.as_str();
        let name = tile_name(db, tile);
        let row = db.tile_xy(tile).1;
        if matches!(ty, "HCLK_L" | "HCLK_R" | "HCLK_L_BOT_UTURN" | "HCLK_R_BOT_UTURN") {
            for s in used.iter().filter(|s| s.starts_with("HCLK_CK_")) {
                if let Some(i) = s.find("BUFHCLK") {
                    line(format!("{name}.ENABLE_BUFFER.{s}"));
                    hclk_rows.entry(row).or_default().insert(s[i..].to_string());
                }
            }
        } else if ty.starts_with("CLK_HROW") {
            for s in used {
                if s.starts_with("CLK_HROW_R_CK_GCLK") {
                    line(format!("{name}.{s}_ACTIVE"));
                    gclks.insert(s[s.find("GCLK").expect("matched")..].to_string());
                } else if s.starts_with("CLK_HROW_CK_IN") && !s.contains("HROW_CK_INT") {
                    line(format!("{name}.{s}_ACTIVE"));
                }
            }
        } else if ty.starts_with("HCLK_CMT") {
            for s in used {
                if s.starts_with("HCLK_CMT_CCIO") {
                    line(format!("{name}.{s}_ACTIVE"));
                    line(format!("{name}.{s}_USED"));
                } else if s.starts_with("HCLK_CMT_CK_") {
                    if let Some(i) = s.find("BUFHCLK") {
                        line(format!("{name}.{s}_USED"));
                        hclk_rows.entry(row).or_default().insert(s[i..].to_string());
                    }
                }
            }
        }
    }
    for tile in 0..db.width * db.height {
        let ty = db.tile_type(tile).name.as_str();
        let name = tile_name(db, tile);
        if ty == "CLK_BUFG_REBUF" {
            for g in &gclks {
                line(format!("{name}.{g}_ENABLE_ABOVE"));
                line(format!("{name}.{g}_ENABLE_BELOW"));
            }
        } else if ty.starts_with("HCLK_CMT") {
            for h in hclk_rows.get(&db.tile_xy(tile).1).into_iter().flatten() {
                line(format!("{name}.HCLK_CMT_CK_{h}_USED"));
            }
        }
    }

    // nextpnr writes a feature at most once per line it is asked for; two
    // routes can't share a pip, but site bits can repeat across sources.
    let mut seen = BTreeSet::new();
    let mut dedup = String::new();
    for l in out.lines() {
        if seen.insert(l.to_string()) {
            let _ = writeln!(dedup, "{l}");
        }
    }
    Ok(dedup)
}
