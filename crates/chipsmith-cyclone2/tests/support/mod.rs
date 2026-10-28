//! yosys as the oracle for chipsmith's Cyclone II synthesis (ADR-0004):
//! the RTL it elaborates, the netlist cyclone2-re's `cyclone2.ys` makes
//! from it, and a sequential equivalence check between an RTL netlist and
//! a synthesized one. Needs yosys and `CHIPSMITH_CYCLONE2_SYNTH` (for
//! `yosys_flow`): run in the devshell.
//!
//! The check is yosys' `equiv_make`/`equiv_struct`/`equiv_simple`/
//! `equiv_induct` (chipsmith-netlist's round-trip test's script) with two
//! additions that let it prove synthesis that removes registers:
//!
//! - Registers synthesis found stuck at their power-up value appear in its
//!   netlist as wires with constant bits. Those claims are proven on the
//!   RTL itself (`sat -tempinduct`: true at power-up, kept by every clock
//!   edge), and then the RTL's registers are replaced by the constants, so
//!   induction compares designs with the same registers.
//! - The Cyclone II cells get simulation models (the cell library,
//!   `cells::LIBRARY`, only declares them).

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use chipsmith_cyclone2::synth::cells;
use chipsmith_netlist::{
    read_json, write_json, CellKind, Design, Gate, GateOp, Module, Net, SigBit, State, Wire,
};

/// Behavioural models of the cells synthesis emits, as the packer and the
/// hardware read them (`cells::LIBRARY`'s comments).
pub const SIM_MODELS: &str = r#"
module LCELL_COMB (input DATAA, input DATAB, input DATAC, input DATAD, input CIN,
                   output COMBOUT, output COUT);
  parameter [15:0] LUT_MASK = 16'hFFFF;
  // Either CIN (arithmetic) or DATAC (logic) is used; the other is tied 1.
  wire c = CIN & DATAC;
  assign COMBOUT = LUT_MASK[{DATAD, c, DATAB, DATAA}];
  assign COUT = LUT_MASK[{1'b0, CIN, DATAB, DATAA}];
endmodule
module LCELL_FF (input DATAIN, input CLK, input ENA, input ACLR, input SCLR, output reg REGOUT);
  initial REGOUT = 1'b0;
  always @(posedge CLK, posedge ACLR)
    if (ACLR) REGOUT <= 1'b0;
    else if (ENA) REGOUT <= SCLR ? 1'b0 : DATAIN;
endmodule
module CY2_IBUF (input PAD, input INCLK, output COMBOUT, output REGOUT);
  parameter INPUT_REGISTER = "none";
  reg q = INPUT_REGISTER == "high";
  always @(posedge INCLK) q <= PAD;
  assign COMBOUT = PAD;
  assign REGOUT = q;
endmodule
module CY2_OBUF (input DATAOUT, input OUTCLK, output PAD);
  parameter OUTPUT_REGISTER = "none";
  reg q = OUTPUT_REGISTER == "high";
  always @(posedge OUTCLK) q <= DATAOUT;
  assign PAD = OUTPUT_REGISTER == "none" ? DATAOUT : q;
endmodule
// The checker cuts every pin (`cut_pins`): EXT is what the outside drives
// while the buffer is off.
module CY2_IOBUF (input DATAOUT, input OE, input INCLK, input OUTCLK, input EXT,
                  output COMBOUT, output REGOUT, output PAD);
  parameter INPUT_REGISTER = "none";
  parameter OUTPUT_REGISTER = "none";
  parameter OE_REGISTER = "none";
  reg qi = INPUT_REGISTER == "high", qd = OUTPUT_REGISTER == "high", qe = OE_REGISTER == "high";
  always @(posedge INCLK) qi <= PAD;
  always @(posedge OUTCLK) begin qd <= DATAOUT; qe <= OE; end
  wire d = OUTPUT_REGISTER == "none" ? DATAOUT : qd;
  wire e = OE_REGISTER == "none" ? OE : qe;
  assign PAD = e ? d : EXT;
  assign COMBOUT = PAD;
  assign REGOUT = qi;
endmodule
"#;

/// A scratch directory under `base` (tests pass `CARGO_TARGET_TMPDIR`).
pub fn scratch(base: &Path, name: &str) -> PathBuf {
    let dir = base.join("synth").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn synth_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("CHIPSMITH_CYCLONE2_SYNTH")
            .expect("CHIPSMITH_CYCLONE2_SYNTH (cyclone2-re's synth/) unset: run in `nix develop`"),
    )
}

/// Runs a yosys script in `cwd`; `Err` with the end of the log if it fails.
pub fn yosys(dir: &Path, name: &str, script: &str, cwd: &Path) -> Result<String, String> {
    let ys = dir.join(format!("{name}.ys"));
    let log = dir.join(format!("{name}.log"));
    std::fs::write(&ys, script).unwrap();
    let out = Command::new("yosys")
        .arg("-q")
        .arg("-l")
        .arg(&log)
        .arg("-s")
        .arg(&ys)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("can't run yosys ({e}): run in `nix develop`"));
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    if out.status.success() {
        Ok(text)
    } else {
        let tail: Vec<&str> = text.lines().rev().take(25).collect();
        Err(format!(
            "yosys {} failed:\n{}\n{}",
            ys.display(),
            tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

fn quoted(files: &[PathBuf]) -> String {
    files
        .iter()
        .map(|f| format!("\"{}\"", f.display()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// yosys' RTL of a design: what chipsmith's synthesis takes (`proc;
/// flatten`, the device library read as blackboxes, top renamed `top`).
pub fn rtl(dir: &Path, top: &str, files: &[PathBuf]) -> PathBuf {
    let json = dir.join("rtl.json");
    let lib = dir.join("cells.v");
    std::fs::write(&lib, cells::LIBRARY).unwrap();
    let rename = if top == "top" {
        String::new()
    } else {
        format!("rename {top} top\n")
    };
    let script = format!(
        "read_verilog -lib {}\nread_verilog {}\n{rename}hierarchy -check -top top\nproc\nflatten\nwrite_json {}\n",
        lib.display(),
        quoted(files),
        json.display()
    );
    yosys(dir, "rtl", &script, dir).unwrap_or_else(|e| panic!("{e}"));
    json
}

/// What the yosys flow makes of the design, as chipsmith's build ran it
/// before its own synthesis: cyclone2-re's `cyclone2.ys` cut at `abc -lut 4`, chipsmith's
/// LUT mapper in between.
pub fn yosys_flow(dir: &Path, top: &str, files: &[PathBuf]) -> Result<PathBuf, String> {
    let script = std::fs::read_to_string(synth_dir().join("synth/cyclone2.ys")).unwrap();
    let lines: Vec<&str> = script.lines().collect();
    let cut = lines
        .iter()
        .position(|l| l.trim() == "abc -lut 4")
        .expect("cyclone2.ys maps LUTs with `abc -lut 4`");
    let (gates, luts, json) = (
        dir.join("flow_gates.json"),
        dir.join("flow_luts.json"),
        dir.join("yosys_flow.json"),
    );
    let rename = if top == "top" {
        String::new()
    } else {
        format!("rename {top} top\n")
    };
    let to_gates = format!(
        "read_verilog {}\n{rename}{}\nwrite_json {}\n",
        quoted(files),
        lines[..cut].join("\n"),
        gates.display()
    );
    yosys(dir, "flow_gates", &to_gates, &synth_dir())?;
    chipsmith_synth::lutmap::map_file(&gates, &luts).map_err(|e| format!("{e:#}"))?;
    let to_cells = format!(
        "read_verilog -lib synth/cells_sim.v\nread_json {}\nhierarchy -top top\n{}\nwrite_json {}\n",
        luts.display(),
        lines[cut + 1..].join("\n"),
        json.display()
    );
    yosys(dir, "flow_cells", &to_cells, &synth_dir())?;
    Ok(json)
}

/// chipsmith's own front end's netlist of the design (what the build
/// synthesizes), the top renamed `top` as the yosys netlists have it.
pub fn frontend(top: &str, files: &[PathBuf]) -> Design {
    let mut fe = chipsmith_verilog::Frontend::new();
    fe.read_library_source(cells::LIBRARY_NAME, cells::LIBRARY)
        .unwrap_or_else(|e| panic!("{e}"));
    for f in files {
        fe.read_file(f).unwrap_or_else(|e| panic!("{e}"));
    }
    let mut d = fe.elaborate(top).unwrap_or_else(|e| panic!("{e}"));
    for m in &mut d.modules {
        if m.name == top {
            m.name = "top".into();
        }
    }
    d
}

pub fn write(design: &Design, path: &Path) {
    write_json(
        design,
        std::io::BufWriter::new(std::fs::File::create(path).unwrap()),
    )
    .unwrap();
}

pub fn read(path: &Path) -> Design {
    read_json(&std::fs::read(path).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn top(d: &Design) -> &Module {
    d.modules
        .iter()
        .find(|m| !m.is_blackbox() && m.name == "top")
        .or_else(|| d.modules.iter().find(|m| !m.is_blackbox()))
        .expect("a top module")
}

/// What the check found.
#[derive(Debug)]
pub struct Proof {
    /// Register bits synthesis made constant, proven invariant on the RTL.
    pub constant_registers: usize,
    /// `$equiv` cells proven: every named net both netlists share.
    pub equivalences: usize,
}

/// Memories synthesis turned into registers: their words are wires
/// `memid[i]` in its netlist.
fn logic_memories(gate: &Module) -> Vec<String> {
    let mut ids: Vec<String> = gate
        .wires
        .iter()
        .filter_map(|w| w.name.strip_suffix("[0]").map(str::to_owned))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn top_mut(d: &mut Design) -> &mut Module {
    d.modules
        .iter_mut()
        .find(|m| m.name == "top")
        .expect("a top module")
}

/// Memories synthesis put in M4K blocks: named by its blocks,
/// `memid.0.<block>`.
fn block_memories(gate: &Module) -> Vec<String> {
    let mut ids: Vec<String> = gate
        .cells
        .iter()
        .filter(|c| c.kind.type_name() == "CY2_M4K")
        .filter_map(|c| c.name.rsplit_once(".0.").map(|(id, _)| id.to_owned()))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// A port for a cut. An output gets nets of its own, through a gate
/// (`b & b`: `opt_clean` would dissolve a buffer): were the port to name
/// the logic's nets, yosys could take its name for them and `equiv_make`
/// would join them with the other side's port.
fn add_port(m: &mut Module, name: String, dir: chipsmith_netlist::Dir, bits: Vec<SigBit>) {
    let bits = if dir == chipsmith_netlist::Dir::Out {
        bits.into_iter()
            .enumerate()
            .map(|(i, b)| {
                let y = SigBit::Net(m.new_net());
                m.add_cell(
                    format!("$cut${name}${i}"),
                    CellKind::Gate(Gate::new(GateOp::And, &[b, b], y).unwrap()),
                );
                y
            })
            .collect()
    } else {
        bits
    };
    let mut w = Wire::new(name, bits);
    w.port = Some(dir);
    m.wires.push(w);
}

/// A memory's interface as both sides cut it: read data in, the rest out.
#[derive(Debug, Clone)]
pub struct MemShape {
    width: usize,
    abits: usize,
}

/// Replaces each `$mem_v2` by ports: its read data become an input
/// `__mem_<id>_rdata`, its addresses, write data and enable outputs.
fn cut_memories(m: &mut Module) -> Result<BTreeMap<String, MemShape>, String> {
    use chipsmith_netlist::Dir;
    let mut shapes = BTreeMap::new();
    let mut ports = Vec::new();
    let mut masked = Vec::new();
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    let mut keep = Vec::new();
    for c in std::mem::take(&mut m.cells) {
        let CellKind::Mem(mem) = &c.kind else {
            keep.push(c);
            continue;
        };
        let id = mem.memid.trim_start_matches('\\').to_owned();
        let [rd] = &mem.rd[..] else {
            return Err(format!("memory {id}: not one read port"));
        };
        let abits = (usize::BITS - mem.size.saturating_sub(1).leading_zeros()) as usize;
        let rdata = (0..mem.width)
            .map(|_| SigBit::Net(m.new_net()))
            .collect::<Vec<_>>();
        for (q, n) in rd.data.iter().zip(&rdata) {
            if let (SigBit::Net(q), n) = (q, n) {
                subst.set(*q, *n);
            }
        }
        ports.push((format!("__mem_{id}_rdata"), Dir::In, rdata));
        ports.push((
            format!("__mem_{id}_raddr"),
            Dir::Out,
            rd.addr[..abits].to_vec(),
        ));
        if let Some(w) = mem.wr.first() {
            // What is written only matters where it is written: masked by
            // the enable, so x in what isn't written can't differ.
            let en: Vec<SigBit> = w.en.clone();
            let addr = w.addr[..abits].to_vec();
            let data = w.data.clone();
            masked.push((format!("__mem_{id}_waddr"), addr, vec![en[0]; abits]));
            masked.push((format!("__mem_{id}_wdata"), data, en.clone()));
            ports.push((format!("__mem_{id}_we"), Dir::Out, vec![en[0]]));
        }
        shapes.insert(
            id,
            MemShape {
                width: mem.width,
                abits,
            },
        );
    }
    m.cells = keep;
    for (name, dir, bits) in ports {
        add_port(m, name, dir, bits);
    }
    for (name, bits, en) in masked {
        let bits = mask(m, &name, &bits, &en);
        add_port(m, name, Dir::Out, bits);
    }
    subst.apply(m);
    Ok(shapes)
}

/// `bits & en`, bit by bit.
fn mask(m: &mut Module, name: &str, bits: &[SigBit], en: &[SigBit]) -> Vec<SigBit> {
    bits.iter()
        .zip(en)
        .enumerate()
        .map(|(i, (&b, &e))| {
            let y = SigBit::Net(m.new_net());
            m.add_cell(
                format!("$cut${name}$en${i}"),
                CellKind::Gate(Gate::new(GateOp::And, &[b, e], y).unwrap()),
            );
            y
        })
        .collect()
}

/// The blocks' side of [`cut_memories`].
fn cut_blocks(m: &mut Module, shapes: &BTreeMap<String, MemShape>) -> Result<(), String> {
    use chipsmith_netlist::Dir;
    let mut blocks: BTreeMap<String, Vec<(usize, chipsmith_netlist::Generic)>> = BTreeMap::new();
    m.cells.retain(|c| {
        if let (CellKind::Other(g), Some((id, b))) = (&c.kind, c.name.rsplit_once(".0.")) {
            if g.ty == "CY2_M4K" {
                blocks
                    .entry(id.to_owned())
                    .or_default()
                    .push((b.parse().unwrap_or(0), g.clone()));
                return false;
            }
        }
        true
    });
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    for (id, mut bs) in blocks {
        bs.sort_by_key(|b| b.0);
        let shape = shapes
            .get(&id)
            .ok_or_else(|| format!("M4K for {id}, which isn't a memory"))?;
        let port = |g: &chipsmith_netlist::Generic, p: &str| g.port(p).unwrap().sig.clone();
        let g0 = &bs[0].1;
        let width = g0.params["WIDTH"].bits().and_then(|c| c.as_u64()).unwrap() as usize;
        let single = g0.params["MODE"].str() == Some("SINGLE");
        let top = |a: Vec<SigBit>| a[12 - shape.abits..].to_vec();
        let rdata: Vec<SigBit> = (0..shape.width).map(|_| SigBit::Net(m.new_net())).collect();
        let mut wdata = Vec::new();
        for (b, g) in &bs {
            let dout = port(g, "DATAOUT");
            let din = port(g, "A_DATAIN");
            for k in 0..width {
                let col = b * width + k;
                if col < shape.width {
                    if let SigBit::Net(n) = dout[k] {
                        subst.set(n, rdata[col]);
                    }
                    wdata.push(din[k]);
                }
            }
        }
        let we = port(g0, "A_WE")[0];
        let (raddr, waddr) = if single {
            (top(port(g0, "A_ADDR")), top(port(g0, "A_ADDR")))
        } else {
            (top(port(g0, "B_ADDR")), top(port(g0, "A_ADDR")))
        };
        add_port(m, format!("__mem_{id}_rdata"), Dir::In, rdata);
        add_port(m, format!("__mem_{id}_raddr"), Dir::Out, raddr);
        if we != SigBit::ZERO {
            let n = format!("__mem_{id}_waddr");
            let waddr = mask(m, &n, &waddr, &vec![we; waddr.len()]);
            add_port(m, n, Dir::Out, waddr);
            let n = format!("__mem_{id}_wdata");
            let wdata = mask(m, &n, &wdata, &vec![we; wdata.len()]);
            add_port(m, n, Dir::Out, wdata);
            add_port(m, format!("__mem_{id}_we"), Dir::Out, vec![we]);
        }
    }
    subst.apply(m);
    Ok(())
}

/// A bit that is 1 when `q` holds one of `values`, as gates added to `m`.
fn in_set(m: &mut Module, q: &[SigBit], values: &[chipsmith_netlist::Const], tag: &str) -> SigBit {
    let mut n = 0;
    let mut gate = |m: &mut Module, op: GateOp, ins: &[SigBit]| {
        let y = SigBit::Net(m.new_net());
        n += 1;
        m.add_cell(
            format!("$claim${tag}${n}"),
            CellKind::Gate(Gate::new(op, ins, y).unwrap()),
        );
        y
    };
    let mut any = SigBit::ZERO;
    for v in values {
        let mut all = SigBit::ONE;
        for (b, s) in q.iter().zip(&v.0) {
            let lit = if *s == State::One {
                *b
            } else {
                gate(m, GateOp::Not, &[*b])
            };
            all = gate(m, GateOp::And, &[all, lit]);
        }
        any = gate(m, GateOp::Or, &[any, all]);
    }
    any
}

/// yosys proves `gate` equivalent to `gold` (both JSON files, the
/// memories already as they should be compared), with x in `gold` read as
/// "any value". Returns how many `$equiv` it proved.
fn equiv(
    dir: &Path,
    name: &str,
    gold: &Path,
    gate: &Path,
    reference: &Module,
) -> Result<usize, String> {
    match equiv_once(dir, name, gold, gate, &[]) {
        Ok(n) => Ok(n),
        Err((msg, unproven)) => {
            // An internal wire synthesis gave a value only where it's
            // observed (a register's next value specialized under its
            // reset) can't be matched by name. Proving every port and
            // register without matching those is still a proof: they are
            // what the netlist shows.
            let must: std::collections::HashSet<&str> = reference
                .wires
                .iter()
                .filter(|w| w.port.is_some())
                .map(|w| w.name.as_str())
                .chain(register_wires(reference))
                .collect();
            if unproven.is_empty() || unproven.iter().any(|u| must.contains(u.as_str())) {
                return Err(msg);
            }
            equiv_once(dir, &format!("{name}_unmatched"), gold, gate, &unproven)
                .map_err(|(msg, _)| msg)
        }
    }
}

/// Names of wires a register drives.
fn register_wires(m: &Module) -> impl Iterator<Item = &str> {
    let q: std::collections::HashSet<Net> = m
        .cells
        .iter()
        .flat_map(|c| match &c.kind {
            CellKind::Ff(ff) => ff.q.iter().filter_map(|b| b.net()).collect(),
            CellKind::FfGate(ff) => ff.q.net().into_iter().collect(),
            _ => Vec::new(),
        })
        .collect();
    m.wires
        .iter()
        .filter(move |w| {
            w.bits
                .iter()
                .any(|b| b.net().is_some_and(|n| q.contains(&n)))
        })
        .map(|w| w.name.as_str())
}

/// One run of the proof, wires named in `unmatched` left out of
/// `equiv_make`. On failure, the message and the unproven wires' names.
fn equiv_once(
    dir: &Path,
    name: &str,
    gold: &Path,
    gate: &Path,
    unmatched: &[String],
) -> Result<usize, (String, Vec<String>)> {
    let blacklist = dir.join(format!("{name}.blacklist"));
    std::fs::write(&blacklist, unmatched.join("\n")).unwrap();
    let sim = dir.join("cy2_sim.v");
    std::fs::write(&sim, SIM_MODELS).unwrap();
    // Tri-states have no SAT model: as opaque cells, merged when their
    // inputs are proven equal (chipsmith-netlist's round-trip check does
    // the same). A pin buffer always on or always off is no tri-state:
    // its multiplexer with a constant select folds first.
    let prep = "hierarchy -top top\nproc -noopt\nflatten\nopt_expr t:$mux\ntribuf\nchtype -set chipsmith_mem t:$mem_v2\n\
                chtype -set chipsmith_tribuf t:$tribuf t:$_TBUF_\nopt_clean";
    let script = format!(
        "read_json {gold}\nread_verilog -overwrite {sim}\n{prep}\nrename top gold\ndesign -stash gold\n\
         read_json {gate}\nread_verilog -overwrite {sim}\n{prep}\nrename top gate\ndesign -stash gate\n\
         design -copy-from gold -as gold gold\ndesign -copy-from gate -as gate gate\n\
         equiv_make -blacklist {blacklist} gold gate equiv\nhierarchy -top equiv\nasync2sync\n\
         equiv_struct -fwd\nequiv_simple -undef -set-assumes\nequiv_induct -undef -set-assumes -ignore-unknown-cells\n\
         equiv_status -assert\n",
        gold = gold.display(),
        gate = gate.display(),
        sim = sim.display(),
        blacklist = blacklist.display(),
    );
    let log = yosys(dir, name, &script, dir).map_err(|e| {
        let full = std::fs::read_to_string(dir.join(format!("{name}.log"))).unwrap_or_default();
        let unproven: Vec<&str> = full.lines().filter(|l| l.contains("Unproven")).collect();
        // `Unproven $equiv <cell>: \wire_gold [3] \wire_gate [3]`
        let mut names: Vec<String> = unproven
            .iter()
            .filter_map(|l| l.split(": ").nth(1)?.split_whitespace().next())
            .filter_map(|w| w.trim_start_matches('\\').strip_suffix("_gold"))
            .map(str::to_owned)
            .collect();
        names.sort();
        names.dedup();
        let shown: Vec<&str> = if unproven.is_empty() {
            e.lines()
                .filter(|l| l.contains("Unproven"))
                .take(10)
                .collect()
        } else {
            unproven.into_iter().take(10).collect()
        };
        (
            format!(
                "{name}: not proven equivalent:\n{}\n(see {})",
                shown.join("\n"),
                dir.join(format!("{name}.log")).display()
            ),
            names,
        )
    })?;
    Ok(log
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("Found ")
                .and_then(|r| r.strip_suffix(" $equiv cells in equiv:"))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(0))
}

/// `design` with its memories collected, the ones in `to_logic` made into
/// registers yosys' way (which names the words as synthesis does), and
/// every register given a power-up value by synthesis' rule.
fn prep_memories(
    dir: &Path,
    name: &str,
    design: &Path,
    to_logic: &[String],
    in_blocks: &[String],
) -> Result<Design, String> {
    let mut map = String::new();
    // A memory in M4Ks reads synchronously: its read registers merged in
    // as yosys does (`memory_dff`), so its ports line up with the blocks'.
    for id in in_blocks {
        map += &format!("memory_dff m:{id} t:$memrd* r:MEMID=\\{id} %i %u t:*dff* %u\n");
    }
    map += "memory_collect\n";
    for id in to_logic {
        map += &format!("memory_map t:$mem_v2 r:MEMID=\\{id} %i\n");
    }
    // Power-up values after merging, as synthesis decides them: an address
    // register with one can't go into a block.
    let with_inits = design.to_path_buf();
    let out = dir.join(format!("{name}.json"));
    yosys(
        dir,
        name,
        &format!(
            "read_json {}\nhierarchy -top top\nproc\nflatten\n{map}opt_clean\nwrite_json {}\n",
            with_inits.display(),
            out.display()
        ),
        dir,
    )?;
    let mut d = read(&out);
    d.modules.retain(|m| !m.is_blackbox());
    for m in &mut d.modules {
        chipsmith_synth::rewrite::define_inits(m);
    }
    Ok(d)
}

/// A latch is a LUT feeding its own output back (synthesis' `ff.rs`),
/// which SAT can't take. Each feedback is cut through a `$ff`, so the LUT
/// computes the latch's next value from what it held one step before:
/// `async2sync`'s model of the RTL's latch.
fn cut_loops(m: &mut Module) -> usize {
    let mut cuts = Vec::new();
    for (i, c) in m.cells.iter().enumerate() {
        let CellKind::Other(g) = &c.kind else {
            continue;
        };
        if g.ty != cells::LCELL_COMB {
            continue;
        }
        let Some(y) = g.port("COMBOUT").map(|p| p.sig[0]) else {
            continue;
        };
        if g.ports
            .iter()
            .any(|p| p.name != "COMBOUT" && p.sig.contains(&y))
        {
            cuts.push((i, y));
        }
    }
    for &(i, y) in &cuts {
        let held = SigBit::Net(m.new_net());
        let CellKind::Other(g) = &mut m.cells[i].kind else {
            unreachable!()
        };
        for p in g.ports.iter_mut().filter(|p| p.name != "COMBOUT") {
            for b in &mut p.sig {
                if *b == y {
                    *b = held;
                }
            }
        }
        m.add_cell(
            format!("$latch_cut${i}"),
            CellKind::Ff(chipsmith_netlist::Ff {
                q: vec![held],
                kind: chipsmith_netlist::FfKind::Global { d: vec![y] },
            }),
        );
    }
    cuts.len()
}

/// yosys' `async2sync` models a latch's asynchronous reset (`$adlatch`,
/// `$dlatchsr`, `$sr`) as overriding Q without being held: released, the
/// latch shows what it held before the reset, which no hardware does. So
/// the reference's latches get their resets through the enable and data
/// first (`EN | rst`, `rst ? value : D`), which it models right.
fn latch_resets_to_logic(m: &mut Module) {
    use chipsmith_netlist::{Ctrl, Ff, FfKind, LatchReset, Polarity, SetClr, Word};
    let mut g = chipsmith_synth::rewrite::Gates::new(m, "latch_reset");
    let high = |g: &mut chipsmith_synth::rewrite::Gates, s: SigBit, p: Polarity| match p {
        Polarity::Pos => s,
        Polarity::Neg => g.not(s),
    };
    let cells = std::mem::take(&mut g.m.cells);
    for mut c in cells {
        let CellKind::Ff(ff) = &c.kind else {
            g.m.cells.push(c);
            continue;
        };
        // Per bit: (Q, EN, D), all active high.
        let set_clr = |g: &mut chipsmith_synth::rewrite::Gates,
                       sr: &SetClr<Word>,
                       i: usize,
                       en: SigBit,
                       d: SigBit| {
            let set = high(g, sr.set[i], sr.set_polarity);
            let clr = high(g, sr.clr[i], sr.clr_polarity);
            let any = g.or(set, clr);
            let en = g.or(en, any);
            let d = g.or(d, set);
            let not_clr = g.not(clr);
            (en, g.and(d, not_clr))
        };
        let bits: Vec<(SigBit, SigBit, SigBit)> = match &ff.kind {
            FfKind::Dlatch {
                en,
                d,
                reset: LatchReset::Async { arst, value },
            } => {
                let r = high(&mut g, arst.sig, arst.polarity);
                let e = high(&mut g, en.sig, en.polarity);
                let e = g.or(e, r);
                (0..ff.q.len())
                    .map(|i| {
                        let v = SigBit::Const(value.0[i]);
                        (ff.q[i], e, g.mux(d[i], v, r))
                    })
                    .collect()
            }
            FfKind::Dlatch {
                en,
                d,
                reset: LatchReset::SetClr(sr),
            } => {
                let e = high(&mut g, en.sig, en.polarity);
                (0..ff.q.len())
                    .map(|i| {
                        let (e, d) = set_clr(&mut g, sr, i, e, d[i]);
                        (ff.q[i], e, d)
                    })
                    .collect()
            }
            FfKind::Sr(sr) => (0..ff.q.len())
                .map(|i| {
                    let (e, d) = set_clr(&mut g, sr, i, SigBit::ZERO, SigBit::ZERO);
                    (ff.q[i], e, d)
                })
                .collect(),
            _ => {
                g.m.cells.push(c);
                continue;
            }
        };
        for (i, (q, en, d)) in bits.into_iter().enumerate() {
            c.kind = CellKind::Ff(Ff::<Word> {
                q: vec![q],
                kind: FfKind::Dlatch {
                    en: Ctrl {
                        sig: en,
                        polarity: Polarity::Pos,
                    },
                    d: vec![d],
                    reset: LatchReset::None,
                },
            });
            let mut bit = c.clone();
            bit.name = format!("{}$bit{i}", c.name);
            g.m.cells.push(bit);
        }
    }
}

/// A tri-stated pin reads back what it drives while enabled and the
/// outside world otherwise. Each one becomes that, on both sides alike:
/// an input `__ext_<pin>` for the outside, the pin an output of
/// `EN ? A : ext`, and logic reading the pin reads that. (As opaque cells,
/// tri-states were only merged once their inputs were proven equal, which
/// a design whose pin reads feed back into its enables never got.)
fn cut_pins(m: &mut Module) {
    use chipsmith_netlist::Dir;
    let mut pads: HashMap<Net, (usize, usize)> = HashMap::new();
    // A pad is named by its inout (or tri-state output) port, though other
    // output ports may read it too.
    for dir in [Dir::InOut, Dir::Out] {
        for (wi, w) in m.wires.iter().enumerate() {
            if w.port == Some(dir) {
                for (i, b) in w.bits.iter().enumerate() {
                    if let SigBit::Net(n) = b {
                        pads.entry(*n).or_insert((wi, i));
                    }
                }
            }
        }
    }
    // (pad, data, enable): the RTL's tri-states.
    let mut drivers: Vec<(Net, SigBit, SigBit)> = Vec::new();
    m.cells.retain(|c| match &c.kind {
        CellKind::Tribuf(t)
            if t.y
                .iter()
                .all(|y| y.net().is_some_and(|n| pads.contains_key(&n))) =>
        {
            for (a, y) in t.a.iter().zip(&t.y) {
                drivers.push((y.net().expect("checked"), *a, t.en));
            }
            false
        }
        _ => true,
    });
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    let mut ext: BTreeMap<usize, Vec<SigBit>> = BTreeMap::new();
    // Every inout gets its outside input, cut or not: one side may have
    // folded a buffer that is always on.
    for (wi, w) in m.wires.iter().enumerate() {
        if w.port == Some(Dir::InOut) {
            ext.insert(wi, Vec::new());
        }
    }
    let widths: Vec<usize> = m.wires.iter().map(|w| w.bits.len()).collect();
    for (wi, bits) in &mut ext {
        *bits = (0..widths[*wi]).map(|_| SigBit::Net(m.new_net())).collect();
        // Never driven (`assign p = 1'bz`): the pin is the outside.
        for (i, b) in m.wires[*wi].bits.iter_mut().enumerate() {
            if matches!(b, SigBit::Const(State::Z | State::X)) {
                *b = bits[i];
            }
        }
    }
    // The pin buffers keep their registers: they take the outside as EXT.
    for c in &mut m.cells {
        let CellKind::Other(g) = &mut c.kind else {
            continue;
        };
        if g.ty != cells::IOBUF {
            continue;
        }
        let Some(pad) = g.port("PAD").and_then(|p| p.sig[0].net()) else {
            continue;
        };
        let outside = pads
            .get(&pad)
            .and_then(|&(wi, i)| ext.get(&wi).map(|bits| bits[i]))
            .unwrap_or(SigBit::ZERO);
        for p in &mut g.ports {
            if p.name == "PAD" {
                p.dir = Some(Dir::Out);
            }
        }
        g.ports.push(chipsmith_netlist::GenericPort {
            name: "EXT".into(),
            dir: Some(Dir::In),
            sig: vec![outside],
        });
    }
    for (pad, a, en) in drivers {
        let (wi, i) = pads[&pad];
        // A tri-state output reads nothing back: released, it is 0.
        let outside = ext.get(&wi).map_or(SigBit::ZERO, |bits| bits[i]);
        let r = SigBit::Net(m.new_net());
        m.add_cell(
            format!("$pin_cut${}${i}", m.wires[wi].name),
            CellKind::Gate(Gate::new(GateOp::Mux, &[outside, a, en], r).unwrap()),
        );
        subst.set(pad, r);
    }
    for (wi, bits) in ext {
        m.wires[wi].port = Some(Dir::Out);
        let mut w = Wire::new(format!("__ext_{}", m.wires[wi].name), bits);
        w.port = Some(Dir::In);
        m.wires.push(w);
    }
    subst.apply(m);
}

/// Power-up values left on wires no register drives any more (a read
/// register `memory_dff` merged into its memory): meaningless, but
/// `async2sync` stops on them when the other side's wire has none.
fn drop_stray_inits(m: &mut Module) {
    let mut q = std::collections::HashSet::new();
    for c in &m.cells {
        match &c.kind {
            CellKind::Ff(ff) => q.extend(ff.q.iter().filter_map(|b| b.net())),
            CellKind::FfGate(ff) => q.extend(ff.q.net()),
            _ => {}
        }
    }
    let mut init = chipsmith_synth::rewrite::inits(m);
    let before = init.len();
    init.retain(|n, _| q.contains(n));
    if init.len() != before {
        chipsmith_synth::rewrite::set_inits(m, &init);
    }
}

/// Proves synthesis correct in two steps. `refined` is the RTL with every
/// x resolved (synthesis' first stage), `gate` its final netlist before
/// wire names are purged. First the RTL against `refined`, x in the RTL
/// matching anything. Then `refined` against `gate`: the registers `gate`
/// has as constants are proven constant on `refined` (exactly: it has no
/// x), replaced by those constants, and the rest compared.
pub fn check(dir: &Path, rtl: &Path, refined: &Design, gate: &Design) -> Result<Proof, String> {
    let gate_mod = top(gate);
    let to_logic = logic_memories(gate_mod);
    let refined_path = dir.join("refined.json");
    write(refined, &refined_path);
    let gate_path = dir.join("gate.json");
    write(gate, &gate_path);

    // Memories in blocks, and memories an earlier stage still has as
    // memories, are compared at their ports (cut out on both sides): the
    // blocks' contents and configuration are checked against the yosys
    // flow's cells instead.
    let mut sync_cut = block_memories(gate_mod);
    let mut cut = sync_cut.clone();
    for c in &gate_mod.cells {
        if let CellKind::Mem(mem) = &c.kind {
            let id = mem.memid.trim_start_matches('\\').to_owned();
            if mem.rd.iter().all(|r| r.sync.is_some()) {
                sync_cut.push(id.clone());
            }
            cut.push(id);
        }
    }
    // Synthesis merged the read registers of the memories it put in
    // blocks into them; yosys' `memory_dff` does the same on both sides.
    let mut gold = prep_memories(dir, "gold_prep", rtl, &to_logic, &sync_cut)?;
    let mut mid = prep_memories(dir, "mid_prep", &refined_path, &to_logic, &sync_cut)?;
    for m in gold.modules.iter_mut().chain(&mut mid.modules) {
        latch_resets_to_logic(m);
        drop_stray_inits(m);
        chipsmith_synth::tribuf::tribuf(m);
        cut_pins(m);
    }
    let mut cut_gate = gate.clone();
    for m in cut_gate.modules.iter_mut().filter(|m| !m.is_blackbox()) {
        cut_loops(m);
        cut_pins(m);
    }
    if !cut.is_empty() {
        cut_memories(top_mut(&mut gold))?;
        let shapes = cut_memories(top_mut(&mut mid))?;
        let gt = cut_gate
            .modules
            .iter_mut()
            .find(|m| !m.is_blackbox())
            .expect("top");
        cut_memories(gt)?;
        cut_blocks(gt, &shapes)?;
    }
    let gold_path = dir.join("gold.json");
    write(&gold, &gold_path);
    let mid_path = dir.join("mid.json");
    write(&mid, &mid_path);
    let first = equiv(dir, "equiv_refine", &gold_path, &mid_path, top(&gold))?;

    let gm = mid
        .modules
        .iter_mut()
        .find(|m| m.name == "top")
        .expect("top");
    // Claims: a gate wire bit that is constant where `refined` has a
    // register.
    let mut ff_q: HashMap<Net, usize> = HashMap::new();
    for (i, c) in gm.cells.iter().enumerate() {
        if let CellKind::Ff(ff) = &c.kind {
            for b in &ff.q {
                if let SigBit::Net(n) = b {
                    ff_q.insert(*n, i);
                }
            }
        }
    }
    let gold_wires: HashMap<&str, &Wire> = gm.wires.iter().map(|w| (w.name.as_str(), w)).collect();
    let mut claims: BTreeMap<Net, bool> = BTreeMap::new();
    // Registers synthesis merged into one: they must be equal too, for the
    // constants that rely on it to be provable by induction.
    let mut merged: BTreeMap<Net, Vec<Net>> = BTreeMap::new();
    for w in &gate_mod.wires {
        let Some(gw) = gold_wires.get(w.name.as_str()) else {
            continue;
        };
        for (b, gb) in w.bits.iter().zip(&gw.bits) {
            match (b, gb) {
                (SigBit::Const(s @ (State::Zero | State::One)), SigBit::Net(n))
                    if ff_q.contains_key(n) =>
                {
                    claims.insert(*n, *s == State::One);
                }
                (SigBit::Net(g), SigBit::Net(n)) if ff_q.contains_key(n) => {
                    let v = merged.entry(*g).or_default();
                    if !v.contains(n) {
                        v.push(*n);
                    }
                }
                _ => {}
            }
        }
    }
    let equal: Vec<(Net, Net)> = merged
        .values()
        .flat_map(|v| v.iter().skip(1).map(move |n| (v[0], *n)))
        .collect();
    // Registers synthesis treats as state registers: one of their values
    // at all times.
    let states = chipsmith_synth::fsm::state_registers(gm);
    if !claims.is_empty() || !states.is_empty() {
        // One output bit per claim, each "this register holds its value".
        let mut with_claims = gm.clone();
        let mut holds = Vec::new();
        let mut names = String::new();
        let net_names: HashMap<Net, String> = gm
            .wires
            .iter()
            .flat_map(|w| {
                w.bits
                    .iter()
                    .enumerate()
                    .filter_map(move |(i, b)| b.net().map(|n| (n, format!("{}[{i}]", w.name))))
            })
            .collect();
        for (k, (n, v)) in claims.iter().enumerate() {
            let b = SigBit::Net(*n);
            names += &format!(
                "{k}: {} = {}\n",
                net_names.get(n).map_or("?", |s| s),
                u8::from(*v)
            );
            holds.push(if *v {
                b
            } else {
                let y = SigBit::Net(with_claims.new_net());
                with_claims.add_cell(
                    format!("$claim${k}"),
                    CellKind::Gate(Gate::new(GateOp::Not, &[b], y).unwrap()),
                );
                y
            });
        }
        for (k, (a, b)) in equal.iter().enumerate() {
            let y = SigBit::Net(with_claims.new_net());
            with_claims.add_cell(
                format!("$claim$eq${k}"),
                CellKind::Gate(
                    Gate::new(GateOp::Xnor, &[SigBit::Net(*a), SigBit::Net(*b)], y).unwrap(),
                ),
            );
            names += &format!(
                "{}: {} == {}\n",
                holds.len(),
                net_names.get(a).map_or("?", |s| s),
                net_names.get(b).map_or("?", |s| s)
            );
            holds.push(y);
        }
        for (k, r) in states.iter().enumerate() {
            names += &format!(
                "{}: {} in {} values\n",
                holds.len(),
                r.q.first()
                    .and_then(|b| b.net())
                    .and_then(|n| net_names.get(&n))
                    .map_or("?", |s| s),
                r.values.len()
            );
            holds.push(in_set(
                &mut with_claims,
                &r.q,
                &r.values,
                &format!("state{k}"),
            ));
        }
        std::fs::write(dir.join("claims.txt"), names).unwrap();
        let all = "1".repeat(holds.len());
        let mut port = Wire::new("__claims", holds);
        port.port = Some(chipsmith_netlist::Dir::Out);
        with_claims.wires.push(port);
        let claims_path = dir.join("gold_claims.json");
        write(
            &Design {
                modules: vec![with_claims],
            },
            &claims_path,
        );
        let log = yosys(
            dir,
            "claims",
            &format!(
                "read_json {}\nhierarchy -top top\nproc\nflatten\nasync2sync\nchtype -set chipsmith_mem t:$mem_v2\n\
                 sat -tempinduct -prove __claims {}'b{all} -maxsteps 8 -show __claims -ignore_unknown_cells\n",
                claims_path.display(),
                all.len()
            ),
            dir,
        )?;
        if !log.contains("Induction step proven: SUCCESS!") {
            return Err(format!(
                "the {} registers synthesis made constant aren't provably constant on the RTL (see {})",
                claims.len(),
                dir.join("claims.log").display()
            ));
        }
        let consts: HashMap<Net, bool> = claims.iter().map(|(n, v)| (*n, *v)).collect();
        chipsmith_synth::opt::seqconst::apply(gm, &consts);
        // The state registers' invariants, proven, as assumptions for the
        // comparison: synthesis compares states by the bits that tell them
        // apart, which only agrees with the RTL in states the RTL reaches.
        for (k, r) in states.iter().enumerate() {
            let q: Vec<SigBit> = r
                .q
                .iter()
                .map(|b| match b {
                    SigBit::Net(n) => consts.get(n).map_or(*b, |v| SigBit::Const(State::from(*v))),
                    c => *c,
                })
                .collect();
            let a = in_set(gm, &q, &r.values, &format!("assume{k}"));
            gm.add_cell(
                format!("$state_assume${k}"),
                CellKind::Other(chipsmith_netlist::Generic {
                    ty: "$assume".into(),
                    params: Default::default(),
                    ports: vec![
                        chipsmith_netlist::GenericPort {
                            name: "A".into(),
                            dir: Some(chipsmith_netlist::Dir::In),
                            sig: vec![a],
                        },
                        chipsmith_netlist::GenericPort {
                            name: "EN".into(),
                            dir: Some(chipsmith_netlist::Dir::In),
                            sig: vec![SigBit::ONE],
                        },
                    ],
                }),
            );
        }
    }
    let cut_gate_path = dir.join("gate_cut.json");
    write(&cut_gate, &cut_gate_path);
    let sub_path = dir.join("mid_constants.json");
    write(&mid, &sub_path);
    let second = equiv(dir, "equiv_synth", &sub_path, &cut_gate_path, top(&mid))?;
    Ok(Proof {
        constant_registers: claims.len(),
        equivalences: first + second,
    })
}
