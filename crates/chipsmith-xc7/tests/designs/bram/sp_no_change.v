// Single port, 2048 x 9, the read data held while writing (NO_CHANGE).
module sp_no_change (input clk, input we, input [10:0] a, input [8:0] d, output reg [8:0] q);
  reg [8:0] mem [0:2047];
  always @(posedge clk)
    if (we) mem[a] <= d;
    else q <= mem[a];
endmodule
