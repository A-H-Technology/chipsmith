// A PLL whose output frequencies can be measured over the JTAG cable. PLL_1
// (fed by CLOCK_50) makes 100 MHz on CLK0 and 40 MHz on CLK1. Each of the
// three clocks runs a Gray-coded counter; the JTAG block makes them a
// 128-bit USER1 register, clocked by TCK, holding from the top "PLL!" and
// the CLK1, CLK0 and CLOCK_50 counters. `cargo run --release -p
// chipsmith-cyclone2 --example de2_user1 -- --words 4` reads it twice, a
// second apart, and prints each counter's rate against CLOCK_50's: 2.000000
// and 0.800000.
//
// LEDG[0] is the PLL's lock. LEDR counts on CLK0 (LEDR[3] toggles at
// 100 MHz / 2^28, every 2.7 s), LEDG[1] on CLK1 (every 0.84 s).
module top(input CLOCK_50, output [3:0] LEDR, output [1:0] LEDG);
  wire c0, c1, locked;
  CY2_PLL #(
    .INCLK0_INPUT_FREQUENCY(20000),
    .CLK0_MULTIPLY_BY(2),
    .CLK0_DIVIDE_BY(1),
    .CLK1_MULTIPLY_BY(4),
    .CLK1_DIVIDE_BY(5)
  ) pll (
    .INCLK(CLOCK_50),
    .CLK0(c0),
    .CLK1(c1),
    .LOCKED(locked)
  );
  assign LEDG[0] = locked;
  assign LEDG[1] = n1[25];

  reg [31:0] n50 = 0, g50 = 0;
  always @(posedge CLOCK_50) begin
    n50 <= n50 + 1'b1;
    g50 <= n50 ^ (n50 >> 1);
  end
  reg [31:0] n0 = 0, g0 = 0;
  always @(posedge c0) begin
    n0 <= n0 + 1'b1;
    g0 <= n0 ^ (n0 >> 1);
  end
  reg [31:0] n1 = 0, g1 = 0;
  always @(posedge c1) begin
    n1 <= n1 + 1'b1;
    g1 <= n1 ^ (n1 >> 1);
  end
  assign LEDR = n0[27:24];

  wire tck, tdi, shift, usr1;
  reg [127:0] dr = 0;
  CY2_JTAG jtag (
    .TDOUSER(dr[0]),
    .TCKUTAP(tck),
    .TDIUTAP(tdi),
    .SHIFTUSER(shift),
    .USR1USER(usr1)
  );
  always @(posedge tck)
    if (shift & usr1) dr <= {tdi, dr[127:1]};
    else dr <= {"PLL!", g1, g0, g50};
endmodule
