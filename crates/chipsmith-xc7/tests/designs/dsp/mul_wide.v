// 32 x 32 unsigned, 64-bit product: several DSP48E1s and adders.
module mul_wide (input clk, input [31:0] a, input [31:0] b, output reg [63:0] q);
  reg [31:0] ra, rb;
  always @(posedge clk) begin
    ra <= a;
    rb <= b;
    q <= ra * rb;
  end
endmodule
