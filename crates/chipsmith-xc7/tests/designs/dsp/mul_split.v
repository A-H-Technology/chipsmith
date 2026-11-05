// 26 x 22 signed, both operands wider than a limb: four DSP48E1s (A in 24
// unsigned + 2 signed bits, B in 17 + 5) and adders, within the Zybo's balls.
module mul_split (input clk, input signed [25:0] a, input signed [21:0] b, output reg signed [47:0] q);
  reg signed [25:0] ra;
  reg signed [21:0] rb;
  always @(posedge clk) begin
    ra <= a;
    rb <= b;
    q <= ra * rb;
  end
endmodule
