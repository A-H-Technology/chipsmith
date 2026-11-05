// The Zybo's USB-UART is wired to the Zynq's PS (MIO 48/49), which the PL
// can't drive without software on the ARM cores. This design talks over the
// JTAG cable instead: BSCANE2 makes the PL a 64-bit USER1 data register.
// Capture loads "ZYBO" and a free-running 125 MHz counter; the host shifts
// both out (`cargo run --release --example xc7_user1`). Two reads a known
// time apart give the clock rate.
//
// The counter is Gray-coded so that sampling it on TCK, asynchronous to clk,
// is off by at most one count. The LEDs show the same counter as blinky.
module jtag_counter(input clk, output [3:0] led);
  reg [31:0] count = 0;
  reg [31:0] gray = 0;
  always @(posedge clk) begin
    count <= count + 1'b1;
    gray <= count ^ (count >> 1);
  end
  assign led = {count[23], count[24], count[25], count[26]};

  wire tck, sel, capture, shift, tdi;
  reg [63:0] dr = 0;
  BSCANE2 #(.JTAG_CHAIN(1)) bscan (
    .TCK(tck), .SEL(sel), .CAPTURE(capture), .SHIFT(shift), .TDI(tdi),
    .TDO(dr[0])
  );
  always @(posedge tck)
    if (sel) begin
      if (capture) dr <= {"ZYBO", gray};
      else if (shift) dr <= {tdi, dr[63:1]};
    end
endmodule
