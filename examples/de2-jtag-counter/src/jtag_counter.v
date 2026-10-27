// The DE2's counterpart of zybo-jtag-counter: a design that answers over the
// JTAG cable, so a load can be checked without watching the board. The
// Cyclone II's JTAG block (CY2_JTAG, cycloneii_jtag) makes the core a USER1
// data register clocked by TCK: every TCK outside Shift-DR loads "DE2!" and a
// Gray-coded 50 MHz counter, Shift-DR shifts it out LSB first. `cargo run
// --release -p chipsmith-cyclone2 --example de2_user1` reads it twice, a
// second apart.
//
// TCK reaches the register through a global clock: the JTAG block's TCK
// output, routed in the core to a clock control (cyclone2-re README, "Global
// clocks from the core"). The counter is Gray-coded so that sampling it on
// TCK, asynchronous to CLOCK_50, is off by at most one count. LEDR shows the
// counter's top bits.
module top(input CLOCK_50, output [3:0] LEDR);
  reg [31:0] count = 0;
  reg [31:0] gray = 0;
  always @(posedge CLOCK_50) begin
    count <= count + 1'b1;
    gray <= count ^ (count >> 1);
  end
  assign LEDR = count[26:23];

  wire tck, tdi, shift, usr1;
  reg [63:0] dr = 0;
  CY2_JTAG jtag (
    .TDOUSER(dr[0]),
    .TCKUTAP(tck),
    .TDIUTAP(tdi),
    .SHIFTUSER(shift),
    .USR1USER(usr1)
  );
  always @(posedge tck)
    if (shift & usr1) dr <= {tdi, dr[63:1]};
    else dr <= {"DE2!", gray};
endmodule
