// Latches as RTL writes them (`always @*` with a missing else): plain,
// active-low, and with an asynchronous clear and preset; `clk` is the gate
// so the co-simulation toggles it.
module latch_rtl(input clk, input en, input rst, input [3:0] d, output reg [3:0] q);
  always @* if (clk) q[0] = d[0];
  always @* if (!clk) q[1] = d[1];
  always @* if (rst) q[2] = 1'b0; else if (clk && en) q[2] = d[2];
  always @* if (rst) q[3] = 1'b1; else if (clk) q[3] = d[3];
endmodule
