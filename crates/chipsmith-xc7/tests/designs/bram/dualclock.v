// Written on one clock, read on another: 1024 x 16, with a read enable.
module dualclock (input clk, input clk2, input we, input re, input [9:0] wa, input [9:0] ra,
                  input [15:0] d, output reg [15:0] q);
  reg [15:0] mem [0:1023];
  always @(posedge clk) if (we) mem[wa] <= d;
  always @(posedge clk2) if (re) q <= mem[ra];
endmodule
