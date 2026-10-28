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

/// Bump when `ChipDb`'s layout changes, or the chip database gains
/// something the router needs (a stale cache would silently lack it).
const CACHE_MAGIC: &[u8] = b"chipsmith-routing-graph-12\n";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TileType {
    pub name: String,
    pub wires: Names,
    /// Element type of each wire, an index into `ChipDb::wire_types`.
    pub wire_types: Vec<u8>,
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
    pub io: PadIo,
}

/// What a pad's I/O element can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PadIo {
    /// Input, output, tri-state or bidir: the bel has an output enable.
    Bidir,
    /// A dedicated clock input (Cyclone II's CLK pins): no output enable,
    /// and no route into its output data.
    ClockInput,
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
    pub fn push(&mut self, name: &str) {
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

/// A site of a tile, as nextpnr's xilinx chip database names it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    pub tile: u32,
    /// Name relative to the tile's other sites of the same prefix, e.g.
    /// `SLICE_X1Y0`: nextpnr's site wires and bels are `<this>.<name>`.
    pub relative: String,
    /// The site's primary type, e.g. `IOB33M`.
    pub ty: String,
}

/// The tile grid, for `ChipDb::assemble`.
pub struct Grid {
    pub width: u32,
    pub height: u32,
    /// Tile type of tile `y * width + x`.
    pub tiles: Vec<u32>,
    pub names: Vec<String>,
    pub distance: [f32; 2],
    pub sites: HashMap<String, Site>,
}

/// Which node each tile wire is in, for `ChipDb::assemble`.
pub struct Nodes {
    /// First flat wire of each tile.
    pub wire_base: Vec<u32>,
    pub node_of: Vec<NodeId>,
    pub node_box: Vec<Box>,
    pub node_type: Vec<u8>,
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
    /// Element type names (`R4`, `C16`, `LOCAL_INTERCONNECT`, ...).
    pub wire_types: Vec<String>,
    /// The vendor's tile names (`INT_L_X2Y10`), where FASM needs them;
    /// empty when tiles go by `X<x>Y<y>` only.
    pub tile_names: Vec<String>,
    /// What one tile of distance costs the router's A* estimate, in x and
    /// in y, in units of one wire.
    pub tile_distance: [f32; 2],
    /// Sites by their absolute vendor name (`SLICE_X30Y39`), where the
    /// device has them; empty otherwise.
    pub sites: HashMap<String, Site>,
    /// Tile type of tile `y * width + x`.
    tiles: Vec<u32>,
    // The big arrays below are stored raw by `write_cache`, not by serde:
    // varint-decoding their ~40M integers took most of a second.
    /// First flat wire of each tile.
    #[serde(skip)]
    wire_base: Vec<u32>,
    #[serde(skip)]
    node_of: Vec<NodeId>,
    /// The tiles each node spans, for distance estimates: a long wire is
    /// near everything along its length, not only its first tile.
    #[serde(skip)]
    pub node_box: Vec<Box>,
    /// Element type of each node, an index into `wire_types`.
    #[serde(skip)]
    pub node_type: Vec<u8>,
    #[serde(skip)]
    edge_start: Vec<u32>,
    #[serde(skip)]
    edges: Vec<Edge>,
}

/// A bounding box of tiles, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Box {
    pub x0: u8,
    pub y0: u8,
    pub x1: u8,
    pub y1: u8,
}

impl Box {
    pub fn tile(x: u8, y: u8) -> Self {
        Box {
            x0: x,
            y0: y,
            x1: x,
            y1: y,
        }
    }

    pub fn add(&mut self, x: u8, y: u8) {
        self.x0 = self.x0.min(x);
        self.y0 = self.y0.min(y);
        self.x1 = self.x1.max(x);
        self.y1 = self.y1.max(y);
    }

    /// Tiles between the two boxes, per axis (0 where they overlap).
    pub fn gap(self, o: Box) -> (u8, u8) {
        let gap = |a0: u8, a1: u8, b0: u8, b1: u8| b0.saturating_sub(a1).max(a0.saturating_sub(b1));
        (
            gap(self.x0, self.x1, o.x0, o.x1),
            gap(self.y0, self.y1, o.y0, o.y1),
        )
    }

    fn word(self) -> u32 {
        u32::from_le_bytes([self.x0, self.y0, self.x1, self.y1])
    }

    fn from_word(w: u32) -> Self {
        let [x0, y0, x1, y1] = w.to_le_bytes();
        Box { x0, y0, x1, y1 }
    }
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
        let mut wire_types: Vec<String> = Vec::new();

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
                    let ty = f.next().context("wire type")?;
                    let ty = match wire_types.iter().position(|w| w == ty) {
                        Some(i) => i,
                        None => {
                            wire_types.push(ty.to_string());
                            wire_types.len() - 1
                        }
                    };
                    ensure!(ty < 256, "more than 256 wire types");
                    wire_index[t].insert(name.to_string(), tt.wires.len() as u32);
                    tt.wires.push(name);
                    tt.wire_types.push(ty as u8);
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
        // Node boxes hold tile coordinates in a byte each.
        ensure!(
            width <= 256 && height <= 256,
            "chip too large for the cache format"
        );

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
        let mut node_box: Vec<Box> = Vec::new();
        let mut node_type: Vec<u8> = Vec::new();
        // (flat wire, its type)
        let flat = |x: u32, y: u32, name: &str| -> Result<(u32, u8)> {
            ensure!(x < width && y < height, "node wire outside the chip");
            let tile = (y * width + x) as usize;
            let tt = tiles[tile] as usize;
            let w = *wire_index[tt]
                .get(name)
                .with_context(|| format!("node wire X{x}Y{y}/{name} undeclared"))?;
            Ok((wire_base[tile] + w, tile_types[tt].wire_types[w as usize]))
        };
        for line in &node_lines {
            let f: Vec<&str> = line.split_ascii_whitespace().skip(1).collect();
            ensure!(
                f.len().is_multiple_of(3) && !f.is_empty(),
                "malformed node line"
            );
            let id = node_box.len() as NodeId;
            let mut ty = 0;
            let mut bbox: Option<Box> = None;
            for (i, w) in f.chunks(3).enumerate() {
                let (x, y): (u32, u32) = (w[0].parse()?, w[1].parse()?);
                match &mut bbox {
                    Some(b) => b.add(x as u8, y as u8),
                    None => bbox = Some(Box::tile(x as u8, y as u8)),
                }
                let (fw, t) = flat(x, y, w[2])?;
                ensure!(
                    node_of[fw as usize] == u32::MAX,
                    "X{x}Y{y}/{} is in two nodes",
                    w[2]
                );
                node_of[fw as usize] = id;
                // A node is one physical wire; its first tile wire names the type.
                if i == 0 {
                    ty = t;
                }
            }
            node_box.push(bbox.expect("node lines have at least one wire"));
            node_type.push(ty);
        }
        for (tile, &t) in tiles.iter().enumerate() {
            let (x, y) = ((tile as u32 % width) as u8, (tile as u32 / width) as u8);
            for w in 0..tile_types[t as usize].wires.len() {
                let fw = wire_base[tile] as usize + w;
                if node_of[fw] == u32::MAX {
                    node_of[fw] = node_box.len() as NodeId;
                    node_box.push(Box::tile(x, y));
                    node_type.push(tile_types[t as usize].wire_types[w]);
                }
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
            let tile = y * width + x;
            let pins = &tile_types[tiles[tile as usize] as usize]
                .bels
                .get(&bel)
                .with_context(|| format!("pad {pin}: no bel {bel} in tile {tile}"))?
                .pins;
            let io = if pins.contains_key("OE") {
                PadIo::Bidir
            } else {
                PadIo::ClockInput
            };
            pads.push(Pad { pin, tile, bel, io });
        }

        Ok(ChipDb::assemble(
            Grid {
                width,
                height,
                tiles,
                names: Vec::new(),
                // Tile x counts routing and logic columns separately: halve it.
                distance: [0.5, 1.0],
                sites: HashMap::new(),
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
        ))
    }

    /// The graph from its parts: every pip of every tile becomes an edge
    /// between the nodes its two wires are in, grouped by source node (CSR).
    pub fn assemble(
        grid: Grid,
        tile_types: Vec<TileType>,
        wire_types: Vec<String>,
        pads: Vec<Pad>,
        nodes: Nodes,
    ) -> Self {
        let Grid {
            width,
            height,
            tiles,
            names: tile_names,
            distance: tile_distance,
            sites,
        } = grid;
        let Nodes {
            wire_base,
            node_of,
            node_box,
            node_type,
        } = nodes;
        let count = node_box.len();
        let mut out_degree = vec![0u32; count + 1];
        for (tile, &t) in tiles.iter().enumerate() {
            for &(s, _) in &tile_types[t as usize].pips {
                out_degree[node_of[(wire_base[tile] + s) as usize] as usize] += 1;
            }
        }
        let mut edge_start = Vec::with_capacity(count + 1);
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
        ChipDb {
            pads,
            width,
            height,
            tile_types,
            wire_types,
            tile_names,
            tile_distance,
            sites,
            tiles,
            wire_base,
            node_of,
            node_box,
            node_type,
            edge_start,
            edges,
        }
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
        put_words(&mut bytes, self.node_box.iter().map(|b| b.word()));
        put_words(&mut bytes, self.node_type.iter().map(|&t| t as u32));
        put_words(&mut bytes, self.edge_start.iter().copied());
        put_words(
            &mut bytes,
            self.edges
                .iter()
                .flat_map(|e| [e.dst, e.tile, e.pip])
                .collect::<Vec<_>>()
                .into_iter(),
        );
        // Through a temporary file, so a reader never sees half a cache.
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
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
        let mut arrays: [&[u8]; 6] = [&[]; 6];
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
        let [wire_base, node_of, node_box, node_type, edge_start, edges] = arrays;
        let (db, wire_base, node_of, node_box, node_type, edge_start, edges) =
            std::thread::scope(|s| {
                let wire_base = s.spawn(|| decode(wire_base, |[w]| w));
                let node_of = s.spawn(|| decode(node_of, |[w]| w));
                let node_type = s.spawn(|| decode(node_type, |[w]| w as u8));
                let node_box = s.spawn(|| decode(node_box, |[w]| Box::from_word(w)));
                let edge_start = s.spawn(|| decode(edge_start, |[w]| w));
                let edges = s.spawn(|| decode(edges, |[dst, tile, pip]| Edge { dst, tile, pip }));
                let db = postcard::from_bytes::<ChipDb>(header);
                let join = "decoding thread panicked";
                (
                    db,
                    wire_base.join().expect(join),
                    node_of.join().expect(join),
                    node_box.join().expect(join),
                    node_type.join().expect(join),
                    edge_start.join().expect(join),
                    edges.join().expect(join),
                )
            });
        let mut db = db?;
        db.wire_base = wire_base;
        db.node_of = node_of;
        db.node_box = node_box;
        db.node_type = node_type;
        db.edge_start = edge_start;
        db.edges = edges;
        Ok(db)
    }

    pub fn nodes(&self) -> usize {
        self.node_box.len()
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

    /// `<tile>.<dst>.<src>`, the pip's FASM feature, on devices whose
    /// tiles have vendor names.
    pub fn pip_feature(&self, tile: u32, pip: u32) -> Option<String> {
        let name = self.tile_names.get(tile as usize)?;
        let tt = self.tile_type(tile);
        let (s, d) = tt.pips[pip as usize];
        Some(format!("{name}.{}.{}", tt.wires.get(d), tt.wires.get(s)))
    }

    /// The tile at `X<x>Y<y>` (nextpnr's tile names).
    pub fn tile_by_name(&self, name: &str) -> Option<u32> {
        let (x, y) = name.strip_prefix('X')?.split_once('Y')?;
        self.tile_index(x.parse().ok()?, y.parse().ok()?)
    }
}

#[cfg(test)]
pub mod tests {
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
