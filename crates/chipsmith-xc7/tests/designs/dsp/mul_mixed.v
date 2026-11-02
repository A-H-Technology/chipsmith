// Mixed signedness through $signed: a signed 12 x unsigned 9 (sign
// extended as Verilog has it), and a 7 x 5 product of narrow operands.
module mul_mixed (input clk, input signed [11:0] a, input [8:0] b, input [6:0] c, input [4:0] d,
                  output reg signed [21:0] q, output reg [11:0] r);
  always @(posedge clk) begin
    q <= a * $signed({1'b0, b});
    r <= c * d;
  end
endmodule
