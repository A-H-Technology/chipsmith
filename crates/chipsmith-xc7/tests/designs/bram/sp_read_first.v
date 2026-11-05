// Single port, 1024 x 18, a read of the word being written returns the old
// data (READ_FIRST).
module sp_read_first (input clk, input we, input [9:0] a, input [17:0] d, output reg [17:0] q);
  reg [17:0] mem [0:1023];
  always @(posedge clk) begin
    if (we) mem[a] <= d;
    q <= mem[a];
  end
endmodule
