// 25 x 18 signed, three stages (operands, product, its register again:
// AREG/BREG, MREG, PREG) with a synchronous reset on the last.
module mul_pipe (input clk, input rst, input signed [24:0] a, input signed [17:0] b, output reg signed [42:0] q);
  reg signed [24:0] ra;
  reg signed [17:0] rb;
  reg signed [42:0] m;
  always @(posedge clk) begin
    ra <= a;
    rb <= b;
    m <= ra * rb;
    if (rst) q <= 0;
    else q <= m;
  end
endmodule
