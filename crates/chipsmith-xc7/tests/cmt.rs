//! PLLE2_ADV and MMCME2_ADV bits against Vivado's: every block of
//! `data/cmt_vivado.txt` (Vivado 2026.1 bitstreams of bench/cmt/harvest.py's
//! designs, read by bit2fasm, bench/cmt/extract.py) configured from its
//! attributes by `chipsmith_xc7::cmt`, its features compared bit for bit,
//! and read back from Vivado's bits to the same configuration.

use std::collections::{BTreeMap, BTreeSet};

use chipsmith_netlist::ParamValue;
use chipsmith_xc7::cmt::{Compensation, Config, Prim};

struct Block {
    name: String,
    prim: Prim,
    params: BTreeMap<String, ParamValue>,
    used: Vec<bool>,
    bits: BTreeSet<String>,
}

fn blocks() -> Vec<Block> {
    let text = include_str!("data/cmt_vivado.txt");
    let mut out: Vec<Block> = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("block ") {
            let mut w = rest.split_whitespace();
            let prim = match w.next().unwrap() {
                "PLL" => Prim::Pll,
                _ => Prim::Mmcm,
            };
            let name = w.next().unwrap().to_string();
            let mut params = BTreeMap::new();
            let mut used = vec![false; prim.outputs()];
            for kv in w {
                let (k, v) = kv.split_once('=').unwrap();
                if let Some(n) = k.strip_prefix("CLKOUT").and_then(|r| r.split('_').next()) {
                    used[n.parse::<usize>().unwrap()] = true;
                }
                params.insert(k.to_string(), ParamValue::Str(v.to_string()));
            }
            out.push(Block {
                name,
                prim,
                params,
                used,
                bits: BTreeSet::new(),
            });
        } else {
            out.last_mut().unwrap().bits.insert(line.trim().to_string());
        }
    }
    out
}

fn optimized(b: &Block) -> bool {
    matches!(b.params.get("BANDWIDTH"), Some(ParamValue::Str(s)) if s == "OPTIMIZED")
}

fn compensation(b: &Block) -> Compensation {
    match b.params.get("COMPENSATION") {
        Some(ParamValue::Str(s)) if s == "ZHOLD" => Compensation::Zhold,
        _ => Compensation::Internal,
    }
}

#[test]
fn features_are_vivados() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for b in blocks().iter().filter(|b| optimized(b)) {
        let Ok(config) = Config::from_params(b.prim, &b.params, &b.used) else {
            continue;
        };
        let ours: BTreeSet<String> = config.features(false).unwrap().into_iter().collect();
        checked += 1;
        if ours != b.bits {
            let extra: Vec<&String> = ours.difference(&b.bits).collect();
            let missing: Vec<&String> = b.bits.difference(&ours).collect();
            failures.push(format!("{}: ours only {extra:?}, Vivado's only {missing:?}", b.name));
        }
    }
    assert!(checked > 0);
    assert!(failures.is_empty(), "{} of {checked} blocks differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn vivados_bits_decode_to_the_attributes() {
    let mut refused = Vec::new();
    for b in blocks().iter().filter(|b| optimized(b)) {
        // A count over 64 that Vivado cut to 6 bits: refused, and nothing
        // else is.
        let want = match Config::from_params(b.prim, &b.params, &b.used) {
            Ok(c) => c,
            Err(e) => {
                assert!(format!("{e:#}").contains("at most 64"), "{}: {e:#}", b.name);
                refused.push(b.name.clone());
                continue;
            }
        };
        let (got, reset) = Config::from_features(b.prim, &b.bits, compensation(b))
            .unwrap_or_else(|e| panic!("{}: {e:#}", b.name));
        assert!(!reset, "{}", b.name);
        assert_eq!(got, want, "{}", b.name);
        assert_eq!(got.compensation(), compensation(b), "{}", b.name);
    }
    assert!(refused.contains(&"d5/blk1".to_string()), "{refused:?}");
}
