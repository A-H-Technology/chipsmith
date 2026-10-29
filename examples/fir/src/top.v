// An 8-tap FIR filter, signed 16-bit samples and coefficients: y[n] =
// sum of c[k] * x[n-k]. Samples come from a 32-bit LFSR mixed with the
// switches; while KEY[0] is held the switches load the coefficients one
// tap a cycle (release to filter). The 35-bit sum folds onto the LEDs so
// nothing is optimised away. Each product is an embedded multiplier with
// its sample register inside; the coefficients feed the multiplier alone
// too, so they move in as well.
module top(input CLOCK_50, input [3:0] KEY, input [15:0] SW, output [17:0] LEDR);
  wire clk = CLOCK_50;
  reg [31:0] lfsr = 32'hACE1_2357;
  always @(posedge clk) lfsr <= {lfsr[30:0], lfsr[31] ^ lfsr[21] ^ lfsr[1] ^ lfsr[0]};
  wire signed [15:0] sample = lfsr[15:0] ^ SW;

  reg signed [15:0] x0 = 0, x1 = 0, x2 = 0, x3 = 0, x4 = 0, x5 = 0, x6 = 0, x7 = 0;
  reg signed [15:0] c0 = 16'sd3, c1 = -16'sd5, c2 = 16'sd7, c3 = 16'sd11,
                    c4 = -16'sd13, c5 = 16'sd17, c6 = -16'sd19, c7 = 16'sd23;
  wire load = ~KEY[0];
  always @(posedge clk) begin
    x0 <= sample; x1 <= x0; x2 <= x1; x3 <= x2; x4 <= x3; x5 <= x4; x6 <= x5; x7 <= x6;
    if (load) begin
      c0 <= SW; c1 <= c0; c2 <= c1; c3 <= c2; c4 <= c3; c5 <= c4; c6 <= c5; c7 <= c6;
    end
  end
  wire signed [31:0] m0 = c0 * x0, m1 = c1 * x1, m2 = c2 * x2, m3 = c3 * x3,
                     m4 = c4 * x4, m5 = c5 * x5, m6 = c6 * x6, m7 = c7 * x7;
  reg signed [34:0] y = 0;
  always @(posedge clk) y <= m0 + m1 + m2 + m3 + m4 + m5 + m6 + m7;
  assign LEDR = y[17:0] ^ {1'b0, y[34:18]};
endmodule
