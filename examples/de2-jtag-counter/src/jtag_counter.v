// The DE2's counterpart of zybo-jtag-counter: a design that answers over the
// JTAG cable, so a load can be checked without watching the board. The
// Cyclone II's JTAG block (CY2_JTAG, cycloneii_jtag) makes the core a USER1
// data register: every TCK outside Shift-DR loads "DE2!" and a Gray-coded
// 50 MHz counter, Shift-DR shifts it out LSB first. `cargo run --release -p
// chipsmith-cyclone2 --example de2_user1` reads it twice, a second apart.
//
// The counter is Gray-coded so that sampling it on TCK, asynchronous to
// CLOCK_50, is off by at most one count. LEDR shows the counter's top bits.
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
  // chipsmith's Cyclone II back end clocks registers from pins only, so TCK
  // is sampled at 50 MHz rather than used as a clock: each rising TCK seen
  // shifts the register (in Shift-DR with USER1 selected) or reloads it
  // (anywhere else, Capture-DR included). That has to land before TDO is
  // taken on TCK's falling edge, which the USB-Blaster's ~6 MHz TCK leaves
  // about 80 ns for; two sampling stages and the update take 40-60 ns.
  reg [1:0] tck_s = 0;
  reg shift_s = 0, usr1_s = 0, tdi_s = 0;
  always @(posedge CLOCK_50) begin
    tck_s <= {tck_s[0], tck};
    shift_s <= shift;
    usr1_s <= usr1;
    tdi_s <= tdi;
    if (tck_s == 2'b01)
      if (shift_s & usr1_s) dr <= {tdi_s, dr[63:1]};
      else dr <= {"DE2!", gray};
  end
endmodule
