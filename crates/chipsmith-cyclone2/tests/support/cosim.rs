//! Co-simulation (iverilog): the RTL against the synthesized netlist, the
//! multipliers as `SIM_MODELS`' behavioural CY2_MULT, on random vectors.
//! What the proofs can't do for a dense product over several blocks
//! (SAT on a wide multiplier), this does for thousands of operand pairs,
//! the extremes among them.

use std::path::Path;
use std::process::Command;

use chipsmith_cyclone2::synth::cells;
use chipsmith_netlist::{Design, Dir};

use super::{read, top, write, yosys, SIM_MODELS};

/// What a co-simulation found.
#[derive(Debug)]
pub struct Cosim {
    pub cycles: usize,
    /// Output bits compared, summed over the cycles (x in the RTL skipped).
    pub compared: u64,
}

/// Simulates `rtl` (yosys' RTL JSON) and `gate` side by side with the same
/// inputs: every input but the clocks is redrawn from `seed` after each
/// falling edge of the first clock, every output compared just after it,
/// `cycles` times. The first 16 cycles draw the extremes (all ones, all
/// zeros, the top bit alone, the top bit clear) so a sign or a carry at
/// the end of an operand is seen. An output bit the RTL leaves x isn't
/// compared. Fails with the seed and the cycle of the first difference.
pub fn cosim(
    dir: &Path,
    rtl: &Path,
    gate: &Design,
    cycles: usize,
    seed: u32,
) -> Result<Cosim, String> {
    let gate_json = dir.join("cosim_gate.json");
    write(gate, &gate_json);
    let sim = dir.join("cy2_sim.v");
    std::fs::write(&sim, SIM_MODELS).unwrap();
    let lib = dir.join("cells.v");
    std::fs::write(&lib, cells::LIBRARY).unwrap();
    let gold_v = dir.join("cosim_gold.v");
    let gate_v = dir.join("cosim_gate.v");
    yosys(
        dir,
        "cosim_netlists",
        &format!(
            "read_json {rtl}\nhierarchy -top top\nproc\nmemory_collect\nopt_clean\nrename top gold\n\
             write_verilog -noattr {gold_v}\ndesign -reset\n\
             read_verilog -lib {lib}\nread_json {gate}\nhierarchy -top top\nproc\nflatten\n\
             opt_clean\nrename top gate\nwrite_verilog -noattr {gate_v}\n",
            lib = lib.display(),
            rtl = rtl.display(),
            gate = gate_json.display(),
            gold_v = gold_v.display(),
            gate_v = gate_v.display(),
        ),
        dir,
    )?;
    let d = read(rtl);
    let t = top(&d);
    let is_clock = |n: &str| n.starts_with("CLOCK_") || n == "clk";
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
    // Each input: an extreme pattern chosen by the cycle for the first 16
    // cycles (input k uses pattern (cyc + k) % 4 so the pairs vary), random
    // after.
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
        .args([&tb_v, &gold_v, &gate_v, &sim])
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
