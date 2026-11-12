//! The device's idle state: the image a design is written over, every block
//! instance as it is when no design uses it (agilex5-re README (n)).

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::db::{Db, PARTS};

/// The idle image: every mapped bit of every instance at its idle value,
/// plus the unnamed constant bits. Parts no map covers stay zero.
pub fn image(db: &mut Db) -> Result<Vec<u8>> {
    let len = PARTS.iter().map(|(_, _, s, n)| s + n).max().unwrap();
    let mut img = vec![0u8; len];
    let idle = crate::db::read_table(&db.dir.join("idle.tsv"))?;
    let mut ones: HashMap<&str, rustc_hash::FxHashSet<&str>> = HashMap::new();
    for l in idle.lines().skip(1) {
        let (t, b) = l.split_once('\t').unwrap_or((l, ""));
        ones.entry(t).or_default().insert(b);
    }
    for t in db.types()? {
        db.load(&t)?;
        let map = db.map(&t);
        let empty = Default::default();
        let on = ones.get(t.as_str()).unwrap_or(&empty);
        for u in map.uids() {
            for (b, p) in map.bits(u) {
                p.set(&mut img, on.contains(b));
            }
        }
    }
    for l in crate::db::read_table(&db.dir.join("idle_inst.tsv"))?
        .lines()
        .skip(1)
    {
        let f: Vec<&str> = l.split('\t').collect();
        let t = db.type_of(f[0])?;
        let p = db
            .map(&t)
            .pos(f[0], f[1])
            .with_context(|| format!("{} {} is not mapped", f[0], f[1]))?;
        p.set(&mut img, f[2] == "1");
    }
    for l in crate::db::read_table(&db.dir.join("idle_unnamed.tsv"))?
        .lines()
        .skip(1)
    {
        let f: Vec<&str> = l.split('\t').collect();
        let (_, _, start, _) = PARTS
            .iter()
            .find(|(o, p, ..)| *o == f[0] && *p == f[1])
            .context("unknown part")?;
        let off: usize = f[2].parse()?;
        img[start + off / 8] |= 1 << (off % 8);
    }
    Ok(img)
}
