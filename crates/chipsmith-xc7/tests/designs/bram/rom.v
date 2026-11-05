// A 1024 x 16 ROM with contents, read under an enable.
module rom (input clk, input en, input [9:0] a, output reg [15:0] q);
  reg [15:0] mem [0:1023];
  integer i;
  initial for (i = 0; i < 1024; i = i + 1) mem[i] = (i * 40503) ^ (i >> 3);
  always @(posedge clk) if (en) q <= mem[a];
endmodule
