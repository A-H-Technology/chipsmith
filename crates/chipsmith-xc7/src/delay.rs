//! 7-series delays, Vivado's slow corner (speed grade -1), for the placer,
//! the router and the timing report:
//!
//! - cell delays from prjxray-db's `timings/slicel.sdf` (Vivado's SDF for a
//!   SLICEL): LUT input to O6, every CARRY4 input to every output it
//!   reaches; the register's clock to Q and setup times as Vivado's timer
//!   has them ([`VIVADO_REGISTER`]), which the SDF's leave the site's own
//!   routing out of;
//! - block RAM's clock to read data (with and without the output
//!   register) and its inputs' setup times, from `timings/BRAM_L.sdf`;
//! - routing per node of the routing graph ([`node_delays`]): the delay of
//!   its class (the pip into it: a single, a double, an IMUX, ...) fitted
//!   to Vivado's net delays on chipsmith's own routing ([`ROUTING`]), plus
//!   a site input's own delay (a slice's A1 0.56 ns, its A6 0.03 ns).
//!   prjxray-db's intrinsic pip delays leave out the loading of the wires
//!   and read 17-21% short of Vivado, connection by connection;
//! - for the placer, routing estimated from distance, hop by hop: per
//!   interconnect hop the mean intrinsic delay of the pips driving that
//!   wire class in `tile_type_INT_L.json` (singles, doubles, quads,
//!   hexes), plus the pip into the site pin (IMUX);
//! - a margin for the clock tree's skew and uncertainty
//!   ([`CLOCK_MARGIN`]) on every path end.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde_json::Value;

use chipsmith_route::chipdb::ChipDb;
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
    /// Added to every path end's setup ([`CLOCK_MARGIN`]).
    pub clock_margin: Ps,
    pub recovery_sr: Ps,
    /// Interconnect hops by length: 1, 2, 4 (horizontal quads) and 6
    /// (vertical hexes), and the pip into a site pin.
    pub single: Ps,
    pub double: Ps,
    pub quad: Ps,
    pub hex: Ps,
    pub imux: Ps,
    pub bram: BramDelays,
    pub dsp: DspDelays,
}

/// A slice register's clock to Q, and its setup times on D, CE and SR (ps),
/// as Vivado 2026.1's timer has them: medians over chipsmith's builds
/// imported into Vivado (`bench/zybo-cell-delays.py`; its report's
/// `Setup_*` lines are what setup takes off the required time, so the
/// setup time is their negative). prjxray-db's SDF leaves out what the
/// site adds between the register and its pins (its FF_INIT: clock to Q
/// 303, setup D -46, CE 109): Vivado's clock to Q is 456 in a SLICEL, 518
/// in a SLICEM; its setup on D -31 (-118 to 256 by where D comes in), on
/// CE 205, on a synchronous reset 660.
const VIVADO_REGISTER: [Ps; 4] = [456.0, -31.0, 205.0, 660.0];

/// What Vivado's timer takes off every register-to-register path for the
/// clock (ps): its uncertainty (35) and the skew its clock tree leaves
/// after pessimism removal, 140 in all at the median over chipsmith's
/// builds (bench/zybo-timing.py: requirement - slack - data path - setup,
/// per endpoint), where chipsmith's clocks arrive everywhere at once.
const CLOCK_MARGIN: Ps = 140.0;

/// A DSP48E1 multiplying A by B (no D port, no pattern detect), from
/// DSP_L.sdf: A to P through the multiplier and the ALU, A's setup into
/// each register stage it may first reach, clock to P from each stage.
#[derive(Debug, Clone, Default)]
pub struct DspDelays {
    pub a_to_p: Ps,
    pub setup_areg: Ps,
    pub setup_mreg: Ps,
    pub setup_preg: Ps,
    pub clock_areg_to_p: Ps,
    pub clock_mreg_to_p: Ps,
    pub clock_preg_to_p: Ps,
    /// A register stage's enable or reset.
    pub setup_control: Ps,
}

impl DspDelays {
    fn parse(sdf: &str) -> Result<Self> {
        let sdf = read_sdf(sdf);
        let get = |cell: &str, kind: &str, from: &str, to: &str| -> Result<Ps> {
            sdf.get(&(cell.into(), kind.into(), from.into(), to.into()))
                .copied()
                .with_context(|| format!("DSP_L.sdf has no {kind} {from} {to} of {cell}"))
        };
        Ok(DspDelays {
            a_to_p: get("DSP48E1_AREG_0_A_INPUT_DIRECT_MREG_0_PREG_0_USE_DPORT_FALSE_USE_MULT_MULTIPLY", "IOPATH", "A", "P")?,
            setup_areg: get("DSP48E1_AREG_1_A_INPUT_DIRECT", "SETUP", "A", "")?,
            setup_mreg: get("DSP48E1_AREG_0_A_INPUT_DIRECT_MREG_1_USE_DPORT_FALSE_USE_MULT_MULTIPLY", "SETUP", "A", "")?,
            setup_preg: get(
                "DSP48E1_AREG_0_A_INPUT_DIRECT_MREG_0_PREG_1_USE_DPORT_FALSE_USE_MULT_MULTIPLY_USE_PATTERN_DETECT_NO_PATDET",
                "SETUP",
                "A",
                "",
            )?,
            clock_areg_to_p: get("DSP48E1_AREG_1_MREG_0_PREG_0_USE_DPORT_FALSE_USE_MULT_MULTIPLY", "IOPATH", "CLK", "P")?,
            clock_mreg_to_p: get("DSP48E1_MREG_1_PREG_0", "IOPATH", "CLK", "P")?,
            clock_preg_to_p: get("DSP48E1_PREG_1", "IOPATH", "CLK", "P")?,
            setup_control: get("DSP48E1_PREG_1", "SETUP", "CEP", "")?,
        })
    }
}

/// A block RAM's: clock to read data from the latch and from the output
/// register, and setup per input, by the pin's name without its index
/// (`ADDRARDADDR`, `DIADI`, ...).
#[derive(Debug, Clone, Default)]
pub struct BramDelays {
    pub clock_to_out: Ps,
    pub clock_to_out_reg: Ps,
    pub setup_addr: Ps,
    pub setup_data: Ps,
    pub setup_we: Ps,
    pub setup_en: Ps,
    pub setup_regce: Ps,
    pub setup_rst: Ps,
}

impl BramDelays {
    /// From BRAM_L.sdf: the slowest of each kind over the block's
    /// variants.
    fn parse(sdf: &str) -> Result<Self> {
        let sdf = read_sdf(sdf);
        let mut b = BramDelays::default();
        for ((cell, kind, from, to), &d) in &sdf {
            let slot = match kind.as_str() {
                "IOPATH" if from.starts_with("CLK") && to.starts_with("DO") => {
                    if cell.contains("_REG_L_1") || cell.contains("_REG_U_1") {
                        &mut b.clock_to_out_reg
                    } else {
                        &mut b.clock_to_out
                    }
                }
                "SETUP" if from.starts_with("ADDR") => &mut b.setup_addr,
                "SETUP" if from.starts_with("DI") => &mut b.setup_data,
                "SETUP" if from.starts_with("WE") => &mut b.setup_we,
                "SETUP" if from.starts_with("EN") => &mut b.setup_en,
                "SETUP" if from.starts_with("REGCE") => &mut b.setup_regce,
                "SETUP" if from.starts_with("RST") => &mut b.setup_rst,
                _ => continue,
            };
            *slot = slot.max(d);
        }
        ensure!(b.clock_to_out > 0.0 && b.clock_to_out_reg > 0.0, "BRAM_L.sdf has no clock to DO");
        Ok(b)
    }

    /// The setup time of input pin `pin` (site pin name, index and all).
    pub fn setup(&self, pin: &str) -> Option<Ps> {
        Some(if pin.starts_with("ADDR") && !pin.contains("TIEHIGH") {
            self.setup_addr
        } else if pin.starts_with("DI") {
            self.setup_data
        } else if pin.starts_with("WE") {
            self.setup_we
        } else if pin.starts_with("EN") {
            self.setup_en
        } else if pin.starts_with("REGCE") {
            self.setup_regce
        } else if pin.starts_with("RST") {
            self.setup_rst
        } else {
            return None;
        })
    }
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
        let bram_path = family.join("timings").join("BRAM_L.sdf");
        let bram = std::fs::read_to_string(&bram_path)
            .with_context(|| format!("reading {}", bram_path.display()))?;
        let dsp_path = family.join("timings").join("DSP_L.sdf");
        let dsp = std::fs::read_to_string(&dsp_path)
            .with_context(|| format!("reading {}", dsp_path.display()))?;
        let mut d = Self::parse(&sdf, &int)?;
        d.bram = BramDelays::parse(&bram)?;
        d.dsp = DspDelays::parse(&dsp)?;
        Ok(d)
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
        // The register's own arcs are Vivado's (VIVADO_REGISTER), the SDF's
        // being checked to be there.
        get("FF_INIT", "IOPATH", "CLK", "Q")?;
        get("FF_INIT", "SETUP", "DIN", "")?;
        get("FF_INIT", "SETUP", "CE", "")?;
        let [clock_to_q, setup_d, setup_ce, recovery_sr] = VIVADO_REGISTER;
        Ok(Delays {
            lut: get("LUT6", "IOPATH", "A1", "O6")?,
            carry,
            clock_to_q,
            setup_d,
            setup_ce,
            clock_margin: CLOCK_MARGIN,
            recovery_sr,
            single: hop("single")?,
            double: hop("double")?,
            quad: hop("quad")?,
            hex: hop("hex")?,
            imux: hop("imux")?,
            bram: BramDelays::default(),
            dsp: DspDelays::default(),
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

/// The slow-max delay of a prjxray-db `delay` quadruple, in ps.
fn slow_max(v: &Value) -> Option<Ps> {
    v.as_array()?.get(3)?.as_str()?.parse::<Ps>().ok().map(|d| d * 1000.0)
}

/// Vivado's routing delay per node class (`node_class`), fitted to its
/// net delays on chipsmith's own placement and routing
/// (`bench/zybo-timing-fit.py`): prjxray-db's intrinsic pip delays leave
/// out the wires' loading, and read 17-21% short connection by connection.
const ROUTING: &str = include_str!("../data/xc7z010-1-routing.tsv");

/// What a node not in [`ROUTING`] costs over prjxray-db's intrinsic delay
/// of the pip into it: about what the fitted classes cost over theirs.
const UNFITTED: Ps = 1.2;

/// A node's class by one of its wires, as the fit names it: a slice's pin
/// as `M:A1` (SLICEM) or `L:A1` (SLICEL); any other wire by its name less
/// the trailing index and the tile side (`EE4BEG`, `IMUX`, `CLB_OUTS`).
pub fn node_class(wire: &str) -> String {
    if let Some(rest) = wire.strip_prefix("CLBLM_").or_else(|| wire.strip_prefix("CLBLL_")) {
        let pin = ["M_", "LL_", "L_"].iter().find_map(|p| Some((*p, rest.strip_prefix(p)?)));
        if let Some((side, pin)) = pin {
            let b = pin.as_bytes();
            let letter = !b.is_empty() && (b'A'..=b'D').contains(&b[0]);
            let site_pin = match pin {
                "CE" | "SR" | "CLK" | "CIN" | "COUT" | "WE" => true,
                _ if letter => matches!(&pin[1..], "" | "1" | "2" | "3" | "4" | "5" | "6" | "X" | "I" | "MUX" | "Q" | "5Q"),
                _ => false,
            };
            if site_pin {
                return format!("{}:{pin}", if side == "M_" { "M" } else { "L" });
            }
        }
    }
    let w = wire.trim_end_matches(|c: char| c.is_ascii_digit());
    let w = w.strip_suffix("_L").unwrap_or(w);
    let w = w.replace("_L_", "_").replace("LOGIC_OUTS_L", "LOGIC_OUTS");
    if w.starts_with("CLBLM_") || w.starts_with("CLBLL_") {
        let mut parts = w.splitn(3, '_');
        if let (Some(_), Some(_), Some(rest)) = (parts.next(), parts.next(), parts.next()) {
            return format!("CLB_{rest}");
        }
    }
    w
}

/// Each routing-graph node's delay: the class of the pip into it (the
/// slowest, if several drive it) from [`ROUTING`], plus a site input's
/// own delay into its site (a slice's A1 0.58 ns, its A6 under 0.05 ns).
/// prjxray-db's tile types (`family`, `.../zynq7`) name the pips and site
/// pins and give the intrinsic delays a class missing from the table
/// falls back on.
pub fn node_delays(db: &ChipDb, family: &Path) -> Result<Vec<Ps>> {
    let fitted: HashMap<&str, Ps> = ROUTING
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?, f.next()?.parse().ok()?))
        })
        .collect();
    // Per tile type: each wire's (pip delay, site pin delay).
    let mut by_type: Vec<HashMap<u32, (Ps, Ps)>> = Vec::with_capacity(db.tile_types.len());
    for tt in &db.tile_types {
        let mut wires: HashMap<u32, (Ps, Ps)> = HashMap::new();
        let path = family.join(format!("tile_type_{}.json", tt.name));
        let Ok(text) = std::fs::read_to_string(&path) else {
            by_type.push(wires);
            continue;
        };
        let json: Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let names: HashMap<&str, u32> = (0..tt.wires.len() as u32).map(|w| (tt.wires.get(w), w)).collect();
        let mut pins: HashMap<u32, Ps> = HashMap::new();
        for site in json["sites"].as_array().into_iter().flatten() {
            for pin in site["site_pins"].as_object().into_iter().flat_map(|p| p.values()) {
                let Some((&w, name)) = pin["wire"].as_str().and_then(|n| Some((names.get(n)?, n))) else {
                    continue;
                };
                let d = fitted
                    .get(node_class(name).as_str())
                    .copied()
                    .unwrap_or_else(|| slow_max(&pin["delay"]).unwrap_or(0.0));
                pins.insert(w, d);
                wires.entry(w).or_default().1 = d;
            }
        }
        for pip in json["pips"].as_object().into_iter().flat_map(|p| p.values()) {
            let Some((&w, name)) = pip["dst_wire"].as_str().and_then(|n| Some((names.get(n)?, n))) else {
                continue;
            };
            if pins.contains_key(&w) {
                continue;
            }
            let d = fitted
                .get(node_class(name).as_str())
                .copied()
                .unwrap_or_else(|| UNFITTED * slow_max(&pip["src_to_dst"]["delay"]).unwrap_or(0.0));
            let e = wires.entry(w).or_default();
            e.0 = e.0.max(d);
        }
        by_type.push(wires);
    }
    let mut pip = vec![0.0 as Ps; db.nodes()];
    let mut pin = vec![0.0 as Ps; db.nodes()];
    for tile in 0..db.width * db.height {
        for (&w, &(p, s)) in &by_type[db.tile_type_index(tile) as usize] {
            let n = db.node(tile, w) as usize;
            pip[n] = pip[n].max(p);
            pin[n] = pin[n].max(s);
        }
    }
    Ok(pip.iter().zip(&pin).map(|(p, s)| p + s).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_classes_as_the_fit_names_them() {
        assert_eq!(node_class("CLBLM_M_A1"), "M:A1");
        assert_eq!(node_class("CLBLL_LL_CE"), "L:CE");
        assert_eq!(node_class("CLBLM_L_AX"), "L:AX");
        assert_eq!(node_class("CLBLM_LOGIC_OUTS4"), "CLB_OUTS");
        assert_eq!(node_class("IMUX_L34"), "IMUX");
        assert_eq!(node_class("EE4BEG0"), "EE4BEG");
        assert_eq!(node_class("SR1BEG_S0"), "SR1BEG_S");
        assert_eq!(node_class("LOGIC_OUTS_L4"), "LOGIC_OUTS");
    }

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
    fn block_ram_delays_are_the_slowest_variants() {
        let sdf = r#"
        (CELL
            (CELLTYPE "RAMBFIFO36E1RAM_MODE_L_FIFO18_DOA_REG_L_0_EN_ECC_READ_FALSE")
            (DELAY (ABSOLUTE
                (IOPATH CLKARDCLKL DOADOL (0.585::1.098)(1.353::2.454))
            ))
        )
        (CELL
            (CELLTYPE "RAMBFIFO36E1RAM_MODE_L_FIFO18_DOA_REG_L_1_EN_ECC_READ_FALSE")
            (DELAY (ABSOLUTE
                (IOPATH CLKARDCLKL DOADOL (0.204::0.327)(0.468::0.882))
            ))
            (TIMINGCHECK
                (SETUP ADDRAU (posedge CLKARDCLKU) (-0.360::0.566))
                (SETUP DIADIL (posedge CLKBWRCLKL) (-0.667::0.737))
            )
        )
        "#;
        let b = BramDelays::parse(sdf).unwrap();
        assert_eq!(b.clock_to_out, 2454.0);
        assert_eq!(b.clock_to_out_reg, 882.0);
        assert_eq!(b.setup("ADDRARDADDR3"), Some(566.0));
        assert_eq!(b.setup("DIPBDIP1"), Some(737.0));
        assert_eq!(b.setup("ADDRATIEHIGH0"), None);
    }

    #[test]
    fn sdf_and_pip_delays_are_read_slow_max_in_ps() {
        let d = Delays::parse(SDF, &int()).unwrap();
        assert_eq!(d.lut, 124.0);
        // The register's arcs are Vivado's, not the SDF's 303, -46, 109.
        assert_eq!(d.clock_to_q, 456.0);
        assert_eq!(d.setup_d, -31.0);
        assert_eq!(d.setup_ce, 205.0);
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
