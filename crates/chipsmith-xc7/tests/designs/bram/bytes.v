// Byte enables: 1024 x 32, each byte written under its own enable.
module bytes (input clk, input [3:0] we, input [9:0] wa, input [9:0] ra, input [31:0] d, output reg [31:0] q);
  reg [31:0] mem [0:1023];
  always @(posedge clk) begin
    if (we[0]) mem[wa][7:0] <= d[7:0];
    if (we[1]) mem[wa][15:8] <= d[15:8];
    if (we[2]) mem[wa][23:16] <= d[23:16];
    if (we[3]) mem[wa][31:24] <= d[31:24];
    q <= mem[ra];
  end
endmodule
