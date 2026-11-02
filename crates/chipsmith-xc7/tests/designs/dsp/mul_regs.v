// 24 x 17 unsigned with registered operands and product (AREG, BREG,
// PREG), each register with an enable.
module mul_regs (input clk, input ce, input [23:0] a, input [16:0] b, output reg [40:0] q);
  reg [23:0] ra;
  reg [16:0] rb;
  always @(posedge clk) if (ce) begin
    ra <= a;
    rb <= b;
    q <= ra * rb;
  end
endmodule
