// Multiplexers in front of registers with an adder on one leg: the LAB's
// synchronous load takes the other leg (synthesis `ff::SyncLoads`).
module top(input CLOCK_50, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  // The sum on the multiplexer's 0 leg: sload = KEY[1].
  reg [7:0] count = 0;
  always @(posedge clk) count <= KEY[1] ? SW[7:0] : count + 8'd1;
  // The sum on its 1 leg: sload = !KEY[2].
  reg [7:0] acc = 0;
  always @(posedge clk) acc <= KEY[2] ? acc + SW[15:8] : {SW[3:0], SW[7:4]};
  // With an enable and a synchronous clear, which act first.
  reg [5:0] gated = 0;
  always @(posedge clk)
    if (!KEY[0]) gated <= 0;
    else if (SW[16]) gated <= SW[17] ? SW[5:0] : gated + SW[11:6];
  // Powers up 1: stored inverted, so its multiplexer stays a LUT.
  reg [3:0] high = 4'hf;
  always @(posedge clk) high <= KEY[3] ? SW[3:0] : high + 4'd3;
  assign LEDR = {gated[1:0], acc, count};
  assign LEDG = {gated[5:2], high, 1'b0};
endmodule
