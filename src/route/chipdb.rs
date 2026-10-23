//! The routing graph, from cyclone2-re's `chipdb.txt.xz` (the same chip
//! description nextpnr's database is built from, so wire, pip and bel names
//! match nextpnr's exactly).
//!
//! The file describes tile types (wires, pips, bels and their pins), the
//! tile grid, and nodes: tile wires in different tiles that are one
//! physical wire. Flattened, every node, and every tile wire in no node, is
//! one routing resource; every pip of every tile is an edge between two.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

pub type NodeId = u32;

/// A prebuilt routing graph (`chipsmith routing-graph`).
pub const GRAPH_VAR: &str = "CHIPSMITH_CYCLONE2_GRAPH";
/// cyclone2-re's `db/` directory, holding `EP2C35F672C6/chipdb.txt.xz`.
pub const DB_VAR: &str = "CHIPSMITH_CYCLONE2_DB";

/// Bump when `ChipDb`'s layout changes.
const CACHE_MAGIC: &[u8] = b"chipsmith-routing-graph-4\n";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TileType {
    pub name: String,
    pub wires: Names,
    /// (src wire, dst wire)
    pub pips: Vec<(u32, u32)>,
    pub bels: HashMap<String, Bel>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Bel {
    /// `LCELL_COMB`, `LCELL_FF`, `IOE`, `CLKCTRL`.
    pub ty: String,
    pub z: u32,
    /// pin name -> wire
    pub pins: HashMap<String, u32>,
}

/// A package pin and the I/O bel behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pad {
    /// Ball name without `PIN_`, e.g. `N2`.
    pub pin: String,
    pub tile: u32,
    pub bel: String,
}

impl TileType {
    /// Wire by name: a linear scan, for tests and one-off lookups. The
    /// parser keeps its own index; the router and placer go by bel pins.
    pub fn wire(&self, name: &str) -> Option<u32> {
        (0..self.wires.len() as u32).find(|&i| self.wires.get(i) == name)
    }
}

/// A tile type's wire names as one string: 2.3M separate `String`s took a
/// second to allocate on every cache load.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Names {
    blob: String,
    ends: Vec<u32>,
}

impl Names {
    fn push(&mut self, name: &str) {
        self.blob.push_str(name);
        self.ends.push(self.blob.len() as u32);
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    pub fn get(&self, i: u32) -> &str {
        let i = i as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.blob[start..self.ends[i] as usize]
    }
}

/// One pip of one tile: an edge of the routing graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub dst: NodeId,
    pub tile: u32,
    pub pip: u32,
}

#[derive(Serialize, Deserialize)]
pub struct ChipDb {
    pub pads: Vec<Pad>,
    pub width: u32,
    pub height: u32,
    pub tile_types: Vec<TileType>,
    /// Tile type of tile `y * width + x`.
    tiles: Vec<u32>,
    // The big arrays below are stored raw by `write_cache`, not by serde:
    // varint-decoding their ~40M integers took most of a second.
    /// First flat wire of each tile.
    #[serde(skip)]
    wire_base: Vec<u32>,
    #[serde(skip)]
    node_of: Vec<NodeId>,
    /// A representative tile of each node, for distance estimates.
    #[serde(skip)]
    pub node_xy: Vec<(u16, u16)>,
    #[serde(skip)]
    edge_start: Vec<u32>,
    #[serde(skip)]
    edges: Vec<Edge>,
}

/// Little-endian `u32` arrays, each preceded by its length as a `u64`.
fn put_words(out: &mut Vec<u8>, words: impl ExactSizeIterator<Item = u32>) {
    out.extend((words.len() as u64).to_le_bytes());
    for w in words {
        out.extend(w.to_le_bytes());
    }
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(self.0.len() >= n, "routing graph cache is truncated");
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn len(&mut self) -> Result<usize> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()) as usize)
    }
}

impl ChipDb {
    pub fn load(path: &Path) -> Result<Self> {
        let file =
            std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let reader = BufReader::with_capacity(1 << 20, lzma_rust2::XzReader::new(file, true));
        Self::parse(reader).with_context(|| format!("reading {}", path.display()))
    }

    pub fn parse(reader: impl BufRead) -> Result<Self> {
        let (mut width, mut height) = (0, 0);
        let mut tile_types: Vec<TileType> = Vec::new();
        let mut type_index: HashMap<String, u32> = HashMap::new();
        let mut current: Option<usize> = None;
        let mut current_bel: Option<String> = None;
        let mut tile_type_of: Vec<(u32, u32, String)> = Vec::new();
        let mut node_lines: Vec<String> = Vec::new();
        let mut pad_lines: Vec<(String, String, String)> = Vec::new();
        // Wire name -> index, per tile type; only needed while parsing.
        let mut wire_index: Vec<HashMap<String, u32>> = Vec::new();

        for line in reader.lines() {
            let line = line?;
            let mut f = line.split_ascii_whitespace();
            let Some(kind) = f.next() else { continue };
            match kind {
                "chip" => {
                    width = f.next().context("chip width")?.parse()?;
                    height = f.next().context("chip height")?.parse()?;
                }
                "tt" => {
                    let name = f.next().context("tile type name")?.to_string();
                    type_index.insert(name.clone(), tile_types.len() as u32);
                    current = Some(tile_types.len());
                    wire_index.push(HashMap::new());
                    tile_types.push(TileType {
                        name,
                        ..Default::default()
                    });
                }
                "end" => current = None,
                "w" => {
                    let t = current.context("wire outside a tile type")?;
                    let tt = &mut tile_types[t];
                    let name = f.next().context("wire name")?;
                    wire_index[t].insert(name.to_string(), tt.wires.len() as u32);
                    tt.wires.push(name);
                }
                "p" => {
                    let t = current.context("pip outside a tile type")?;
                    let tt = &mut tile_types[t];
                    let (src, dst) = (f.next().context("pip src")?, f.next().context("pip dst")?);
                    let (Some(&s), Some(&d)) = (wire_index[t].get(src), wire_index[t].get(dst))
                    else {
                        bail!("pip {src} -> {dst} in {} names an undeclared wire", tt.name);
                    };
                    tt.pips.push((s, d));
                }
                "b" => {
                    let tt = &mut tile_types[current.context("bel outside a tile type")?];
                    let name = f.next().context("bel name")?.to_string();
                    let ty = f.next().context("bel type")?.to_string();
                    let z = f.next().context("bel z")?.parse()?;
                    tt.bels.insert(
                        name.clone(),
                        Bel {
                            ty,
                            z,
                            pins: HashMap::new(),
                        },
                    );
                    current_bel = Some(name);
                }
                "bp" => {
                    let t = current.context("bel pin outside a tile type")?;
                    let tt = &mut tile_types[t];
                    let (pin, wire) = (f.next().context("pin")?, f.next().context("pin wire")?);
                    let w = *wire_index[t]
                        .get(wire)
                        .with_context(|| format!("bel pin {pin} on undeclared wire {wire}"))?;
                    let bel = current_bel.as_ref().context("bel pin outside a bel")?;
                    tt.bels
                        .get_mut(bel)
                        .unwrap()
                        .pins
                        .insert(pin.to_string(), w);
                }
                "t" => {
                    let x = f.next().context("tile x")?.parse()?;
                    let y = f.next().context("tile y")?.parse()?;
                    tile_type_of.push((x, y, f.next().context("tile type")?.to_string()));
                }
                "node" => node_lines.push(line),
                "pad" => {
                    let pin = f.next().context("pad pin")?.to_string();
                    let tile = f.next().context("pad tile")?.to_string();
                    let bel = f.next().context("pad bel")?.to_string();
                    pad_lines.push((pin, tile, bel));
                }
                "pkg" => {}
                other => bail!("unknown chipdb line kind {other:?}"),
            }
        }
        ensure!(width > 0 && height > 0, "no chip line");

        let mut tiles = vec![u32::MAX; (width * height) as usize];
        for (x, y, ty) in tile_type_of {
            ensure!(x < width && y < height, "tile X{x}Y{y} outside the chip");
            tiles[(y * width + x) as usize] = *type_index
                .get(&ty)
                .with_context(|| format!("tile X{x}Y{y} has unknown type {ty}"))?;
        }
        ensure!(!tiles.contains(&u32::MAX), "tiles without a type");

        let mut wire_base = Vec::with_capacity(tiles.len());
        let mut flat_wires: u32 = 0;
        for &t in &tiles {
            wire_base.push(flat_wires);
            flat_wires += tile_types[t as usize].wires.len() as u32;
        }

        // Nodes first, then every remaining tile wire is a node of its own.
        let mut node_of = vec![u32::MAX; flat_wires as usize];
        let mut node_xy: Vec<(u16, u16)> = Vec::new();
        let flat = |x: u32, y: u32, name: &str| -> Result<u32> {
            ensure!(x < width && y < height, "node wire outside the chip");
            let tile = (y * width + x) as usize;
            let w = *wire_index[tiles[tile] as usize]
                .get(name)
                .with_context(|| format!("node wire X{x}Y{y}/{name} undeclared"))?;
            Ok(wire_base[tile] + w)
        };
        for line in &node_lines {
            let f: Vec<&str> = line.split_ascii_whitespace().skip(1).collect();
            ensure!(
                f.len().is_multiple_of(3) && !f.is_empty(),
                "malformed node line"
            );
            let id = node_xy.len() as NodeId;
            for w in f.chunks(3) {
                let (x, y) = (w[0].parse()?, w[1].parse()?);
                let fw = flat(x, y, w[2])? as usize;
                ensure!(node_of[fw] == u32::MAX, "X{x}Y{y}/{} is in two nodes", w[2]);
                node_of[fw] = id;
            }
            node_xy.push((f[0].parse()?, f[1].parse()?));
        }
        for (tile, &t) in tiles.iter().enumerate() {
            let (x, y) = ((tile as u32 % width) as u16, (tile as u32 / width) as u16);
            for w in 0..tile_types[t as usize].wires.len() {
                let fw = wire_base[tile] as usize + w;
                if node_of[fw] == u32::MAX {
                    node_of[fw] = node_xy.len() as NodeId;
                    node_xy.push((x, y));
                }
            }
        }

        // Edges, grouped by source node (CSR).
        let nodes = node_xy.len();
        let mut out_degree = vec![0u32; nodes + 1];
        for (tile, &t) in tiles.iter().enumerate() {
            for &(s, _) in &tile_types[t as usize].pips {
                out_degree[node_of[(wire_base[tile] + s) as usize] as usize] += 1;
            }
        }
        let mut edge_start = Vec::with_capacity(nodes + 1);
        let mut acc = 0;
        for d in &out_degree {
            edge_start.push(acc);
            acc += d;
        }
        let mut fill = edge_start.clone();
        let mut edges = vec![
            Edge {
                dst: 0,
                tile: 0,
                pip: 0
            };
            acc as usize
        ];
        for (tile, &t) in tiles.iter().enumerate() {
            for (pip, &(s, d)) in tile_types[t as usize].pips.iter().enumerate() {
                let src = node_of[(wire_base[tile] + s) as usize] as usize;
                edges[fill[src] as usize] = Edge {
                    dst: node_of[(wire_base[tile] + d) as usize],
                    tile: tile as u32,
                    pip: pip as u32,
                };
                fill[src] += 1;
            }
        }

        let mut pads = Vec::new();
        for (pin, tile, bel) in pad_lines {
            let (x, y) = tile
                .strip_prefix('X')
                .and_then(|t| t.split_once('Y'))
                .and_then(|(x, y)| Some((x.parse::<u32>().ok()?, y.parse::<u32>().ok()?)))
                .with_context(|| format!("pad {pin}: tile {tile}"))?;
            ensure!(x < width && y < height, "pad {pin} off chip");
            pads.push(Pad {
                pin,
                tile: y * width + x,
                bel,
            });
        }

        Ok(ChipDb {
            pads,
            width,
            height,
            tile_types,
            tiles,
            wire_base,
            node_of,
            node_xy,
            edge_start,
            edges,
        })
    }

    /// The flattened graph, for `read_cache`. Starts with a magic and a
    /// format version, so a cache from another chipsmith is refused rather
    /// than misread.
    pub fn write_cache(&self, path: &Path) -> Result<()> {
        let header = postcard::to_stdvec(self)?;
        let mut bytes = CACHE_MAGIC.to_vec();
        bytes.extend((header.len() as u64).to_le_bytes());
        bytes.extend(header);
        put_words(&mut bytes, self.wire_base.iter().copied());
        put_words(&mut bytes, self.node_of.iter().copied());
        put_words(
            &mut bytes,
            self.node_xy
                .iter()
                .map(|&(x, y)| x as u32 | (y as u32) << 16),
        );
        put_words(&mut bytes, self.edge_start.iter().copied());
        put_words(
            &mut bytes,
            self.edges
                .iter()
                .flat_map(|e| [e.dst, e.tile, e.pip])
                .collect::<Vec<_>>()
                .into_iter(),
        );
        std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
    }

    pub fn read_cache(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let body = bytes.strip_prefix(CACHE_MAGIC).with_context(|| {
            format!(
                "{} is not a routing graph of this chipsmith version",
                path.display()
            )
        })?;
        let mut c = Cursor(body);
        let header_len = c.len()?;
        let header = c.take(header_len)?;
        let mut arrays: [&[u8]; 5] = [&[]; 5];
        for a in &mut arrays {
            let n = c.len()?;
            *a = c.take(n * 4)?;
        }
        ensure!(c.0.is_empty(), "routing graph cache has trailing bytes");

        // Converting ~70 MB is bound by first-touch page faults, which
        // parallelise across cores.
        fn decode<T: Send, const N: usize>(b: &[u8], f: impl Fn([u32; N]) -> T) -> Vec<T> {
            b.as_chunks::<4>()
                .0
                .as_chunks::<N>()
                .0
                .iter()
                .map(|w| f(w.map(u32::from_le_bytes)))
                .collect()
        }
        let [wire_base, node_of, node_xy, edge_start, edges] = arrays;
        let (db, wire_base, node_of, node_xy, edge_start, edges) = std::thread::scope(|s| {
            let wire_base = s.spawn(|| decode(wire_base, |[w]| w));
            let node_of = s.spawn(|| decode(node_of, |[w]| w));
            let node_xy = s.spawn(|| decode(node_xy, |[w]| (w as u16, (w >> 16) as u16)));
            let edge_start = s.spawn(|| decode(edge_start, |[w]| w));
            let edges = s.spawn(|| decode(edges, |[dst, tile, pip]| Edge { dst, tile, pip }));
            let db = postcard::from_bytes::<ChipDb>(header);
            let join = "decoding thread panicked";
            (
                db,
                wire_base.join().expect(join),
                node_of.join().expect(join),
                node_xy.join().expect(join),
                edge_start.join().expect(join),
                edges.join().expect(join),
            )
        });
        let mut db = db?;
        db.wire_base = wire_base;
        db.node_of = node_of;
        db.node_xy = node_xy;
        db.edge_start = edge_start;
        db.edges = edges;
        Ok(db)
    }

    /// The EP2C35's graph: the prebuilt one at `GRAPH_VAR` (the nix package
    /// ships one), or else parsed from the chip database under `DB_VAR`, which
    /// takes several seconds longer. A missing or stale cache at `GRAPH_VAR`
    /// is rewritten from the parse when the path is writable (the devshell
    /// points it into `target/`).
    pub fn load_for_device() -> Result<Self> {
        let cache = std::env::var_os(GRAPH_VAR).map(std::path::PathBuf::from);
        if let Some(cache) = cache.as_deref().filter(|c| c.exists()) {
            match Self::read_cache(cache) {
                Ok(db) => return Ok(db),
                Err(e) => eprintln!("    {e:#}; parsing the chip database instead"),
            }
        }
        let db = std::env::var_os(DB_VAR).with_context(|| {
            format!("{DB_VAR} is not set; run chipsmith from its nix package or `nix develop`")
        })?;
        let graph = Self::load(&Path::new(&db).join("EP2C35F672C6/chipdb.txt.xz"))?;
        if let Some(cache) = cache {
            if graph.write_cache(&cache).is_ok() {
                eprintln!("    cached the routing graph in {}", cache.display());
            }
        }
        Ok(graph)
    }

    pub fn nodes(&self) -> usize {
        self.node_xy.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn tile_index(&self, x: u32, y: u32) -> Option<u32> {
        (x < self.width && y < self.height).then_some(y * self.width + x)
    }

    pub fn tile_xy(&self, tile: u32) -> (u32, u32) {
        (tile % self.width, tile / self.width)
    }

    pub fn tile_type(&self, tile: u32) -> &TileType {
        &self.tile_types[self.tiles[tile as usize] as usize]
    }

    pub fn tile_type_index(&self, tile: u32) -> u32 {
        self.tiles[tile as usize]
    }

    pub fn node(&self, tile: u32, wire: u32) -> NodeId {
        self.node_of[(self.wire_base[tile as usize] + wire) as usize]
    }

    pub fn downhill(&self, node: NodeId) -> &[Edge] {
        let n = node as usize;
        &self.edges[self.edge_start[n] as usize..self.edge_start[n + 1] as usize]
    }

    /// `X<x>Y<y>/<wire>`, nextpnr's wire name.
    pub fn wire_name(&self, tile: u32, wire: u32) -> String {
        let (x, y) = self.tile_xy(tile);
        format!("X{x}Y{y}/{}", self.tile_type(tile).wires.get(wire))
    }

    /// `X<x>Y<y>/<dst>/<src>`, nextpnr's pip name.
    pub fn pip_name(&self, tile: u32, pip: u32) -> String {
        let tt = self.tile_type(tile);
        let (s, d) = tt.pips[pip as usize];
        let (x, y) = self.tile_xy(tile);
        format!("X{x}Y{y}/{}/{}", tt.wires.get(d), tt.wires.get(s))
    }

    pub fn pip_dst_wire(&self, tile: u32, pip: u32) -> u32 {
        self.tile_type(tile).pips[pip as usize].1
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Two tiles of one type: a pip A -> B in each, and node joins
    /// X0Y0/B with X1Y0/A, so a signal goes X0Y0/A -> B = X1Y0/A -> X1Y0/B.
    pub const TINY: &str = "\
chip 2 1
tt T
w A W
w B W
p A B
b BEL TYPE 0
bp I A I
bp O B O
end
t 0 0 T
t 1 0 T
node 0 0 B 1 0 A
";

    #[test]
    fn nodes_join_tile_wires_and_pips_become_edges() {
        let db = ChipDb::parse(TINY.as_bytes()).unwrap();
        // Three resources: X0Y0/A, the joined node, X1Y0/B.
        assert_eq!(db.nodes(), 3);
        let a0 = db.node(0, 0);
        let joined = db.node(0, 1);
        assert_eq!(db.node(1, 0), joined);
        let b1 = db.node(1, 1);
        assert_eq!(
            db.downhill(a0).iter().map(|e| e.dst).collect::<Vec<_>>(),
            [joined]
        );
        assert_eq!(
            db.downhill(joined)
                .iter()
                .map(|e| e.dst)
                .collect::<Vec<_>>(),
            [b1]
        );
        assert_eq!(db.pip_name(1, 0), "X1Y0/B/A");
        assert_eq!(db.tile_type(0).bels["BEL"].pins["O"], 1);
        assert_eq!(db.tile_type(0).bels["BEL"].ty, "TYPE");
    }
}
