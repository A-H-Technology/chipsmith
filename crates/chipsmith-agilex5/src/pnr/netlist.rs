//! yosys JSON (tools/a5pnr/synth.sh) → cells with constants folded.

use super::det::HashMap;

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// A signal: a net or a constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sig {
    Net(u32),
    Const(bool),
}

#[derive(Clone, Debug)]
pub enum Cell {
    /// A LUT over `ins` (bit i of the index is ins[i]); `mask` has 2^len bits.
    Lut {
        ins: Vec<u32>,
        mask: u64,
        out: u32,
    },
    /// Half an ALM in arithmetic mode (yosys/cells.v): p and g over
    /// {D, C, B, A}; after folding, only C and D carry nets.
    Arith {
        p: u16,
        g: u16,
        c: Option<u32>,
        d: Option<u32>,
        ci: Sig,
        so: Option<u32>,
        co: Option<u32>,
    },
    Ff(Ff),
    M20k(M20k),
    /// One bit of an MLAB RAM (yosys/mlab.txt).
    Mlab(Mlab),
    /// An IOPLL: output 0 of the reference pin's PLL (yosys/cells.v).
    /// `rst` is its reset, a constant-0 LUT's net (`tie_pll_resets`).
    Pll {
        refclk: u32,
        out: u32,
        rst: u32,
        ref_khz: u64,
        out_khz: u64,
    },
    /// A DSP block's multiplier (yosys/cells.v A5_MUL, README (z11)): the
    /// mode's operand bits (constants already on LUTs) and the product bits.
    Mul {
        mode: super::dsp::Mode,
        a: Vec<Sig>,
        b: Vec<Sig>,
        p: Vec<Option<u32>>,
    },
    /// A tristate driver (yosys `$_TBUF_`): y = e ? a : z. Only an output
    /// pin with an OE register takes one (README (z10)).
    Tbuf {
        a: Sig,
        e: Sig,
        y: u32,
    },
}

/// An M20K as a 512 x 32 simple dual-port RAM (yosys/cells_sim.v).
#[derive(Clone, Debug)]
pub struct M20k {
    pub clk: u32,
    pub we: Sig,
    pub be: Vec<Sig>,
    pub re: Sig,
    pub waddr: Vec<Sig>,
    pub wdata: Vec<Sig>,
    pub raddr: Vec<Sig>,
    pub rdata: Vec<Option<u32>>,
    pub outreg: bool,
    /// 512 words.
    pub init: Vec<u32>,
}

/// One bit of a 32-word MLAB RAM (yosys/mlab.txt, README (z9)): written
/// on clk's rising edge when we, at waddr; read at raddr with no clock;
/// init bit a is word a. `name` is yosys' `<memory>.<read port>.<bit>`.
#[derive(Clone, Debug)]
pub struct Mlab {
    pub name: String,
    pub clk: u32,
    pub we: Sig,
    pub waddr: Vec<Sig>,
    pub wdata: Sig,
    pub raddr: Vec<Sig>,
    pub rdata: Option<u32>,
    pub init: u32,
}

/// A flop: positive-edge clock, optional clock enable, synchronous clear
/// (to 0) and asynchronous clear (to 0); powers up at 0. `sclr_first`: the
/// clear acts whether or not the enable is high (yosys `$_SDFFE_`), else
/// only when it is (`$_SDFFCE_`).
#[derive(Clone, Debug)]
pub struct Ff {
    pub d: Sig,
    pub clk: u32,
    pub ce: Option<u32>,
    pub sclr: Option<u32>,
    pub sclr_first: bool,
    pub aclr: Option<Aclr>,
    pub q: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Aclr {
    pub net: u32,
    pub active_low: bool,
}

pub struct Port {
    pub name: String,
    pub input: bool,
    pub net: Sig,
}

pub struct Netlist {
    pub nets: Vec<String>,
    /// Every visible name of a net (simcheck picks the RTL's register).
    pub aliases: HashMap<u32, Vec<String>>,
    pub cells: Vec<Cell>,
    pub ports: Vec<Port>,
}

fn bits(v: &Value) -> Result<Vec<Sig>> {
    v.as_array()
        .context("connection is not a bit list")?
        .iter()
        .map(|b| match b {
            Value::Number(n) => Ok(Sig::Net(n.as_u64().context("bit")? as u32)),
            Value::String(s) if s == "0" || s == "x" || s == "z" => Ok(Sig::Const(false)),
            Value::String(s) if s == "1" => Ok(Sig::Const(true)),
            _ => bail!("bad bit {b}"),
        })
        .collect()
}

fn param(v: &Value, name: &str) -> Result<Vec<bool>> {
    if let Some(n) = v.get(name).and_then(Value::as_u64) {
        return Ok((0..64).map(|i| n >> i & 1 == 1).collect());
    }
    let s = v
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("parameter {name}"))?;
    // yosys writes parameters MSB first.
    Ok(s.bytes().rev().map(|c| c == b'1').collect())
}

/// Fold constant and repeated inputs out of a truth table over `ins`.
fn fold(ins: &[Sig], table: &[bool]) -> (Vec<u32>, Vec<bool>) {
    let mut nets: Vec<u32> = Vec::new();
    for s in ins {
        if let Sig::Net(n) = s {
            if !nets.contains(n) {
                nets.push(*n);
            }
        }
    }
    let mut out = vec![false; 1 << nets.len()];
    for (j, o) in out.iter_mut().enumerate() {
        let mut idx = 0;
        for (i, s) in ins.iter().enumerate() {
            let v = match s {
                Sig::Const(c) => *c,
                Sig::Net(n) => j >> nets.iter().position(|m| m == n).unwrap() & 1 == 1,
            };
            idx |= (v as usize) << i;
        }
        *o = table[idx];
    }
    // Drop inputs the function doesn't depend on.
    let mut k = 0;
    while k < nets.len() {
        let dep = (0..out.len()).any(|j| out[j] != out[j ^ (1 << k)]);
        if dep {
            k += 1;
            continue;
        }
        out = (0..out.len() / 2)
            .map(|j| out[(j >> k << (k + 1)) | (j & ((1 << k) - 1))])
            .collect();
        nets.remove(k);
    }
    (nets, out)
}

pub fn read(path: &str, top: &str) -> Result<Netlist> {
    let j: Value = serde_json::from_str(
        &std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?,
    )?;
    let m = j["modules"]
        .get(top)
        .with_context(|| format!("no module {top}"))?;
    // A net's name: a visible register or wire name over a port's, over
    // yosys' own `$` names (simcheck sets registers by these names).
    let ports_obj = m["ports"].as_object().context("ports")?;
    let mut names: HashMap<u32, (u8, String)> = HashMap::default();
    let mut aliases: HashMap<u32, Vec<String>> = HashMap::default();
    for (n, v) in m["netnames"].as_object().context("netnames")? {
        let hidden = v.get("hide_name").and_then(Value::as_u64) == Some(1) || n.starts_with('$');
        let rank = if hidden {
            2
        } else if ports_obj.contains_key(n) {
            1
        } else {
            0
        };
        // Bit i of a netname is index offset + i (offset - i when upto),
        // as the RTL declares it (femtorv32's instr is [31:2]).
        let offset = v.get("offset").and_then(Value::as_i64).unwrap_or(0);
        let upto = v.get("upto").and_then(Value::as_u64) == Some(1);
        let width = v["bits"].as_array().map_or(0, |a| a.len()) as i64;
        for (i, b) in bits(&v["bits"])?.iter().enumerate() {
            let idx = if upto {
                offset + width - 1 - i as i64
            } else {
                offset + i as i64
            };
            if let Sig::Net(id) = b {
                if !hidden {
                    aliases.entry(*id).or_default().push(format!("{n}[{idx}]"));
                }
                let e = names.entry(*id).or_insert((u8::MAX, String::new()));
                if rank < e.0 {
                    *e = (rank, format!("{n}[{idx}]"));
                }
            }
        }
    }
    let names: HashMap<u32, String> = names.into_iter().map(|(k, (_, v))| (k, v)).collect();
    // Nets no netname covers (chipsmith's synthesis purges unnamed wires,
    // tools/a5synth) still get a slot.
    let mut max = names.keys().copied().max().unwrap_or(0) as usize + 1;
    for c in m["cells"].as_object().context("cells")?.values() {
        for v in c["connections"]
            .as_object()
            .into_iter()
            .flat_map(|o| o.values())
        {
            for b in v.as_array().into_iter().flatten() {
                if let Some(n) = b.as_u64() {
                    max = max.max(n as usize + 1);
                }
            }
        }
    }
    let mut nets: Vec<String> = (0..max)
        .map(|i| {
            names
                .get(&(i as u32))
                .cloned()
                .unwrap_or_else(|| format!("n{i}"))
        })
        .collect();
    let mut cells = Vec::new();
    let fresh = |nets: &mut Vec<String>, name: String| -> u32 {
        nets.push(name);
        nets.len() as u32 - 1
    };
    for (name, c) in m["cells"].as_object().context("cells")? {
        let t = c["type"].as_str().unwrap_or("");
        let conn = &c["connections"];
        let p = &c["parameters"];
        let one = |port: &str| -> Result<Sig> { Ok(bits(&conn[port])?[0]) };
        let net = |s: Sig| -> Result<u32> {
            match s {
                Sig::Net(n) => Ok(n),
                Sig::Const(_) => bail!("{name}: output tied to a constant"),
            }
        };
        match t {
            "$lut" => {
                let ins = bits(&conn["A"])?;
                let table = param(p, "LUT")?;
                let (ins, t) = fold(&ins, &table);
                if ins.len() > 6 {
                    bail!("{name}: LUT with {} inputs", ins.len());
                }
                let mask = t
                    .iter()
                    .enumerate()
                    .fold(0u64, |a, (i, &b)| a | (b as u64) << i);
                cells.push(Cell::Lut {
                    ins,
                    mask,
                    out: net(one("Y")?)?,
                });
            }
            t if t.starts_with("$_DFF") || t.starts_with("$_SDFF") => {
                // yosys' fine flop cells, as tools/a5pnr/synth.sh legalises them.
                let (ce, sclr, sclr_first, aclr) = match t {
                    "$_DFF_P_" => (false, false, false, None),
                    "$_DFFE_PP_" => (true, false, false, None),
                    "$_SDFF_PP0_" => (false, true, false, None),
                    "$_SDFFE_PP0P_" => (true, true, true, None),
                    "$_SDFFCE_PP0P_" => (true, true, false, None),
                    "$_DFF_PP0_" => (false, false, false, Some(false)),
                    "$_DFF_PN0_" => (false, false, false, Some(true)),
                    "$_DFFE_PP0P_" => (true, false, false, Some(false)),
                    "$_DFFE_PN0P_" => (true, false, false, Some(true)),
                    _ => bail!("{name}: flop type {t} is not supported"),
                };
                let ctl = |port: &str| -> Result<Option<u32>> {
                    Ok(match one(port)? {
                        Sig::Net(n) => Some(n),
                        Sig::Const(_) => bail!("{name}: {port} tied to a constant"),
                    })
                };
                cells.push(Cell::Ff(Ff {
                    d: one("D")?,
                    clk: net(one("C")?)?,
                    ce: if ce { ctl("E")? } else { None },
                    sclr: if sclr { ctl("R")? } else { None },
                    sclr_first,
                    aclr: match aclr {
                        Some(active_low) => Some(Aclr {
                            net: ctl("R")?.unwrap(),
                            active_low,
                        }),
                        None => None,
                    },
                    q: net(one("Q")?)?,
                }))
            }
            "A5_ARITH" => {
                let ins = [one("A")?, one("B")?, one("C")?, one("D")?];
                let (pt, gt) = (param(p, "P")?, param(p, "G")?);
                let (pn, pf) = fold(&ins, &pt);
                let (gn, gf) = fold(&ins, &gt);
                // Both functions over one input list: the union, at most two nets,
                // which go on the half's own C and D pins (README (w)).
                let mut nets_u: Vec<u32> = pn.clone();
                for n in &gn {
                    if !nets_u.contains(n) {
                        nets_u.push(*n);
                    }
                }
                if nets_u.len() > 2 {
                    bail!(
                        "{name}: arithmetic cell with {} variable inputs",
                        nets_u.len()
                    );
                }
                let expand = |sub: &[u32], f: &[bool]| -> u16 {
                    let mut m = 0u16;
                    for idx in 0..16usize {
                        // C is index bit 2, D bit 3.
                        let mut j = 0;
                        for (k, n) in sub.iter().enumerate() {
                            let pos = nets_u.iter().position(|x| x == n).unwrap();
                            j |= (idx >> (2 + pos) & 1) << k;
                        }
                        if f[j] {
                            m |= 1 << idx;
                        }
                    }
                    m
                };
                let so = match bits(&conn["SO"]) {
                    Ok(v) => match v.first() {
                        Some(Sig::Net(n)) => Some(*n),
                        _ => None,
                    },
                    Err(_) => None,
                };
                let co = match bits(&conn["CO"]) {
                    Ok(v) => match v.first() {
                        Some(Sig::Net(n)) => Some(*n),
                        _ => None,
                    },
                    Err(_) => None,
                };
                cells.push(Cell::Arith {
                    p: expand(&pn, &pf),
                    g: expand(&gn, &gf),
                    c: nets_u.first().copied(),
                    d: nets_u.get(1).copied(),
                    ci: one("CI")?,
                    so,
                    co,
                });
            }
            "A5_M20K" => {
                let rdata = bits(&conn["RDATA"])?
                    .into_iter()
                    .map(|s| if let Sig::Net(n) = s { Some(n) } else { None })
                    .collect();
                let init_bits = p
                    .get("INIT")
                    .map(|_| param(p, "INIT"))
                    .transpose()?
                    .unwrap_or_default();
                let init = (0..512)
                    .map(|a| {
                        (0..32).fold(0u32, |w, k| {
                            w | (init_bits.get(32 * a + k).copied().unwrap_or(false) as u32) << k
                        })
                    })
                    .collect();
                let outreg = p
                    .get("OUTREG")
                    .map(|_| param(p, "OUTREG"))
                    .transpose()?
                    .is_some_and(|v| v.iter().any(|&b| b));
                cells.push(Cell::M20k(M20k {
                    clk: net(one("CLK")?)?,
                    we: one("WE")?,
                    // Unconnected enables are high (phase 6's instances have none).
                    be: if conn.get("BE").is_some() {
                        bits(&conn["BE"])?
                    } else {
                        vec![Sig::Const(true); 4]
                    },
                    re: if conn.get("RE").is_some() {
                        one("RE")?
                    } else {
                        Sig::Const(true)
                    },
                    waddr: bits(&conn["WADDR"])?,
                    wdata: bits(&conn["WDATA"])?,
                    raddr: bits(&conn["RADDR"])?,
                    rdata,
                    outreg,
                    init,
                }));
            }
            "$__A5_MLAB_" => {
                let init = param(p, "INIT")?
                    .iter()
                    .enumerate()
                    .fold(0u32, |w, (a, &b)| w | (b as u32) << a);
                let rdata = match one("PORT_R_RD_DATA")? {
                    Sig::Net(n) => Some(n),
                    Sig::Const(_) => None,
                };
                cells.push(Cell::Mlab(Mlab {
                    name: name.clone(),
                    clk: net(one("PORT_W_CLK")?)?,
                    we: one("PORT_W_WR_EN")?,
                    waddr: bits(&conn["PORT_W_ADDR"])?,
                    wdata: one("PORT_W_WR_DATA")?,
                    raddr: bits(&conn["PORT_R_ADDR"])?,
                    rdata,
                    init,
                }));
            }
            "A5_MUL" => {
                let flag = |k: &str| -> Result<bool> { Ok(param(p, k)?.iter().any(|&b| b)) };
                let mode = super::dsp::Mode {
                    wide: flag("WIDE")?,
                    signed: flag("SIGNED")?,
                };
                let w = mode.width();
                let pb = bits(&conn["P"])?
                    .into_iter()
                    .map(|s| if let Sig::Net(n) = s { Some(n) } else { None })
                    .collect();
                cells.push(Cell::Mul {
                    mode,
                    a: bits(&conn["A"])?[..w].to_vec(),
                    b: bits(&conn["B"])?[..w].to_vec(),
                    p: pb,
                });
            }
            "$_TBUF_" => {
                cells.push(Cell::Tbuf {
                    a: one("A")?,
                    e: one("E")?,
                    y: net(one("Y")?)?,
                });
            }
            // chipsmith's synthesis keeps the word-level cell (tools/a5synth).
            "$tribuf" => {
                for ((a, e), y) in bits(&conn["A"])?
                    .into_iter()
                    .zip(std::iter::repeat(one("EN")?))
                    .zip(bits(&conn["Y"])?)
                {
                    cells.push(Cell::Tbuf { a, e, y: net(y)? });
                }
            }
            "A5_IOPLL" => {
                let khz = |k: &str| -> Result<u64> {
                    Ok(param(p, k)?
                        .iter()
                        .enumerate()
                        .fold(0u64, |a, (i, &b)| a | (b as u64) << i))
                };
                cells.push(Cell::Pll {
                    refclk: net(one("REFCLK")?)?,
                    out: net(one("OUTCLK")?)?,
                    rst: u32::MAX,
                    ref_khz: khz("REF_KHZ")?,
                    out_khz: khz("OUT_KHZ")?,
                });
            }
            _ => bail!("{name}: cell type {t} is not supported"),
        }
    }
    let mut ports = Vec::new();
    for (name, v) in m["ports"].as_object().context("ports")? {
        let input = v["direction"] == "input";
        for (i, b) in bits(&v["bits"])?.into_iter().enumerate() {
            let n = if v["bits"].as_array().map_or(0, |a| a.len()) > 1 {
                format!("{name}[{i}]")
            } else {
                name.clone()
            };
            ports.push(Port {
                name: n,
                input,
                net: b,
            });
        }
    }
    // A flop with a constant D still needs a driver it can sit behind.
    for k in 0..cells.len() {
        if let Cell::Ff(Ff {
            d: Sig::Const(v), ..
        }) = cells[k]
        {
            let n = fresh(&mut nets, format!("const{}", v as u8));
            cells.push(Cell::Lut {
                ins: vec![],
                mask: v as u64,
                out: n,
            });
            if let Cell::Ff(f) = &mut cells[k] {
                f.d = Sig::Net(n);
            }
        }
    }
    // M20K inputs tied to a constant: an address or data bit, or an enable
    // held low, needs a driver (an unrouted pin's level is only known for
    // the enables, high).
    let mut consts: [Option<u32>; 2] = [None, None];
    for k in 0..cells.len() {
        let Cell::M20k(m) = &cells[k] else { continue };
        let mut m = m.clone();
        let mut tie =
            |s: &mut Sig, idle_high: bool, cells: &mut Vec<Cell>, nets: &mut Vec<String>| {
                if let Sig::Const(v) = *s {
                    if idle_high && v {
                        return;
                    }
                    let n = *consts[v as usize].get_or_insert_with(|| {
                        let n = fresh(nets, format!("const{}", v as u8));
                        cells.push(Cell::Lut {
                            ins: vec![],
                            mask: v as u64,
                            out: n,
                        });
                        n
                    });
                    *s = Sig::Net(n);
                }
            };
        // WE held low needs no driver: the pin's inverter (README (y)).
        if m.we != Sig::Const(false) {
            tie(&mut m.we, true, &mut cells, &mut nets);
        }
        tie(&mut m.re, true, &mut cells, &mut nets);
        for s in m.be.iter_mut() {
            tie(s, true, &mut cells, &mut nets);
        }
        for s in m
            .waddr
            .iter_mut()
            .chain(m.wdata.iter_mut())
            .chain(m.raddr.iter_mut())
        {
            tie(s, false, &mut cells, &mut nets);
        }
        cells[k] = Cell::M20k(m);
    }
    // An MLAB RAM address bit constant on one port only (picorv32's
    // register file) is a driven constant there: an unrouted address pin
    // reads 1 (README (z6)), and only a bit constant on both ports may stay
    // unrouted.
    for k in 0..cells.len() {
        let Cell::Mlab(m) = &cells[k] else { continue };
        let mut m = m.clone();
        let mut changed = false;
        for j in 0..m.waddr.len().min(m.raddr.len()) {
            let (w, r) = (m.waddr[j], m.raddr[j]);
            if matches!(w, Sig::Const(_)) == matches!(r, Sig::Const(_)) {
                continue;
            }
            for s in [&mut m.waddr[j], &mut m.raddr[j]] {
                if let Sig::Const(v) = *s {
                    let n = *consts[v as usize].get_or_insert_with(|| {
                        let n = fresh(&mut nets, format!("const{}", v as u8));
                        cells.push(Cell::Lut {
                            ins: vec![],
                            mask: v as u64,
                            out: n,
                        });
                        n
                    });
                    *s = Sig::Net(n);
                    changed = true;
                }
            }
        }
        if changed {
            cells[k] = Cell::Mlab(m);
        }
    }
    // A multiplier's constant operand bits: a pin left unrouted reads 1
    // (README (z11)), so every operand pin carries a driven signal.
    for k in 0..cells.len() {
        let Cell::Mul { a, b, .. } = &cells[k] else {
            continue;
        };
        let (mut a, mut b) = (a.clone(), b.clone());
        for s in a.iter_mut().chain(b.iter_mut()) {
            if let Sig::Const(v) = *s {
                let n = *consts[v as usize].get_or_insert_with(|| {
                    let n = fresh(&mut nets, format!("const{}", v as u8));
                    cells.push(Cell::Lut {
                        ins: vec![],
                        mask: v as u64,
                        out: n,
                    });
                    n
                });
                *s = Sig::Net(n);
            }
        }
        if let Cell::Mul { a: a0, b: b0, .. } = &mut cells[k] {
            *a0 = a;
            *b0 = b;
        }
    }
    for v in aliases.values_mut() {
        v.sort();
    }
    Ok(Netlist {
        nets,
        aliases,
        cells,
        ports,
    })
}

/// Every IOPLL's reset, held off: an undriven reset pin holds the PLL in
/// reset, so each PLL gets a constant-0 LUT routed to it (End::PllRst)
/// [obs, board 2026-11-06: plo with the pin undriven, or tied to a LUT
/// that outputs 1, never clocks its counter; tied to 0 it runs at 100 MHz.
/// Quartus' pa, whose RTL ties rst to 0, drives the pin from a 6-LUT whose
/// stored mask is all ones, which outputs 0 (emit::six_stored)].
pub fn tie_pll_resets(nl: &mut Netlist) {
    for i in 0..nl.cells.len() {
        if let Cell::Pll { rst, .. } = nl.cells[i] {
            if rst != u32::MAX {
                continue;
            }
            let n = nl.nets.len() as u32;
            nl.nets.push(format!("pll{i}_rst"));
            nl.cells.push(Cell::Lut {
                ins: vec![],
                mask: 0,
                out: n,
            });
            if let Cell::Pll { rst, .. } = &mut nl.cells[i] {
                *rst = n;
            }
        }
    }
}
