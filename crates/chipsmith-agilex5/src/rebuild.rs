//! A placed and routed design (a5pnr's design file) over the idle image:
//! the configuration image chipsmith assembles (agilex5-re README (q)).
//!
//! A design file is lines of
//!
//!   pin    <ball>=<use>[:<std>]          I/O, through db/sm4revb/io/template.tsv
//!   edge   <src node> <dst node>         a routing graph edge
//!   attr   <uid> <attribute> <setting>   a CUSTOM attribute
//!   direct <uid> <attribute> <bits>      a DIRECT attribute, MSB first
//!   bit    <uid> <bit> <0|1>             a named bit no attribute explains
//!   raw    <object/part> <offset> <0|1>  an image bit no named bit covers
//!
//! over the idle image, written in that order. agilex5-re's `a5db extract`
//! reads one back from an image.

use anyhow::{bail, Context, Result};

use crate::db::{Db, PARTS};
use crate::graph::{Graph, Join};
use crate::{blank, pins};

/// Write a CUSTOM setting's bits (every bit the setting lists, as `a5db set`).
fn write_setting(
    db: &mut Db,
    btype: &str,
    uid: &str,
    path: &str,
    setting: &str,
    img: &mut [u8],
) -> Result<()> {
    let a = db
        .attribute(btype, path)
        .with_context(|| format!("{btype} has no attribute {path}"))?;
    let s = db
        .bcm
        .settings(&a)
        .iter()
        .find(|s| s.name == setting)
        .with_context(|| format!("{uid} {path} has no setting {setting}"))?;
    let map = db.map(btype);
    for (b, &(v, _)) in s.bits.iter().zip(&s.values) {
        map.pos(uid, &format!("{}{b}", a.prefix))
            .with_context(|| format!("{uid} {}{b} is not mapped", a.prefix))?
            .set(img, v);
    }
    Ok(())
}

fn write_direct(
    db: &Db,
    btype: &str,
    uid: &str,
    path: &str,
    value: &str,
    img: &mut [u8],
) -> Result<()> {
    let bits = db
        .bcm
        .direct_bits(btype, path)
        .with_context(|| format!("{btype} {path} is not DIRECT"))?;
    if bits.len() != value.len() {
        bail!("{uid} {path}: {} bits, got {}", bits.len(), value.len());
    }
    let map = db.map(btype);
    for (b, c) in bits.iter().zip(value.bytes()) {
        map.pos(uid, b)
            .with_context(|| format!("{uid} {b} is not mapped"))?
            .set(img, c == b'1');
    }
    Ok(())
}

fn write_raw(part: &str, off: usize, v: bool, img: &mut [u8]) -> Result<()> {
    let (o, p) = part
        .split_once('/')
        .with_context(|| format!("{part}: expected object/part"))?;
    let (_, _, start, _) = PARTS
        .iter()
        .find(|(a, b, ..)| *a == o && *b == p)
        .with_context(|| format!("no part {part}"))?;
    let m = 1u8 << (off % 8);
    img[start + off / 8] = if v {
        img[start + off / 8] | m
    } else {
        img[start + off / 8] & !m
    };
    Ok(())
}

/// The edges' bits, written into img.
fn write_edges(join: &mut Join, edges: &[usize], img: &mut [u8]) -> Result<()> {
    for &e in edges {
        if let Some(Ok(w)) = join.edge(e)? {
            for (p, v) in w {
                p.set(img, v);
            }
        }
    }
    Ok(())
}

pub fn build(db: &mut Db, graph: &Graph, design: &str) -> Result<Vec<u8>> {
    let t = std::time::Instant::now();
    let mut img = blank::image(db)?;
    eprintln!("build: idle image {:.1} s", t.elapsed().as_secs_f64());
    let lines: Vec<Vec<&str>> = design
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| l.split('\t').collect())
        .collect();
    let specs: Vec<String> = lines
        .iter()
        .filter(|f| f[0] == "pin")
        .map(|f| f[1].to_owned())
        .collect();
    let pins = pins::parse(db, &specs)?;
    let rows = pins::table(db, &pins)?;
    crate::db::write_rows(db, &mut img, &rows)?;
    let idx = graph.lookup(
        lines
            .iter()
            .filter(|f| f[0] == "edge")
            .flat_map(|f| [f[1], f[2]]),
    );
    eprintln!(
        "build: pins and graph index {:.1} s",
        t.elapsed().as_secs_f64()
    );
    let mut edges = Vec::new();
    for f in lines.iter().filter(|f| f[0] == "edge") {
        let (s, d) = (
            idx.get(f[1]).with_context(|| format!("no node {}", f[1]))?,
            idx.get(f[2]).with_context(|| format!("no node {}", f[2]))?,
        );
        edges.push(
            graph
                .edge(*s, *d)
                .with_context(|| format!("no edge {} -> {}", f[1], f[2]))?,
        );
    }
    let mut join = Join::new(db, graph);
    write_edges(&mut join, &edges, &mut img)?;
    drop(join);
    eprintln!("build: edges {:.1} s", t.elapsed().as_secs_f64());
    for kind in ["attr", "direct", "bit", "raw"] {
        for f in lines.iter().filter(|f| f[0] == kind) {
            match kind {
                "attr" => {
                    let t = db.type_of(f[1])?;
                    write_setting(db, &t, f[1], f[2], f[3], &mut img)?
                }
                "direct" => {
                    let t = db.type_of(f[1])?;
                    write_direct(db, &t, f[1], f[2], f[3], &mut img)?
                }
                "bit" => {
                    let t = db.type_of(f[1])?;
                    db.map(&t)
                        .pos(f[1], f[2])
                        .with_context(|| format!("{} {} is not mapped", f[1], f[2]))?
                        .set(&mut img, f[3] == "1")
                }
                _ => write_raw(f[1], f[2].parse()?, f[3] == "1", &mut img)?,
            }
        }
    }
    Ok(img)
}
