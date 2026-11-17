// Shift-register LUTs on the board, read over JTAG (USER1;
// `cargo run --release -p chipsmith-jtag --example xc7_srl`).
//
// A 31-bit LFSR's sequence goes through delay lines that synthesis turns
// into SRLC32Es (`srl` in the build's summary), and each line's output is
// checked against a second copy of the LFSR started that many steps
// later:
//
// - `a`: 100 stages on the rising edge (registers at both ends, three
//   SRLs between, the 64-stage ones in series);
// - `b`: 37 stages, shifting only on enabled cycles (every third), its
//   checker enabled alike;
// - `c`: 20 stages on the falling edge;
// - `t`: an SRLC32E instance read at an address that sweeps 0 to 31,
//   checked against a register copy of the same 32 stages.
//
// USER1: "SR", then 8-bit saturating error counts for a, b, c and t, then
// a 16-bit count of checks (in thousands).
module zybo_srl(input clk, output [3:0] led);
  function [30:0] step(input [30:0] s);
    step = {s[29:0], s[30] ^ s[27]};
  endfunction

  reg [30:0] lfsr = 31'h1;
  always @(posedge clk) lfsr <= step(lfsr);
  wire bit_in = lfsr[30];

  // a: 100 stages.
  reg [99:0] a = 0;
  always @(posedge clk) a <= {a[98:0], bit_in};
  reg [30:0] a_ref = 31'h1;
  reg [7:0] warm = 0;
  always @(posedge clk) begin
    if (warm != 8'hff) warm <= warm + 1'b1;
    if (warm >= 8'd100) a_ref <= step(a_ref);
  end

  // b: 37 stages on every third cycle.
  reg [1:0] third = 0;
  always @(posedge clk) third <= third == 2 ? 2'd0 : third + 1'b1;
  wire en = third == 0;
  reg [36:0] b = 0;
  reg [30:0] b_lfsr = 31'h1, b_ref = 31'h1;
  reg [5:0] b_warm = 0;
  always @(posedge clk) if (en) begin
    b <= {b[35:0], b_lfsr[30]};
    b_lfsr <= step(b_lfsr);
    if (b_warm != 6'd63) b_warm <= b_warm + 1'b1;
    if (b_warm >= 6'd37) b_ref <= step(b_ref);
  end

  // c: 20 stages on the falling edge, fed from the rising-edge LFSR.
  reg [19:0] c = 0;
  reg [30:0] c_lfsr = 31'h1, c_ref = 31'h1;
  reg [5:0] c_warm = 0;
  always @(negedge clk) begin
    c <= {c[18:0], c_lfsr[30]};
    c_lfsr <= step(c_lfsr);
    if (c_warm != 6'd63) c_warm <= c_warm + 1'b1;
    if (c_warm >= 6'd20) c_ref <= step(c_ref);
  end

  // t: an SRLC32E at a sweeping address against a register copy.
  reg [31:0] shadow = 0;
  reg [4:0] tap = 0;
  wire t_q;
  always @(posedge clk) begin
    shadow <= {shadow[30:0], bit_in};
    tap <= tap + 1'b1;
  end
  SRLC32E t_srl (.CLK(clk), .CE(1'b1), .D(bit_in), .A(tap), .Q(t_q));

  reg [7:0] e_a = 0, e_b = 0, e_c = 0, e_t = 0;
  reg [9:0] sub = 0;
  reg [15:0] checks = 0;
  reg b_check = 0;
  always @(posedge clk) begin
    if (warm == 8'hff) begin
      if (a[99] != a_ref[30] && e_a != 8'hff) e_a <= e_a + 1'b1;
      if (t_q != shadow[tap] && e_t != 8'hff) e_t <= e_t + 1'b1;
      if (b_check && b[36] != b_ref[30] && e_b != 8'hff) e_b <= e_b + 1'b1;
      if (c[19] != c_ref[30] && e_c != 8'hff) e_c <= e_c + 1'b1;
      sub <= sub == 999 ? 10'd0 : sub + 1'b1;
      if (sub == 999) checks <= checks + 1'b1;
    end
    // b's line and checker move on enabled cycles: compare the cycle after.
    b_check <= en && b_warm == 6'd63;
  end
  assign led = {e_t != 0, e_c != 0, e_b != 0, e_a != 0};

  wire tck, sel, capture, shift, tdi;
  reg [63:0] dr = 0;
  BSCANE2 #(.JTAG_CHAIN(1)) bscan (
    .TCK(tck), .SEL(sel), .CAPTURE(capture), .SHIFT(shift), .TDI(tdi),
    .TDO(dr[0])
  );
  always @(posedge tck)
    if (sel) begin
      if (capture) dr <= {"SR", e_a, e_b, e_c, e_t, checks};
      else if (shift) dr <= {tdi, dr[63:1]};
    end
endmodule
