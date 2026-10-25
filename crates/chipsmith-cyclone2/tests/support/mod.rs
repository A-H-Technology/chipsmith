//! yosys as the oracle for chipsmith's Cyclone II synthesis (ADR-0004):
//! the RTL it elaborates, the netlist cyclone2-re's `cyclone2.ys` makes
//! from it, and a sequential equivalence check between an RTL netlist and
//! a synthesized one. Needs yosys and `CHIPSMITH_CYCLONE2_SYNTH`: run in
//! the devshell.
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
//! - The Cyclone II cells get simulation models (cyclone2-re's
//!   `cells_sim.v` only declares them).

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use chipsmith_netlist::{
    read_json, write_json, CellKind, Design, Gate, GateOp, Module, Net, SigBit, State, Wire,
};

/// Behavioural models of the cells synthesis emits, as the packer and the
/// hardware read them (cyclone2-re's `cells_sim.v` comments).
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
module CY2_IBUF (input PAD, output COMBOUT);
  assign COMBOUT = PAD;
endmodule
module CY2_OBUF (input DATAOUT, output PAD);
  assign PAD = DATAOUT;
endmodule
"#;

/// A scratch directory under `base` (tests pass `CARGO_TARGET_TMPDIR`).
pub fn scratch(base: &Path, name: &str) -> PathBuf {
    let dir = base.join("synth").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn synth_dir() -> PathBuf {
    PathBuf::from(std::env::var("CHIPSMITH_CYCLONE2_SYNTH").expect(
        "CHIPSMITH_CYCLONE2_SYNTH (cyclone2-re's synth/) unset: run in `nix develop`",
    ))
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
    let lib = synth_dir().join("synth/cells_sim.v");
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

/// What the yosys flow makes of the design, as chipsmith's build runs it
/// today: cyclone2-re's `cyclone2.ys` cut at `abc -lut 4`, chipsmith's
/// LUT mapper in between.
pub fn yosys_flow(dir: &Path, top: &str, files: &[PathBuf]) -> PathBuf {
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
    yosys(dir, "flow_gates", &to_gates, &synth_dir()).unwrap_or_else(|e| panic!("{e}"));
    chipsmith_synth::lutmap::map_file(&gates, &luts).unwrap();
    let to_cells = format!(
        "read_verilog -lib synth/cells_sim.v\nread_json {}\nhierarchy -top top\n{}\nwrite_json {}\n",
        luts.display(),
        lines[cut + 1..].join("\n"),
        json.display()
    );
    yosys(dir, "flow_cells", &to_cells, &synth_dir()).unwrap_or_else(|e| panic!("{e}"));
    json
}

pub fn write(design: &Design, path: &Path) {
    write_json(design, std::io::BufWriter::new(std::fs::File::create(path).unwrap())).unwrap();
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

/// yosys proves `gate` equivalent to `gold` (both JSON files, the
/// memories already as they should be compared), with x in `gold` read as
/// "any value". Returns how many `$equiv` it proved.
fn equiv(dir: &Path, name: &str, gold: &Path, gate: &Path) -> Result<usize, String> {
    let sim = dir.join("cy2_sim.v");
    std::fs::write(&sim, SIM_MODELS).unwrap();
    let prep = "hierarchy -top top\nproc\nflatten\nchtype -set chipsmith_mem t:$mem_v2\n\
                chtype -set chipsmith_tribuf t:$tribuf t:$_TBUF_\nopt_clean";
    let script = format!(
        "read_json {gold}\nread_verilog -overwrite {sim}\n{prep}\nrename top gold\ndesign -stash gold\n\
         read_json {gate}\nread_verilog -overwrite {sim}\n{prep}\nrename top gate\ndesign -stash gate\n\
         design -copy-from gold -as gold gold\ndesign -copy-from gate -as gate gate\n\
         equiv_make gold gate equiv\nhierarchy -top equiv\nasync2sync\n\
         equiv_struct -fwd\nequiv_simple -undef\nequiv_induct -undef -ignore-unknown-cells\n\
         equiv_status -assert\n",
        gold = gold.display(),
        gate = gate.display(),
        sim = sim.display(),
    );
    let log = yosys(dir, name, &script, dir).map_err(|e| {
        let unproven: Vec<&str> = e.lines().filter(|l| l.contains("Unproven")).take(10).collect();
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
fn prep_memories(dir: &Path, name: &str, design: &Path, to_logic: &[String]) -> Result<Design, String> {
    let mut map = String::new();
    for id in to_logic {
        map += &format!("memory_map t:$mem_v2 r:MEMID=\\{id} %i\n");
    }
    let out = dir.join(format!("{name}.json"));
    yosys(
        dir,
        name,
        &format!(
            "read_json {}\nhierarchy -top top\nproc\nflatten\nmemory_collect\n{map}opt_clean\nwrite_json {}\n",
            design.display(),
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

    let gold = prep_memories(dir, "gold_prep", rtl, &to_logic)?;
    let gold_path = dir.join("gold.json");
    write(&gold, &gold_path);
    let mut mid = prep_memories(dir, "refined_prep", &refined_path, &to_logic)?;
    let mid_path = dir.join("mid.json");
    write(&mid, &mid_path);
    let first = equiv(dir, "equiv_refine", &gold_path, &mid_path)?;

    let gm = mid.modules.iter_mut().find(|m| m.name == "top").expect("top");
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
    if !claims.is_empty() {
        // One output bit per claim, each "this register holds its value".
        let mut with_claims = gm.clone();
        let mut holds = Vec::new();
        let mut names = String::new();
        let net_names: HashMap<Net, String> = gm
            .wires
            .iter()
            .flat_map(|w| {
                w.bits.iter().enumerate().filter_map(move |(i, b)| {
                    b.net().map(|n| (n, format!("{}[{i}]", w.name)))
                })
            })
            .collect();
        for (k, (n, v)) in claims.iter().enumerate() {
            let b = SigBit::Net(*n);
            names += &format!("{k}: {} = {}\n", net_names.get(n).map_or("?", |s| s), u8::from(*v));
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
                CellKind::Gate(Gate::new(GateOp::Xnor, &[SigBit::Net(*a), SigBit::Net(*b)], y).unwrap()),
            );
            names += &format!(
                "{}: {} == {}\n",
                holds.len(),
                net_names.get(a).map_or("?", |s| s),
                net_names.get(b).map_or("?", |s| s)
            );
            holds.push(y);
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
                 sat -tempinduct -enable_undef -set-def-inputs -prove-x __claims {}'b{all} -maxsteps 8 -show __claims -ignore_unknown_cells\n",
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
    }
    let sub_path = dir.join("mid_constants.json");
    write(&mid, &sub_path);
    let second = equiv(dir, "equiv_synth", &sub_path, &gate_path)?;
    Ok(Proof {
        constant_registers: claims.len(),
        equivalences: first + second,
    })
}
