//! 7-series delays for the placer, from prjxray-db (slow corner, max):
//!
//! - cell delays from `timings/slicel.sdf` (Vivado's SDF for a SLICEL):
//!   LUT input to O6, every CARRY4 input to every output it reaches, the
//!   register's clock-to-Q and its setup (D, CE) and recovery (SR) times;
//! - routing estimated from distance, hop by hop: per interconnect hop the
//!   mean intrinsic delay of the pips driving that wire class in
//!   `tile_type_INT_L.json` (singles, doubles, quads, hexes), plus the pip
//!   into the site pin (IMUX).
//!
//! prjxray's pip delays leave out the wires' RC (most INT wires carry
//! none), so routed delays come out low against nextpnr's or Vivado's.
//! The placer only needs them in proportion: criticality is relative to the
//! estimated critical path. Doubling the wire hops to stand in for the RC
//! placed picorv32 worse by nextpnr's STA (README), so they stay as read.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde_json::Value;

use chipsmith_route::timing::Ps;

#[derive(Debug, Clone)]
pub struct Delays {
    /// LUT input to O6 (or a RAMD32 read address to its output).
    pub lut: Ps,
    /// CARRY4 (input, output) pin pairs, as the SDF names them (`S0`,
    /// `CO3`): the slowest of its variants.
    pub carry: HashMap<(String, String), Ps>,
    pub clock_to_q: Ps,
    pub setup_d: Ps,
    pub setup_ce: Ps,
    pub recovery_sr: Ps,
    /// Interconnect hops by length: 1, 2, 4 (horizontal quads) and 6
    /// (vertical hexes), and the pip into a site pin.
    pub single: Ps,
    pub double: Ps,
    pub quad: Ps,
    pub hex: Ps,
    pub imux: Ps,
}

/// The slow-corner maximum of an SDF triple pair, `(a::b)(c::d)` -> d, or of
/// a single triple, `(a::b)` -> b; in ps.
fn slowest(fields: &[&str]) -> Option<Ps> {
    let last = fields.last()?;
    let v: f32 = last
        .trim_matches(|c| c == '(' || c == ')')
        .rsplit("::")
        .next()?
        .parse()
        .ok()?;
    Some(v * 1000.0)
}

/// IOPATHs and timing checks of every cell in an SDF:
/// (cell type, kind, from, to) -> ps. Timing checks have kind SETUP,
/// RECOVERY, ... and `to` empty.
fn read_sdf(text: &str) -> HashMap<(String, String, String, String), Ps> {
    let mut out = HashMap::new();
    let mut cell = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("(CELLTYPE \"") {
            cell = rest.trim_end_matches(['"', ')']).to_string();
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let entry = match fields.first().copied() {
            Some("(IOPATH") if fields.len() >= 4 => {
                slowest(&fields[3..]).map(|d| (("IOPATH", fields[1], fields[2]), d))
            }
            Some(k @ ("(SETUP" | "(RECOVERY" | "(HOLD")) if fields.len() >= 4 => {
                slowest(&fields[3..]).map(|d| ((&k[1..], fields[1], ""), d))
            }
            _ => None,
        };
        if let Some(((kind, from, to), d)) = entry {
            let key = (
                cell.clone(),
                kind.to_string(),
                from.to_string(),
                to.to_string(),
            );
            let slot = out.entry(key).or_insert(d);
            *slot = slot.max(d);
        }
    }
    out
}

/// Mean slow-max delay of the pips driving each wire class in an INT tile.
fn hop_delays(tile_type: &Value) -> Result<HashMap<&'static str, Ps>> {
    let mut sums: HashMap<&'static str, (f32, u32)> = HashMap::new();
    for pip in tile_type["pips"].as_object().context("no pips")?.values() {
        let dst = pip["dst_wire"].as_str().unwrap_or_default();
        let class = if dst.starts_with("IMUX") {
            "imux"
        } else {
            match dst.get(2..3) {
                Some("1") => "single",
                Some("2") => "double",
                Some("4") => "quad",
                Some("6") => "hex",
                _ => continue,
            }
        };
        let Some(d) = pip["src_to_dst"]["delay"]
            .as_array()
            .and_then(|d| d.get(3))
            .and_then(Value::as_str)
            .and_then(|d| d.parse::<f32>().ok())
        else {
            continue;
        };
        let s = sums.entry(class).or_default();
        s.0 += d * 1000.0;
        s.1 += 1;
    }
    Ok(sums
        .into_iter()
        .map(|(k, (s, n))| (k, s / n as f32))
        .collect())
}

impl Delays {
    /// From a prjxray-db family directory (`.../zynq7`).
    pub fn load(family: &Path) -> Result<Self> {
        let sdf_path = family.join("timings").join("slicel.sdf");
        let sdf = std::fs::read_to_string(&sdf_path)
            .with_context(|| format!("reading {}", sdf_path.display()))?;
        let int_path = family.join("tile_type_INT_L.json");
        let int: Value = serde_json::from_str(
            &std::fs::read_to_string(&int_path)
                .with_context(|| format!("reading {}", int_path.display()))?,
        )?;
        Self::parse(&sdf, &int)
    }

    fn parse(sdf: &str, int: &Value) -> Result<Self> {
        let sdf = read_sdf(sdf);
        let get = |cell: &str, kind: &str, from: &str, to: &str| -> Result<Ps> {
            sdf.get(&(cell.into(), kind.into(), from.into(), to.into()))
                .copied()
                .with_context(|| format!("slicel.sdf has no {kind} {from} {to} of {cell}"))
        };
        let mut carry: HashMap<(String, String), Ps> = HashMap::new();
        for ((cell, kind, from, to), &d) in &sdf {
            if cell.starts_with("CARRY4") && kind == "IOPATH" {
                let slot = carry.entry((from.clone(), to.clone())).or_insert(d);
                *slot = slot.max(d);
            }
        }
        ensure!(
            carry.contains_key(&("CIN".into(), "CO3".into())),
            "slicel.sdf has no CARRY4"
        );
        let hops = hop_delays(int)?;
        let hop = |class: &str| {
            hops.get(class)
                .copied()
                .with_context(|| format!("INT_L has no {class} pips"))
        };
        Ok(Delays {
            lut: get("LUT6", "IOPATH", "A1", "O6")?,
            carry,
            clock_to_q: get("FF_INIT", "IOPATH", "CLK", "Q")?,
            setup_d: get("FF_INIT", "SETUP", "DIN", "")?,
            setup_ce: get("FF_INIT", "SETUP", "CE", "")?,
            recovery_sr: sdf
                .iter()
                .filter(|((_, kind, from, _), _)| kind == "RECOVERY" && from == "SR")
                .map(|(_, &d)| d)
                .fold(0.0, Ps::max),
            single: hop("single")?,
            double: hop("double")?,
            quad: hop("quad")?,
            hex: hop("hex")?,
            imux: hop("imux")?,
        })
    }

    /// A routed connection `dx` grid columns and `dy` rows long: the
    /// longest wires first, vertical hexes and horizontal quads (an
    /// interconnect column is two grid columns), then doubles and singles.
    /// Even a connection within one tile leaves and re-enters through one
    /// single.
    pub fn route(&self, dx: i32, dy: i32) -> Ps {
        let (dx, dy) = ((dx.abs() + 1) / 2, dy.abs());
        let hops = |n: i32, long: i32, long_delay: Ps| {
            let rest = n % long;
            (n / long) as Ps * long_delay
                + (rest / 2) as Ps * self.double
                + (rest % 2) as Ps * self.single
        };
        let wires = hops(dy, 6, self.hex) + hops(dx, 4, self.quad);
        wires.max(self.single) + self.imux
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDF: &str = r#"
    (CELL
        (CELLTYPE "CARRY4")
        (INSTANCE SLICEL)
        (DELAY
            (ABSOLUTE
                (IOPATH CIN CO3 (0.039::0.049)(0.092::0.114))
            )
        )
    )
    (CELL
        (CELLTYPE "FF_INIT")
        (INSTANCE SLICEL)
        (DELAY
            (ABSOLUTE
                (IOPATH CLK Q (0.099::0.124)(0.244::0.303))
            )
        )
        (TIMINGCHECK
                (SETUP CE (posedge CLK) (0.088::0.109))
                (SETUP DIN (posedge CLK) (-0.057::-0.046))
        )
    )
    (CELL
        (CELLTYPE "LUT6")
        (INSTANCE SLICEL/A6LUT)
        (DELAY
            (ABSOLUTE
                (IOPATH A1 O6 (0.045::0.056)(0.1::0.124))
            )
        )
    )
    "#;

    fn int() -> Value {
        let pip = |dst: &str, d: &str| serde_json::json!({"dst_wire": dst, "src_to_dst": {"delay": ["0", "0", "0", d]}});
        serde_json::json!({"pips": {
            "a": pip("NL1BEG0", "0.100"), "b": pip("EE2BEG0", "0.120"),
            "c": pip("WW4BEG0", "0.140"), "d": pip("NN6BEG0", "0.180"),
            "e": pip("SS6BEG0", "0.200"), "f": pip("IMUX_L1", "0.150"),
        }})
    }

    #[test]
    fn sdf_and_pip_delays_are_read_slow_max_in_ps() {
        let d = Delays::parse(SDF, &int()).unwrap();
        assert_eq!(d.lut, 124.0);
        assert_eq!(d.clock_to_q, 303.0);
        assert_eq!(d.setup_d, -46.0);
        assert_eq!(d.setup_ce, 109.0);
        assert_eq!(d.carry[&("CIN".to_string(), "CO3".to_string())], 114.0);
        assert!((d.hex - 190.0).abs() < 1e-3);
        // Same tile: out and back in through one single.
        assert!((d.route(0, 0) - 250.0).abs() < 1e-3);
        // 13 rows: two hexes and a single; 6 grid columns: three
        // interconnect columns, a double and a single.
        let want = 2.0 * 190.0 + 100.0 + 120.0 + 100.0 + 150.0;
        assert!((d.route(6, 13) - want).abs() < 1e-3);
    }
}
