// Clocks from a PLL, through the bitstream: 125 MHz, 62.5 MHz 4 ns (90
// degrees) late, 250 MHz, and 125 MHz high a quarter of the time, each
// clocking a counter of its own; the quarter-duty clock's register is on
// its falling edge. A fifth output, 250 MHz 2.5 ns late, samples the
// 62.5 MHz and quarter-duty clocks as data, so their phase and duty cycle
// show at a 4 ns resolution rather than the co-simulation's sampling of
// the outputs. An MMCM beside it makes 150 MHz on a fractional divider
// (6.625 of a 993.75 MHz VCO) and 66.25 MHz on a whole one, its feedback
// through a BUFG. (crates/chipsmith/tests/xc7_bitstream.rs, `cmt`.)
module cmt(input clk, input [3:0] d, output [20:0] q);
  wire c0, c1, c2, c3, c4, locked;
  XC7_PLL #(
    .INCLK0_INPUT_FREQUENCY(8000),
    .CLK0_MULTIPLY_BY(1), .CLK0_DIVIDE_BY(1),
    .CLK1_MULTIPLY_BY(1), .CLK1_DIVIDE_BY(2), .CLK1_PHASE_SHIFT(4000),
    .CLK2_MULTIPLY_BY(2), .CLK2_DIVIDE_BY(1),
    .CLK3_MULTIPLY_BY(1), .CLK3_DIVIDE_BY(1), .CLK3_DUTY_CYCLE(25),
    .CLK4_MULTIPLY_BY(2), .CLK4_DIVIDE_BY(1), .CLK4_PHASE_SHIFT(2500)
  ) pll (.INCLK(clk), .ARESET(1'b0), .CLK0(c0), .CLK1(c1), .CLK2(c2), .CLK3(c3), .CLK4(c4),
         .LOCKED(locked));
  reg [2:0] a = 0, b = 0, c = 0, e = 0;
  always @(posedge c0) if (locked) a <= a + d[0];
  always @(posedge c1) if (locked) b <= b + 1'b1 + d[1];
  always @(posedge c2) if (locked) c <= c + 1'b1;
  always @(negedge c3) if (locked) e <= e + d[3] + 1'b1;
  reg [1:0] s = 0;
  always @(posedge c4) if (locked) s <= {c3, c1};
  wire m0, m1, mlocked;
  XC7_MMCM #(
    .INCLK0_INPUT_FREQUENCY(8000),
    .CLK0_MULTIPLY_BY(6), .CLK0_DIVIDE_BY(5),
    .CLK1_MULTIPLY_BY(53), .CLK1_DIVIDE_BY(100)
  ) mmcm (.INCLK(clk), .ARESET(1'b0), .CLK0(m0), .CLK1(m1), .LOCKED(mlocked));
  reg [2:0] f = 0, g = 0;
  reg t = 0;
  always @(posedge m0) if (mlocked) f <= f + d[2] + 1'b1;
  always @(posedge m1) if (mlocked) g <= g + 1'b1;
  always @(posedge c4) if (locked) t <= m0;
  assign q = {t, g, f, s, e, c, b, a};
endmodule
