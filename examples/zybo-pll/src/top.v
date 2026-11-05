// Two counters on two clocks from one PLL: 125 MHz (CLK0) and 62.5 MHz
// 4 ns (90 degrees of it) after (CLK1), from the Zybo's 125 MHz. led[1:0]
// are the 125 MHz counter's top bits, led[3:2] the 62.5 MHz one's: on the
// board LD0 blinks at twice LD2's rate. LOCKED holds both counters until
// the PLL has locked.
module top(input clk, output [3:0] led);
  wire fast, slow, locked;
  XC7_PLL #(
    .INCLK0_INPUT_FREQUENCY(8000),
    .CLK0_MULTIPLY_BY(1), .CLK0_DIVIDE_BY(1),
    .CLK1_MULTIPLY_BY(1), .CLK1_DIVIDE_BY(2), .CLK1_PHASE_SHIFT(4000)
  ) pll (.INCLK(clk), .ARESET(1'b0), .CLK0(fast), .CLK1(slow), .LOCKED(locked));
  reg [26:0] a = 0;
  reg [25:0] b = 0;
  always @(posedge fast) if (locked) a <= a + 1'b1;
  always @(posedge slow) if (locked) b <= b + 1'b1;
  assign led = {b[24], b[25], a[25], a[26]};
endmodule
