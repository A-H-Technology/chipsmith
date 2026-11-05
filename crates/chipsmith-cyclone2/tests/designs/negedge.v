// Registers on both edges of one clock, with an enable and an asynchronous
// clear: the falling-edge ones take the LAB's clock inversion, not an
// inverter in front of a register (which would be a clock from logic).
module top(input clk, input en, input clr, input [7:0] d, output [7:0] q, output [7:0] r, output [3:0] s);
  reg [7:0] p, n;
  reg [3:0] ne;
  always @(posedge clk) p <= d;
  always @(negedge clk) n <= p ^ d;
  always @(negedge clk or posedge clr)
    if (clr) ne <= 0;
    else if (en) ne <= d[3:0] + ne;
  assign q = p;
  assign r = n;
  assign s = ne;
endmodule
