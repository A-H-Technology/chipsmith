// A small memory read synchronously: 64 x 8.
module tiny (input clk, input we, input [5:0] wa, input [5:0] ra, input [7:0] d, output reg [7:0] q);
  reg [7:0] mem [0:63];
  always @(posedge clk) begin
    if (we) mem[wa] <= d;
    q <= mem[ra];
  end
endmodule
