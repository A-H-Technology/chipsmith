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
//! - the clock tree's buffer enables, from the pips the clocks use, and a
//!   BRAM tile's address cascade enables ([`cascout_enable`]);
//! - block RAM ([`bram_lines`]) as Vivado's bitstreams have it read back
//!   by bit2fasm, which the widths' encoding nextpnr's `write_bram_width`
//!   agrees with.

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

/// The tile bit that drives a BRAM tile's address cascade out of
/// `wire`, if it is one (`BRAM_CASCOUT_ADDR{ARD,BWR}ADDRU<n>`, fed by
/// the always-on pip from the tile's own upper address pin): without it
/// the neighbouring tile's `CASCIN{TOP,BOT}` reads nothing (prjxray's
/// 060-bram-cascades; nextpnr's `CASCOUT_*_ACTIVE`). One bit per port and
/// tile, for all its address bits. Missing, the Zybo's bram-selftest read
/// back wrong words wherever a route took a cascade (2026-11-06).
pub(crate) fn cascout_enable(wire: &str) -> Option<&'static str> {
    let port = wire.strip_prefix("BRAM_CASCOUT_ADDR")?;
    if port.starts_with("ARDADDRU") {
        Some("CASCOUT_ARD_ACTIVE")
    } else if port.starts_with("BWRADDRU") {
        Some("CASCOUT_BWR_ACTIVE")
    } else {
        None
    }
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
                // A 5LUT's table below the 6LUT's (A6 high selects the top).
                let init = match (s.lut_init[k], s.lut5_init[k]) {
                    (t6, Some(t5)) => Some(t6.unwrap_or(0) & 0xffff_ffff_0000_0000 | t5),
                    (t6, None) => t6,
                };
                if let Some(init) = init {
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
                    let m = if m == "5Q" {
                        format!("{l}5Q")
                    } else {
                        m.to_string()
                    };
                    line(format!("{p}.{l}OUTMUX.{m}"));
                }
                if let Some(ff) = s.ff5[k] {
                    if ff.zini {
                        line(format!("{p}.{l}5FF.ZINI"));
                    }
                    if ff.zrst {
                        line(format!("{p}.{l}5FF.ZRST"));
                    }
                }
                if let Some(m) = s.ff5mux[k] {
                    line(format!("{p}.{l}5FFMUX.{m}"));
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
            let has_ff = s.ff.iter().chain(&s.ff5).any(Option::is_some);
            if has_ff && s.ffsync {
                line(format!("{p}.FFSYNC"));
            }
            line(format!(
                "{p}.{}",
                if s.clkinv { "CLKINV" } else { "NOCLKINV" }
            ));
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

    for b in &phys.brams {
        for l in bram_lines(db, b) {
            line(l);
        }
    }
    for d in &phys.dsps {
        for l in dsp_lines(db, d) {
            line(l);
        }
    }

    for c in &phys.cmts {
        let p = format!(
            "{}.{}",
            tile_name(db, c.tile),
            c.config.prim().site_prefix()
        );
        for f in c.config.features(c.reset)? {
            line(format!("{p}.{f}"));
        }
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
        line(format!(
            "{}.BSCAN.JTAG_CHAIN_{chain}",
            tile_name(db, s.tile)
        ));
    }

    // Routing.
    for e in pips {
        for l in pip_lines(db, e) {
            line(l);
        }
    }
    for e in pips {
        let tt = db.tile_type(e.tile);
        let (_, d) = tt.pips[e.pip as usize];
        if let Some(f) = cascout_enable(tt.wires.get(d)) {
            line(format!("{}.{f}", tile_name(db, e.tile)));
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
        if matches!(
            ty,
            "HCLK_L" | "HCLK_R" | "HCLK_L_BOT_UTURN" | "HCLK_R_BOT_UTURN"
        ) {
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

/// One block RAM's features: per RAMB18 half it uses (a RAMB36E1 both).
pub(crate) fn bram_lines(db: &ChipDb, b: &crate::phys::BramConf) -> Vec<String> {
    use crate::bram::{Size, WriteMode};
    let c = &b.config;
    let full = c.size == Size::Full;
    let tile = tile_name(db, b.tile);
    let mut out = Vec::new();
    let halves: Vec<usize> = match b.half {
        Some(h) => vec![h],
        None => vec![0, 1],
    };
    // Vivado: PERFORMANCE where port B does nothing.
    let b_unused = c.ports[1].read.is_none() && c.ports[1].write.is_none();
    for h in halves {
        let p = format!("{tile}.RAMB18_Y{h}");
        let mut f = |s: String| out.push(format!("{p}.{s}"));
        f("IN_USE".into());
        // Each half's share of the widths (a RAMB36E1's halves each take
        // half its word; x1 is a bit from each in turn) and SDP's.
        for (name, w) in [
            ("READ_WIDTH_A", c.ports[0].read),
            ("READ_WIDTH_B", c.ports[1].read),
            ("WRITE_WIDTH_A", c.ports[0].write),
            ("WRITE_WIDTH_B", c.ports[1].write),
        ] {
            let Some(w) = w else { continue };
            let bits = w.bits();
            let half = if full && bits > 1 { bits / 2 } else { bits };
            if name == "READ_WIDTH_A" && ((full && bits == 72) || (h == 1 && half == 36)) {
                f("READ_WIDTH_A_18".into());
            }
            if half == 36 {
                let side = &name[..name.len() - 2];
                f(format!("SDP_{side}_36"));
                if name.starts_with("WRITE") {
                    f("WRITE_WIDTH_A_18".into());
                    f("WRITE_WIDTH_B_18".into());
                } else {
                    f("READ_WIDTH_B_18".into());
                }
            } else if half > 1 {
                f(format!("{name}_{half}"));
            }
        }
        for (k, l) in ['A', 'B'].into_iter().enumerate() {
            let port = &c.ports[k];
            let out_reg = port.out_reg || (c.sdp && c.ports[0].out_reg);
            if out_reg {
                f(format!("DO{l}_REG"));
            }
            match port.mode {
                WriteMode::WriteFirst => {}
                WriteMode::ReadFirst => f(format!("WRITE_MODE_{l}_READ_FIRST")),
                WriteMode::NoChange => f(format!("WRITE_MODE_{l}_NO_CHANGE")),
            }
            if !port.rstreg_first {
                f(format!("RSTREG_PRIORITY_{l}_REGCE"));
            }
        }
        if b_unused {
            f("RDADDR_COLLISION_HWCONFIG_PERFORMANCE".into());
        }
        let inv = [
            ("CLKARDCLK", c.inv.clk[0]),
            ("CLKBWRCLK", c.inv.clk[1]),
            ("ENARDEN", c.inv.en[0]),
            ("ENBWREN", c.inv.en[1]),
            ("RSTRAMARSTRAM", c.inv.rstram[0]),
            ("RSTRAMB", c.inv.rstram[1]),
            ("RSTREGARSTREG", c.inv.rstreg[0]),
            ("RSTREGB", c.inv.rstreg[1]),
            ("REGCLKARDRCLK", c.inv.clk[0]),
            ("REGCLKB", if c.sdp { c.inv.clk[0] } else { c.inv.clk[1] }),
        ];
        for (pin, inverted) in inv {
            if !inverted {
                f(format!("ZINV_{pin}"));
            }
        }
        // INIT and SRVAL, in the half's order (`bram::half_bits`), inverted.
        for (k, l) in ['A', 'B'].into_iter().enumerate() {
            let (init, srval, w) = half_values(c, k, h);
            let z = |v: u64| !crate::bram::half_bits(v, w) & 0x3ffff;
            f(format!("ZINIT_{l}[17:0] = 18'b{:018b}", z(init)));
            f(format!("ZSRVAL_{l}[17:0] = 18'b{:018b}", z(srval)));
        }
        // Contents: a RAMB36E1's are its halves' bits in turn.
        let data = c.size.data_bits();
        for (prefix, base, words) in [
            ("INIT", 0, data / 256 / c.size.halves()),
            ("INITP", data, data / 8 / 256 / c.size.halves()),
        ] {
            for i in 0..words {
                let bit = |k: usize| -> bool {
                    if full {
                        c.contents[base + 2 * (256 * i + k) + h]
                    } else {
                        c.contents[base + 256 * i + k]
                    }
                };
                if (0..256).any(bit) {
                    let v: String = (0..256)
                        .rev()
                        .map(|k| if bit(k) { '1' } else { '0' })
                        .collect();
                    f(format!("{prefix}_{i:02X}[255:0] = 256'b{v}"));
                }
            }
        }
    }
    out
}

/// A port's INIT and SRVAL as half `h` of its block holds them, and the
/// width of the word they are in: a RAMB18E1's own (SDP's wide read: each
/// port's x18 half of it); a RAMB36E1's word of the port's read width split
/// bit by bit between the halves as its pins are (bit 2i + h to half h's
/// i; Vivado's bitstreams agree), each half's in the layout of half the
/// width.
pub(crate) fn half_values(
    c: &crate::bram::Config,
    k: usize,
    h: usize,
) -> (u64, u64, crate::bram::Width) {
    use crate::bram::{Size, Width};
    let p = &c.ports[k];
    // SDP's wide read: each port's pins hold half its word, x18 (x36).
    let w = match c.ports[0].read {
        Some(r) if c.sdp && r == c.size.sdp() => c.size.tdp_max(),
        _ => p.read.unwrap_or(c.size.tdp_max()),
    };
    if c.size == Size::Half {
        return (p.init, p.srval, w);
    }
    let (dw, pw) = (w.data(), w.parity());
    let pick = |v: u64| -> u64 {
        let data = (0..dw / 2).fold(0u64, |a, i| a | (v >> (2 * i + h) & 1) << i);
        let par = (0..pw / 2).fold(0u64, |a, j| a | (v >> (dw + 2 * j + h) & 1) << j);
        data | par << (dw / 2)
    };
    let half = Width::from_bits(w.bits() / 2).unwrap_or(Width::X1);
    (pick(p.init), pick(p.srval), half)
}

/// One DSP48E1's features, as Vivado's bitstreams read back by bit2fasm
/// have them (prjxray's names; "Z" ones set where the value is 0).
pub(crate) fn dsp_lines(db: &ChipDb, d: &crate::phys::DspConf) -> Vec<String> {
    let c = &d.config;
    let p = format!("{}.DSP48.DSP_{}", tile_name(db, d.tile), d.which);
    let mut out = Vec::new();
    let mut f = |s: String| out.push(format!("{p}.{s}"));
    match c.areg {
        0 => f("AREG_0".into()),
        2 => f("AREG_2".into()),
        _ => {}
    }
    match c.breg {
        0 => f("BREG_0".into()),
        2 => f("BREG_2".into()),
        _ => {}
    }
    if !(c.areg == 2 && c.acascreg == 1) {
        f("ZAREG_2_ACASCREG_1".into());
    }
    if !(c.breg == 2 && c.bcascreg == 1) {
        f("ZBREG_2_BCASCREG_1".into());
    }
    for (&on, name) in c.stages.iter().zip(crate::dsp::STAGES) {
        if !on {
            f(format!("Z{name}[0]"));
        }
    }
    if c.a_cascade {
        f("A_INPUT[0]".into());
    }
    if c.b_cascade {
        f("B_INPUT[0]".into());
    }
    if c.use_dport {
        f("USE_DPORT[0]".into());
    }
    for (&v, (pin, w)) in c.inverted.iter().zip(crate::dsp::INVERTIBLE) {
        let z = !v & ((1 << w) - 1);
        if w == 1 {
            if z == 1 {
                f(format!("ZIS_{pin}_INVERTED"));
            }
        } else {
            f(format!(
                "ZIS_{pin}_INVERTED[{}:0] = {w}'b{:0w$b}",
                w - 1,
                z,
                w = w
            ));
        }
    }
    f(format!("MASK[47:0] = 48'b{:048b}", c.mask));
    f(format!("PATTERN[47:0] = 48'b{:048b}", c.pattern));
    out
}
