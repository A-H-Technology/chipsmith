// Latches as the LE can't hold them: plain, active-low, with an
// asynchronous reset, powering up 1, wide, and one feeding a register.
module top(input clk, input en, input n_en, input rst, input [3:0] d, output reg [3:0] q1,
           output reg q2, output reg [1:0] q3, output reg q4, output reg [3:0] r);
  always @* if (en) q1 = d;
  always @* if (!n_en) q2 = d[0] ^ d[1];
  always @* if (rst) q3 = 2'b10; else if (en & d[3]) q3 = d[1:0];
  initial q4 = 1'b1;
  always @* if (en & n_en) q4 = d[2];
  always @(posedge clk) r <= q1 + {3'b0, q2};
endmodule
