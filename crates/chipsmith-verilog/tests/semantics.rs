//! The subtle parts of Verilog, one small design each, proven equivalent
//! to yosys' reading of it (see tests/yosys.rs): expression sizing and
//! signedness, blocking and non-blocking order, case priority, register,
//! latch and memory inference, generate scoping, functions and tasks.
//!
//! Needs yosys on PATH: run in the devshell.

mod common;

use common::{check, scratch, Case, Lib};

/// Proves `src` (with top module `top`) equivalent under both front ends.
fn equiv(name: &str, src: &str) {
    let dir = scratch(&format!("sem-{name}"));
    let file = dir.join(format!("{name}.v"));
    std::fs::write(&file, src).unwrap();
    let case = Case {
        name: format!("sem-{name}"),
        files: vec![file],
        top: "top".into(),
        lib: Lib::Cyclone2,
    };
    if let Err(e) = check(&case) {
        panic!("{name}: {e}");
    }
}

/// IEEE 1364-2005 §5.4/5.5: context-determined operands take the widest
/// width; one unsigned operand makes the whole context unsigned.
#[test]
fn expression_sizing_and_signedness() {
    equiv(
        "sizing",
        r#"
module top(input [3:0] a, input [7:0] b, input signed [3:0] sa, input signed [7:0] sb,
           input [2:0] sh, output [15:0] y0, y1, y2, y3, y4, y5, y6, y7, y8, y9,
           output [7:0] z0, z1, z2, z3, z4, z5, z6, z7, output w0, w1, w2, w3, w4, w5, w6, w7);
  assign y0 = a + b;                    // zero-extended to 16
  assign y1 = sa + sb;                  // sign-extended: both signed
  assign y2 = sa + b;                   // b unsigned: sa zero-extended
  assign y3 = $signed(a) + 1'b1;        // 1'b1 unsigned: zero-extends a
  assign y4 = $signed(a) + 1'sb1;       // signed: sign-extends
  assign y5 = sa * sb;
  assign y6 = -a;                       // 16-bit negation of zero-extended a
  assign y7 = ~sa;                      // sign-extended, then inverted
  assign y8 = {a, b} >>> 2;             // concatenation is unsigned
  assign y9 = sb >>> sh;                // arithmetic shift
  assign z0 = sb >>> 1;
  assign z1 = sa >>> 1;                 // 4-bit signed in 8-bit context: extended first
  assign z2 = b << sh;
  assign z3 = sb / 8'sd3;
  assign z4 = sb % 8'sd3;
  assign z5 = (sa ? a : b) + 1;
  assign z6 = sa < 0 ? -sa : sa;        // 0 is signed: a signed comparison
  assign z7 = 2 ** a;
  assign w0 = sa < sb;                  // signed compare
  assign w1 = sa < b;                   // unsigned compare (b unsigned)
  assign w2 = $unsigned(sa) > 4'd7;
  assign w3 = &a | ^b;
  assign w4 = a == 4'b1x01;
  assign w5 = a === 4'b1x01;
  assign w6 = !b && a;
  assign w7 = -3'sd1 > 3'sd0;
endmodule
"#,
    );
}

#[test]
fn constant_expressions() {
    equiv(
        "consts",
        r#"
module top(output [31:0] p0, p1, p2, p3, p4, p5, p6, p7, output [63:0] q);
  localparam signed [7:0] N = -8'sd5;
  localparam [7:0] U = N;               // the bits, unsigned
  localparam W = $clog2(1000);
  localparam X = 'hx;                   // unsized x fills its width
  function integer fact(input integer n);
    if (n <= 1) fact = 1; else fact = n * fact(n - 1);
  endfunction
  function [7:0] rev(input [7:0] v);
    integer i;
    for (i = 0; i < 8; i = i + 1) rev[i] = v[7 - i];
  endfunction
  assign p0 = N >>> 1;
  assign p1 = U >> 1;
  assign p2 = W;
  assign p3 = fact(10);
  assign p4 = rev(8'h1d);
  assign p5 = N / 2;
  assign p6 = {4{2'b10}};
  assign p7 = 7 % -3;
  assign q = 64'h1 << 40 | X[3:0];
endmodule
"#,
    );
}

/// Non-blocking assignments read the old value; blocking ones the new.
#[test]
fn blocking_and_nonblocking() {
    equiv(
        "blocking",
        r#"
module top(input clk, input [3:0] d, output reg [3:0] a, b, c, e, output reg [3:0] t);
  always @(posedge clk) begin
    a <= d;
    b <= a;          // the old a
    t = a + 1;       // blocking: t is the new value in this block
    c <= t;
    t = t + d;
    e <= t;
  end
endmodule
"#,
    );
    equiv(
        "swap",
        r#"
module top(input clk, input load, input [7:0] i, output reg [7:0] x, y);
  always @(posedge clk)
    if (load) begin x <= i; y <= ~i; end
    else begin x <= y; y <= x; end
endmodule
"#,
    );
}

/// Overlapping casez patterns: the first match wins.
#[test]
fn casez_priority() {
    equiv(
        "casez",
        r#"
module top(input [3:0] s, input [7:0] a, b, c, d, output reg [7:0] y, output reg [1:0] enc);
  always @* begin
    casez (s)
      4'b1???: y = a;
      4'b?1??: y = b;
      4'b??1?: y = c;
      default: y = d;
    endcase
    casex (s)
      4'b0001, 4'b001x: enc = 1;
      4'b01xx: enc = 2;
      4'b1xxx: enc = 3;
      default: enc = 0;
    endcase
  end
endmodule
"#,
    );
    equiv(
        "parallel_case",
        r#"
module top(input clk, input [2:0] f, output reg [1:0] q);
  always @(posedge clk) begin
    (* parallel_case *)
    case (1'b1)
      f[0]: q <= 1;
      f[1]: q <= 2;
      f[2]: q <= 3;
    endcase
  end
endmodule
"#,
    );
}

#[test]
fn async_resets() {
    equiv(
        "adff",
        r#"
module top(input clk, rst, rstn, input [3:0] d, input [3:0] ld,
           output reg [3:0] q0, q1, q2, q3, q4);
  always @(posedge clk or posedge rst)
    if (rst) q0 <= 4'h5; else q0 <= d;
  always @(posedge clk or negedge rstn)
    if (!rstn) q1 <= 0; else q1 <= q1 + d;
  always @(negedge clk, negedge rstn)
    if (rstn == 1'b0) begin q2 <= 4'ha; end else q2 <= ~d;
  // A reset value from a signal: an asynchronous load.
  always @(posedge clk or posedge rst)
    if (rst) q3 <= ld; else q3 <= d;
  // A set and a reset.
  always @(posedge clk or posedge rst or negedge rstn)
    if (rst) q4 <= 0;
    else if (!rstn) q4 <= 4'b1010;
    else q4 <= d;
endmodule
"#,
    );
}

#[test]
fn latches() {
    equiv(
        "latch",
        r#"
module top(input en, input [3:0] d, input s, output reg [3:0] q, output reg [3:0] r);
  always @* if (en) q = d;
  always @* begin
    r[1:0] = d[1:0];
    if (s) r[3:2] = d[3:2];
  end
endmodule
"#,
    );
}

/// Read-during-write: a read in the writing block sees the old word; a
/// read through a registered address sees the new one.
#[test]
fn memories() {
    equiv(
        "mem",
        r#"
module top(input clk, input we, input [3:0] wa, ra, input [7:0] wd, input [1:0] be,
           output reg [7:0] old, output [7:0] new_, output reg [7:0] q2);
  reg [7:0] m [0:15];
  reg [3:0] ra_q;
  always @(posedge clk) begin
    if (we) m[wa] <= wd;
    old <= m[ra];
    ra_q <= ra;
  end
  assign new_ = m[ra_q];

  // Byte-wise writes, two write ports in one block (the later wins).
  reg [7:0] n [0:15];
  always @(posedge clk) begin
    if (be[0]) n[wa][3:0] <= wd[3:0];
    if (be[1]) n[wa][7:4] <= wd[7:4];
    if (we) n[ra] <= 8'h00;
    q2 <= n[ra];
  end
endmodule
"#,
    );
}

#[test]
fn memory_initial_contents() {
    let dir = scratch("sem-readmem");
    std::fs::write(
        dir.join("rom.hex"),
        "// contents\n01 23 45 67\n@6 89 ab\n/* tail */ cd\n",
    )
    .unwrap();
    equiv(
        "readmem",
        &format!(
            r#"
module top(input clk, input [2:0] a, output reg [7:0] q, output reg [7:0] r);
  reg [7:0] rom [0:7];
  reg [7:0] tab [0:7];
  integer i;
  initial begin
    $readmemh("{}", rom);
    for (i = 0; i < 8; i = i + 1) tab[i] = i * 37;
  end
  always @(posedge clk) begin q <= rom[a]; r <= tab[a]; end
endmodule
"#,
            dir.join("rom.hex").display()
        ),
    );
}

/// Memories written from logic or with `=` become registers.
#[test]
fn memory_to_registers() {
    equiv(
        "mem2reg",
        r#"
module top(input clk, input [1:0] a, b, input [3:0] d, output reg [3:0] y, output [3:0] z);
  reg [3:0] m [0:3];
  always @(posedge clk) begin
    m[a] = d;
    y <= m[b];
  end
  reg [3:0] c [0:3];
  integer k;
  always @* begin
    for (k = 0; k < 4; k = k + 1) c[k] = d + k;
  end
  assign z = c[b];
endmodule
"#,
    );
}

#[test]
fn variable_indices() {
    equiv(
        "indices",
        r#"
module top(input clk, input [2:0] i, input [3:0] j, input d, input [1:0] d2,
           input [15:0] v, input [0:15] u, output [3:0] s0, s1, s2, s3, output b0, b1,
           output reg [7:0] w, output reg [0:7] x);
  assign s0 = v[j +: 4];
  assign s1 = v[j -: 4];
  assign s2 = u[j +: 4];
  assign s3 = u[j -: 4];
  assign b0 = v[j];
  assign b1 = u[j];
  always @(posedge clk) begin
    w[i] <= d;
    x[i +: 2] <= d2;
  end
endmodule
"#,
    );
}

/// Names inside generate blocks are their own; a loop's blocks are
/// `name[i]`, and the genvar is a constant in each.
#[test]
fn generate_scoping() {
    equiv(
        "generate",
        r#"
module top(input clk, input [7:0] a, output [7:0] y, output [3:0] z, output w);
  parameter MODE = 2;
  genvar g;
  generate
    for (g = 0; g < 8; g = g + 1) begin : bit_
      wire t;
      assign t = a[g] ^ a[(g + 1) % 8];
      if (g % 2 == 0) begin : even
        wire u = ~t;
        assign y[g] = u;
      end else begin : odd
        assign y[g] = t;
      end
    end
  endgenerate
  // The same name in two blocks.
  for (g = 0; g < 4; g = g + 1) begin : q
    reg r;
    always @(posedge clk) r <= bit_[g].t;
    assign z[g] = r;
  end
  case (MODE)
    1: assign w = 1'b0;
    2: begin : two assign w = ^a; end
    default: assign w = 1'b1;
  endcase
endmodule
"#,
    );
}

#[test]
fn functions_and_tasks() {
    equiv(
        "functions",
        r#"
module top(input clk, input [7:0] a, b, output reg [7:0] s, output [3:0] n, output reg [7:0] mx, mn);
  function automatic [3:0] popcount(input [7:0] v);
    integer i;
    begin
      popcount = 0;
      for (i = 0; i < 8; i = i + 1) popcount = popcount + v[i];
    end
  endfunction
  function [7:0] sat_add(input [7:0] x, input [7:0] y);
    reg [8:0] t;
    begin
      t = x + y;
      sat_add = t[8] ? 8'hff : t[7:0];
    end
  endfunction
  task order(input [7:0] x, input [7:0] y, output [7:0] hi, output [7:0] lo);
    if (x > y) begin hi = x; lo = y; end
    else begin hi = y; lo = x; end
  endtask
  assign n = popcount(a ^ b);
  always @(posedge clk) begin
    s <= sat_add(a, b);
    order(a, b, mx, mn);
  end
endmodule
"#,
    );
}

#[test]
fn hierarchy_and_parameters() {
    equiv(
        "hierarchy",
        r#"
module add #(parameter W = 4, parameter signed [7:0] K = 1) (input [W-1:0] a, output [W:0] y);
  localparam KK = K * 2;
  assign y = a + KK;
endmodule
module cnt(input clk, output reg [3:0] q);
  parameter STEP = 1;
  initial q = 0;
  always @(posedge clk) q <= q + STEP;
endmodule
module top(input clk, input [7:0] a, output [8:0] y0, output [4:0] y1, output [3:0] c0, c1, output [2:0] narrow);
  add #(.W(8), .K(-3)) u0 (.a(a), .y(y0));
  add u1 (a[3:0], y1);
  add #(8) u2 (.a(a), .y(narrow));     // the output is truncated
  cnt c (.clk(clk), .q(c0));
  cnt d (.clk(clk), .q(c1));
  defparam d.STEP = 3;
endmodule
"#,
    );
}

#[test]
fn tristates() {
    equiv(
        "tristate",
        r#"
module top(input oe, input oe2, input [3:0] d, inout [3:0] io, output [3:0] r, inout od);
  assign io = oe ? d : 4'bz;
  assign r = io;
  // Two drivers on one net, never both on.
  assign od = oe2 ? 1'b0 : 1'bz;
  assign od = oe ? d[0] : 1'bz;
endmodule
"#,
    );
}

#[test]
fn preprocessor() {
    equiv(
        "preproc",
        r#"
`timescale 1ns/1ps
`define W 4
`define ADD(x, y) ((x) + (y))
`ifdef UNDEFINED
  this is not Verilog
`elsif W
module top(input [`W-1:0] a, b, output [`W:0] y);
  assign y = `ADD(a, b);
endmodule
`endif
"#,
    );
}

#[test]
fn gate_primitives() {
    equiv(
        "gates",
        r#"
module top(input a, b, c, en, output y0, y1, y2, y3, y4, y5);
  and g0 (y0, a, b, c);
  nor (y1, a, b);
  xnor g2 (y2, a, c);
  not (y3, y4, a);
  bufif1 (y5, b, en);
endmodule
"#,
    );
}
