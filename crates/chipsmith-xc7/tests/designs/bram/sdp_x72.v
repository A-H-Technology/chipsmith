// Simple dual port, 512 x 72, one clock: one RAMB36E1 in SDP mode.
module sdp_x72 (input clk, input we, input [8:0] wa, input [8:0] ra, input [71:0] d, output reg [71:0] q);
  reg [71:0] mem [0:511];
  always @(posedge clk) begin
    if (we) mem[wa] <= d;
    q <= mem[ra];
  end
endmodule
