// An M4K self-test that answers over the JTAG cable, like
// de2-jtag-counter: every 4,096 cycles each memory below is written with a
// pattern of the pass number, then read back for 4,096 cycles and compared.
// USER1 holds "M4K!", the passes (top 16 bits of the count de2_user1
// prints) and the reads that differed (bottom 16 bits, saturating): a good
// board shows the passes rising and 0 errors. LEDR[0] lights on an error,
// LEDR[3:1] show the pass count.
//
//   s9   512 x 9 simple dual port
//   d9   2048 x 9 over four blocks (decoded writes, multiplexed reads)
//   b16  256 x 16 written a byte at a time (byte enables)
//   w32  128 x 32 written, read as 512 x 8 (mixed widths)
//   t18  256 x 18 true dual port, even words on port A, odd on B
//   o9   512 x 9 with its output registered again (output register)
//   r18  256 x 18 ROM read under a read enable (clock enable)
//   s8   64 x 8 (shallower than its block)
module top(input CLOCK_50, output [3:0] LEDR);
  wire clk = CLOCK_50;
  reg [11:0] a = 0;
  // Writing, or reading back (idle for the first 4,096 cycles). Two
  // registers rather than one and its inverse, both powering up 0: an
  // inverter alone in front of a block's write enable is one quartus_map
  // folds into the block (README), so Quartus wouldn't build the same bits.
  reg rd = 0, we = 0;
  reg [15:0] pass = 0;
  reg [15:0] errors = 0;
  always @(posedge clk) begin
    a <= a + 1'b1;
    if (&a) begin
      we <= ~we & ~rd | rd;
      rd <= we;
      if (rd) pass <= pass + 1'b1;
    end
  end
  // The pattern: the address mixed with the pass, a few gates deep (the
  // check has to meet 50 MHz).
  function [35:0] h(input [11:0] x, input [15:0] p);
    h = {x, x ^ p[11:0], ~x ^ {p[3:0], p[15:8]}};
  endfunction

  // Read-back pipeline: the address and the pass of the last two reads.
  reg [11:0] a1 = 0, a2 = 0;
  reg [15:0] p1 = 0, p2 = 0;
  reg v1 = 0, v2 = 0;
  always @(posedge clk) begin
    a1 <= a; p1 <= pass; v1 <= rd;
    a2 <= a1; p2 <= p1; v2 <= v1;
  end
  // What is written now (w_*) and what last cycle's read should give (x_*).
  // A memory of 2^n words sees address bits n and up as 0: the same
  // pattern for every address that reaches one word within a pass.
  wire [35:0] w_a = h({3'h0, a[8:0]}, pass), x_a = h({3'h0, a1[8:0]}, p1);
  wire [35:0] h_w = h({5'h0, a[6:0]}, pass), h_x = h({5'h0, a1[8:2]}, p1);
  // 32 bits that all change (a constant written bit costs an LE, which
  // Quartus would replace by its own GND, elsewhere).
  wire [31:0] w_w = {h_w[19:12], h_w[23:0]}, x_w = {h_x[19:12], h_x[23:0]};

  reg [8:0] s9 [0:511];
  reg [8:0] q_s9;
  always @(posedge clk) begin
    if (we) s9[a[8:0]] <= w_a[8:0];
    q_s9 <= s9[a[8:0]];
  end
  wire e_s9 = v1 && q_s9 != x_a[8:0];

  wire [35:0] w_d = h({1'b0, a[10:0]}, pass), x_d = h({1'b0, a1[10:0]}, p1);
  reg [8:0] d9 [0:2047];
  reg [8:0] q_d9;
  always @(posedge clk) begin
    if (we) d9[a[10:0]] <= w_d[8:0];
    q_d9 <= d9[a[10:0]];
  end
  wire e_d9 = v1 && q_d9 != x_d[8:0];

  wire [35:0] w_8 = h({4'h0, a[7:0]}, pass), x_8 = h({4'h0, a1[7:0]}, p1);
  reg [15:0] b16 [0:255];
  reg [15:0] q_b16;
  always @(posedge clk) begin
    if (we & ~a[8]) b16[a[7:0]][7:0] <= w_8[7:0];
    if (we & a[8]) b16[a[7:0]][15:8] <= w_8[15:8];
    q_b16 <= b16[a[7:0]];
  end
  wire e_b16 = v1 && q_b16 != x_8[15:0];

  reg [7:0] w32 [0:511];
  reg [7:0] q_w32;
  always @(posedge clk) begin
    if (we) begin
      w32[{a[6:0], 2'd0}] <= w_w[7:0];
      w32[{a[6:0], 2'd1}] <= w_w[15:8];
      w32[{a[6:0], 2'd2}] <= w_w[23:16];
      w32[{a[6:0], 2'd3}] <= w_w[31:24];
    end
    q_w32 <= w32[a[8:0]];
  end
  wire [7:0] x_w8 = x_w >> (8 * a1[1:0]);
  wire e_w32 = v1 && q_w32 != x_w8;

  // Port A on even words, port B on the odd word next to it.
  wire [35:0] w_ta = h({4'h0, a[7:0]}, pass), w_tb = h({4'h0, a[7:0] ^ 8'd1}, pass);
  wire [35:0] x_ta = h({4'h0, a1[7:0]}, p1), x_tb = h({4'h0, a1[7:0] ^ 8'd1}, p1);
  reg [17:0] t18 [0:255];
  reg [17:0] q_ta, q_tb;
  wire wt = we & ~a[0];
  always @(posedge clk) begin
    if (wt) t18[a[7:0]] <= w_ta[17:0];
    q_ta <= wt ? w_ta[17:0] : t18[a[7:0]];
  end
  always @(posedge clk) begin
    if (wt) t18[a[7:0] ^ 8'd1] <= w_tb[17:0];
    q_tb <= wt ? w_tb[17:0] : t18[a[7:0] ^ 8'd1];
  end
  wire e_t18 = v1 && (q_ta != x_ta[17:0] || q_tb != x_tb[17:0]);

  wire [35:0] w_9 = h({3'h0, a[8:0]} ^ 12'h1a5, pass), y_9 = h({3'h0, a2[8:0]} ^ 12'h1a5, p2);
  reg [8:0] o9 [0:511];
  reg [8:0] q_o9, p_o9;
  always @(posedge clk) begin
    if (we) o9[a[8:0]] <= w_9[8:0];
    q_o9 <= o9[a[8:0]];
    p_o9 <= q_o9;
  end
  wire e_o9 = v2 && p_o9 != y_9[8:0];

  reg [17:0] r18 [0:255];
  integer i;
  initial for (i = 0; i < 256; i = i + 1) r18[i] = {i[7:0], ~i[7:0], i[1:0]};
  reg [17:0] q_r18;
  always @(posedge clk) if (rd) q_r18 <= r18[a[7:0]];
  wire e_r18 = v1 && q_r18 != {a1[7:0], ~a1[7:0], a1[1:0]};

  wire [35:0] w_6 = h({6'h0, a[5:0]}, pass), x_6 = h({6'h0, a1[5:0]}, p1);
  reg [7:0] s8 [0:63];
  reg [5:0] s8_a;
  always @(posedge clk) begin
    if (we) s8[a[5:0]] <= w_6[7:0];
    s8_a <= a[5:0];
  end
  wire e_s8 = v1 && s8[s8_a] != x_6[7:0];

  wire bad = e_s9 | e_d9 | e_b16 | e_w32 | e_t18 | e_o9 | e_r18 | e_s8;
  always @(posedge clk) if (bad && ~&errors) errors <= errors + 1'b1;
  assign LEDR = {pass[2:0], errors != 0};

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
    else dr <= {"M4K!", pass, errors};
endmodule
