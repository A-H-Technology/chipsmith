// Slice latches on the board, read over JTAG (USER1;
// `cargo run --release -p chipsmith-jtag --example xc7_latch`).
//
// Four latches as RTL writes them (synthesis makes LDCEs and LDPEs of
// them): gated high, gated low, gated with an enable and a clear, gated
// with a preset. Their gate, enable, reset and data come from registers
// clocked by `clk`, so they change once a cycle; each latch's output is
// sampled on the next edge and checked against a register model of what
// it should hold by then (the data while open, the last data while shut,
// the reset's value while reset).
//
// USER1: "LT", then 8-bit saturating error counts for latches 0 to 3,
// then a 16-bit count of checks (in thousands).
module zybo_latch(input clk, output [3:0] led);
  reg [15:0] n = 0;
  always @(posedge clk) n <= n + 1'b1;

  // The latches' inputs, registered: the gate opens for 32 cycles in 64,
  // the data changes every 16 (twice while open, 8 cycles off the gate's
  // edges), the enable every 512 and the reset is 4 cycles in 4096, each on
  // edges where the gate holds still. A reset released, or an enable changed, on the edge the gate
  // closes races the gate's later BUFG and leaves the latch either way;
  // in real designs as here.
  wire [15:0] n_ge = n + 16'd56;   // flips 8 cycles into a closed phase
  wire [15:0] n_d = n + 16'd8;     // flips 8 cycles off the gate
  reg g = 0, ge = 0, rst = 0;
  reg [3:0] d = 0;
  always @(posedge clk) begin
    g <= n[5];
    ge <= n_ge[9];
    rst <= n[11:2] == 10'd5;       // cycles 20 to 23 of 4096, gate shut
    d <= {n_d[4] ^ n_d[12], n_d[4] ^ n_d[13], n_d[4] ^ n_d[14], n_d[4] ^ n_d[15]};
  end

  reg [3:0] q;
  always @* if (g) q[0] = d[0];
  always @* if (!g) q[1] = d[1];
  always @* if (rst) q[2] = 1'b0; else if (g && ge) q[2] = d[2];
  always @* if (rst) q[3] = 1'b1; else if (g) q[3] = d[3];

  // What each latch holds this cycle, kept in registers one cycle behind:
  // `want` after an edge is what the latch held during the cycle before.
  reg [3:0] want = 0, seen = 0;
  always @(posedge clk) begin
    if (g) want[0] <= d[0];
    if (!g) want[1] <= d[1];
    if (rst) want[2] <= 1'b0; else if (g && ge) want[2] <= d[2];
    if (rst) want[3] <= 1'b1; else if (g) want[3] <= d[3];
    seen <= q;
  end

  reg [7:0] e [0:3];
  reg ready = 0;
  reg [9:0] sub = 0;
  reg [15:0] checks = 0;
  integer i;
  initial for (i = 0; i < 4; i = i + 1) e[i] = 0;
  always @(posedge clk) begin
    // Every latch has been open and reset once by now.
    if (n == 16'hffff) ready <= 1'b1;
    if (ready) begin
      for (i = 0; i < 4; i = i + 1)
        if (seen[i] != want[i] && e[i] != 8'hff) e[i] <= e[i] + 1'b1;
      sub <= sub == 999 ? 10'd0 : sub + 1'b1;
      if (sub == 999) checks <= checks + 1'b1;
    end
  end
  assign led = {e[3] != 0, e[2] != 0, e[1] != 0, e[0] != 0};

  wire tck, sel, capture, shift, tdi;
  reg [63:0] dr = 0;
  BSCANE2 #(.JTAG_CHAIN(1)) bscan (
    .TCK(tck), .SEL(sel), .CAPTURE(capture), .SHIFT(shift), .TDI(tdi),
    .TDO(dr[0])
  );
  always @(posedge tck)
    if (sel) begin
      if (capture) dr <= {"LT", e[0], e[1], e[2], e[3], checks};
      else if (shift) dr <= {tdi, dr[63:1]};
    end
endmodule
