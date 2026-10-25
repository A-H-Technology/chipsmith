//! Compare two packed netlists (ours and nextpnr's `--pack-only` output):
//! the same cells by name and type, the same parameters, every connected
//! port on the same net (by net name), and the nets in the same id order
//! (the order the router routes them in), and the same top-level ports
//! (direction, each bit's net), which the bitstream reads pin modes from.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

/// Flags written only when set by one side and as `0` by the other mean
/// the same.
const FLAGS: [&str; 4] = ["ENA_INV", "SCLR_INV", "DATA_INVERT", "OE_INVERT"];

#[derive(Debug, PartialEq, Eq)]
struct Cell {
    ty: String,
    params: BTreeMap<String, String>,
    port: Option<String>,
    /// Connected ports: port -> net name.
    conns: BTreeMap<String, String>,
}

struct Packed {
    cells: BTreeMap<String, Cell>,
    /// Connected nets' names in id order.
    net_order: Vec<String>,
    /// Top-level port -> direction and each bit's net name.
    ports: BTreeMap<String, (String, Vec<String>)>,
}

fn read(path: &Path) -> Result<Packed> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let json: Value = serde_json::from_str(&text)?;
    let top = &json["modules"]["top"];
    // The router's choice: the first name (sorted) of each id.
    let mut names: BTreeMap<u64, String> = BTreeMap::new();
    for (name, n) in top["netnames"].as_object().context("no netnames")? {
        if let Some(id) = n["bits"][0].as_u64() {
            names.entry(id).or_insert_with(|| name.clone());
        }
    }
    let mut connected = std::collections::BTreeSet::new();
    let mut cells = BTreeMap::new();
    for (name, c) in top["cells"].as_object().context("no cells")? {
        let mut params: BTreeMap<String, String> = c["parameters"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
            .collect();
        params.retain(|k, v| !(FLAGS.contains(&k.as_str()) && !v.contains('1')));
        let mut conns = BTreeMap::new();
        for (p, bits) in c["connections"].as_object().context("connections")? {
            if let Some(id) = bits[0].as_u64() {
                connected.insert(id);
                let net = names.get(&id).cloned().unwrap_or_else(|| format!("#{id}"));
                conns.insert(p.clone(), net);
            }
        }
        cells.insert(
            name.clone(),
            Cell {
                ty: c["type"].as_str().unwrap_or_default().to_string(),
                params,
                port: c["attributes"]["PORT"].as_str().map(str::to_string),
                conns,
            },
        );
    }
    let net_order = connected
        .into_iter()
        .map(|id| names.get(&id).cloned().unwrap_or_else(|| format!("#{id}")))
        .collect();
    let mut ports = BTreeMap::new();
    for (name, p) in top["ports"].as_object().context("no ports")? {
        let bits = p["bits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|b| match b.as_u64() {
                Some(id) => names.get(&id).cloned().unwrap_or_else(|| format!("#{id}")),
                None => b.to_string(),
            })
            .collect();
        let dir = p["direction"].as_str().unwrap_or_default().to_string();
        ports.insert(name.clone(), (dir, bits));
    }
    Ok(Packed {
        cells,
        net_order,
        ports,
    })
}

#[derive(Debug)]
pub struct Diff {
    pub equal: bool,
    pub same_net_order: bool,
    pub report: String,
}

/// Compare `ours` with `theirs`.
pub fn diff_files(ours: &Path, theirs: &Path) -> Result<Diff> {
    let (a, b) = (read(ours)?, read(theirs)?);
    let mut report = String::new();
    let mut differences = 0;
    let mut note = |report: &mut String, line: String| {
        differences += 1;
        if differences <= 20 {
            report.push_str(&line);
            report.push('\n');
        }
    };
    for (name, ca) in &a.cells {
        match b.cells.get(name) {
            None => note(&mut report, format!("only ours: {name} ({})", ca.ty)),
            Some(cb) if ca != cb => note(
                &mut report,
                format!("differs: {name}\n  ours   {ca:?}\n  theirs {cb:?}"),
            ),
            Some(_) => {}
        }
    }
    for (name, cb) in &b.cells {
        if !a.cells.contains_key(name) {
            note(&mut report, format!("only theirs: {name} ({})", cb.ty));
        }
    }
    if a.ports != b.ports {
        note(
            &mut report,
            format!(
                "top-level ports differ\n  ours   {:?}\n  theirs {:?}",
                a.ports, b.ports
            ),
        );
    }
    let same_net_order = a.net_order == b.net_order;
    let mut types: BTreeMap<&str, usize> = BTreeMap::new();
    for c in a.cells.values() {
        *types.entry(&c.ty).or_default() += 1;
    }
    let equal = differences == 0;
    let _ = write!(
        report,
        "{} cells {:?}, {} ports, {} connected nets: {}; net id order {}",
        a.cells.len(),
        types,
        a.ports.len(),
        a.net_order.len(),
        if equal {
            "identical".to_string()
        } else {
            format!("{differences} differences")
        },
        if same_net_order {
            "identical"
        } else {
            "differs"
        },
    );
    Ok(Diff {
        equal,
        same_net_order,
        report,
    })
}
