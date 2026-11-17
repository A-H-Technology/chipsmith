// Pin settings on the board, read over JTAG (USER1, 64 bits;
// `cargo run --release -p chipsmith-jtag --example xc7_io`).
//
// Pmod JE has nothing plugged in, so a pin's level is its pull's: `up`
// reads 1 and `down` 0 every cycle. The keeper pins are driven to a level
// for 16 cycles, released, and read 8 ms later: a keeper holds the
// level it was last driven to, so they read back whatever was driven,
// alternately 1 and 0. `loop` is driven by a counter bit and read back at
// its own ball once it has settled.
//
// USER1 holds "IO", then four 8-bit saturating error counts (up read 0,
// down read 1, a keeper read back the other level, loop read back the
// other level), then a 16-bit count of keeper trials.
module zybo_io(
  input clk,
  output [3:0] led,
  input up,
  input down,
  inout [1:0] keep,
  inout loop
);
  reg [19:0] t = 0;     // keeper trial phase, 8.4 ms
  reg level = 0;        // what the keepers are driven to this trial
  reg drive = 0;
  reg [15:0] trials = 0;
  reg [7:0] e_up = 0, e_down = 0, e_keep = 0, e_loop = 0;
  reg [3:0] count = 0;

  assign keep = drive ? {2{level}} : 2'bzz;
  // Released while the keepers are driven, so synthesis can't read count[3]
  // in place of the ball.
  assign loop = drive ? 1'bz : count[3];

  // Inputs registered once before they're compared.
  reg up_r = 1, down_r = 0, loop_r = 0;
  reg [1:0] keep_r = 0;

  always @(posedge clk) begin
    up_r <= up;
    down_r <= down;
    keep_r <= keep;
    loop_r <= loop;
    count <= count + 1'b1;

    if (!up_r && e_up != 8'hff) e_up <= e_up + 1'b1;
    if (down_r && e_down != 8'hff) e_down <= e_down + 1'b1;
    // count[3] has been on the ball for six cycles by the time loop_r
    // samples it here, so the round trip through the pad isn't timed.
    if (t > 64 && count[2:0] == 3'd6 && loop_r != count[3] && e_loop != 8'hff)
      e_loop <= e_loop + 1'b1;

    t <= t + 1'b1;
    drive <= t < 16;
    if (&t) begin
      if (keep_r != {2{level}} && e_keep != 8'hff) e_keep <= e_keep + 1'b1;
      if (trials != 16'hffff) trials <= trials + 1'b1;
      level <= ~level;
    end
  end

  assign led = {e_keep != 0, e_loop != 0, e_down != 0, e_up != 0};

  wire tck, sel, capture, shift, tdi;
  reg [63:0] dr = 0;
  BSCANE2 #(.JTAG_CHAIN(1)) bscan (
    .TCK(tck), .SEL(sel), .CAPTURE(capture), .SHIFT(shift), .TDI(tdi),
    .TDO(dr[0])
  );
  // Sampled on TCK, asynchronous to clk: a count can be off by a
  // half-written update, which only matters while it changes.
  always @(posedge tck)
    if (sel) begin
      if (capture) dr <= {"IO", e_up, e_down, e_keep, e_loop, trials};
      else if (shift) dr <= {tdi, dr[63:1]};
    end
endmodule
