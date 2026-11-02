// The read data registered again (the block's output register), with its
// own enable and a synchronous reset; the first register with contents.
module outreg (input clk, input we, input ce, input rst, input [10:0] wa, input [10:0] ra,
               input [7:0] d, output reg [7:0] q);
  reg [7:0] mem [0:2047];
  reg [7:0] r;
  integer i;
  initial for (i = 0; i < 2048; i = i + 1) mem[i] = i[7:0] ^ i[10:3];
  always @(posedge clk) begin
    if (we) mem[wa] <= d;
    r <= mem[ra];
    if (rst) q <= 8'h00;
    else if (ce) q <= r;
  end
endmodule
