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
    eprintln!("{checked} OPTIMIZED blocks compared");
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
    eprintln!("{} refused (a count over 64)", refused.len());
    assert!(refused.contains(&"d5/blk1".to_string()), "{refused:?}");
}

/// A decimal (`62.5`) as a fraction.
fn decimal(s: &str) -> (u64, u64) {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let den = 10u64.pow(frac.len() as u32);
    let num: u64 = format!("{int}{frac}").parse().unwrap();
    let g = gcd(num, den);
    (num / g, den / g)
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Vivado's clocking wizard's choices (data/clk_wiz.txt) against
/// `Request::solve`: every request with an exact answer gets the wizard's
/// counters, but for the MMCM's low frequencies listed here, where the
/// wizard's jitter model takes other counters for the same frequencies
/// (bench/cmt/README.md). A request the wizard could only approximate is
/// refused.
#[test]
fn the_clocking_wizards_counters() {
    use chipsmith_xc7::cmt::{OutputRequest, Request};
    let known: &[&str] = &[
        // The jitter model's own choices, the same frequencies.
        "MMCM {125 25}",
        // A 33% duty no counter makes: the wizard takes 32.5% at an
        // 800 MHz VCO, chipsmith the nearest of its own candidates.
        "MMCM {125 40%33}",
        // The wizard approximates (63.99, 47.99 MHz) where a 1200 MHz VCO
        // makes both exactly (CLKOUT0 /18.75, CLKOUT1 /25).
        "MMCM {125 64,48}",
        "MMCM {125 12.5}",
        "MMCM {125 8}",
        "MMCM {125 31.25}",
        // A phase on a fractional divider, which chipsmith doesn't build.
        "MMCM {125 100@30}",
    ];
    let (mut agree, mut differ, mut refused) = (0, Vec::new(), 0);
    for line in include_str!("data/clk_wiz.txt").lines().filter(|l| !l.starts_with('#')) {
        if line.contains(" ERR ") {
            continue;
        }
        let (req, rest) = line.split_once("} ").unwrap();
        let (prim_s, req) = req.split_once(" {").unwrap();
        let prim = if prim_s == "PLL" { Prim::Pll } else { Prim::Mmcm };
        let (fin, outs) = req.split_once(' ').unwrap();
        let (fin_n, fin_d) = decimal(fin);
        // 1e6 / f_in MHz in ps, if whole.
        if (1_000_000 * fin_d) % fin_n != 0 {
            continue;
        }
        let period = (1_000_000 * fin_d / fin_n) as u32;
        let mut outputs = vec![None; prim.outputs()];
        for (k, o) in outs.split(',').enumerate() {
            let (o, duty) = o.split_once('%').map_or((o, 50.0), |(o, d)| (o, d.parse().unwrap()));
            let (f, phase) = o.split_once('@').map_or((o, 0.0), |(f, p)| (f, p.parse::<f64>().unwrap()));
            let (n, d) = decimal(f);
            // f_out / f_in = (n / d) / (fin_n / fin_d).
            let (mul, div) = (n * fin_d, d * fin_n);
            let g = gcd(mul, div);
            let period_out = f64::from(period) * (div / g) as f64 / (mul / g) as f64;
            outputs[k] = Some(OutputRequest {
                multiply: (mul / g) as u32,
                divide: (div / g) as u32,
                phase_ps: (phase / 360.0 * period_out).round() as i32,
                duty_percent: duty as u32,
            });
        }
        let r = Request { prim, inclk_period_ps: period, outputs, compensation: Compensation::Zhold };
        let field = |k: &str| rest.split_whitespace().find_map(|w| w.strip_prefix(k)).unwrap();
        let want_m: f64 = field("M=").parse().unwrap();
        let want_d: u8 = field("D=").parse().unwrap();
        let want_o: Vec<f64> = rest
            .split_whitespace()
            .filter(|w| w.starts_with('O'))
            .map(|w| w.split_once('=').unwrap().1.split('/').next().unwrap().parse().unwrap())
            .collect();
        let Ok(s) = r.solve() else {
            refused += 1;
            continue;
        };
        let got_o: Vec<f64> = s.config.outputs().iter().flatten().map(|o| o.divide().value()).collect();
        let tag = format!("{prim_s} {{{req}}}");
        if s.config.mult().value() == want_m && s.config.divclk() == want_d && got_o == want_o {
            agree += 1;
        } else {
            differ.push(tag);
        }
    }
    eprintln!("clocking wizard: {agree} agree, {} differ, {refused} refused", differ.len());
    assert_eq!(differ, known, "requests where chipsmith's counters aren't the wizard's");
    assert!(agree >= 100, "{agree}");
}
