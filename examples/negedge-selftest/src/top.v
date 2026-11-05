// Falling-edge registers checked on the board over the JTAG cable. A
// rising-edge counter p; n takes it on the falling edge and q takes n on
// the next rising edge, so just before each rising edge q is p - 1 (were
// n clocked on the rising edge too, q would trail by 2). n2 takes p on the
// falling edges where p is odd (an enable on the inverted LAB clock), so
// before each rising edge it holds p rounded down to odd. Every rising
// edge after the first few checks both; a mismatch counts an error
// (saturating). USER1 holds "NEG!", the error count and p, both Gray-coded
// (`de2_user1 -- --words 3`: counter 0 runs at 50 MHz, counter 1 stays 0).
// LEDR[0] lights on an error.
module top(input CLOCK_50, output LEDR);
  wire clk = CLOCK_50;
  reg [31:0] p = 0, n = 0, n2 = 0, q = 0;
  reg [31:0] errors = 0;
  always @(negedge clk) n <= p;
  always @(negedge clk) if (p[0]) n2 <= p;
  always @(posedge clk) begin
    p <= p + 1'b1;
    q <= n;
    if (p > 4 && (q != p - 1 || n2 != (p[0] ? p : p - 1)) && ~&errors)
      errors <= errors + 1'b1;
  end
  assign LEDR = errors != 0;

  wire tck, tdi, shift, usr1;
  reg [95:0] dr = 0;
  CY2_JTAG jtag (
    .TDOUSER(dr[0]),
    .TCKUTAP(tck),
    .TDIUTAP(tdi),
    .SHIFTUSER(shift),
    .USR1USER(usr1)
  );
  // Gray-coded in the CLOCK_50 domain, so a TCK-side load mid-count is one
  // count off at worst.
  reg [31:0] gp = 0, ge = 0;
  always @(posedge clk) begin
    gp <= p ^ (p >> 1);
    ge <= errors ^ (errors >> 1);
  end
  always @(posedge tck)
    if (shift & usr1) dr <= {tdi, dr[95:1]};
    else dr <= {"NEG!", ge, gp};
endmodule
