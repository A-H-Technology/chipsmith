//! I/O pins from the template (README (p)): `db/sm4revb/io/template.tsv`
//! expanded for a set of used pins into the `uid, bit, value` rows `a5db
//! apply` writes. The package's I/O balls are `db/sm4revb/io/pins.tsv`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};

use crate::db::Db;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Use {
    In,
    Out,
    ClockIn,
    /// An input or output with its IO register (FAST_INPUT_REGISTER,
    /// FAST_OUTPUT_REGISTER), clocked from the fabric.
    InReg,
    OutReg,
}

impl Use {
    fn name(self) -> &'static str {
        match self {
            Use::In => "in",
            Use::Out => "out",
            Use::ClockIn => "clock_in",
            Use::InReg => "in_reg",
            Use::OutReg => "out_reg",
        }
    }
}

/// Where a ball sits: an HVIO pin (bank half b, group g, pin k) or an IO96
/// pin (lane, pin in the lane).
#[derive(Clone, Debug)]
enum Site {
    Hvio { b: u32, g: u32, k: u32 },
    Io96 { lane: u32, pin: u32 },
}

#[derive(Clone, Debug)]
pub struct Pin {
    ball: String,
    uid: String,
    site: Site,
    usage: Use,
    std: String,
    /// Which of the input register's two 4:1 muxes feeds the fabric; None
    /// takes the template's (Quartus' usual) choice.
    ireg: Option<u32>,
}

/// `<ball>=<in|in0|in1|out|clock_in|in_reg|out_reg>[:<standard>]`; the standard is 3.3, 1.8
/// or 1.1 (or the full Quartus name) and defaults to 1.1-V on IO96 pins.
/// `in0`/`in1` pick the input register mux the fabric route leaves through
/// (`in` is mux0 on IO96 pins and mux1 on HVIO pins, Quartus' usual choice).
pub fn parse(db: &Db, specs: &[String]) -> Result<Vec<Pin>> {
    let text =
        std::fs::read_to_string(db.dir.join("io/pins.tsv")).context("reading io/pins.tsv")?;
    let mut balls = BTreeMap::new();
    for l in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("ball\t"))
    {
        let f: Vec<&str> = l.split('\t').collect();
        balls.insert(f[0].to_owned(), (f[1].to_owned(), f[2].to_owned()));
    }
    let mut pins = Vec::new();
    for s in specs {
        let (ball, rest) = s
            .split_once('=')
            .with_context(|| format!("{s}: expected <ball>=<use>[:<standard>]"))?;
        let (u, std) = rest
            .split_once(':')
            .map_or((rest, None), |(a, b)| (a, Some(b)));
        let ireg = match u {
            "in0" => Some(0),
            "in1" => Some(1),
            _ => None,
        };
        let usage = match u {
            "in" | "in0" | "in1" => Use::In,
            "out" => Use::Out,
            "clock_in" => Use::ClockIn,
            "in_reg" => Use::InReg,
            "out_reg" => Use::OutReg,
            _ => bail!("{s}: use must be in, in0, in1, out, clock_in, in_reg or out_reg"),
        };
        let (uid, pad) = balls
            .get(ball)
            .with_context(|| format!("{ball} is not an HVIO or IO96 ball"))?;
        let num = |t: &str| -> Result<u32> { Ok(t.parse()?) };
        let site = if let Some(r) = pad.strip_prefix("HVIO_B") {
            let f: Vec<&str> = r.split('_').collect();
            Site::Hvio {
                b: num(f[0])?,
                g: num(f[1])?,
                k: num(f[2])?,
            }
        } else {
            let n = num(pad.trim_start_matches('P').split('X').next().unwrap())?;
            Site::Io96 {
                lane: n / 12,
                pin: n % 12,
            }
        };
        let std = match (std, &site) {
            (Some("3.3"), _) => "3.3-V LVCMOS".to_owned(),
            (Some("1.8"), _) => "1.8-V LVCMOS".to_owned(),
            (Some("1.1"), _) | (None, Site::Io96 { .. }) => "1.1-V".to_owned(),
            (Some(x), _) => x.to_owned(),
            (None, Site::Hvio { .. }) => bail!("{s}: an HVIO pin needs a standard (:3.3 or :1.8)"),
        };
        pins.push(Pin {
            ball: ball.to_owned(),
            uid: uid.clone(),
            site,
            usage,
            std,
            ireg,
        });
    }
    Ok(pins)
}

/// Evaluate the `{…}` fields of a template string: integers and the
/// variables, joined by + and * (no parentheses).
fn expand(t: &str, vars: &[(&str, String)]) -> Result<String> {
    let mut out = String::new();
    let mut rest = t;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let j = rest[i..]
            .find('}')
            .with_context(|| format!("unclosed {{ in {t}"))?
            + i;
        let e = &rest[i + 1..j];
        if let Some((_, v)) = vars.iter().find(|(k, _)| *k == e) {
            out.push_str(v);
        } else {
            let mut sum: i64 = 0;
            for term in e.split('+') {
                let mut prod: i64 = 1;
                for f in term.split('*') {
                    prod *= match vars.iter().find(|(k, _)| *k == f) {
                        Some((_, v)) => v
                            .parse::<i64>()
                            .with_context(|| format!("{f} in {t} is not a number"))?,
                        None => f.parse::<i64>().with_context(|| format!("{f} in {t}"))?,
                    };
                }
                sum += prod;
            }
            out.push_str(&sum.to_string());
        }
        rest = &rest[j + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The `uid, bit, value` rows for a set of pins, sorted; fails if two rows
/// disagree on a bit.
pub fn table(db: &mut Db, pins: &[Pin]) -> Result<Vec<(String, String, String)>> {
    let text = std::fs::read_to_string(db.dir.join("io/template.tsv"))
        .context("reading io/template.tsv")?;
    let rows: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("block\t"))
        .map(|l| l.split('\t').collect())
        .collect();
    let mut out: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    let mut put = |uid: String, bit: String, v: &str, why: &str| -> Result<()> {
        if let Some((old, w)) = out.get(&(uid.clone(), bit.clone())) {
            if old != v {
                bail!("{uid} {bit}: {why} sets {v}, {w} set {old}");
            }
            return Ok(());
        }
        out.insert((uid, bit), (v.to_owned(), why.to_owned()));
        Ok(())
    };
    // pio_0_0 is image object io96_0, pio_1_0 io96_1.
    let obj = |uid: &str| {
        uid.replace("pio_", "io96_")
            .strip_suffix("_0")
            .unwrap_or(uid)
            .to_owned()
    };
    let mut lanes: BTreeMap<(String, u32), BTreeSet<Use>> = BTreeMap::new();
    for p in pins {
        let (block, mut vars) = match p.site {
            Site::Hvio { b, g, k } => (
                "hviowr",
                vec![
                    ("pin", format!("xx20_u{b}/xx4_u{g}/xx1_u{k}")),
                    ("bank", format!("xx20_u{b}")),
                    ("n", (4 * g + k).to_string()),
                ],
            ),
            Site::Io96 { lane, pin } => {
                lanes
                    .entry((p.uid.clone(), lane))
                    .or_default()
                    .insert(p.usage);
                (
                    "io96",
                    vec![("L", lane.to_string()), ("p", pin.to_string())],
                )
            }
        };
        vars.push(("uid", p.uid.clone()));
        vars.push(("obj", obj(&p.uid)));
        let mut hit = false;
        for r in &rows {
            if r[0] != block || r[2] != p.usage.name() || (r[3] != "*" && r[3] != p.std) {
                continue;
            }
            match r[1] {
                "pin" => {}
                _ => continue,
            }
            hit = true;
            let mut bit = expand(r[5], &vars)?;
            if let Some(k) = p.ireg {
                bit = bit
                    .replace("ireg_4to1_mux0/", &format!("ireg_4to1_mux{k}/"))
                    .replace("ireg_4to1_mux1/", &format!("ireg_4to1_mux{k}/"));
            }
            put(expand(r[4], &vars)?, bit, r[6], &p.ball)?;
        }
        if !hit {
            bail!(
                "{} ({} {}): no template rows for {} {}",
                p.ball,
                block,
                p.uid,
                p.usage.name(),
                p.std
            );
        }
        // An HVIO clock pin reaches the DCM through its bank half's clock
        // muxes (README (r)): pins 0 and 1 of the xx4_u2 group through the
        // 2:1 ioclk mux, pins 2 and 3 through the 4:1 ss clock mux, the one
        // numbered like the bank half (Quartus' choice; either ss mux takes
        // either half).
        if let (Use::ClockIn, Site::Hvio { b, g: 2, k }) = (p.usage, &p.site) {
            let (path, setting) = if *k < 2 {
                (
                    format!(
                        "xif/xif_clk_mux/xif_p2c_ioclk_2to1_mux_u{b}/hviowr_if_p2c_ioclk_mux_sel"
                    ),
                    format!("P2C_IOCLK_2TO1_MUX_P{k}"),
                )
            } else {
                (
                    format!(
                        "xif/xif_clk_mux/xif_p2c_ssclk_4to1_mux_u{b}/hviowr_if_p2c_ssclk_mux_sel"
                    ),
                    format!("O_SSCLK_BANK{b}_P{k}"),
                )
            };
            let a = db
                .attribute("hviowr", &path)
                .with_context(|| format!("hviowr has no {path}"))?;
            let s = db
                .bcm
                .settings(&a)
                .iter()
                .find(|s| s.name == setting)
                .with_context(|| format!("{path} has no {setting}"))?;
            let rows: Vec<(String, bool)> = s
                .bits
                .iter()
                .zip(&s.values)
                .map(|(b, &(v, _))| (format!("{}{b}", a.prefix), v))
                .collect();
            for (bit, v) in rows {
                put(p.uid.clone(), bit, if v { "1" } else { "0" }, &p.ball)?;
            }
        } else if p.usage == Use::ClockIn && matches!(p.site, Site::Hvio { .. }) {
            bail!(
                "{}: not a clock pin (HVIO clock pins are HVIO_B<b>_2_<k>)",
                p.ball
            );
        }
    }
    let banks: BTreeSet<String> = lanes.keys().map(|(u, _)| u.clone()).collect();
    for ((uid, lane), uses) in &lanes {
        let vars = vec![
            ("L", lane.to_string()),
            ("uid", uid.clone()),
            ("obj", obj(uid)),
        ];
        let scopes = [
            "lane_any",
            if uses
                .iter()
                .any(|u| matches!(u, Use::In | Use::ClockIn | Use::InReg))
            {
                "lane_in"
            } else {
                "lane_out_only"
            },
        ];
        for r in rows
            .iter()
            .filter(|r| r[0] == "io96" && scopes.contains(&r[1]))
        {
            put(
                expand(r[4], &vars)?,
                expand(r[5], &vars)?,
                r[6],
                &format!("{uid} lane {lane}"),
            )?;
        }
    }
    for uid in &banks {
        let vars = vec![("uid", uid.clone()), ("obj", obj(uid))];
        for r in rows.iter().filter(|r| r[0] == "io96" && r[1] == "bank") {
            put(expand(r[4], &vars)?, expand(r[5], &vars)?, r[6], uid)?;
        }
    }
    Ok(out.into_iter().map(|((u, b), (v, _))| (u, b, v)).collect())
}
