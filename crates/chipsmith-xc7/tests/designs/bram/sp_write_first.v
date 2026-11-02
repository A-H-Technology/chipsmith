// Single port, 512 x 36, a read of the word being written returns the new
// data (WRITE_FIRST), under a port enable.
module sp_write_first (input clk, input en, input we, input [8:0] a, input [35:0] d, output reg [35:0] q);
  reg [35:0] mem [0:511];
  always @(posedge clk)
    if (en) begin
      if (we) begin
        mem[a] <= d;
        q <= d;
      end else
        q <= mem[a];
    end
endmodule
