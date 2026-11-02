//! FASM to configuration frames to a `.bit`, in process, replacing
//! prjxray's `fasm2frames` and `xc7frames2bit`, byte for byte (the `.bit`
//! header aside, which carries no date here). prjxray-db is the data
//! (ADR-0003):
//!
//! - `<fabric>/tilegrid.json`: each tile's bits, as frames from a base
//!   address and words from an offset, per configuration bus (CLB_IO_CLK,
//!   BLOCK_RAM); a tile without bits of its own type reads another type's
//!   through an alias (its site names and word offset mapped);
//! - `segbits_<type>.db` (and `.block_ram.db`): each feature's bits, as
//!   `<frame>_<bit>` in the tile, `!` for a bit that must stay 0;
//!   `ppips_<type>.db` names the features that have no bits;
//! - `<part>/required_features.fasm`: features every design gets;
//! - `<part>/part.yaml`: the configuration columns, which fix the order
//!   frames are written in.
//!
//! Frames are written for every column of the part, top half then bottom,
//! bus by bus, each row followed by two empty frames, each frame's word 50
//! carrying its ECC, then the start-up sequence xc7frames2bit writes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

/// Words in a 7-series configuration frame.
pub const FRAME_WORDS: usize = 101;

/// Configuration frames by address.
pub type Frames = BTreeMap<u32, [u32; FRAME_WORDS]>;

/// One tile's bits on one bus.
#[derive(Debug, Clone)]
struct Bits {
    base: u32,
    frames: u32,
    /// In words.
    /// In words; through an alias, below the tile's own words where the
    /// aliased type's first bits lie (a single-IOB tile's, -2).
    offset: i64,
    alias: Option<Alias>,
}

#[derive(Debug, Clone)]
struct Alias {
    ty: String,
    start_offset: u32,
    sites: HashMap<String, String>,
}

/// A feature bit: the frame within the tile, the bit within its words,
/// and whether it is set (or must stay clear).
#[derive(Debug, Clone, Copy)]
struct SegBit {
    frame: u32,
    bit: u32,
    set: bool,
}

#[derive(Default)]
struct TypeBits {
    /// Per bus (0 CLB_IO_CLK, 1 BLOCK_RAM): feature -> bits.
    segbits: [HashMap<String, Vec<SegBit>>; 2],
    ppips: std::collections::HashSet<String>,
    /// A vector feature's bits by index: base -> index -> (bus, feature).
    addresses: HashMap<String, HashMap<u32, (usize, String)>>,
    /// Whether the type has segbits of its own.
    own: bool,
}

pub struct Assembler {
    family: PathBuf,
    part: String,
    /// tile -> (type, per bus).
    tiles: HashMap<String, (String, [Option<Bits>; 2])>,
    types: HashMap<String, TypeBits>,
    /// (frame, word, bit) -> set, and the line that decided it.
    bits: HashMap<(u32, u32, u32), (bool, String)>,
}

const BUSES: [&str; 2] = ["CLB_IO_CLK", "BLOCK_RAM"];

fn parse_bit(s: &str) -> Result<SegBit> {
    let (set, s) = match s.strip_prefix('!') {
        Some(rest) => (false, rest),
        None => (true, s),
    };
    let (f, b) = s.split_once('_').with_context(|| format!("segbit {s}"))?;
    Ok(SegBit {
        frame: f.parse()?,
        bit: b.parse()?,
        set,
    })
}

fn hex(v: &Value) -> Result<u32> {
    let s = v.as_str().context("a hex string")?;
    Ok(u32::from_str_radix(s.trim_start_matches("0x"), 16)?)
}

impl Assembler {
    /// `family` is prjxray-db's family directory (`.../zynq7`); the part's
    /// fabric's tile grid and the part's required features are read.
    pub fn new(family: &Path, part: crate::part::Xc7Part) -> Result<Self> {
        let fabric = part.die;
        let grid_path = family.join(fabric).join("tilegrid.json");
        let grid: Value = serde_json::from_str(
            &std::fs::read_to_string(&grid_path)
                .with_context(|| format!("reading {}", grid_path.display()))?,
        )?;
        let mut tiles = HashMap::new();
        for (name, t) in grid.as_object().context("tilegrid")? {
            let ty = t["type"].as_str().context("tile type")?.to_string();
            let mut buses: [Option<Bits>; 2] = [None, None];
            if let Some(bits) = t["bits"].as_object() {
                for (bus, b) in bits {
                    let Some(i) = BUSES.iter().position(|x| x == bus) else {
                        continue;
                    };
                    let alias = match b.get("alias") {
                        Some(a) if !a.is_null() => Some(Alias {
                            ty: a["type"].as_str().context("alias type")?.to_string(),
                            start_offset: a["start_offset"].as_u64().unwrap_or(0) as u32,
                            sites: a["sites"]
                                .as_object()
                                .map(|m| {
                                    m.iter()
                                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                                        .collect()
                                })
                                .unwrap_or_default(),
                        }),
                        _ => None,
                    };
                    buses[i] = Some(Bits {
                        base: hex(&b["baseaddr"])?,
                        frames: b["frames"].as_u64().context("frames")? as u32,
                        offset: b["offset"].as_u64().context("offset")? as i64,
                        alias,
                    });
                }
            }
            tiles.insert(name.clone(), (ty, buses));
        }
        Ok(Assembler {
            family: family.to_path_buf(),
            part: part.part.to_string(),
            tiles,
            types: HashMap::new(),
            bits: HashMap::new(),
        })
    }

    fn load_type(&mut self, ty: &str) -> Result<()> {
        if self.types.contains_key(ty) {
            return Ok(());
        }
        let lower = ty.to_lowercase();
        let mut t = TypeBits::default();
        for (bus, file) in [(0, format!("segbits_{lower}.db")), (1, format!("segbits_{lower}.block_ram.db"))] {
            let path = self.family.join(file);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if bus == 0 {
                t.own = true;
            }
            for line in text.lines() {
                let mut parts = line.split_ascii_whitespace();
                let Some(feature) = parts.next() else { continue };
                let bits: Vec<SegBit> = parts.map(parse_bit).collect::<Result<_>>()?;
                if let (Some(s), Some(e)) = (feature.rfind('['), feature.rfind(']')) {
                    let idx: u32 = feature[s + 1..e].parse()?;
                    t.addresses
                        .entry(feature[..s].to_string())
                        .or_default()
                        .insert(idx, (bus, feature.to_string()));
                }
                t.segbits[bus].insert(feature.to_string(), bits);
            }
        }
        if let Ok(text) = std::fs::read_to_string(self.family.join(format!("ppips_{lower}.db"))) {
            for line in text.lines() {
                if let Some(f) = line.split_ascii_whitespace().next() {
                    t.ppips.insert(f.to_string());
                }
            }
        }
        self.types.insert(ty.to_string(), t);
        Ok(())
    }

    /// Sets the bits of `feature` (below the tile) at vector index
    /// `address` of `tile`.
    fn enable(&mut self, tile: &str, feature: &str, address: u32, line: &str) -> Result<()> {
        let (ty, buses) = self
            .tiles
            .get(tile)
            .with_context(|| format!("no tile {tile} in line {line:?}"))?
            .clone();
        self.load_type(&ty)?;
        let own = self.types[&ty].own;
        let alias = buses.iter().flatten().find_map(|b| b.alias.clone());
        // A tile type's own segbits win over an alias (prjxray's grid.py).
        let (lookup_ty, key, buses) = match alias.filter(|_| !own) {
            None => (ty.clone(), format!("{ty}.{feature}"), buses),
            Some(a) => {
                self.load_type(&a.ty)?;
                // A feature in the type's own ppips has no bits.
                if self.types[&ty].ppips.contains(&format!("{ty}.{feature}")) {
                    return Ok(());
                }
                let mut parts: Vec<String> = feature.split('.').map(str::to_string).collect();
                if let Some(site) = parts.first_mut() {
                    if let Some(to) = a.sites.get(site.as_str()) {
                        *site = to.clone();
                    }
                }
                let shifted = buses.map(|b| {
                    b.map(|b| Bits {
                        offset: b.offset - b.alias.as_ref().map_or(0, |a| a.start_offset as i64),
                        ..b
                    })
                });
                (a.ty.clone(), format!("{}.{}", a.ty, parts.join(".")), shifted)
            }
        };
        let t = &self.types[&lookup_ty];
        if t.ppips.contains(&key) {
            return Ok(());
        }
        let found: Option<(usize, &Vec<SegBit>)> = (address == 0)
            .then(|| (0..2).find_map(|bus| t.segbits[bus].get(&key).map(|b| (bus, b))))
            .flatten()
            .or_else(|| {
                let (bus, f) = t.addresses.get(&key)?.get(&address)?;
                Some((*bus, &t.segbits[*bus][f]))
            });
        let Some((bus, segbits)) = found else {
            bail!("segbits of {lookup_ty} have no {key} (address {address}), line {line:?}");
        };
        let b = buses[bus]
            .as_ref()
            .with_context(|| format!("tile {tile} has no {} bits", BUSES[bus]))?;
        let updates: Vec<((u32, u32, u32), bool)> = segbits
            .iter()
            .map(|sb| {
                let word_bit = b.offset * 32 + sb.bit as i64;
                ((b.base + sb.frame, (word_bit / 32) as u32, (word_bit % 32) as u32), sb.set)
            })
            .collect();
        for (k, set) in updates {
            ensure!(k.1 < FRAME_WORDS as u32, "line {line:?}: word {} out of the frame", k.1);
            match self.bits.get(&k) {
                Some((old, other)) if *old != set => {
                    bail!("line {line:?} and line {other:?} want frame bit {k:?} both ways")
                }
                Some(_) => {}
                None => {
                    self.bits.insert(k, (set, line.to_string()));
                }
            }
        }
        Ok(())
    }

    /// One FASM line: a feature, or a vector of them (`X[63:0] = 64'b...`,
    /// `X[3]`); features with value 0 set nothing.
    pub fn add_line(&mut self, line: &str) -> Result<()> {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            return Ok(());
        }
        let (name, value) = match line.split_once('=') {
            Some((n, v)) => (n.trim(), Some(v.trim())),
            None => (line, None),
        };
        let (tile, feature) = name.split_once('.').with_context(|| format!("FASM line {line:?}"))?;
        let (base, lo) = match feature.strip_suffix(']').and_then(|f| f.rsplit_once('[')) {
            Some((base, range)) => {
                let lo = range.rsplit(':').next().unwrap_or(range);
                (base, Some(lo.parse::<u32>()?))
            }
            None => (feature, None),
        };
        // The value's bits, the lowest first (BRAM contents are 256 wide).
        let value: Vec<bool> = match value {
            None => vec![true],
            Some(v) => {
                let digits = v.split_once("'b").map_or(v, |(_, d)| d);
                ensure!(
                    !digits.is_empty() && digits.chars().all(|c| c == '0' || c == '1'),
                    "value in {line:?}"
                );
                digits.chars().rev().map(|c| c == '1').collect()
            }
        };
        for (i, &set) in value.iter().enumerate() {
            if set {
                let address = lo.map_or(0, |lo| lo + i as u32);
                let feature = if lo.is_some() { base } else { feature };
                self.enable(tile, feature, address, line)?;
            }
        }
        Ok(())
    }

    /// Every frame of every tile, with the bits the lines set; the part's
    /// required features first.
    pub fn frames(mut self, fasm: &str) -> Result<Frames> {
        let required = self.family.join(&self.part).join("required_features.fasm");
        let required = std::fs::read_to_string(&required).unwrap_or_default();
        for line in fasm.lines().chain(required.lines()) {
            self.add_line(line)?;
        }
        let mut frames: Frames = BTreeMap::new();
        for (_, buses) in self.tiles.values() {
            for b in buses.iter().flatten() {
                for f in b.base..b.base + b.frames {
                    frames.entry(f).or_insert([0; FRAME_WORDS]);
                }
            }
        }
        for (&(frame, word, bit), &(set, _)) in &self.bits {
            let f = frames.entry(frame).or_insert([0; FRAME_WORDS]);
            if set {
                f[word as usize] |= 1 << bit;
            }
        }
        Ok(frames)
    }
}

/// The frames as fasm2frames writes them (`.frm`): one line per frame.
pub fn frm(frames: &Frames) -> String {
    let mut out = String::new();
    for (addr, words) in frames {
        out += &format!("0x{addr:08X} ");
        let w: Vec<String> = words.iter().map(|w| format!("0x{w:08X}")).collect();
        out += &w.join(",");
        out.push('\n');
    }
    out
}

/// prjxray's 7-series frame ECC: a Hamming code over the frame's bits into
/// word 50's low 13 bits.
fn ecc(words: &[u32; FRAME_WORDS]) -> u32 {
    let mut ecc = 0u32;
    for (idx, &w) in words.iter().enumerate() {
        let idx = idx as u32;
        let base = idx * 32
            + if idx > 0x25 {
                0x1360
            } else if idx > 0x6 {
                0x1340
            } else {
                0x1320
            };
        let data = if idx == 0x32 { w & 0xFFFF_E000 } else { w };
        for i in 0..32 {
            if data >> i & 1 == 1 {
                ecc ^= base + i;
            }
        }
        if idx == 0x64 {
            let mut v = ecc & 0xFFF;
            v ^= v >> 8;
            v ^= v >> 4;
            v ^= v >> 2;
            v ^= v >> 1;
            ecc ^= (v & 1) << 12;
        }
    }
    ecc & 0x1FFF
}

/// The part's configuration frames in write order, `None` for the two
/// empty frames after each row: (bus, half, row, column, frame count) from
/// `part.yaml`, sorted.
fn frame_order(part_yaml: &str) -> Result<(u32, Vec<Option<u32>>)> {
    let mut idcode = None;
    let (mut half, mut row, mut bus, mut col) = (0u32, 0u32, 0u32, 0u32);
    let mut columns: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
    for line in part_yaml.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("idcode: ") {
            idcode = Some(u32::from_str_radix(v.trim_start_matches("0x"), 16)?);
        } else if let Some((k, v)) = t.split_once(": !<xilinx/xc7series/") {
            match v.trim_end_matches('>') {
                "global_clock_region" => half = u32::from(k == "bottom"),
                "row" => row = k.parse()?,
                "configuration_bus" => {
                    bus = match k {
                        "CLB_IO_CLK" => 0,
                        "BLOCK_RAM" => 1,
                        "CFG_CLB" => 2,
                        other => bail!("part.yaml: bus {other}"),
                    }
                }
                "configuration_column" => col = k.parse()?,
                _ => {}
            }
        } else if let Some(n) = t.strip_prefix("frame_count: ") {
            columns.push((bus, half, row, col, n.parse()?));
        }
    }
    columns.sort();
    let mut order = Vec::new();
    let mut prev = None;
    for &(bus, half, row, col, n) in &columns {
        if prev.is_some_and(|p| p != (bus, half, row)) {
            order.extend([None, None]);
        }
        prev = Some((bus, half, row));
        for minor in 0..n {
            order.push(Some(bus << 23 | half << 22 | row << 17 | col << 7 | minor));
        }
    }
    order.extend([None, None]);
    Ok((idcode.context("part.yaml has no idcode")?, order))
}

/// A Type 1 packet writing `words` to register `reg`.
fn write(out: &mut Vec<u32>, reg: u32, words: &[u32]) {
    out.push(0x3000_0000 | reg << 13 | words.len() as u32);
    out.extend_from_slice(words);
}

const NOP: u32 = 0x2000_0000;
const CMD: u32 = 4;

/// The `.bit` of `frames` for `part` (`part.yaml`'s directory under
/// `family`), named `design`: xc7frames2bit's packets, a header without a
/// date.
pub fn bit(family: &Path, part: &str, design: &str, frames: &Frames) -> Result<Vec<u8>> {
    let yaml = std::fs::read_to_string(family.join(part).join("part.yaml"))
        .with_context(|| format!("reading {part}/part.yaml"))?;
    let (idcode, order) = frame_order(&yaml)?;
    let mut w: Vec<u32> = vec![0xAA99_5566, NOP];
    write(&mut w, 17, &[0]); // TIMER
    write(&mut w, 16, &[0]); // WBSTAR
    write(&mut w, CMD, &[0]); // NULL
    w.push(NOP);
    write(&mut w, CMD, &[7]); // RCRC
    w.extend([NOP, NOP]);
    write(&mut w, 19, &[0]);
    write(&mut w, 9, &[0x0200_3FE5]); // COR0
    write(&mut w, 14, &[0]); // COR1
    write(&mut w, 12, &[idcode]); // IDCODE
    write(&mut w, CMD, &[9]); // SWITCH
    w.push(NOP);
    write(&mut w, 6, &[0x401]); // MASK
    write(&mut w, 5, &[0x501]); // CTL0
    write(&mut w, 6, &[0]); // MASK
    write(&mut w, 24, &[0]); // CTL1
    w.extend([NOP; 8]);
    write(&mut w, 1, &[0]); // FAR
    write(&mut w, CMD, &[1]); // WCFG
    w.push(NOP);
    write(&mut w, 2, &[]); // FDRI, then a Type 2 packet with the frames
    let words = order.len() * FRAME_WORDS;
    w.push(0x5000_0000 | words as u32);
    for a in &order {
        let mut f = a.and_then(|a| frames.get(&a)).copied().unwrap_or([0; FRAME_WORDS]);
        if a.is_some() {
            f[50] = (f[50] & 0xFFFF_E000) | ecc(&f);
        }
        w.extend_from_slice(&f);
    }
    write(&mut w, CMD, &[7]); // RCRC
    w.extend([NOP, NOP]);
    write(&mut w, CMD, &[0xA]); // GRESTORE
    w.push(NOP);
    write(&mut w, CMD, &[3]); // LFRM
    w.extend([NOP; 100]);
    write(&mut w, CMD, &[5]); // START
    w.push(NOP);
    write(&mut w, 1, &[0x03BE_0000]); // FAR
    write(&mut w, 6, &[0x501]); // MASK
    write(&mut w, 5, &[0x501]); // CTL0
    write(&mut w, CMD, &[7]); // RCRC
    w.extend([NOP, NOP]);
    write(&mut w, CMD, &[0xD]); // DESYNC
    w.extend([NOP; 400]);

    // The payload: bus-width detection and sync in front of the packets.
    let mut payload: Vec<u8> = vec![0xFF; 32];
    payload.extend([0x00, 0x00, 0x00, 0xBB, 0x11, 0x22, 0x00, 0x44]);
    payload.extend([0xFF; 8]);
    for x in &w {
        payload.extend(x.to_be_bytes());
    }
    let mut out: Vec<u8> = vec![0x00, 0x09, 0x0F, 0xF0, 0x0F, 0xF0, 0x0F, 0xF0, 0x0F, 0xF0, 0x00, 0x00, 0x01];
    let mut field = |key: u8, text: &str| {
        out.push(key);
        let bytes: Vec<u8> = text.bytes().chain(std::iter::once(0)).collect();
        out.extend((bytes.len() as u16).to_be_bytes());
        out.extend(bytes);
    };
    field(b'a', &format!("{design};Generator=chipsmith"));
    field(b'b', part);
    out.push(b'e');
    out.extend((payload.len() as u32).to_be_bytes());
    out.extend(payload);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_follow_the_columns_with_two_empty_frames_after_each_row() {
        let yaml = "idcode: 0x3722093\nglobal_clock_regions:\n  top: !<xilinx/xc7series/global_clock_region>\n    rows:\n      0: !<xilinx/xc7series/row>\n        configuration_buses:\n          CLB_IO_CLK: !<xilinx/xc7series/configuration_bus>\n            configuration_columns:\n              0: !<xilinx/xc7series/configuration_column>\n                frame_count: 2\n              1: !<xilinx/xc7series/configuration_column>\n                frame_count: 1\n";
        let (id, order) = frame_order(yaml).unwrap();
        assert_eq!(id, 0x3722093);
        assert_eq!(order, vec![Some(0), Some(1), Some(1 << 7), None, None]);
    }

    #[test]
    fn an_empty_frame_has_no_ecc() {
        assert_eq!(ecc(&[0; FRAME_WORDS]), 0);
    }
}
