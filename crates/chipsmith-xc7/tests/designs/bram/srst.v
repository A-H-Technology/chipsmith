// A synchronous reset and an enable on the read data (the block's latch
// reset, SRVAL) with an initial value.
module srst (input clk, input we, input en, input rst, input [9:0] wa, input [9:0] ra,
             input [17:0] d, output reg [17:0] q);
  reg [17:0] mem [0:1023];
  initial q = 18'h2a5a5;
  always @(posedge clk) begin
    if (we) mem[wa] <= d;
    if (en) begin
      if (rst) q <= 18'h15a5a;
      else q <= mem[ra];
    end
  end
endmodule
