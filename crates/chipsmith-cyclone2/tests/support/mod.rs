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
  // A multiplexer tree rather than an index: an input the LUT doesn't
  // depend on can be x without making the output x, as in the hardware
  // (co-simulation would otherwise see x where the RTL has a value).
  wire [7:0] d8 = DATAD ? LUT_MASK[15:8] : LUT_MASK[7:0];
  wire [3:0] c4 = c ? d8[7:4] : d8[3:0];
  wire [1:0] b2 = DATAB ? c4[3:2] : c4[1:0];
  assign COMBOUT = DATAA ? b2[1] : b2[0];
  wire [3:0] k4 = CIN ? LUT_MASK[7:4] : LUT_MASK[3:0];
  wire [1:0] k2 = DATAB ? k4[3:2] : k4[1:0];
  assign COUT = DATAA ? k2[1] : k2[0];
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
// The embedded multiplier (cells.v): the product of the low A_WIDTH and
// B_WIDTH bits, each two's complement when its *_SIGNED is set, with the
// operand and product registers *_REG ask for (rising edge of CLK, enable
// active high, clear asynchronous to 0). Each signedness case is written
// so yosys makes one `$mul` of the operands' own widths and signs (as the
// RTL's is), which the proof then matches structurally rather than by
// SAT.
module CY2_MULT (input [17:0] A, input [17:0] B, input CLK, input ENA, input ACLR,
                 output [35:0] P);
  parameter A_WIDTH = 18;
  parameter B_WIDTH = 18;
  parameter A_SIGNED = 0;
  parameter B_SIGNED = 0;
  parameter A_REG = 0;
  parameter B_REG = 0;
  parameter P_REG = 0;
  reg [A_WIDTH-1:0] qa = 0;
  reg [B_WIDTH-1:0] qb = 0;
  reg [A_WIDTH+B_WIDTH-1:0] qp = 0;
  always @(posedge CLK, posedge ACLR)
    if (ACLR) begin qa <= 0; qb <= 0; qp <= 0; end
    else if (ENA) begin qa <= A[A_WIDTH-1:0]; qb <= B[B_WIDTH-1:0]; qp <= p; end
  // Generate, not `?:`: a multiplexer with a constant select in front of
  // the `$mul` stops equiv_struct matching it and sends the proof to SAT.
  wire [A_WIDTH-1:0] a;
  wire [B_WIDTH-1:0] b;
  wire [A_WIDTH+B_WIDTH-1:0] p;
  generate
    if (A_REG) assign a = qa; else assign a = A[A_WIDTH-1:0];
    if (B_REG) assign b = qb; else assign b = B[B_WIDTH-1:0];
  endgenerate
  generate
    if (A_SIGNED && B_SIGNED) assign p = $signed(a) * $signed(b);
    else if (A_SIGNED) assign p = $signed(a) * $signed({1'b0, b});
    else if (B_SIGNED) assign p = $signed({1'b0, a}) * $signed(b);
    else assign p = a * b;
  endgenerate
  wire [A_WIDTH+B_WIDTH-1:0] out;
  generate
    if (P_REG) assign out = qp; else assign out = p;
  endgenerate
  assign P = {{(36-A_WIDTH-B_WIDTH){1'bx}}, out};
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

pub fn top(d: &Design) -> &Module {
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
        .filter_map(|c| {
            let mut parts = c.name.rsplitn(3, '.');
            parts.nth(2).map(str::to_owned)
        })
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
fn cut_memories(m: &mut Module, canonical: bool) -> Result<BTreeMap<String, MemShape>, String> {
    use chipsmith_netlist::Dir;
    let mut shapes = BTreeMap::new();
    let mut ports = Vec::new();
    let mut masked = Vec::new();
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    // Ports in an order both sides agree on: yosys' and chipsmith's front
    // ends number a memory's ports differently. A port by the names on its
    // address, then by the address's constant bits (the words of a wide
    // port), a read port last by the names of what its data reach (a wire
    // on them, or on the register they go into).
    let mut names: HashMap<Net, Vec<String>> = HashMap::new();
    for w in &m.wires {
        if w.name.starts_with('$') {
            continue;
        }
        for b in &w.bits {
            if let SigBit::Net(n) = b {
                names.entry(*n).or_default().push(w.name.clone());
            }
        }
    }
    let mut ff_q: HashMap<Net, Net> = HashMap::new();
    for c in &m.cells {
        if let CellKind::Ff(ff) = &c.kind {
            if let chipsmith_netlist::FfKind::Dff { d, .. } = &ff.kind {
                for (dq, q) in d.iter().zip(&ff.q) {
                    if let (SigBit::Net(dn), SigBit::Net(qn)) = (dq, q) {
                        ff_q.insert(*dn, *qn);
                    }
                }
            }
        }
    }
    let label = |bits: &[SigBit]| -> String {
        let mut v: Vec<String> = Vec::new();
        for b in bits {
            match b {
                SigBit::Net(n) => {
                    let ns = names
                        .get(n)
                        .or_else(|| ff_q.get(n).and_then(|q| names.get(q)));
                    v.extend(ns.into_iter().flatten().cloned());
                }
                SigBit::Const(c) => v.push(format!("={}", c.char())),
            }
        }
        v.sort();
        v.dedup();
        v.join(",")
    };
    let mut keep = Vec::new();
    for c in std::mem::take(&mut m.cells) {
        let CellKind::Mem(mem) = &c.kind else {
            keep.push(c);
            continue;
        };
        let id = mem.memid.trim_start_matches('\\').to_owned();
        let abits = (usize::BITS - mem.size.saturating_sub(1).leading_zeros()) as usize;
        let mut rds: Vec<&chipsmith_netlist::MemRd> = mem.rd.iter().collect();
        // Two stages of chipsmith's keep their order; only the RTL's and
        // chipsmith's need lining up.
        rds.sort_by_cached_key(|r| {
            if !canonical {
                return (String::new(), String::new(), String::new());
            }
            let consts: String = r
                .addr
                .iter()
                .map(|b| match b {
                    SigBit::Const(c) => c.char(),
                    _ => 'n',
                })
                .collect();
            (label(&r.addr), consts, label(&r.data))
        });
        let mut wrs: Vec<&chipsmith_netlist::MemWr> = mem.wr.iter().collect();
        wrs.sort_by_cached_key(|w| {
            if !canonical {
                return (String::new(), String::new());
            }
            let consts: String = w
                .addr
                .iter()
                .map(|b| match b {
                    SigBit::Const(c) => c.char(),
                    _ => 'n',
                })
                .collect();
            (label(&w.addr), consts)
        });
        for (i, rd) in rds.into_iter().enumerate() {
            let rdata = (0..rd.data.len())
                .map(|_| SigBit::Net(m.new_net()))
                .collect::<Vec<_>>();
            for (q, n) in rd.data.iter().zip(&rdata) {
                if let (SigBit::Net(q), n) = (q, n) {
                    subst.set(*q, *n);
                }
            }
            ports.push((format!("__mem_{id}_rdata{i}"), Dir::In, rdata));
            ports.push((
                format!("__mem_{id}_raddr{i}"),
                Dir::Out,
                rd.addr[..abits.min(rd.addr.len())].to_vec(),
            ));
        }
        for (j, w) in wrs.into_iter().enumerate() {
            // What is written only matters where it is written: masked by
            // the enables, so x in what isn't written can't differ.
            let addr = w.addr[..abits.min(w.addr.len())].to_vec();
            let any: Vec<SigBit> = {
                let mut acc = SigBit::ZERO;
                for (k, &e) in w.en.iter().enumerate() {
                    let y = SigBit::Net(m.new_net());
                    m.add_cell(
                        format!("$cut${id}$any{j}${k}"),
                        CellKind::Gate(Gate::new(GateOp::Or, &[acc, e], y).unwrap()),
                    );
                    acc = y;
                }
                vec![acc; addr.len()]
            };
            masked.push((format!("__mem_{id}_waddr{j}"), addr, any));
            masked.push((format!("__mem_{id}_wdata{j}"), w.data.clone(), w.en.clone()));
            ports.push((format!("__mem_{id}_we{j}"), Dir::Out, w.en.clone()));
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

/// Drops the names of nets nothing drives any more (a register synthesis
/// or a claim removed): yosys would read them as x and clash with the
/// power-up value the name still carries.
fn drop_undriven(m: &mut Module) {
    let mut driven = std::collections::HashSet::new();
    for c in &m.cells {
        c.kind.for_each_port(|p| {
            if p.dir != Some(chipsmith_netlist::Dir::In) {
                driven.extend(p.bits.iter().filter_map(|b| b.net()));
            }
        });
    }
    m.wires.retain(|w| {
        w.port.is_some()
            || w.bits
                .iter()
                .any(|b| b.net().is_none_or(|n| driven.contains(&n)))
    });
    // An undriven bit of a wire that stays has no power-up value either.
    let inputs: std::collections::HashSet<_> = m
        .wires
        .iter()
        .filter(|w| w.port.is_some())
        .flat_map(|w| w.bits.iter().filter_map(|b| b.net()))
        .collect();
    for w in &mut m.wires {
        let Some(chipsmith_netlist::ParamValue::Bits(c)) =
            w.attributes.get_mut(chipsmith_netlist::attr::INIT)
        else {
            continue;
        };
        for (b, s) in w.bits.iter().zip(c.0.iter_mut()) {
            if b.net()
                .is_some_and(|n| !driven.contains(&n) && !inputs.contains(&n))
            {
                *s = State::X;
            }
        }
        if c.0.iter().all(|s| *s == State::X) {
            w.attributes.remove(chipsmith_netlist::attr::INIT);
        }
    }
}

/// Drops power-up values on wire bits that are constants: yosys'
/// `async2sync` calls them a conflict.
fn drop_constant_inits(m: &mut Module) {
    for w in &mut m.wires {
        let Some(chipsmith_netlist::ParamValue::Bits(c)) =
            w.attributes.get_mut(chipsmith_netlist::attr::INIT)
        else {
            continue;
        };
        for (b, s) in w.bits.iter().zip(c.0.iter_mut()) {
            if matches!(b, SigBit::Const(_)) {
                *s = State::X;
            }
        }
        if c.0.iter().all(|s| *s == State::X) {
            w.attributes.remove(chipsmith_netlist::attr::INIT);
        }
    }
}

/// A memory as synthesis saw it, for the blocks' model.
#[derive(Debug, Clone)]
pub struct MemInfo {
    width: usize,
    size: usize,
    init: chipsmith_netlist::Const,
}

/// The memories of `d` by name.
fn mem_infos(d: &Design) -> BTreeMap<String, MemInfo> {
    let mut out = BTreeMap::new();
    for c in &top(d).cells {
        if let CellKind::Mem(mem) = &c.kind {
            out.insert(
                mem.memid.trim_start_matches('\\').to_owned(),
                MemInfo {
                    width: mem.width,
                    size: mem.size,
                    init: mem.init.clone(),
                },
            );
        }
    }
    out
}

/// Replaces every CY2_M4K by a model of the block (as `cells.v` and the
/// hardware have it) whose contents are registers named after the words
/// of the memory it holds part of, `memid[word]`, as yosys' `memory_map`
/// names the RTL's: the comparison then proves the words equal at every
/// clock edge, and the read logic around the blocks against the RTL's
/// reads. Also checks the blocks' INIT against the memory's contents.
///
/// Which memory bit a block bit is follows synthesis' layout (block
/// `memid.<row>.<col>`, the shape's base words and slice, a shallow
/// memory's repeated top address bit); a wrong guess here can only make
/// the proof fail.
fn model_blocks(m: &mut Module, mems: &BTreeMap<String, MemInfo>) -> Result<(), String> {
    use chipsmith_cyclone2::m4k::{depth, Ports, Sel, Shape, Side, Width};
    use chipsmith_netlist::{Bmux, Const, Ctrl, Demux, DffControl, Ff, FfKind, ParamValue};
    use chipsmith_synth::rewrite::Gates;

    let blocks: Vec<chipsmith_netlist::Cell> = {
        let (b, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut m.cells)
            .into_iter()
            .partition(|c| c.kind.type_name() == "CY2_M4K");
        m.cells = keep;
        b
    };
    // memid -> word -> bit -> content net.
    let mut words: BTreeMap<String, BTreeMap<usize, BTreeMap<usize, SigBit>>> = BTreeMap::new();
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    let mut inits: Vec<(SigBit, State)> = Vec::new();
    let mut g = Gates::new(m, "m4k_model");
    for c in blocks {
        let CellKind::Other(cell) = &c.kind else {
            unreachable!()
        };
        let mut parts = c.name.rsplitn(3, '.');
        let (col, row, id) = (parts.next(), parts.next(), parts.next());
        let (Some(id), Some(row), Some(col)) = (id, row, col) else {
            return Err(format!("{}: not memid.row.col", c.name));
        };
        let (row, col): (usize, usize) = (
            row.parse().map_err(|_| format!("{}: row", c.name))?,
            col.parse().map_err(|_| format!("{}: col", c.name))?,
        );
        let info = mems
            .get(id)
            .ok_or_else(|| format!("{}: no memory {id}", c.name))?;
        let shape = Shape::parse(
            |k| cell.params.get(k).and_then(|v| v.str()).map(str::to_owned),
            |k| {
                cell.params
                    .get(k)
                    .and_then(|v| v.bits())
                    .and_then(|b| b.as_u64())
            },
            |_| unreachable!("synthesis writes SLICE"),
        )
        .map_err(|e| format!("{}: {e:#}", c.name))?;
        let sig = |name: &str| cell.port(name).map(|p| p.sig.clone());
        let bit = |name: &str, dflt: SigBit| sig(name).map_or(dflt, |s| s[0]);
        let init = match cell.params.get("INIT") {
            Some(ParamValue::Bits(c)) => c.clone(),
            _ => return Err(format!("{}: no INIT", c.name)),
        };

        // Linear position -> memory (word, bit), and the contents check.
        let (w, size) = (info.width, info.size);
        let abits = (usize::BITS - (size - 1).leading_zeros()) as usize;
        let base = shape.base();
        let block_bits = depth(base).trailing_zeros() as usize;
        let mut held: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
        for bw in 0..depth(base) {
            let word = if abits >= block_bits {
                row * depth(base) + bw
            } else {
                let top = bw >> (abits - 1) & 1;
                let all = (1usize << (block_bits - abits)) - 1;
                if bw >> abits != if top == 1 { all } else { 0 } {
                    continue;
                }
                bw & ((1 << abits) - 1)
            };
            for b in 0..shape.slice() as usize {
                let k = col * shape.slice() as usize + b;
                if word < size && k < w {
                    held.insert(shape.linear(base, bw, b), (word, k));
                }
            }
        }
        let rom = [(Side::A, "A_WE"), (Side::B, "B_WE")]
            .iter()
            .all(|&(side, we)| !shape.writable(side) || bit(we, SigBit::ZERO) == SigBit::ZERO);
        let mut content: BTreeMap<usize, SigBit> = BTreeMap::new();
        for (&l, &(word, k)) in &held {
            let want = info.init.0[word * w + k];
            let have = if init.0[l] == State::One {
                State::One
            } else {
                State::Zero
            };
            if matches!(want, State::Zero | State::One) && want != have {
                return Err(format!(
                    "{}: INIT bit {l} is {have:?}, {id}[{word}][{k}] is {want:?}",
                    c.name
                ));
            }
            // A block that never writes holds constants.
            let q = if rom {
                SigBit::Const(have)
            } else {
                let q = SigBit::Net(g.m.new_net());
                inits.push((q, have));
                q
            };
            content.insert(l, q);
            words
                .entry(id.to_owned())
                .or_default()
                .entry(word)
                .or_default()
                .insert(k, q);
        }

        // The block's ports.
        let clocks = [bit("CLK", SigBit::X), bit("CLK1", SigBit::X)];
        let enas = [bit("ENA0", SigBit::ONE), bit("ENA1", SigBit::ONE)];
        let clrs = [bit("CLR0", SigBit::ZERO), bit("CLR1", SigBit::ZERO)];
        let sel = |s: Sel| s as usize;
        let tdp = matches!(shape.ports(), Ports::TrueDual { .. });
        struct PortSigs {
            width: Width,
            clock: usize,
            addr: Vec<SigBit>,
            we: SigBit,
            din: Vec<SigBit>,
            be: Vec<SigBit>,
        }
        let port = |side: Side| -> Option<PortSigs> {
            let width = shape.width(side)?;
            let n = width.addr_bits() as usize;
            let (p, a_or_b) = match side {
                Side::A => ("A", 36),
                Side::B => ("B", 18),
            };
            let writes = shape.writable(side);
            Some(PortSigs {
                width,
                clock: sel(shape.input_clock(side)),
                addr: sig(&format!("{p}_ADDR")).unwrap_or(vec![SigBit::ZERO; 12])[12 - n..]
                    .to_vec(),
                we: if writes {
                    bit(&format!("{p}_WE"), SigBit::ZERO)
                } else {
                    SigBit::ZERO
                },
                din: sig(&format!("{p}_DATAIN")).unwrap_or(vec![SigBit::ZERO; a_or_b]),
                be: sig(&format!("{p}_BYTEENA"))
                    .unwrap_or(vec![SigBit::ONE; if side == Side::A { 4 } else { 2 }]),
            })
        };
        let ports: Vec<(Side, PortSigs)> = [Side::A, Side::B]
            .into_iter()
            .filter_map(|s| port(s).map(|p| (s, p)))
            .collect();
        // Where each linear position is in a port's words.
        let place = |width: Width| -> HashMap<usize, (usize, usize)> {
            let mut v = HashMap::new();
            for word in 0..depth(width) {
                for col in 0..width.bits() {
                    v.insert(width.linear(word, col), (word, col));
                }
            }
            v
        };
        // Writes: per port, per word, a strobe.
        let mut strobes = Vec::new();
        for (_, p) in &ports {
            if p.we == SigBit::ZERO {
                strobes.push(None);
                continue;
            }
            let go = g.and(p.we, enas[p.clock]);
            let y = g.m.new_sig(depth(p.width));
            g.add(CellKind::Demux(Demux {
                a: vec![go],
                s: p.addr.clone(),
                y: y.clone(),
            }));
            strobes.push(Some(y));
        }
        let places: Vec<_> = ports.iter().map(|(_, p)| place(p.width)).collect();
        // The contents change on the writing ports' clock (a block on two
        // clocks writes on one port only).
        let write_clock = ports
            .iter()
            .find(|(_, p)| p.we != SigBit::ZERO)
            .map_or(0, |(_, p)| p.clock);
        for (&l, &q) in content.iter().filter(|_| !rom) {
            let mut d = q;
            let mut hits = Vec::new();
            for (i, (_, p)) in ports.iter().enumerate() {
                let Some(st) = &strobes[i] else { continue };
                let Some(&(word, col)) = places[i].get(&l) else {
                    continue;
                };
                let hit = g.and(st[word], p.be[col / 9]);
                d = g.mux(d, p.din[col], hit);
                hits.push(hit);
            }
            if let [a, b] = hits[..] {
                let both = g.and(a, b);
                d = g.mux(d, SigBit::X, both);
            }
            g.add(CellKind::Ff(Ff {
                q: vec![q],
                kind: FfKind::Dff {
                    clk: Ctrl::pos(clocks[write_clock]),
                    d: vec![d],
                    control: DffControl::Plain { en: None },
                },
            }));
        }
        // Reads: the word at the registered address, the new data of a
        // write through the same port (a masked byte: undefined), then the
        // output register.
        let dout = sig("DATAOUT").unwrap_or_default();
        for (side, r) in shape.read_ports() {
            let i = ports
                .iter()
                .position(|(s, _)| *s == side)
                .expect("a read port");
            let p = &ports[i].1;
            let flow = !matches!(shape.ports(), Ports::SimpleDual { .. }) && p.we != SigBit::ZERO;
            let offset = if tdp && side == Side::B { 18 } else { 0 };
            for col in 0..p.width.bits() {
                let a: Vec<SigBit> = (0..depth(p.width))
                    .map(|word| {
                        content
                            .get(&p.width.linear(word, col))
                            .copied()
                            .unwrap_or(SigBit::X)
                    })
                    .collect();
                let y = g.m.new_sig(1);
                g.add(CellKind::Bmux(Bmux {
                    a,
                    s: p.addr.clone(),
                    y: y.clone(),
                }));
                let mut v = y[0];
                if flow {
                    let new = g.mux(SigBit::X, p.din[col], p.be[col / 9]);
                    v = g.mux(v, new, p.we);
                }
                let reg = |g: &mut Gates, clk: usize, d: SigBit, clr: Option<SigBit>| {
                    let q = SigBit::Net(g.m.new_net());
                    let en = Some(Ctrl::pos(enas[clk]));
                    let control = match clr {
                        None => DffControl::Plain { en },
                        Some(c) => DffControl::Async {
                            arst: Ctrl::pos(c),
                            value: Const(vec![State::Zero]),
                            en,
                        },
                    };
                    g.add(CellKind::Ff(Ff {
                        q: vec![q],
                        kind: FfKind::Dff {
                            clk: Ctrl::pos(clocks[clk]),
                            d: vec![d],
                            control,
                        },
                    }));
                    q
                };
                let mut out = reg(&mut g, p.clock, v, None);
                inits.push((out, State::Zero));
                if let Some(o) = r.out_reg {
                    let clr = o.clear.map(|c| clrs[sel(c)]);
                    out = reg(&mut g, sel(o.clock), out, clr);
                    inits.push((out, State::Zero));
                }
                if let Some(SigBit::Net(n)) = dout.get(offset + col) {
                    subst.set(*n, out);
                }
            }
        }
    }
    let m = g.m;
    for (id, ws) in words {
        let info = &mems[&id];
        for (word, bits) in ws {
            if bits.len() != info.width {
                return Err(format!(
                    "{id}[{word}]: {} of {} bits in blocks",
                    bits.len(),
                    info.width
                ));
            }
            m.wires.push(Wire::new(
                format!("{id}[{word}]"),
                bits.into_values().collect(),
            ));
        }
    }
    for (i, (q, v)) in inits.into_iter().enumerate() {
        let mut w = Wire::new(format!("$m4k_model_init${i}"), vec![q]);
        w.attributes.insert(
            chipsmith_netlist::attr::INIT.into(),
            ParamValue::Bits(chipsmith_netlist::Const(vec![v])),
        );
        m.wires.push(w);
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
    words: bool,
) -> Result<usize, String> {
    match equiv_once(dir, name, gold, gate, &[], words) {
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
            equiv_once(
                dir,
                &format!("{name}_unmatched"),
                gold,
                gate,
                &unproven,
                words,
            )
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
    words: bool,
) -> Result<usize, (String, Vec<String>)> {
    let blacklist = dir.join(format!("{name}.blacklist"));
    std::fs::write(&blacklist, unmatched.join("\n")).unwrap();
    let sim = dir.join("cy2_sim.v");
    std::fs::write(&sim, SIM_MODELS).unwrap();
    // Tri-states have no SAT model: as opaque cells, merged when their
    // inputs are proven equal (chipsmith-netlist's round-trip check does
    // the same). A pin buffer always on or always off is no tri-state:
    // its multiplexer with a constant select folds first.
    // `opt_dff -nodffe -nosdff` on the block models (`cy2_sim.v`) only drops
    // a register's constant enable or reset: a model's register with its
    // enable tied on and its clear tied off becomes the plain `$dff` the
    // RTL has, so equiv_struct matches it instead of SAT proving through
    // the multiplier behind it. Not on the design's own registers: it also
    // folds their constant bits, which on serv's RTL leaves a constant
    // with two init values and yosys stops.
    let prep = "hierarchy -top top\nproc -noopt\nflatten\nopt_expr t:$mux\nopt_dff -nodffe -nosdff a:src=*cy2_sim.v*\ntribuf\nchtype -set chipsmith_mem t:$mem_v2\n\
                chtype -set chipsmith_tribuf t:$tribuf t:$_TBUF_\nopt_clean";
    let script = format!(
        "read_json {gold}\nread_verilog -overwrite {sim}\n{prep}\nrename top gold\ndesign -stash gold\n\
         read_json {gate}\nread_verilog -overwrite {sim}\n{prep}\nrename top gate\ndesign -stash gate\n\
         design -copy-from gold -as gold gold\ndesign -copy-from gate -as gate gate\n\
         equiv_make -blacklist {blacklist} gold gate equiv\nhierarchy -top equiv\nasync2sync\n\
         equiv_struct -fwd\n{simple}equiv_induct -undef -set-assumes -ignore-unknown-cells\n\
         equiv_status -assert\n",
        // With memories as words, `equiv_simple` proves a few hundred of
        // tens of thousands of word bits (each needs the step before):
        // induction alone, which proves them all, takes a third of the
        // time (m4k-bytes: 22 min against over an hour).
        simple = if words { "" } else { "equiv_simple -undef -set-assumes\n" },
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
    cut: &[String],
) -> Result<Design, String> {
    // The RTL's memories get their read registers merged as synthesis
    // merges them (through enable and reset multiplexers: `opt_dff` first;
    // `memory_dff` reads a bypass multiplexer as transparency), so a read
    // transparent in one is transparent in the other. `proc` leaves a write
    // port's address behind a multiplexer with an x input and an unnamed
    // wire; folding it (only fixing what the RTL left x, so the proof gets
    // no weaker) puts the address' names on the port, which is what lines
    // the ports up with synthesis' (`cut_memories`).
    let mut map = if name.starts_with("gold_prep") && !cut.is_empty() {
        let mut m = String::from("opt_expr -mux_undef\nopt_clean\nopt_dff\n");
        for id in cut {
            m += &format!("memory_dff m:{id} t:$memrd* r:MEMID=\\{id} %i %u t:*dff* %u\n");
        }
        m + "memory_collect\n"
    } else {
        String::from("memory_collect\n")
    };
    // A memory compared at its ports reads asynchronously on both sides,
    // its read registers outside it (`memory_nordff`), so whatever registers
    // each side merged, the logic with them outside lines up.
    for id in cut {
        map += &format!("memory_nordff t:$mem_v2 r:MEMID=\\{id} %i\n");
    }
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
        // A register yosys made a constant (one nothing reads) keeps its
        // wire's power-up value, which `async2sync` then calls a conflict.
        for w in &mut m.wires {
            let Some(chipsmith_netlist::ParamValue::Bits(c)) =
                w.attributes.get_mut(chipsmith_netlist::attr::INIT)
            else {
                continue;
            };
            for (b, s) in w.bits.iter().zip(c.0.iter_mut()) {
                if matches!(b, SigBit::Const(_)) {
                    *s = State::X;
                }
            }
            if c.0.iter().all(|s| *s == State::X) {
                w.attributes.remove(chipsmith_netlist::attr::INIT);
            }
        }
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
    check_impl(dir, Some(rtl), refined, gate, false)
}

/// Synthesis with blocks (M4Ks or multipliers) checked around them, never
/// through them: the RTL against the stage before the mapping (memories
/// compared at their ports, products as `$mul`), and the stage right
/// after it against the result (the blocks compared at their pins, their
/// parameters equal). The mapping itself is the per-shape proofs'
/// (`m4k_shape_*`, `mult_*`: one block per configuration) and the
/// co-simulation's (several blocks and the logic between).
pub fn check_staged(
    dir: &Path,
    rtl: &Path,
    refined: &Design,
    before: &Design,
    after: &Design,
    gate: &Design,
) -> Result<Proof, String> {
    let pre = dir.join("pre");
    let post = dir.join("post");
    std::fs::create_dir_all(&pre).unwrap();
    std::fs::create_dir_all(&post).unwrap();
    let a = check_impl(&pre, Some(rtl), refined, before, false)?;
    let b = check_impl(&post, None, after, gate, true)?;
    Ok(Proof {
        constant_registers: a.constant_registers + b.constant_registers,
        equivalences: a.equivalences + b.equivalences,
    })
}

/// A stage after the M4K mapping against a later one, the blocks
/// compared at their pins.
pub fn check_after_m4k(dir: &Path, after_m4k: &Design, gate: &Design) -> Result<Proof, String> {
    check_impl(dir, None, after_m4k, gate, true)
}

/// Replaces every CY2_M4K and CY2_MULT by ports: its inputs out, its
/// outputs in (`__blk_<cell>_<port>`), so two netlists with the same
/// blocks are compared around them. Returns each block's parameters.
fn cut_block_cells(
    m: &mut Module,
) -> BTreeMap<String, BTreeMap<String, chipsmith_netlist::ParamValue>> {
    use chipsmith_netlist::Dir;
    let mut params = BTreeMap::new();
    let mut ports = Vec::new();
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    let mut keep = Vec::new();
    for c in std::mem::take(&mut m.cells) {
        let CellKind::Other(g) = &c.kind else {
            keep.push(c);
            continue;
        };
        if g.ty != "CY2_M4K" && g.ty != "CY2_MULT" {
            keep.push(c);
            continue;
        }
        let tag: String = c
            .name
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect();
        for p in &g.ports {
            let name = format!("__blk_{tag}_{}", p.name);
            if p.dir == Some(Dir::Out) {
                let fresh: Vec<SigBit> = p.sig.iter().map(|_| SigBit::Net(m.new_net())).collect();
                for (old, new) in p.sig.iter().zip(&fresh) {
                    if let SigBit::Net(n) = old {
                        subst.set(*n, *new);
                    }
                }
                ports.push((name, Dir::In, fresh));
            } else {
                ports.push((name, Dir::Out, p.sig.clone()));
            }
        }
        params.insert(c.name.clone(), g.params.clone());
    }
    m.cells = keep;
    for (name, dir, bits) in ports {
        add_port(m, name, dir, bits);
    }
    subst.apply(m);
    params
}

fn check_impl(
    dir: &Path,
    rtl: Option<&Path>,
    refined: &Design,
    gate: &Design,
    cut_blocks: bool,
) -> Result<Proof, String> {
    let gate_mod = top(gate);
    let to_logic = logic_memories(gate_mod);
    let refined_path = dir.join("refined.json");
    write(refined, &refined_path);
    let gate_path = dir.join("gate.json");
    write(gate, &gate_path);

    // Step 1, the RTL against `refined`: every memory compared at its
    // ports (cut out on both sides, its read registers outside it), the
    // ones in blocks too. Step 2, `refined` against `gate`: memories still
    // memories in `gate` cut likewise; the ones in blocks either as words
    // on both sides (the blocks through their model, `model_blocks`) or,
    // with `cut_blocks`, the blocks cut at their pins on both sides.
    let in_blocks = block_memories(gate_mod);
    let mut cut = Vec::new();
    for c in &gate_mod.cells {
        if let CellKind::Mem(mem) = &c.kind {
            cut.push(mem.memid.trim_start_matches('\\').to_owned());
        }
    }
    let prep_side = |d: &mut Design| {
        for m in d.modules.iter_mut() {
            latch_resets_to_logic(m);
            drop_stray_inits(m);
            chipsmith_synth::tribuf::tribuf(m);
            cut_pins(m);
        }
    };
    // At their ports the RTL's and synthesis' memories can differ beyond
    // what `memory_nordff` evens out (yosys and chipsmith's front end
    // order a memory's ports differently: a wide read, a true dual port):
    // then the memories as words instead, slower but exact.
    let step1 = |rtl: &Path, words: bool| -> Result<usize, String> {
        let mut all_cut = cut.clone();
        if !cut_blocks {
            all_cut.extend(in_blocks.iter().cloned());
        }
        let mut mapped1 = to_logic.clone();
        if words {
            mapped1.append(&mut all_cut);
        }
        let sfx = if words { "_words" } else { "" };
        let mut gold = prep_memories(dir, &format!("gold_prep{sfx}"), rtl, &mapped1, &all_cut)?;
        let mut mid1 = prep_memories(
            dir,
            &format!("mid_cut_prep{sfx}"),
            &refined_path,
            &mapped1,
            &all_cut,
        )?;
        prep_side(&mut gold);
        prep_side(&mut mid1);
        if cut_blocks {
            cut_block_cells(top_mut(&mut mid1));
        }
        // Cut memories keep their contents: the RTL's, where it gives any,
        // must be synthesis'.
        let ours = mem_infos(&mid1);
        for (id, g) in mem_infos(&gold) {
            let Some(o) = ours.get(&id) else { continue };
            if g.init
                .0
                .iter()
                .zip(&o.init.0)
                .any(|(a, b)| matches!(a, State::Zero | State::One) && a != b)
            {
                return Err(format!("memory {id}: contents differ from the RTL's"));
            }
        }
        if !all_cut.is_empty() {
            cut_memories(top_mut(&mut gold), true)?;
            cut_memories(top_mut(&mut mid1), true)?;
        }
        let gold_path = dir.join(format!("gold{sfx}.json"));
        write(&gold, &gold_path);
        let mid_path = dir.join(format!("mid{sfx}.json"));
        write(&mid1, &mid_path);
        equiv(
            dir,
            &format!("equiv_refine{sfx}"),
            &gold_path,
            &mid_path,
            top(&gold),
            words,
        )
    };
    let first = match rtl {
        Some(rtl) => match step1(rtl, false) {
            Ok(n) => n,
            Err(e)
                if e.contains("not proven equivalent")
                    && !(cut.is_empty() && in_blocks.is_empty()) =>
            {
                eprintln!(
                    "{}: memories at their ports didn't line up; as words",
                    dir.display()
                );
                step1(rtl, true)?
            }
            Err(e) => return Err(e),
        },
        None => 0,
    };

    let mut mapped = to_logic.clone();
    if !cut_blocks {
        mapped.extend(in_blocks.iter().cloned());
    }
    let mut mid = prep_memories(dir, "mid_prep", &refined_path, &mapped, &cut)?;
    prep_side(&mut mid);
    // A stage that still has memories: their read registers outside them
    // too (`memory_nordff`), as on the other side.
    let mut cut_gate = if cut.is_empty() {
        gate.clone()
    } else {
        let lib = dir.join("cells.v");
        std::fs::write(&lib, cells::LIBRARY).unwrap();
        let out = dir.join("gate_nordff.json");
        let nordff: String = cut
            .iter()
            .map(|id| format!("memory_nordff t:$mem_v2 r:MEMID=\\{id} %i\n"))
            .collect();
        yosys(
            dir,
            "gate_nordff",
            &format!(
                "read_verilog -lib {}\nread_json {}\nhierarchy -top top\n{nordff}opt_clean\nwrite_json {}\n",
                lib.display(),
                gate_path.display(),
                out.display()
            ),
            dir,
        )?;
        let mut d = read(&out);
        d.modules.retain(|m| !m.is_blackbox());
        d
    };
    {
        let gt = cut_gate
            .modules
            .iter_mut()
            .find(|m| !m.is_blackbox())
            .expect("top");
        if cut_blocks {
            let theirs = cut_block_cells(gt);
            let ours = cut_block_cells(top_mut(&mut mid));
            if theirs != ours {
                return Err("the blocks' names or parameters differ between the stages".into());
            }
        } else {
            model_blocks(gt, &mem_infos(refined))?;
        }
        if !cut.is_empty() {
            cut_memories(gt, false)?;
        }
        drop_undriven(gt);
        drop_constant_inits(gt);
    }
    for m in cut_gate.modules.iter_mut().filter(|m| !m.is_blackbox()) {
        cut_loops(m);
        cut_pins(m);
    }
    if !cut.is_empty() {
        cut_memories(top_mut(&mut mid), false)?;
    }
    let words = !cut_blocks && !in_blocks.is_empty();

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
    drop_undriven(top_mut(&mut mid));
    let sub_path = dir.join("mid_constants.json");
    write(&mid, &sub_path);
    let second = equiv(
        dir,
        "equiv_synth",
        &sub_path,
        &cut_gate_path,
        top(&mid),
        words,
    )?;
    Ok(Proof {
        constant_registers: claims.len(),
        equivalences: first + second,
    })
}

/// A behavioural model of CY2_M4K for co-simulation (iverilog): the block
/// in physical terms (cells.v, crate::m4k), the same semantics as
/// `model_blocks`, which the proofs use, but as one array, so 10,000 cycles
/// of a design with a dozen blocks take seconds, not hours. An
/// unconnected input reads as cells.v says: enables on, clears off, no
/// write.
pub const COSIM_M4K: &str = r#"
module CY2_M4K (input CLK, input CLK1, input ENA0, input ENA1, input CLR0, input CLR1,
                input [11:0] A_ADDR, input [35:0] A_DATAIN, input A_WE, input [3:0] A_BYTEENA,
                input [11:0] B_ADDR, input [17:0] B_DATAIN, input B_WE, input [1:0] B_BYTEENA,
                output [35:0] DATAOUT);
  parameter MODE = "SINGLE";
  parameter WIDTH = 1;
  parameter B_WIDTH = WIDTH;
  parameter B_CLOCK = 0;
  parameter A_OUTREG = "NONE";
  parameter A_OUTCLR = "NONE";
  parameter B_OUTREG = "NONE";
  parameter B_OUTCLR = "NONE";
  parameter BASE = 1;
  parameter SLICE = 1;
  parameter PACKING = "DENSE";
  parameter [4607:0] INIT = 4608'b0;
  localparam TDP = MODE == "TRUE_DUAL";
  localparam SDP = MODE == "SIMPLE_DUAL";
  localparam PA = WIDTH, PB = SDP || TDP ? B_WIDTH : WIDTH;
  function integer abits(input integer w);
    abits = w == 1 ? 12 : w == 2 ? 11 : w == 4 ? 10 : w == 9 ? 9 : w == 18 ? 8 : 7;
  endfunction
  // Linear position of column c of word a of a port of width p.
  function integer lin(input integer p, input integer a, input integer c);
    integer l;
    begin
      l = a * p + c;
      lin = p < 9 ? l / 8 * 9 + l % 8 : l;
    end
  endfunction
  wire clk0 = CLK;
  wire clk1 = (CLK1 === 1'bz) ? CLK : CLK1;
  wire ena0 = (ENA0 === 1'bz) ? 1'b1 : ENA0;
  wire ena1 = (ENA1 === 1'bz) ? 1'b1 : ENA1;
  wire clr0 = (CLR0 === 1'bz) ? 1'b0 : CLR0;
  wire clr1 = (CLR1 === 1'bz) ? 1'b0 : CLR1;
  wire wea = (A_WE === 1'bz) ? 1'b0 : A_WE;
  wire web = TDP && !(B_WE === 1'bz) ? B_WE : 1'b0;
  wire [3:0] bea = (A_BYTEENA === 4'bz) ? 4'hf : A_BYTEENA;
  wire [1:0] beb = (B_BYTEENA === 2'bz) ? 2'h3 : B_BYTEENA;
  wire bclk = B_CLOCK ? clk1 : clk0;
  wire bena = B_CLOCK ? ena1 : ena0;
  // Continuous, not `always @*`: an address that never changes (tied, or
  // a counter's top bits for the whole run) never wakes an `always @*`, so
  // it would stay x.
  integer c;
  wire [11:0] aa = A_ADDR >> (12 - abits(PA));
  wire [11:0] ab = B_ADDR >> (12 - abits(PB));
  reg mem [0:4607];
  integer i;
  initial for (i = 0; i < 4608; i = i + 1) mem[i] = INIT[i];
  // Port A: writes (but a read-only simple dual port), reads but in
  // simple dual port; each read sees its own port's write (new data, a
  // byte left out undefined) and the other's old data.
  reg [35:0] qa = 0, qb = 0;
  always @(posedge clk0) if (ena0) begin
    if (!SDP)
      for (c = 0; c < PA; c = c + 1)
        qa[c] <= wea ? ((PA < 18 || bea[c / 9]) ? A_DATAIN[c] : 1'bx) : mem[lin(PA, aa, c)];
    if (wea)
      for (c = 0; c < PA; c = c + 1)
        if (PA < 18 || bea[c / 9]) mem[lin(PA, aa, c)] <= A_DATAIN[c];
  end
  always @(posedge bclk) if (bena && (SDP || TDP)) begin
    for (c = 0; c < PB; c = c + 1)
      qb[c] <= web ? ((PB < 18 || beb[c / 9]) ? B_DATAIN[c] : 1'bx) : mem[lin(PB, ab, c)];
    if (web)
      for (c = 0; c < PB; c = c + 1)
        if (PB < 18 || beb[c / 9]) mem[lin(PB, ab, c)] <= B_DATAIN[c];
  end
  // Both ports writing one bit on one edge: undefined.
  always @(posedge clk0) if (TDP && !B_CLOCK && ena0 && wea && web)
    for (c = 0; c < PB; c = c + 1)
      if (lin(PA, aa, 0) <= lin(PB, ab, c) && lin(PB, ab, c) <= lin(PA, aa, PA - 1)) mem[lin(PB, ab, c)] <= 1'bx;
  // Output registers.
  reg [35:0] pa = 0, pb = 0;
  wire oca = A_OUTREG == "CLOCK1" ? clk1 : clk0, ocb = B_OUTREG == "CLOCK1" ? clk1 : clk0;
  wire oea = A_OUTREG == "CLOCK1" ? ena1 : ena0, oeb = B_OUTREG == "CLOCK1" ? ena1 : ena0;
  wire cla = A_OUTCLR == "CLEAR0" ? clr0 : A_OUTCLR == "CLEAR1" ? clr1 : 1'b0;
  wire clb = B_OUTCLR == "CLEAR0" ? clr0 : B_OUTCLR == "CLEAR1" ? clr1 : 1'b0;
  always @(posedge oca or posedge cla) if (cla) pa <= 0; else if (oea) pa <= qa;
  always @(posedge ocb or posedge clb) if (clb) pb <= 0; else if (oeb) pb <= qb;
  wire [35:0] oa = A_OUTREG == "NONE" ? qa : pa;
  wire [35:0] ob = B_OUTREG == "NONE" ? qb : pb;
  assign DATAOUT = TDP ? {ob[17:0], oa[17:0]} : SDP ? ob : oa;
endmodule

// The JTAG block with no cable: TCK and the user-instruction outputs stay
// low, so a USER1 register (the self-tests') only ever loads, on neither
// side; the outputs compared are the design's own.
module CY2_JTAG (input TDOUSER, output TMSUTAP, output TCKUTAP, output TDIUTAP,
                 output SHIFTUSER, output CLKDRUSER, output UPDATEUSER,
                 output RUNIDLEUSER, output USR1USER);
  assign {TMSUTAP, TCKUTAP, TDIUTAP, SHIFTUSER, CLKDRUSER, UPDATEUSER, RUNIDLEUSER, USR1USER} = 0;
endmodule
"#;

/// What a co-simulation found.
#[derive(Debug)]
pub struct Cosim {
    pub cycles: usize,
    /// Output bits compared, summed over the cycles (x in the RTL skipped).
    pub compared: u64,
}

/// Simulates the RTL and the synthesized netlist side by side (iverilog),
/// the blocks as [`COSIM_M4K`], with the same random inputs: every input but the clocks redrawn
/// from `seed` after each falling edge of the first clock, every output
/// compared just after it, `cycles` times. An output bit the RTL leaves
/// x isn't compared. Fails with the seed and the cycle of the first
/// difference.
pub fn cosim(
    dir: &Path,
    rtl: &Path,
    refined: &Design,
    gate: &Design,
    cycles: usize,
    seed: u32,
) -> Result<Cosim, String> {
    use chipsmith_netlist::Dir;
    let _ = refined;
    let gate_json = dir.join("cosim_gate.json");
    write(gate, &gate_json);
    let sim = dir.join("cy2_sim.v");
    std::fs::write(&sim, SIM_MODELS).unwrap();
    let lib = dir.join("cells.v");
    std::fs::write(&lib, cells::LIBRARY).unwrap();
    let m4k_v = dir.join("cosim_m4k.v");
    std::fs::write(&m4k_v, COSIM_M4K).unwrap();
    let gold_v = dir.join("cosim_gold.v");
    let gate_v = dir.join("cosim_gate.v");
    yosys(
        dir,
        "cosim_netlists",
        &format!(
            "read_json {rtl}\nhierarchy -top top\nproc\nmemory_collect\nopt_clean\nrename top gold\n\
             write_verilog -noattr {gold_v}\ndesign -reset\n\
             read_verilog -lib {lib}\nread_json {gate}\nread_verilog -overwrite {sim}\nhierarchy -top top\nproc\nflatten\n\
             opt_clean\nrename top gate\nwrite_verilog -noattr {gate_v}\n",
            lib = lib.display(),
            rtl = rtl.display(),
            gate = gate_json.display(),
            sim = sim.display(),
            gold_v = gold_v.display(),
            gate_v = gate_v.display(),
        ),
        dir,
    )?;
    // The ports, from the RTL.
    let d = read(rtl);
    let t = top(&d);
    let is_clock = |n: &str| n.starts_with("CLOCK_") || n == "clk" || n == "clk2" || n == "rclk";
    let mut clocks = Vec::new();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for (_, w, dir) in t.ports() {
        match dir {
            Dir::In if is_clock(&w.name) => clocks.push(w.name.clone()),
            Dir::In => inputs.push((w.name.clone(), w.bits.len())),
            Dir::Out => outputs.push((w.name.clone(), w.bits.len())),
            Dir::InOut => return Err(format!("co-simulation: inout port {}", w.name)),
        }
    }
    let Some(main) = clocks.first().cloned() else {
        return Err("co-simulation: no clock".into());
    };
    let mut tb = String::from("`timescale 1ns/1ps\nmodule tb;\n");
    // Clock periods: 20 ns, then 37 ns, 26 ns, ... so edges rarely meet.
    for (i, c) in clocks.iter().enumerate() {
        let half = [10.0, 18.5, 13.0, 23.5][i % 4];
        tb += &format!("  reg {c} = 0;\n  always #{half} {c} = ~{c};\n");
    }
    for (n, w) in &inputs {
        tb += &format!("  reg [{}:0] {n};\n", w - 1);
    }
    for (n, w) in &outputs {
        tb += &format!("  wire [{}:0] {n}_g, {n}_t;\n", w - 1);
    }
    let conns = |sfx: &str| -> String {
        clocks
            .iter()
            .map(|c| format!(".{c}({c})"))
            .chain(inputs.iter().map(|(n, _)| format!(".{n}({n})")))
            .chain(outputs.iter().map(|(n, _)| format!(".{n}({n}_{sfx})")))
            .collect::<Vec<_>>()
            .join(", ")
    };
    tb += &format!("  gold g({});\n  gate t({});\n", conns("g"), conns("t"));
    tb += &format!("  integer seed = {seed}, cyc = 0, i;\n  reg [63:0] compared = 0;\n");
    // Each input: for the first 16 cycles an extreme (all ones, all
    // zeros, the top bit alone, the top bit clear; input k uses pattern
    // (cyc + k) % 4 so the pairs vary), so a sign or a carry at the end
    // of a multiplier operand is seen; random after.
    let draw: String = inputs
        .iter()
        .enumerate()
        .map(|(k, (n, w))| {
            let words = w.div_ceil(32);
            let parts: Vec<String> = (0..words).map(|_| "$random(seed)".to_string()).collect();
            let top = w - 1;
            format!(
                "    if (cyc < 16) case ((cyc + {k}) % 4)\n      0: {n} = {{{w}{{1'b1}}}};\n      1: {n} = 0;\n      \
                 2: begin {n} = 0; {n}[{top}] = 1'b1; end\n      3: begin {n} = {{{w}{{1'b1}}}}; {n}[{top}] = 1'b0; end\n    endcase\n    \
                 else {n} = {{{}}};\n",
                parts.join(", ")
            )
        })
        .collect();
    tb += &format!("  initial begin\n{draw}  end\n");
    let mut check = String::new();
    for (n, w) in &outputs {
        check += &format!(
            "    for (i = 0; i < {w}; i = i + 1)\n      if ({n}_g[i] !== 1'bx) begin\n        compared = compared + 1;\n        \
             if ({n}_g[i] !== {n}_t[i]) begin\n          $display(\"MISMATCH seed=%0d cycle=%0d {n}[%0d] rtl=%b netlist=%b\", {seed}, cyc, i, {n}_g[i], {n}_t[i]);\n          $finish;\n        end\n      end\n"
        );
    }
    tb += &format!(
        "  always @(negedge {main}) begin\n    #1;\n{check}    cyc = cyc + 1;\n{draw}    if (cyc == {cycles}) begin\n      $display(\"PASS %0d\", compared);\n      $finish;\n    end\n  end\nendmodule\n"
    );
    let tb_v = dir.join("cosim_tb.v");
    std::fs::write(&tb_v, tb).unwrap();
    let vvp = dir.join("cosim.vvp");
    let out = Command::new("iverilog")
        .args(["-g2012", "-o"])
        .arg(&vvp)
        .args([&tb_v, &gold_v, &gate_v, &m4k_v])
        .output()
        .map_err(|e| format!("iverilog: {e} (run in the devshell)"))?;
    if !out.status.success() {
        return Err(format!(
            "iverilog failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let out = Command::new("vvp")
        .arg("-n")
        .arg(&vvp)
        .output()
        .map_err(|e| format!("vvp: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    std::fs::write(dir.join("cosim.log"), &text).unwrap();
    if let Some(l) = text.lines().find(|l| l.starts_with("MISMATCH")) {
        return Err(l.to_string());
    }
    let compared = text
        .lines()
        .find_map(|l| l.strip_prefix("PASS "))
        .and_then(|n| n.trim().parse().ok())
        .ok_or_else(|| {
            format!(
                "co-simulation didn't finish (see {})",
                dir.join("cosim.log").display()
            )
        })?;
    Ok(Cosim { cycles, compared })
}
