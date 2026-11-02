// Memories of the RAM32M's shapes: a 32 x 8 with one read port, a 16 x 5
// with two and contents, a 32 x 4 with three on a falling edge, and a
// 32 x 2 whose read port is the write address.
module lutram(input clk, input we, input [4:0] wa, input [4:0] ra, input [4:0] rb,
              input [4:0] rc, input [7:0] wd,
              output [7:0] q0, output [4:0] q1, output [4:0] q2, output [3:0] q3,
              output [3:0] q4, output [3:0] q5, output [1:0] q6);
  reg [7:0] m0 [0:31];
  reg [4:0] m1 [0:15];
  reg [3:0] m2 [0:31];
  reg [1:0] m3 [0:31];
  integer i;
  initial for (i = 0; i < 16; i = i + 1) m1[i] = i * 3;
  always @(posedge clk) if (we) m0[wa] <= wd;
  always @(posedge clk) if (wd[7]) m1[wa[3:0]] <= wd[4:0];
  always @(negedge clk) if (we) m2[wa] <= wd[3:0] ^ wd[7:4];
  always @(posedge clk) if (we) m3[wa] <= wd[1:0];
  assign q0 = m0[ra];
  assign q1 = m1[ra[3:0]];
  assign q2 = m1[rb[3:0]];
  assign q3 = m2[ra];
  assign q4 = m2[rb];
  assign q5 = m2[rc];
  assign q6 = m3[wa];
endmodule
