// A combinational 18 x 18 signed product: one DSP48E1, no registers.
module mul_comb (input clk, input signed [17:0] a, input signed [17:0] b, output reg signed [35:0] q);
  wire signed [35:0] p = a * b;
  always @(posedge clk) q <= p;
endmodule
