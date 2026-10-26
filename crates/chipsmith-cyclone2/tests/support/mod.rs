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
module CY2_IOBUF (input DATAOUT, input OE, input INCLK, input OUTCLK,
                  output COMBOUT, output REGOUT, inout PAD);
  parameter INPUT_REGISTER = "none";
  parameter OUTPUT_REGISTER = "none";
  parameter OE_REGISTER = "none";
  reg qi = INPUT_REGISTER == "high", qd = OUTPUT_REGISTER == "high", qe = OE_REGISTER == "high";
  always @(posedge INCLK) qi <= PAD;
  always @(posedge OUTCLK) begin qd <= DATAOUT; qe <= OE; end
  wire d = OUTPUT_REGISTER == "none" ? DATAOUT : qd;
  wire e = OE_REGISTER == "none" ? OE : qe;
  assign PAD = e ? d : 1'bz;
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
fn equiv(dir: &Path, name: &str, gold: &Path, gate: &Path) -> Result<usize, String> {
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
         equiv_make gold gate equiv\nhierarchy -top equiv\nasync2sync\n\
         equiv_struct -fwd\nequiv_simple -undef -set-assumes\nequiv_induct -undef -set-assumes -ignore-unknown-cells\n\
         equiv_status -assert\n",
        gold = gold.display(),
        gate = gate.display(),
        sim = sim.display(),
    );
    let log = yosys(dir, name, &script, dir).map_err(|e| {
        let unproven: Vec<&str> = e
            .lines()
            .filter(|l| l.contains("Unproven"))
            .take(10)
            .collect();
        format!(
            "{name}: not proven equivalent:\n{}\n(see {})",
            unproven.join("\n"),
            dir.join(format!("{name}.log")).display()
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

    // Memories in blocks become registers on both sides, the blocks
    // through their model (`model_blocks`), and are compared word by word.
    // Memories an earlier stage still has as memories are compared at
    // their ports (cut out on both sides).
    let in_blocks = block_memories(gate_mod);
    let mut sync_cut = in_blocks.clone();
    let mut cut = Vec::new();
    for c in &gate_mod.cells {
        if let CellKind::Mem(mem) = &c.kind {
            let id = mem.memid.trim_start_matches('\\').to_owned();
            if mem.rd.iter().all(|r| r.sync.is_some()) {
                sync_cut.push(id.clone());
            }
            cut.push(id);
        }
    }
    let mut mapped = to_logic.clone();
    mapped.extend(in_blocks.iter().cloned());
    // Synthesis merged the read registers of the memories it put in
    // blocks into them; yosys' `memory_dff` does the same on both sides.
    // The RTL against the netlist with x resolved: memories are memories
    // on both sides, compared at their ports.
    let mut all_cut = cut.clone();
    all_cut.extend(in_blocks.iter().cloned());
    let mut gold = prep_memories(dir, "gold_prep", rtl, &to_logic, &sync_cut)?;
    let mut mid_cut = prep_memories(dir, "mid_cut_prep", &refined_path, &to_logic, &sync_cut)?;
    if !all_cut.is_empty() {
        cut_memories(top_mut(&mut gold))?;
        cut_memories(top_mut(&mut mid_cut))?;
    }
    // That against the result: the memories in blocks as words.
    let mut mid = prep_memories(dir, "mid_prep", &refined_path, &mapped, &sync_cut)?;
    let mut cut_gate = gate.clone();
    {
        let gt = cut_gate
            .modules
            .iter_mut()
            .find(|m| !m.is_blackbox())
            .expect("top");
        model_blocks(gt, &mem_infos(refined))?;
        if !cut.is_empty() {
            cut_memories(gt)?;
        }
    }
    if !cut.is_empty() {
        cut_memories(top_mut(&mut mid))?;
    }
    let gold_path = dir.join("gold.json");
    write(&gold, &gold_path);
    let mid_path = dir.join("mid_cut.json");
    write(&mid_cut, &mid_path);
    let first = equiv(dir, "equiv_refine", &gold_path, &mid_path)?;

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
                "read_json {}\nhierarchy -top top\nproc\nflatten\nchtype -set chipsmith_mem t:$mem_v2\n\
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
    let second = equiv(dir, "equiv_synth", &sub_path, &cut_gate_path)?;
    Ok(Proof {
        constant_registers: claims.len(),
        equivalences: first + second,
    })
}
