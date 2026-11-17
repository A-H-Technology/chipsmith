// MUXF7s and MUXF8s a design instantiates, as Vivado-written code does: a
// 16:1 multiplexer of four LUT4-sized 4:1 trees, registered.
module muxf(input clk, input [15:0] d, input [3:0] s, output reg q);
  wire [3:0] m4;
  genvar i;
  generate for (i = 0; i < 4; i = i + 1) begin : quarter
    assign m4[i] = d[4 * i + s[1:0]];
  end endgenerate
  wire f7a, f7b, f8;
  MUXF7 lo (.I0(m4[0]), .I1(m4[1]), .S(s[2]), .O(f7a));
  MUXF7 hi (.I0(m4[2]), .I1(m4[3]), .S(s[2]), .O(f7b));
  MUXF8 top (.I0(f7a), .I1(f7b), .S(s[3]), .O(f8));
  always @(posedge clk) q <= f8;
endmodule
