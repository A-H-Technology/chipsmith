// I/O-tile registers on the board, read over JTAG (USER1;
// `cargo run --release -p chipsmith-jtag --example xc7_ioreg`).
//
// Each Pmod JE pin (nothing plugged in) is driven by its output register,
// enabled by its output-enable register, and read back by its input
// register, all three in the pin's I/O tile: `rise` on the clock's rising
// edge with a pull-up, `fall` on its falling edge with a pull-down. The pin
// is driven with a counter bit for 1024 cycles and released for 1024 (a
// weak pull takes a while to charge a floating pin); at a phase's last
// cycle, what the input register read must be the driven bit, or the
// pull's level while released.
//
// USER1: "IR", then 8-bit error counts for `rise` and `fall` (saturating),
// then a 32-bit count of checks (122 thousand a second).
module zybo_ioreg(input clk, output [3:0] led, inout rise, inout fall);
  reg [31:0] n = 0;
  always @(posedge clk) n <= n + 1'b1;
  wire drive = ~n[10];    // drive for 1024 cycles, release for 1024
  wire data = n[11];      // what's driven changes every other phase

  // The registers the manifest puts in the I/O tiles.
  reg r_o = 0, r_t = 1, r_i = 0;
  always @(posedge clk) begin
    r_o <= data;
    r_t <= ~drive;        // T high releases
    r_i <= rise;
  end
  assign rise = r_t ? 1'bz : r_o;

  reg f_o = 0, f_t = 1, f_i = 0;
  always @(negedge clk) begin
    f_o <= data;
    f_t <= ~drive;
    f_i <= fall;
  end
  assign fall = f_t ? 1'bz : f_o;

  // Checked at a phase's last cycle, long after its change went through
  // the output registers, the pin and the input registers.
  reg [7:0] e_rise = 0, e_fall = 0;
  reg [31:0] checks = 0;
  always @(posedge clk)
    if (n[9:0] == 10'h3ff && n > 4096) begin
      if (r_i != (drive ? data : 1'b1) && e_rise != 8'hff) e_rise <= e_rise + 1'b1;
      if (f_i != (drive ? data : 1'b0) && e_fall != 8'hff) e_fall <= e_fall + 1'b1;
      checks <= checks + 1'b1;
    end
  assign led = {2'b0, e_fall != 0, e_rise != 0};

  wire tck, sel, capture, shift, tdi;
  reg [63:0] dr = 0;
  BSCANE2 #(.JTAG_CHAIN(1)) bscan (
    .TCK(tck), .SEL(sel), .CAPTURE(capture), .SHIFT(shift), .TDI(tdi),
    .TDO(dr[0])
  );
  always @(posedge tck)
    if (sel) begin
      if (capture) dr <= {"IR", e_rise, e_fall, checks};
      else if (shift) dr <= {tdi, dr[63:1]};
    end
endmodule
