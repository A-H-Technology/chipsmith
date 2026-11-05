//! chipsmith's block RAM models (`chipsmith_xc7::synth::cells::MODELS`)
//! against Vivado's own simulation models (unisims) on random
//! configurations and stimulus: each block twice, one of each, side by
//! side in iverilog, every read output bit compared each cycle where
//! Vivado's is defined.
//!
//! Vivado's sources are licensed, so the test runs only with
//! `CHIPSMITH_UNISIMS` naming Vivado's `data/verilog/src` directory (the
//! one holding `glbl.v` and `unisims/`); without it, it passes and says so.
//! `CHIPSMITH_MODELS_SEEDS` (default 40) sets the number of runs, six
//! blocks each.
//!
//! The configurations are those a netlist has: a port's read and write
//! widths alike where it has both (asymmetric ports, which neither
//! chipsmith nor Vivado builds, differ from unisim where the other port
//! writes the same edge), a narrow port's write enables all driven alike,
//! and a READ_FIRST write of some of its bytes not read by a narrower
//! port on the same edge (unisim gives that read the new data, UG473 the
//! old; synthesis doesn't rely on either, `synth/bram.rs`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, p: f64) -> bool {
        ((self.next() >> 11) as f64 / (1u64 << 53) as f64) < p
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len() as u64) as usize]
    }
    fn hex(&mut self, bits: usize) -> String {
        let mut s = String::new();
        for _ in 0..bits.div_ceil(4) {
            s.push(char::from_digit(self.below(16) as u32, 16).unwrap());
        }
        format!("{bits}'h{s}")
    }
}

fn dbits(w: u32) -> u32 {
    if w >= 9 {
        w / 9 * 8
    } else {
        w
    }
}

fn pbits(w: u32) -> u32 {
    w / 9
}

/// A testbench of `n` random blocks, `cycles` long.
fn testbench(seed: u64, n: usize, cycles: usize) -> String {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let two_clocks = rng.chance(0.3);
    let mut tb = String::from("`timescale 1ps/1ps\nmodule tb;\n  reg clka = 0, clkb = 0;\n");
    tb += "  integer cyc = 0, bad = 0, compared = 0, stricter = 0, i;\n  reg [7:0] l;\n  always #5000 clka = ~clka;\n";
    tb += if two_clocks {
        "  always #3700 clkb = ~clkb;\n"
    } else {
        "  always @* clkb = clka;\n"
    };
    let (mut idle, mut draw, mut check_a, mut check_b) =
        (String::new(), String::new(), String::new(), String::new());
    for k in 0..n {
        let big = rng.chance(0.4);
        let prim = if big { "RAMB36E1" } else { "RAMB18E1" };
        let mut tdp: Vec<u32> = vec![1, 2, 4, 9, 18];
        if big {
            tdp.push(36);
        }
        let wide = if big { 72 } else { 36 };
        let sdp = rng.chance(0.35);
        let (ra, rb, wa, wb, ma, mb);
        if sdp {
            let mut all = tdp.clone();
            all.push(wide);
            (ra, wb) = if rng.chance(0.5) {
                (wide, rng.pick(&all))
            } else {
                (rng.pick(&all), wide)
            };
            (rb, wa) = (0, 0);
            let m = rng.pick(&["READ_FIRST", "WRITE_FIRST"]);
            (ma, mb) = (m, m);
        } else {
            let mut z = vec![0];
            z.extend(&tdp);
            let mut a = [rng.pick(&z), rng.pick(&z), rng.pick(&z), rng.pick(&z)];
            if a[0] == 0 && a[1] == 0 {
                a[0] = rng.pick(&tdp);
            }
            // A port reading and writing does so at one width.
            if a[0] != 0 && a[2] != 0 {
                a[2] = a[0];
            }
            if a[1] != 0 && a[3] != 0 {
                a[3] = a[1];
            }
            (ra, rb, wa, wb) = (a[0], a[1], a[2], a[3]);
            let modes = ["READ_FIRST", "WRITE_FIRST", "NO_CHANGE"];
            (ma, mb) = (rng.pick(&modes), rng.pick(&modes));
        }
        let (abits, dw, pw, iw, nin, ninp) = if big {
            (16, 32, 4, 36, 128, 16)
        } else {
            (14, 16, 2, 18, 64, 8)
        };
        let doa_reg = rng.chance(0.4) as u32;
        let dob_reg = if sdp { doa_reg } else { rng.chance(0.4) as u32 };
        let mut p = format!(
            ".RAM_MODE(\"{}\"), .READ_WIDTH_A({ra}), .READ_WIDTH_B({rb}), .WRITE_WIDTH_A({wa}), .WRITE_WIDTH_B({wb}), \
             .WRITE_MODE_A(\"{ma}\"), .WRITE_MODE_B(\"{mb}\"), .DOA_REG({doa_reg}), .DOB_REG({dob_reg})",
            if sdp { "SDP" } else { "TDP" }
        );
        for l in ['A', 'B'] {
            let _ = write!(
                p,
                ", .INIT_{l}({}), .SRVAL_{l}({}), .RSTREG_PRIORITY_{l}(\"{}\")",
                rng.hex(iw),
                rng.hex(iw),
                rng.pick(&["RSTREG", "REGCE"])
            );
        }
        for i in 0..nin {
            let _ = write!(p, ", .INIT_{i:02X}({})", rng.hex(256));
        }
        for i in 0..ninp {
            let _ = write!(p, ", .INITP_{i:02X}({})", rng.hex(256));
        }
        let pins = [
            "CLKARDCLK",
            "CLKBWRCLK",
            "ENARDEN",
            "ENBWREN",
            "RSTRAMARSTRAM",
            "RSTRAMB",
            "RSTREGARSTREG",
            "RSTREGB",
        ];
        let mut inv = [0u32; 8];
        if rng.chance(0.2) {
            for (i, pin) in pins.iter().enumerate() {
                inv[i] = rng.chance(0.3) as u32;
                let _ = write!(p, ", .IS_{pin}_INVERTED(1'b{})", inv[i]);
            }
        }
        let _ = writeln!(
            tb,
            "  // {k}: {prim} {} RA{ra} RB{rb} WA{wa} WB{wb} {ma}/{mb}",
            if sdp { "SDP" } else { "TDP" }
        );
        let (wea, web) = if big { (4, 8) } else { (2, 4) };
        let sigs: [(&str, u32); 16] = [
            ("ena", 1),
            ("enb", 1),
            ("regcea", 1),
            ("regceb", 1),
            ("rstrama", 1),
            ("rstramb", 1),
            ("rstrega", 1),
            ("rstregb", 1),
            ("wea", wea),
            ("webwe", web),
            ("addra", abits),
            ("addrb", abits),
            ("dia", dw),
            ("dib", dw),
            ("dipa", pw),
            ("dipb", pw),
        ];
        for (s, w) in sigs {
            let _ = writeln!(tb, "  reg [{}:0] {s}{k};", w - 1);
        }
        for o in ["doa", "dob"] {
            let _ = writeln!(tb, "  wire [{}:0] {o}_v{k}, {o}_c{k};", dw - 1);
        }
        for o in ["dopa", "dopb"] {
            let _ = writeln!(tb, "  wire [{}:0] {o}_v{k}, {o}_c{k};", pw - 1);
        }
        for (side, module) in [("v", prim.to_string()), ("c", format!("CS_{prim}"))] {
            let extra = if big {
                ".CASCADEINA(1'b0), .CASCADEINB(1'b0), .INJECTDBITERR(1'b0), .INJECTSBITERR(1'b0), "
            } else {
                ""
            };
            let _ = writeln!(
                tb,
                "  {module} #({p}) u{side}{k} (.CLKARDCLK(clka), .CLKBWRCLK(clkb), .ENARDEN(ena{k}), .ENBWREN(enb{k}), \
                 .REGCEAREGCE(regcea{k}), .REGCEB(regceb{k}), .RSTRAMARSTRAM(rstrama{k}), .RSTRAMB(rstramb{k}), \
                 .RSTREGARSTREG(rstrega{k}), .RSTREGB(rstregb{k}), {extra}.ADDRARDADDR(addra{k}), .ADDRBWRADDR(addrb{k}), \
                 .DIADI(dia{k}), .DIBDI(dib{k}), .DIPADIP(dipa{k}), .DIPBDIP(dipb{k}), .WEA(wea{k}), .WEBWE(webwe{k}), \
                 .DOADO(doa_{side}{k}), .DOBDO(dob_{side}{k}), .DOPADOP(dopa_{side}{k}), .DOPBDOP(dopb_{side}{k}));"
            );
        }
        // Idle through Vivado's global set/reset (100 ns): enables and
        // resets off as the inversions have them.
        let _ = writeln!(
            idle,
            "    ena{k} = {}; enb{k} = {}; rstrama{k} = {}; rstramb{k} = {}; rstrega{k} = {}; rstregb{k} = {}; \
             regcea{k} = 0; regceb{k} = 0; wea{k} = 0; webwe{k} = 0; addra{k} = 0; addrb{k} = 0; dia{k} = 0; dib{k} = 0; dipa{k} = 0; dipb{k} = 0;",
            inv[2], inv[3], inv[4], inv[5], inv[6], inv[7]
        );
        // A few hot words, so the ports collide.
        let hot: Vec<u64> = (0..3).map(|_| rng.below(1 << abits)).collect();
        for s in ["addra", "addrb"] {
            let _ = writeln!(
                draw,
                "    case ($urandom % 4) 0: {s}{k} = {}; 1: {s}{k} = {}; 2: {s}{k} = {}; default: {s}{k} = $urandom; endcase",
                hot[0], hot[1], hot[2]
            );
        }
        // Byte enables alike where a narrow port's whole word is one
        // enable, and where a narrower read on the other side would see a
        // part-written word (see the module comment).
        let narrow_reader = |w: u32, r: u32| r != 0 && r < w;
        let alike_a = narrow_reader(wa, rb);
        let alike_b = narrow_reader(wb, ra);
        // The write enable pins as netlists drive them: each lane's enable
        // repeated over the port's pins (all of WEBWE for SDP's wide write),
        // WEBWE's others 0.
        let we_pins = |name: String, w: u32, total: u32, pins: u32, alike: bool| -> String {
            let lanes = (w / 9).max(1);
            let mut bits: Vec<String> = (0..pins)
                .rev()
                .map(|i| {
                    if alike {
                        "l[0]".to_string()
                    } else {
                        format!("l[{}]", i % lanes)
                    }
                })
                .collect();
            if total > pins {
                bits.insert(0, format!("{}'b0", total - pins));
            }
            format!("begin l = $urandom; {name} = {{{}}}; end", bits.join(", "))
        };
        let a_pins = if big { 4 } else { 2 };
        let b_pins = if sdp && wb == wide { web } else { a_pins };
        for (s, w) in sigs {
            if s.starts_with("addr") {
                continue;
            }
            let line = match s {
                "ena" | "enb" | "regcea" | "regceb" => format!("{s}{k} = ($urandom % 8) != 0;"),
                _ if s.starts_with("rst") => format!("{s}{k} = ($urandom % 10) == 0;"),
                "wea" => we_pins(format!("{s}{k}"), wa, w, a_pins, alike_a),
                "webwe" => we_pins(format!("{s}{k}"), wb, w, b_pins, alike_b),
                _ => format!("{s}{k} = $urandom;"),
            };
            let _ = writeln!(draw, "    {line}");
        }
        let cmp = |port: char, w: u32, whole: bool| -> String {
            if w == 0 {
                return String::new();
            }
            let groups: Vec<(String, u32)> = if whole {
                vec![
                    ("doa".into(), dw),
                    ("dob".into(), dw),
                    ("dopa".into(), pw),
                    ("dopb".into(), pw),
                ]
            } else {
                vec![
                    (format!("do{port}"), dbits(w)),
                    (format!("dop{port}"), pbits(w)),
                ]
            };
            groups
                .into_iter()
                .filter(|(_, n)| *n > 0)
                .map(|(g, n)| {
                    format!(
                        "    for (i = 0; i < {n}; i = i + 1) if ({g}_v{k}[i] !== 1'bx) begin compared = compared + 1; \
                         if ({g}_c{k}[i] === 1'bx) stricter = stricter + 1; else if ({g}_c{k}[i] !== {g}_v{k}[i]) begin bad = bad + 1; \
                         if (bad < 10) $display(\"MISMATCH block {k} cycle %0d {g}[%0d] vivado %b chipsmith %b\", cyc, i, {g}_v{k}[i], {g}_c{k}[i]); end end\n"
                    )
                })
                .collect()
        };
        check_a += &cmp('a', ra, sdp && ra == wide);
        let b = cmp('b', if sdp { 0 } else { rb }, false);
        if two_clocks {
            check_b += &b;
        } else {
            check_a += &b;
        }
    }
    let _ = write!(
        tb,
        "  initial begin\n{idle}  end\n  always @(negedge clka) begin #100; if (cyc >= 12) begin\n{draw}  end end\n\
         always @(posedge clka) begin #2000;\n{check_a}    cyc = cyc + 1;\n    if (cyc == {cycles}) begin $display(\"DONE %0d %0d %0d\", bad, compared, stricter); $finish; end\n  end\n"
    );
    if two_clocks {
        let _ = write!(
            tb,
            "  always @(posedge clkb) begin #1500;\n{check_b}  end\n"
        );
    }
    tb + "endmodule\n"
}

#[test]
fn block_ram_models_match_vivados() {
    let Ok(unisims) = std::env::var("CHIPSMITH_UNISIMS") else {
        eprintln!("CHIPSMITH_UNISIMS unset: Vivado's models aren't here to compare against");
        return;
    };
    let unisims = PathBuf::from(unisims);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("models");
    std::fs::create_dir_all(&dir).unwrap();
    let ours = chipsmith_xc7::synth::cells::MODELS
        .replace("module RAMB18E1 (", "module CS_RAMB18E1 (")
        .replace("module RAMB36E1 (", "module CS_RAMB36E1 (");
    std::fs::write(dir.join("ours.v"), ours).unwrap();
    let seeds: u64 = std::env::var("CHIPSMITH_MODELS_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let mut failures = Vec::new();
    let (mut compared, mut stricter) = (0, 0);
    for seed in 1..=seeds {
        let tb = dir.join(format!("tb{seed}.v"));
        std::fs::write(&tb, testbench(seed, 6, 600)).unwrap();
        let vvp = dir.join(format!("tb{seed}.vvp"));
        let out = Command::new("iverilog")
            .args(["-g2012", "-s", "tb", "-s", "glbl", "-o"])
            .arg(&vvp)
            .arg(&tb)
            .arg(dir.join("ours.v"))
            .arg(unisims.join("unisims/RAMB18E1.v"))
            .arg(unisims.join("unisims/RAMB36E1.v"))
            .arg(unisims.join("glbl.v"))
            .output()
            .expect("iverilog: run in the devshell");
        assert!(
            out.status.success(),
            "iverilog: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = Command::new("vvp").arg("-n").arg(&vvp).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let done: Option<Vec<u64>> = text
            .lines()
            .find_map(|l| l.strip_prefix("DONE "))
            .map(|l| l.split(' ').filter_map(|n| n.parse().ok()).collect());
        match done.as_deref() {
            Some([0, c, s]) => {
                compared += c;
                stricter += s;
            }
            _ => failures.push(format!(
                "seed {seed}: {}",
                text.lines()
                    .filter(|l| l.starts_with("MISMATCH") || l.starts_with("DONE"))
                    .take(5)
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
        }
    }
    // `stricter`: bits chipsmith's model leaves x (a collision) where
    // Vivado's gives a value.
    eprintln!(
        "{seeds} seeds of 6 blocks, 600 cycles each: {} failed; {compared} output bits compared, \
         {stricter} of them x in chipsmith's model only",
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A testbench of `n` random DSP48E1 multipliers, yosys' model (renamed
/// `CS_DSP48E1`) beside Vivado's, `cycles` long: the configurations
/// synthesis and the decoder make (A and B of 0 to 2 stages, M, P and the
/// control registers on or off, either clock edge (Vivado's
/// IS_CLK_INVERTED against yosys' model on the inverted clock, the
/// decoder's reading), P = A * B or Vivado's
/// cascaded sum P = A * B + PCIN >>> 17), random data, enables and resets.
fn dsp_testbench(seed: u64, n: usize, cycles: usize) -> String {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut tb = String::from(
        "`timescale 1ps/1ps\nmodule tb;\n  reg clk = 0;\n  integer cyc = 0, bad = 0, compared = 0, stricter = 0, i;\n",
    );
    let (mut draw, mut check) = (String::new(), String::new());
    for k in 0..n {
        let areg = rng.below(3);
        let breg = rng.below(3);
        let acasc = if areg == 2 && rng.chance(0.5) { 1 } else { areg };
        let bcasc = if breg == 2 && rng.chance(0.5) { 1 } else { breg };
        let inverted = rng.chance(0.3);
        let mut params = format!(".AREG({areg}), .BREG({breg}), .ACASCREG({acasc}), .BCASCREG({bcasc})");
        for p in [
            "MREG", "PREG", "INMODEREG", "OPMODEREG", "ALUMODEREG", "CARRYINREG", "CARRYINSELREG", "CREG",
            "ADREG", "DREG",
        ] {
            let _ = write!(params, ", .{p}({})", rng.below(2));
        }
        let opmode = rng.pick(&["7'b0000101", "7'b1010101"]);
        for r in [
            "a", "b", "c", "d", "pcin", "cea1", "cea2", "ceb1", "ceb2", "cem", "cep", "cectrl", "ceinmode",
            "cealumode", "cecarryin", "rsta", "rstb", "rstm", "rstp", "rstctrl", "rstinmode", "rstalumode",
            "rstallcarryin",
        ] {
            let w = match r {
                "a" => 30,
                "b" => 18,
                "c" | "pcin" => 48,
                "d" => 25,
                _ => 1,
            };
            let v = if r.starts_with("ce") {
                "($random % 8 != 0)"
            } else if r.starts_with("rst") {
                "($random % 16 == 0)"
            } else {
                "{$random, $random}"
            };
            let _ = writeln!(tb, "  reg [{}:0] {r}{k} = 0;", w - 1);
            let _ = writeln!(draw, "    {r}{k} <= {v};");
        }
        let _ = writeln!(tb, "  wire [47:0] p_c{k}, p_v{k};");
        let ports = |p: &str, clk: &str| {
            format!(
                ".CLK({clk}), .A(a{k}), .B(b{k}), .C(c{k}), .D(d{k}), .PCIN(pcin{k}), .ACIN(30'b0), .BCIN(18'b0), \
                 .CARRYCASCIN(1'b0), .MULTSIGNIN(1'b0), .OPMODE({opmode}), .ALUMODE(4'b0), .INMODE(5'b0), \
                 .CARRYIN(1'b0), .CARRYINSEL(3'b0), .CEA1(cea1{k}), .CEA2(cea2{k}), .CEB1(ceb1{k}), .CEB2(ceb2{k}), \
                 .CEC(1'b1), .CED(1'b1), .CEAD(1'b1), .CEM(cem{k}), .CEP(cep{k}), .CECTRL(cectrl{k}), \
                 .CEINMODE(ceinmode{k}), .CEALUMODE(cealumode{k}), .CECARRYIN(cecarryin{k}), \
                 .RSTA(rsta{k}), .RSTB(rstb{k}), .RSTC(1'b0), .RSTD(1'b0), .RSTM(rstm{k}), .RSTP(rstp{k}), \
                 .RSTCTRL(rstctrl{k}), .RSTINMODE(rstinmode{k}), .RSTALUMODE(rstalumode{k}), \
                 .RSTALLCARRYIN(rstallcarryin{k}), .P({p}{k})"
            )
        };
        // yosys' model has no IS_CLK_INVERTED: its clock is inverted
        // instead, as the decoder folds it.
        let (yosys_clk, vivado_inv) = if inverted { ("!clk", "1") } else { ("clk", "0") };
        let _ = writeln!(tb, "  CS_DSP48E1 #({params}) yosys{k} ({});", ports("p_c", yosys_clk));
        let _ = writeln!(
            tb,
            "  DSP48E1 #({params}, .IS_CLK_INVERTED(1'b{vivado_inv})) vivado{k} ({});",
            ports("p_v", "clk")
        );
        let _ = writeln!(
            check,
            "    for (i = 0; i < 48; i = i + 1) if (p_v{k}[i] !== 1'bx) begin compared = compared + 1; \
             if (p_c{k}[i] === 1'bx) stricter = stricter + 1; else if (p_c{k}[i] !== p_v{k}[i]) begin bad = bad + 1; \
             if (bad < 10) $display(\"MISMATCH dsp {k} cycle %0d p[%0d] vivado %b yosys %b\", cyc, i, p_v{k}[i], p_c{k}[i]); end end"
        );
    }
    // GSR holds unisim's registers for its first 100 ns; the stimulus
    // starts after it.
    let _ = write!(
        tb,
        "  always #5000 clk = !clk;\n  always @(negedge clk) begin #100; if ($time > 200000) begin\n{draw}  end end\n\
         always @(posedge clk) begin #4500; if ($time > 300000) begin\n{check}  end\n    cyc = cyc + 1;\n    \
         if (cyc == {cycles}) begin $display(\"DONE %0d %0d %0d\", bad, compared, stricter); $finish; end\n  end\n"
    );
    tb + "endmodule\n"
}

/// yosys' DSP48E1 model (`xilinx/cells_sim.v`), which the co-simulations
/// use, against Vivado's: `CHIPSMITH_MODELS_SEEDS` runs of six blocks.
#[test]
fn dsp_model_matches_vivados() {
    let Ok(unisims) = std::env::var("CHIPSMITH_UNISIMS") else {
        eprintln!("CHIPSMITH_UNISIMS unset: Vivado's models aren't here to compare against");
        return;
    };
    let unisims = PathBuf::from(unisims);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("models_dsp");
    std::fs::create_dir_all(&dir).unwrap();
    let datdir = Command::new("yosys-config").arg("--datdir").output().expect("yosys-config: run in the devshell");
    let sim = std::fs::read_to_string(
        PathBuf::from(String::from_utf8(datdir.stdout).unwrap().trim()).join("xilinx/cells_sim.v"),
    )
    .unwrap();
    let start = sim.find("module DSP48E1 (").expect("cells_sim.v has a DSP48E1");
    let end = start + sim[start..].find("endmodule").unwrap() + "endmodule".len();
    let ours = sim[start..end].replacen("module DSP48E1 (", "module CS_DSP48E1 (", 1);
    std::fs::write(dir.join("ours.v"), ours).unwrap();
    let seeds: u64 = std::env::var("CHIPSMITH_MODELS_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(40);
    let mut failures = Vec::new();
    let (mut compared, mut stricter) = (0, 0);
    for seed in 1..=seeds {
        let tb = dir.join(format!("tb{seed}.v"));
        std::fs::write(&tb, dsp_testbench(seed, 6, 600)).unwrap();
        let vvp = dir.join(format!("tb{seed}.vvp"));
        let out = Command::new("iverilog")
            .args(["-g2012", "-s", "tb", "-s", "glbl", "-o"])
            .arg(&vvp)
            .arg(&tb)
            .arg(dir.join("ours.v"))
            .arg(unisims.join("unisims/DSP48E1.v"))
            .arg(unisims.join("glbl.v"))
            .output()
            .expect("iverilog: run in the devshell");
        assert!(out.status.success(), "iverilog: {}", String::from_utf8_lossy(&out.stderr));
        let out = Command::new("vvp").arg("-n").arg(&vvp).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let done: Option<Vec<u64>> = text
            .lines()
            .find_map(|l| l.strip_prefix("DONE "))
            .map(|l| l.split(' ').filter_map(|n| n.parse().ok()).collect());
        match done.as_deref() {
            Some([0, c, s]) => {
                compared += c;
                stricter += s;
            }
            _ => failures.push(format!(
                "seed {seed}: {}",
                text.lines()
                    .filter(|l| l.starts_with("MISMATCH") || l.starts_with("DONE"))
                    .take(5)
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
        }
    }
    eprintln!(
        "{seeds} seeds of 6 DSPs, 600 cycles each: {} failed; {compared} output bits compared, \
         {stricter} of them x in yosys' model only",
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
