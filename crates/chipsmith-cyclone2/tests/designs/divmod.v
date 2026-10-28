// Division and modulo: unsigned and signed, by a signal, by constants
// (a power of two, an odd number), registered so the check sees state.
module top(input clk, input [7:0] a, input [5:0] b, input signed [7:0] sa,
           input signed [4:0] sb, output reg [7:0] q, output reg [5:0] r,
           output reg signed [7:0] sq, output reg signed [4:0] sr,
           output reg [7:0] k, output reg [3:0] kr, output reg [9:0] p);
  always @(posedge clk) begin
    q <= a / b;
    r <= a % b;
    sq <= sa / sb;
    sr <= sa % sb;
    k <= a / 8'd10;
    kr <= a % 4'd7;
    p <= {a, 2'b0} / 10'd16 + a % 8'd32;
  end
endmodule
