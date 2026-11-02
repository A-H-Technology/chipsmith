// Deeper than a block: 16384 x 4.
module deep (input clk, input we, input [13:0] wa, input [13:0] ra, input [3:0] d, output reg [3:0] q);
  reg [3:0] mem [0:16383];
  always @(posedge clk) begin
    if (we) mem[wa] <= d;
    q <= mem[ra];
  end
endmodule
