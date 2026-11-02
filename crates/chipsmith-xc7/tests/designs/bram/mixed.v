// Mixed widths: written 32 bits at a time, read 8.
module mixed (input clk, input we, input [7:0] wa, input [9:0] ra, input [31:0] d, output reg [7:0] q);
  reg [7:0] mem [0:1023];
  always @(posedge clk) begin
    if (we) begin
      mem[{wa, 2'd0}] <= d[7:0];
      mem[{wa, 2'd1}] <= d[15:8];
      mem[{wa, 2'd2}] <= d[23:16];
      mem[{wa, 2'd3}] <= d[31:24];
    end
    q <= mem[ra];
  end
endmodule
