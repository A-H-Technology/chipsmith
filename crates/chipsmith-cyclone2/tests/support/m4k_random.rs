//! Random memories for M4K mapping: one memory per design, its kind,
//! widths, depth and features drawn from a seed, written the way Verilog
//! describes memories (no vendor primitives), with a DE2 top around it:
//! addresses and data from a counter and the switches, read data folded
//! onto the LEDs. The synthesis test proves each equivalent to its RTL;
//! `cargo run --example m4k_random` writes them as projects for building
//! and for Quartus to assemble.

/// xorshift64*: the same designs on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len())]
    }
    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Rom,
    /// Read and write at one address, the read registered by its address:
    /// it sees the write.
    SingleNew,
    /// Read and write at one address, the read data registered: old data.
    SingleOld,
    SimpleDual,
    TrueDual,
}

/// The second register on the read data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutReg {
    None,
    Plain,
    Enabled,
    Cleared,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub seed: u64,
    pub kind: Kind,
    /// Bits per word, and words.
    pub width: usize,
    pub depth: usize,
    /// Words per write and per read (mixed widths).
    pub wr_factor: usize,
    pub rd_factor: usize,
    pub byteena: bool,
    /// A read enable, and whether the write sits under it too.
    pub read_enable: bool,
    pub write_under_enable: bool,
    pub out_reg: OutReg,
    pub sync_reset: bool,
    /// The read on CLOCK_27.
    pub two_clocks: bool,
    pub init: bool,
}

impl Config {
    pub fn new(seed: u64) -> Config {
        let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        for _ in 0..4 {
            r.next();
        }
        let kind = r.pick(&[
            Kind::Rom,
            Kind::SingleNew,
            Kind::SingleOld,
            Kind::SimpleDual,
            Kind::SimpleDual,
            Kind::TrueDual,
        ]);
        let width: usize = r.pick(&[1, 2, 3, 4, 5, 8, 9, 12, 16, 18, 24, 32, 36, 40]);
        let depth: usize = r.pick(&[16, 64, 100, 256, 512, 600, 1024, 2048, 4096, 5000]);
        // At most 16 blocks' worth.
        let depth = depth
            .min((16 * 4096 / width).next_power_of_two() / 2)
            .max(16);
        let (mut wr_factor, mut rd_factor) = (1, 1);
        if kind == Kind::SimpleDual && width <= 9 && r.chance(40) {
            let f = r.pick(&[2, 4]);
            if r.chance(50) {
                wr_factor = f;
            } else {
                rd_factor = f;
            }
        }
        let byteena = width.is_multiple_of(8)
            && (16..=32).contains(&width)
            && matches!(kind, Kind::SingleOld | Kind::SimpleDual)
            && wr_factor == 1
            && r.chance(50);
        let read_enable = kind != Kind::TrueDual && r.chance(40);
        let write_under_enable = read_enable && kind != Kind::Rom && r.chance(50);
        let two_clocks = kind == Kind::SimpleDual && r.chance(30);
        let out_reg = if kind == Kind::SingleNew || kind == Kind::TrueDual {
            OutReg::None
        } else {
            r.pick(&[
                OutReg::None,
                OutReg::None,
                OutReg::Plain,
                OutReg::Enabled,
                OutReg::Cleared,
            ])
        };
        let sync_reset = out_reg == OutReg::None
            && matches!(kind, Kind::Rom | Kind::SimpleDual | Kind::SingleOld)
            && r.chance(25);
        let init = kind == Kind::Rom || r.chance(30);
        Config {
            seed,
            kind,
            width,
            depth,
            wr_factor,
            rd_factor,
            byteena,
            read_enable,
            write_under_enable,
            out_reg,
            sync_reset,
            two_clocks,
            init,
        }
    }

    pub fn name(&self) -> String {
        format!("m4k-random-{}", self.seed)
    }

    /// The design: `top` with CLOCK_50 (and CLOCK_27), SW, KEY, LEDR, LEDG.
    pub fn verilog(&self) -> String {
        let (w, d) = (self.width, self.depth);
        let abits = (usize::BITS - (d - 1).leading_zeros()) as usize;
        let mut v = format!("// {self:?}\n");
        v += "module top(input CLOCK_50, input CLOCK_27, input [17:0] SW, input [3:0] KEY, \
              output [17:0] LEDR, output [8:0] LEDG);\n";
        v += "  wire clk = CLOCK_50;\n";
        v += &format!(
            "  wire rclk = {};\n",
            if self.two_clocks {
                "CLOCK_27"
            } else {
                "CLOCK_50"
            }
        );
        v += "  reg [47:0] cnt = 0;\n  always @(posedge clk) cnt <= cnt + 1'b1;\n";
        v += "  reg [23:0] rcnt = 0;\n  always @(posedge rclk) rcnt <= rcnt + 1'b1;\n";
        v += "  wire [47:0] dd = {cnt[29:6], cnt[23:0]} ^ {SW, SW[5:0], SW, SW[5:0]};\n";
        // Address and control from counter bits that move slowly enough to
        // see, mixed with the switches.
        v += "  wire [15:0] wa = cnt[35:20] ^ {SW[15:8], 8'h0};\n";
        v += "  wire [15:0] ra = rcnt[23:8] ^ {8'h0, SW[7:0]};\n";
        v += "  wire we = SW[17] & cnt[2];\n  wire web = SW[16] & cnt[3];\n";
        v += "  wire re = rcnt[3] | SW[14];\n";
        v += "  reg clr = 0, rst = 0, oen = 0;\n";
        v += "  always @(posedge rclk) begin clr <= SW[13] & rcnt[9]; rst <= SW[12] & rcnt[8]; oen <= rcnt[5] | SW[11]; end\n";
        v += &format!("  reg [{}:0] m [0:{}];\n", w - 1, d - 1);
        if self.init {
            v += "  integer i;\n";
            v += &format!(
                "  initial for (i = 0; i < {d}; i = i + 1) m[i] = (i * 64'h9E3779B97F4A7C15 + {}) >> {};\n",
                self.seed,
                64 - w
            );
        }
        let wa = |f: usize| -> String {
            let lg = f.trailing_zeros() as usize;
            format!("wa[{}:0]", abits - lg - 1)
        };
        let ra = |f: usize| -> String {
            let lg = f.trailing_zeros() as usize;
            format!("ra[{}:0]", abits - lg - 1)
        };
        // A word address: the port address with its sub-word index.
        let at = |base: &str, f: usize, i: usize| -> String {
            if f == 1 {
                base.to_string()
            } else {
                let lg = f.trailing_zeros();
                format!("{{{base}, {lg}'d{i}}}")
            }
        };
        let q_bits = w * self.rd_factor;
        // n bits of the 48-bit write data from bit lo on, wrapping.
        let bits = |lo: usize, n: usize| -> String {
            let v: Vec<String> = (0..n)
                .rev()
                .map(|k| format!("dd[{}]", (lo + k) % 48))
                .collect();
            format!("{{{}}}", v.join(", "))
        };
        let write = |v: &mut String, addr: &str, en: &str, f: usize, data_off: usize| {
            if self.byteena {
                for b in 0..w / 8 {
                    *v += &format!(
                        "      if ({en} & cnt[{}]) m[{addr}][{}:{}] <= {};\n",
                        4 + b,
                        8 * b + 7,
                        8 * b,
                        bits(data_off + 8 * b, 8)
                    );
                }
            } else {
                *v += &format!("      if ({en}) begin\n");
                for i in 0..f {
                    *v += &format!(
                        "        m[{}] <= {};\n",
                        at(addr, f, i),
                        bits(data_off + i * w, w)
                    );
                }
                *v += "      end\n";
            }
        };
        let read_expr = |addr: &str, f: usize| -> String {
            let parts: Vec<String> = (0..f)
                .rev()
                .map(|i| format!("m[{}]", at(addr, f, i)))
                .collect();
            if f == 1 {
                parts[0].clone()
            } else {
                format!("{{{}}}", parts.join(", "))
            }
        };
        let en = if self.read_enable { "re" } else { "1'b1" };
        v += &format!("  reg [{}:0] q;\n", q_bits - 1);
        match self.kind {
            Kind::Rom => {
                v += "  always @(posedge rclk) begin\n";
                self.read_stmt(&mut v, en, &read_expr(&ra(1), 1));
                v += "  end\n";
            }
            Kind::SingleNew => {
                v += &format!("  reg [{}:0] a_q;\n", abits - 1);
                v += "  always @(posedge clk) begin\n";
                write(&mut v, &wa(1), "we", 1, 0);
                v += &format!("    a_q <= {};\n  end\n", wa(1));
                v += "  always @* q = m[a_q];\n";
            }
            Kind::SingleOld => {
                v += "  always @(posedge clk) begin\n";
                if self.write_under_enable {
                    v += &format!("    if ({en}) begin\n");
                    write(&mut v, &wa(1), "we", 1, 0);
                    v += "    end\n";
                } else {
                    write(&mut v, &wa(1), "we", 1, 0);
                }
                self.read_stmt(&mut v, en, &read_expr(&wa(1), 1));
                v += "  end\n";
            }
            Kind::SimpleDual => {
                v += "  always @(posedge clk) begin\n";
                let we = if self.write_under_enable && !self.two_clocks {
                    "we & re"
                } else {
                    "we"
                };
                write(&mut v, &wa(self.wr_factor), we, self.wr_factor, 0);
                v += "  end\n  always @(posedge rclk) begin\n";
                self.read_stmt(&mut v, en, &read_expr(&ra(self.rd_factor), self.rd_factor));
                v += "  end\n";
            }
            Kind::TrueDual => {
                // Each port's new data read through, as Quartus' template
                // writes a true dual port; the other port's old data.
                v += "  reg [W:0] qb;\n"
                    .replace("W", &(w - 1).to_string())
                    .as_str();
                v += "  always @(posedge clk) begin\n";
                write(&mut v, &wa(1), "we", 1, 0);
                v += &format!("    q <= we ? {} : m[{}];\n  end\n", bits(0, w), wa(1));
                v += "  always @(posedge clk) begin\n";
                write(&mut v, &ra(1), "web", 1, 12);
                v += &format!("    qb <= web ? {} : m[{}];\n  end\n", bits(12, w), ra(1));
            }
        }
        let out = match (self.out_reg, self.kind) {
            (OutReg::None, _) | (_, Kind::SingleNew | Kind::TrueDual) => "q".to_string(),
            (o, _) => {
                v += &format!("  reg [{}:0] p;\n", q_bits - 1);
                match o {
                    OutReg::Plain => v += "  always @(posedge rclk) p <= q;\n",
                    OutReg::Enabled => v += "  always @(posedge rclk) if (oen) p <= q;\n",
                    _ => v +=
                        "  always @(posedge rclk or posedge clr) if (clr) p <= 0; else p <= q;\n",
                }
                "p".to_string()
            }
        };
        let all = if self.kind == Kind::TrueDual {
            format!("{{qb, {out}}}")
        } else {
            out
        };
        let n = q_bits + if self.kind == Kind::TrueDual { w } else { 0 };
        v += &format!("  wire [{}:0] all = {all};\n", n - 1);
        v += "  reg [26:0] f;\n  integer n;\n";
        v += &format!(
            "  always @* begin\n    f = 27'h0;\n    for (n = 0; n < {n}; n = n + 1) f[n % 27] = f[n % 27] ^ all[n];\n  end\n"
        );
        v += "  assign LEDR = f[17:0];\n  assign LEDG = f[26:18];\nendmodule\n";
        v
    }

    fn read_stmt(&self, v: &mut String, en: &str, expr: &str) {
        let body = if self.sync_reset {
            format!("if (rst) q <= 0; else q <= {expr};")
        } else {
            format!("q <= {expr};")
        };
        if self.read_enable {
            *v += &format!("    if ({en}) begin {body} end\n");
        } else {
            *v += &format!("    {body}\n");
        }
    }

    /// A chipsmith.toml for the design, with the DE2's pins.
    #[allow(dead_code)]
    pub fn manifest(&self, de2_pins: &str) -> String {
        format!(
            "# A random memory ({:?}).\n[project]\nname = \"{}\"\ntop = \"top\"\nsources = [\"src/*.v\"]\n\n\
             [target]\ndevice = \"EP2C35F672C6\"\n\n{de2_pins}\n[clocks]\nCLOCK_50 = \"50 MHz\"\nCLOCK_27 = \"27 MHz\"\n",
            self.kind,
            self.name()
        )
    }
}
