// Registers the LE register can't be: asynchronous set and reset on one
// bit, an asynchronous load of a signal, an asynchronous reset to 1 that
// powers up 0, with enables and active-low controls.
module top(input clk, input s, input r, input n_r, input ld, input en, input [3:0] ad,
           input [3:0] d, output reg q1, output reg q2, output reg [3:0] q3,
           output reg [3:0] q4, output reg q5);
  always @(posedge clk or posedge s or posedge r)
    if (r) q1 <= 1'b0; else if (s) q1 <= 1'b1; else q1 <= d[0];
  always @(posedge clk or posedge s or negedge n_r)
    if (!n_r) q2 <= 1'b0; else if (s) q2 <= 1'b1; else if (en) q2 <= d[1];
  always @(posedge clk or posedge ld)
    if (ld) q3 <= ad; else q3 <= d;
  always @(posedge clk or posedge ld)
    if (ld) q4 <= ad ^ d; else if (en) q4 <= q4 + 4'd1;
  initial q5 = 1'b0;
  always @(posedge clk or posedge r)
    if (r) q5 <= 1'b1; else q5 <= d[3];
endmodule
