//! The 7-series routing graph, from Project X-Ray's database (prjxray-db,
//! one family directory such as `zynq7/`):
//!
//! - `<fabric>/tilegrid.json`: every tile's name, grid position and type;
//! - `tile_type_<T>.json`: each tile type's wires, pips and sites;
//! - `<fabric>/tileconn.json`: which wires of neighbouring tiles are one
//!   node, as rules per pair of tile types and grid offset;
//! - `segbits_<t>.db` and `ppips_<t>.db`: the FASM features, i.e. which pips
//!   a bitstream can express.
//!
//! Wires, pips and coordinates are named as openXC7's nextpnr names them
//! (its chip database is built from the same files by `xilinx_gen.py`), so
//! a route reads and writes in nextpnr's `X<x>Y<y>/<dst>/<src>` pip names.
//!
//! Pips kept, as nextpnr keeps them:
//! - bidirectional pips (`is_directional` 0) become one pip each way;
//! - a pip with no FASM feature (neither in segbits nor in ppips) is
//!   dropped where the tile type has a bit database: a bitstream can't
//!   express it. `ppips` (`always`, `default`, `hint`) need no bits and stay.
//!
//! Dropped, unlike nextpnr: pseudo pips (`is_pseudo` 1). They route
//! through a site (a LUT, an IOB or OLOGIC path) and so need that site's
//! configuration as well as a free bel; the router doesn't model sites.
//! Routes nextpnr takes through them stay nextpnr's (`route::xc7`).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde_json::Value;

use super::chipdb::{Bel, Box, ChipDb, Grid, Nodes, Pad, PadIo, Site, TileType};
use crate::device::Xc7Part;

/// A prebuilt 7-series routing graph (`chipsmith xc7-routing-graph`).
pub const GRAPH_VAR: &str = "CHIPSMITH_XC7_GRAPH";

fn read_json(path: &Path) -> Result<Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// A coarse wire class from the interconnect's naming, for reports and the
/// delay model to come: `EE2BEG0` is a double, `NN6END3` a hex.
pub fn wire_class(name: &str) -> &'static str {
    if name.contains("VCC_WIRE") || name.contains("GND_WIRE") {
        return "CONST";
    }
    if name.contains("CLK") {
        return "CLOCK";
    }
    if name.starts_with("LV") {
        return "LONG_V";
    }
    if name.starts_with("LH") {
        return "LONG_H";
    }
    const DIRECTIONS: [&str; 16] = [
        "EE", "WW", "NN", "SS", "NE", "NW", "SE", "SW", "NL", "NR", "SL", "SR", "WL", "WR", "EL",
        "ER",
    ];
    let b = name.as_bytes();
    if b.len() > 2 && DIRECTIONS.contains(&&name[..2]) {
        match b[2] {
            b'1' => return "SINGLE",
            b'2' => return "DOUBLE",
            b'4' => return "QUAD",
            b'6' => return "HEX",
            _ => {}
        }
    }
    for (pattern, class) in [
        ("IMUX", "PINFEED"),
        ("BYP", "BYPASS"),
        ("FAN", "FAN"),
        ("CTRL", "CONTROL"),
        ("LOGIC_OUTS", "OUTPUT"),
    ] {
        if name.contains(pattern) {
            return class;
        }
    }
    "OTHER"
}

/// The FASM features a tile type's pips can have: `<TYPE>.<dst>.<src>` from
/// its segbits and ppips. `None` when there is no bit database for it at
/// all, as for most tile types without configurable routing.
///
/// A top or bottom of column `*_SING` I/O tile is expressed through its base
/// type's database (`RIOI3_SING` through `segbits_rioi3.db`), whose rows
/// name its wires without the `_SING` infix; same rule as nextpnr's
/// `xilinx_gen.py` (`dealias_tile_type`).
fn features(family: &Path, tile_type: &str) -> Result<Option<HashSet<String>>> {
    let base = tile_type.strip_suffix("_SING");
    let mut out = HashSet::new();
    let mut found = false;
    for lookup in std::iter::once(tile_type).chain(base) {
        for kind in ["segbits", "ppips"] {
            let path = family.join(format!("{kind}_{}.db", lookup.to_lowercase()));
            if !path.exists() {
                continue;
            }
            found = true;
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            for line in text.lines() {
                let Some(key) = line.split_ascii_whitespace().next() else {
                    continue;
                };
                // Re-prefix the base type's rows with this tile type.
                match key.strip_prefix(lookup).filter(|_| lookup != tile_type) {
                    Some(rest) => out.insert(format!("{tile_type}{rest}")),
                    None => out.insert(key.to_string()),
                };
            }
        }
    }
    Ok(found.then_some(out))
}

/// Whether a pip `dst <- src` of `tile_type` has a FASM feature.
fn has_feature(features: &HashSet<String>, tile_type: &str, dst: &str, src: &str) -> bool {
    if features.contains(&format!("{tile_type}.{dst}.{src}")) {
        return true;
    }
    let dealias = |w: &str| w.replacen("_SING_", "_", 1);
    tile_type.ends_with("_SING")
        && features.contains(&format!("{tile_type}.{}.{}", dealias(dst), dealias(src)))
}

/// What `import` read, for the record.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub tiles: usize,
    pub tile_types: usize,
    pub tile_wires: usize,
    pub nodes: usize,
    pub pips: usize,
    /// Pips of the tile type definitions (not of every tile) dropped
    /// because they route through a site.
    pub pseudo_dropped: usize,
    /// ... dropped because no FASM feature expresses them.
    pub featureless_dropped: usize,
    /// ... that are bidirectional (each became two).
    pub bidirectional: usize,
}

struct ImportedType {
    tt: TileType,
    /// wire name -> index
    index: HashMap<String, u32>,
}

fn import_tile_type(
    family: &Path,
    name: &str,
    wire_types: &mut Vec<String>,
    stats: &mut Stats,
) -> Result<ImportedType> {
    let mut tt = TileType {
        name: name.to_string(),
        ..Default::default()
    };
    let mut index = HashMap::new();
    let path = family.join(format!("tile_type_{name}.json"));
    if !path.exists() {
        // Tile types with nothing in them (NULL, the PS's blank tiles).
        return Ok(ImportedType { tt, index });
    }
    let json = read_json(&path)?;
    // serde_json's maps are sorted, so wires come out in name order, as
    // nextpnr's generator numbers them.
    for wire in json["wires"].as_object().context("no wires")?.keys() {
        let class = wire_class(wire);
        let ty = match wire_types.iter().position(|w| w == class) {
            Some(i) => i,
            None => {
                wire_types.push(class.to_string());
                wire_types.len() - 1
            }
        };
        index.insert(wire.clone(), tt.wires.len() as u32);
        tt.wires.push(wire);
        tt.wire_types.push(ty as u8);
    }
    let features = features(family, name)?;
    for (key, pip) in json["pips"].as_object().context("no pips")? {
        let field = |f: &str| {
            pip[f]
                .as_str()
                .with_context(|| format!("{name} pip {key}: no {f}"))
        };
        let (src, dst) = (field("src_wire")?, field("dst_wire")?);
        let (Some(&s), Some(&d)) = (index.get(src), index.get(dst)) else {
            anyhow::bail!("{name} pip {key} names an undeclared wire");
        };
        if field("is_pseudo")? == "1" {
            stats.pseudo_dropped += 1;
            continue;
        }
        let expressible = |dst: &str, src: &str| {
            features
                .as_ref()
                .is_none_or(|f| has_feature(f, name, dst, src))
        };
        let mut directions = vec![(s, d, expressible(dst, src))];
        if field("is_directional")? == "0" {
            stats.bidirectional += 1;
            directions.push((d, s, expressible(src, dst)));
        }
        for (s, d, ok) in directions {
            if ok {
                tt.pips.push((s, d));
            } else {
                stats.featureless_dropped += 1;
            }
        }
    }
    for (z, site) in json["sites"]
        .as_array()
        .context("no sites")?
        .iter()
        .enumerate()
    {
        let site_name = format!(
            "{}_{}",
            site["prefix"].as_str().context("site prefix")?,
            site["name"].as_str().context("site name")?
        );
        let mut pins = HashMap::new();
        for (pin, data) in site["site_pins"].as_object().context("site pins")? {
            if let Some(wire) = data["wire"].as_str() {
                let w = *index
                    .get(wire)
                    .with_context(|| format!("{name} site pin {pin} on undeclared wire {wire}"))?;
                pins.insert(pin.clone(), w);
            }
        }
        tt.bels.insert(
            site_name,
            Bel {
                ty: site["type"].as_str().context("site type")?.to_string(),
                z: z as u32,
                pins,
            },
        );
    }
    Ok(ImportedType { tt, index })
}

/// Union-find over flat tile wires.
struct Joins(Vec<u32>);

impl Joins {
    fn find(&mut self, mut a: u32) -> u32 {
        while self.0[a as usize] != a {
            let up = self.0[self.0[a as usize] as usize];
            self.0[a as usize] = up;
            a = up;
        }
        a
    }

    fn join(&mut self, a: u32, b: u32) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            // The lower one stays the root, so roots don't depend on order.
            let (lo, hi) = (a.min(b), a.max(b));
            self.0[hi as usize] = lo;
        }
    }
}

/// The routing graph of `fabric` (e.g. `xc7z010`) from prjxray-db's
/// `family` directory (e.g. `.../zynq7`), with the package pins of `part`
/// (e.g. `xc7z010clg400-1`).
pub fn import(family: &Path, fabric: &str, part: &str) -> Result<(ChipDb, Stats)> {
    let mut stats = Stats::default();
    let grid = read_json(&family.join(fabric).join("tilegrid.json"))?;
    let grid = grid.as_object().context("tilegrid is not an object")?;
    // (name, x, y, type)
    let mut tiles: Vec<(&str, u32, u32, &str)> = Vec::new();
    // (site, x, y, primary type)
    let mut site_list: Vec<(&str, u32, u32, &str)> = Vec::new();
    for (name, t) in grid {
        let coord = |f: &str| {
            t[f].as_u64()
                .with_context(|| format!("tile {name}: no {f}"))
                .map(|v| v as u32)
        };
        let ty = t["type"]
            .as_str()
            .with_context(|| format!("tile {name}: no type"))?;
        let (x, y) = (coord("grid_x")?, coord("grid_y")?);
        tiles.push((name, x, y, ty));
        for (site, ty) in t["sites"].as_object().context("sites")? {
            site_list.push((site, x, y, ty.as_str().context("site type")?));
        }
    }
    let width = tiles.iter().map(|t| t.1).max().context("no tiles")? + 1;
    let height = tiles.iter().map(|t| t.2).max().context("no tiles")? + 1;
    ensure!(
        width <= 256 && height <= 256,
        "{fabric}: {width}x{height} grid too large for the cache format"
    );
    ensure!(
        tiles.len() == (width * height) as usize,
        "{fabric}: {} tiles for a {width}x{height} grid",
        tiles.len()
    );

    let mut types: Vec<ImportedType> = Vec::new();
    let mut type_index: HashMap<&str, u32> = HashMap::new();
    let mut wire_types: Vec<String> = Vec::new();
    let mut tile_of = vec![u32::MAX; tiles.len()];
    let mut names = vec![String::new(); tiles.len()];
    for &(name, x, y, ty) in &tiles {
        let t = match type_index.get(ty) {
            Some(&t) => t,
            None => {
                types.push(import_tile_type(family, ty, &mut wire_types, &mut stats)?);
                type_index.insert(ty, types.len() as u32 - 1);
                types.len() as u32 - 1
            }
        };
        let i = (y * width + x) as usize;
        ensure!(tile_of[i] == u32::MAX, "two tiles at X{x}Y{y}");
        tile_of[i] = t;
        names[i] = name.to_string();
    }
    ensure!(wire_types.len() <= 256, "more than 256 wire classes");

    let mut wire_base = Vec::with_capacity(tile_of.len());
    let mut flat: u32 = 0;
    for &t in &tile_of {
        wire_base.push(flat);
        flat += types[t as usize].tt.wires.len() as u32;
    }
    stats.tiles = tiles.len();
    stats.tile_types = types.len();
    stats.tile_wires = flat as usize;

    // Nodes: tileconn's rules, applied to every tile of the first type that
    // has a tile of the second type at the offset.
    let mut joins = Joins((0..flat).collect());
    let tileconn = read_json(&family.join(fabric).join("tileconn.json"))?;
    // tile type -> (dx, dy, other type, wire pairs)
    type Rule = (i64, i64, u32, Vec<(u32, u32)>);
    let mut rules: HashMap<u32, Vec<Rule>> = HashMap::new();
    for rule in tileconn.as_array().context("tileconn is not a list")? {
        let pair = rule["tile_types"].as_array().context("tile_types")?;
        let (Some(&a), Some(&b)) = (
            pair[0].as_str().and_then(|t| type_index.get(t)),
            pair[1].as_str().and_then(|t| type_index.get(t)),
        ) else {
            // A tile type this fabric doesn't have.
            continue;
        };
        let delta = rule["grid_deltas"].as_array().context("grid_deltas")?;
        let (dx, dy) = (
            delta[0].as_i64().context("dx")?,
            delta[1].as_i64().context("dy")?,
        );
        let mut wires = Vec::new();
        for w in rule["wire_pairs"].as_array().context("wire_pairs")? {
            let wa = w[0].as_str().context("wire")?;
            let wb = w[1].as_str().context("wire")?;
            let (Some(&ia), Some(&ib)) = (
                types[a as usize].index.get(wa),
                types[b as usize].index.get(wb),
            ) else {
                anyhow::bail!("tileconn joins undeclared wires {wa} / {wb}");
            };
            wires.push((ia, ib));
        }
        rules.entry(a).or_default().push((dx, dy, b, wires));
    }
    for y in 0..height {
        for x in 0..width {
            let tile = (y * width + x) as usize;
            let Some(rules) = rules.get(&tile_of[tile]) else {
                continue;
            };
            for (dx, dy, other, wires) in rules {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= width as i64 || ny >= height as i64 {
                    continue;
                }
                let neighbour = (ny as u32 * width + nx as u32) as usize;
                if tile_of[neighbour] != *other {
                    continue;
                }
                for &(a, b) in wires {
                    joins.join(wire_base[tile] + a, wire_base[neighbour] + b);
                }
            }
        }
    }

    // Number the nodes in flat wire order; a node's type is that of its
    // first wire in an interconnect tile, where it has one (that is where
    // its pips are), else of its first wire.
    let is_int = |t: u32| {
        matches!(
            types[t as usize].tt.name.as_str(),
            "INT_L" | "INT_R" | "INT"
        )
    };
    let mut node_of = vec![u32::MAX; flat as usize];
    let mut node_box: Vec<Box> = Vec::new();
    let mut node_type: Vec<u8> = Vec::new();
    let mut typed_in_int: Vec<bool> = Vec::new();
    // Node of each root wire, as nodes are numbered.
    let mut root_node = vec![u32::MAX; flat as usize];
    for (tile, &t) in tile_of.iter().enumerate() {
        let (x, y) = ((tile as u32 % width) as u8, (tile as u32 / width) as u8);
        let tt = &types[t as usize].tt;
        for w in 0..tt.wires.len() as u32 {
            let fw = wire_base[tile] + w;
            let root = joins.find(fw) as usize;
            let ty = tt.wire_types[w as usize];
            let n = root_node[root];
            let n = if n == u32::MAX {
                let n = node_box.len() as u32;
                node_box.push(Box::tile(x, y));
                node_type.push(ty);
                typed_in_int.push(is_int(t));
                root_node[root] = n;
                n
            } else {
                node_box[n as usize].add(x, y);
                if is_int(t) && !typed_in_int[n as usize] {
                    node_type[n as usize] = ty;
                    typed_in_int[n as usize] = true;
                }
                n
            };
            node_of[fw as usize] = n;
        }
    }
    stats.nodes = node_box.len();
    stats.pips = tile_of
        .iter()
        .map(|&t| types[t as usize].tt.pips.len())
        .sum();

    let mut pads = Vec::new();
    let pins = family.join(part).join("package_pins.csv");
    let tile_by_name: HashMap<&str, u32> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i as u32))
        .collect();
    let text =
        std::fs::read_to_string(&pins).with_context(|| format!("reading {}", pins.display()))?;
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        ensure!(f.len() >= 4, "{}: short line {line:?}", pins.display());
        let tile = *tile_by_name
            .get(f[3])
            .with_context(|| format!("pin {}: no tile {}", f[0], f[3]))?;
        pads.push(Pad {
            pin: f[0].to_string(),
            tile,
            bel: f[2].to_string(),
            io: PadIo::Bidir,
        });
    }

    // nextpnr's relative site names: per tile and site prefix, coordinates
    // less the smallest of that prefix in the tile (`xilinx_device.py`).
    let split = |site: &str| -> Option<(String, u32, u32)> {
        let (prefix, xy) = site.rsplit_once('_')?;
        let (x, y) = xy.strip_prefix('X')?.split_once('Y')?;
        Some((prefix.to_string(), x.parse().ok()?, y.parse().ok()?))
    };
    let mut base: HashMap<(u32, u32, String), (u32, u32)> = HashMap::new();
    for &(site, x, y, _) in &site_list {
        let (prefix, sx, sy) = split(site).with_context(|| format!("site name {site}"))?;
        let b = base.entry((x, y, prefix)).or_insert((sx, sy));
        *b = (b.0.min(sx), b.1.min(sy));
    }
    let mut sites = HashMap::new();
    for &(site, x, y, ty) in &site_list {
        let (prefix, sx, sy) = split(site).expect("checked above");
        let (bx, by) = base[&(x, y, prefix.clone())];
        sites.insert(
            site.to_string(),
            Site {
                tile: y * width + x,
                relative: format!("{prefix}_X{}Y{}", sx - bx, sy - by),
                ty: ty.to_string(),
            },
        );
    }

    let tile_types = types.into_iter().map(|t| t.tt).collect();
    let db = ChipDb::assemble(
        Grid {
            width,
            height,
            tiles: tile_of,
            names,
            // Grid x counts interconnect and site columns alike, two or
            // more per interconnect tile; y is one per interconnect row.
            // Swept on picorv32 (README): 0.5/1 routes in half the time
            // but with 3.7% more pips and an 11% slower critical path.
            distance: [0.25, 0.5],
            sites,
        },
        tile_types,
        wire_types,
        pads,
        Nodes {
            wire_base,
            node_of,
            node_box,
            node_type,
        },
    );
    Ok((db, stats))
}

/// The part's graph: the prebuilt one at `GRAPH_VAR` (the nix package
/// ships one), or else imported from prjxray-db (`flow::PRJXRAY_DB_VAR`)
/// and, where `GRAPH_VAR` names a writable path, cached there.
pub fn load(part: Xc7Part) -> Result<ChipDb> {
    let cache = std::env::var_os(GRAPH_VAR).map(std::path::PathBuf::from);
    if let Some(cache) = cache.as_deref().filter(|c| c.exists()) {
        match ChipDb::read_cache(cache) {
            Ok(db) => return Ok(db),
            Err(e) => eprintln!("    {e:#}; importing prjxray-db instead"),
        }
    }
    let var = crate::flow::PRJXRAY_DB_VAR;
    let root = std::env::var_os(var).with_context(|| {
        format!("{var} is not set; run chipsmith from its nix package or `nix develop`")
    })?;
    let (db, _) = import(&Path::new(&root).join(part.family), part.die, part.part)?;
    if let Some(cache) = cache {
        if db.write_cache(&cache).is_ok() {
            eprintln!("    cached the routing graph in {}", cache.display());
        }
    }
    Ok(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interconnect_wires_are_classed_by_length() {
        assert_eq!(wire_class("EE2BEG0"), "DOUBLE");
        assert_eq!(wire_class("NN6END3"), "HEX");
        assert_eq!(wire_class("WR1BEG_S0"), "SINGLE");
        assert_eq!(wire_class("SW4END2"), "QUAD");
        assert_eq!(wire_class("LV_L18"), "LONG_V");
        assert_eq!(wire_class("IMUX_L34"), "PINFEED");
        assert_eq!(wire_class("GCLK_L_B11"), "CLOCK");
        assert_eq!(wire_class("LOGIC_OUTS_L4"), "OUTPUT");
    }

    #[test]
    fn joins_keep_the_lowest_wire_as_root() {
        let mut j = Joins((0..5).collect());
        j.join(3, 1);
        j.join(4, 3);
        assert_eq!(j.find(4), 1);
        assert_eq!(j.find(2), 2);
    }
}
