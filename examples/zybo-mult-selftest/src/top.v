// examples/mult-selftest on the Zybo: the same nine products of
// pseudo-random operands from a 64-bit LFSR, each checked against a
// bit-serial shift-and-add reference in LUTs (64 cycles per product), on
// DSP48E1s instead of the DE2's multipliers. The slots run in turn, 128
// cycles each; after the ninth the pass count steps and the LFSR moves on.
// USER1 (BSCANE2, as zybo-jtag-counter) holds "MUL!", the passes and the
// products that differed (saturating): a good board shows the passes
// rising and 0 errors (`cargo run --release -p chipsmith-jtag --example
// xc7_user1`). led[0] lights on an error, led[3:1] show the pass count's
// top bits. The test logic runs at 62.5 MHz (below).
//
//   0  9 x 9 unsigned              (one DSP)
//   1  9 x 9 signed
//   2  18 x 18 unsigned            (two DSPs: 18 bits exceed B's 17 unsigned)
//   3  18 x 18 signed              (one DSP)
//   4  12 signed x 7 unsigned      (one DSP, mixed signs, narrow)
//   5  9 x 9 unsigned, operands registered in the DSP (AREG, BREG)
//   6  18 x 18 signed, product registered in the DSP (PREG)
//   7  32 x 32 unsigned to 64 bits (four DSPs and adders)
//   8  24 x 24 signed to 48 bits   (two DSPs and adders)
module top(input clk, output [3:0] led);
  // The test runs at 62.5 MHz, half the Zybo's clock: the 32 x 32 product
  // (four DSPs and their sum) takes about 14 ns, and the operands hold
  // still all slot long, which the DE2's test relies on, but chipsmith has
  // no multicycle constraints. A register toggling makes the clock (into
  // a BUFG, as every clock).
  reg half = 0;
  always @(posedge clk) half <= ~half;
  wire tclk = half;
  // x^64 + x^63 + x^61 + x^60 + 1, maximal.
  reg [63:0] lfsr = 64'h9E3779B97F4A7C15;
  wire [31:0] a = lfsr[31:0], b = lfsr[63:32];
  reg [3:0] mode = 0;
  reg [6:0] step = 0;
  reg [15:0] pass = 0;
  reg [15:0] errors = 0;

  wire [17:0] p0 = a[8:0] * b[8:0];
  wire signed [8:0] a1 = a[8:0], b1 = b[8:0];
  wire signed [17:0] p1 = a1 * b1;
  wire [35:0] p2 = a[17:0] * b[17:0];
  wire signed [17:0] a3 = a[17:0], b3 = b[17:0];
  wire signed [35:0] p3 = a3 * b3;
  wire signed [11:0] a4 = a[11:0];
  wire [6:0] b4 = b[6:0];
  wire signed [18:0] p4 = a4 * $signed({1'b0, b4});
  // Registers read by the product alone move into the block.
  reg [8:0] ra5 = 0, rb5 = 0;
  always @(posedge tclk) begin ra5 <= a[8:0]; rb5 <= b[8:0]; end
  wire [17:0] p5 = ra5 * rb5;
  // A product registered into the block (its operands are wires shared
  // with the reference, so they stay outside; b6 differs from b3 so the
  // two products aren't merged).
  wire signed [17:0] b6 = b3 ^ 18'sh2AAAA;
  reg signed [35:0] p6 = 0;
  always @(posedge tclk) p6 <= a3 * b6;
  wire [63:0] p7 = a * b;
  wire signed [23:0] a8 = a[23:0], b8 = b[23:0];
  wire signed [47:0] p8 = a8 * b8;

  // The slot's operands, extended to 64 bits with their signs, and what
  // the block made, extended the same way: the low 64 bits of the
  // products agree whatever the signs.
  reg [63:0] ra, rb, want;
  always @* begin
    case (mode)
      4'd0: begin ra = a[8:0]; rb = b[8:0]; want = p0; end
      4'd1: begin ra = {{55{a[8]}}, a[8:0]}; rb = {{55{b[8]}}, b[8:0]}; want = {{46{p1[17]}}, p1}; end
      4'd2: begin ra = a[17:0]; rb = b[17:0]; want = p2; end
      4'd3: begin ra = {{46{a[17]}}, a[17:0]}; rb = {{46{b[17]}}, b[17:0]}; want = {{28{p3[35]}}, p3}; end
      4'd4: begin ra = {{52{a[11]}}, a[11:0]}; rb = b[6:0]; want = {{45{p4[18]}}, p4}; end
      4'd5: begin ra = a[8:0]; rb = b[8:0]; want = p5; end
      4'd6: begin ra = {{46{a[17]}}, a[17:0]}; rb = {{46{b6[17]}}, b6}; want = {{28{p6[35]}}, p6}; end
      4'd7: begin ra = a; rb = b; want = p7; end
      default: begin ra = {{40{a[23]}}, a[23:0]}; rb = {{40{b[23]}}, b[23:0]}; want = {{16{p8[47]}}, p8}; end
    endcase
  end

  // The reference: shift and add, one operand bit a cycle, modulo 2^64.
  // The block's product is registered first (it is stable all slot long),
  // so the compare isn't timed through the multipliers and adder trees.
  reg [63:0] ma = 0, mb = 0, acc = 0, want_q = 0;
  wire bad = step == 7'd70 && acc != want_q;
  always @(posedge tclk) begin
    step <= step + 1'b1;
    want_q <= want;
    if (step == 0) begin
      ma <= ra;
      mb <= rb;
      acc <= 0;
    end else if (step <= 7'd64) begin
      if (mb[0]) acc <= acc + ma;
      ma <= {ma[62:0], 1'b0};
      mb <= {1'b0, mb[63:1]};
    end
    if (bad && ~&errors) errors <= errors + 1'b1;
    if (&step) begin
      lfsr <= {lfsr[62:0], lfsr[63] ^ lfsr[62] ^ lfsr[60] ^ lfsr[59]};
      if (mode == 4'd8) begin
        mode <= 0;
        pass <= pass + 1'b1;
      end else begin
        mode <= mode + 1'b1;
      end
    end
  end
  assign led = {pass[15:13], errors != 0};

  wire tck, sel, capture, shift, tdi;
  reg [63:0] dr = 0;
  BSCANE2 #(.JTAG_CHAIN(1)) bscan (
    .TCK(tck), .SEL(sel), .CAPTURE(capture), .SHIFT(shift), .TDI(tdi),
    .TDO(dr[0])
  );
  always @(posedge tck)
    if (sel) begin
      if (capture) dr <= {"MUL!", pass, errors};
      else if (shift) dr <= {tdi, dr[63:1]};
    end
endmodule
