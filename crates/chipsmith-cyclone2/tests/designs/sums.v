// Sums of more than two terms: signed and unsigned, subtractions, narrower
// and wider terms, a partial sum read elsewhere too (not merged), and an
// all-negative sum.
module top(input clk, input [7:0] a, input [7:0] b, input [5:0] c, input signed [3:0] d,
           input signed [7:0] e, output reg [9:0] s1, output reg [7:0] s2,
           output reg signed [8:0] s3, output reg [7:0] s4, output reg [7:0] p,
           output reg [7:0] s5);
  wire [7:0] ab = a + b;
  always @(posedge clk) begin
    s1 <= a + b + c + 10'd3;
    s2 <= a - b + c - a[3:0];
    s3 <= e + d - $signed({1'b0, c}) + d;
    s4 <= ab + c + a;
    p <= ab;
    s5 <= 8'd0 - a - b - c;
  end
endmodule
