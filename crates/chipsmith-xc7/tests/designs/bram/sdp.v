// Simple dual port, 512 x 32, one clock, the read seeing old data.
module sdp (input clk, input we, input [8:0] wa, input [8:0] ra, input [31:0] d, output reg [31:0] q);
  reg [31:0] mem [0:511];
  always @(posedge clk) begin
    if (we) mem[wa] <= d;
    q <= mem[ra];
  end
endmodule
