// The DE2's SRAM (IS61LV25616AL-10: 256K x 16 bits, 512 KB, 10 ns,
// asynchronous) written with a pseudo-random pattern and read back, over
// and over, from a 100 MHz controller on PLL_1's CLK0.
//
// The chip is always selected with both bytes enabled. A write takes 3
// clocks and is WE#-controlled with OE# high: address and data out with
// WE# high, WE# low for one clock (10 ns; the part wants 8), WE# high with
// address and data held (they may change at that edge: 0 ns hold). A read
// takes 2 clocks: the address out, and the input registers sample the data
// on the edge that puts out the next address (access time 10 ns; the old
// data holds 2.5 ns after an address change). Between the two phases the
// bus is idle for a few clocks with neither side driving.
//
// Each pass writes all 256K words, the data a 32-bit LFSR seeded from the
// pass number, then reads them back in the same order and compares.
// USER1 (a 128-bit register clocked by TCK) holds from the top "SRM!",
// then the passes, the words that read back wrong and a CLOCK_50 counter,
// each Gray-coded: `cargo run --release -p chipsmith-cyclone2 --example
// de2_user1 -- --words 4` prints counter 1 as the errors (0 on a good
// board) and counter 2 as the passes.
//
// LEDG[0] is the PLL's lock, LEDR[0] lights on any error, LEDR[3:1] count
// passes.
module top(
  input CLOCK_50,
  output [17:0] SRAM_ADDR,
  inout [15:0] SRAM_DQ,
  output SRAM_WE_N,
  output SRAM_OE_N,
  output SRAM_UB_N,
  output SRAM_LB_N,
  output SRAM_CE_N,
  output [3:0] LEDR,
  output LEDG
);
  // FPGA clock edges from the one that puts a read address out to the one
  // whose input registers sample its data (the next address's); 2 or more.
  // On the DE2 2026-10-30, 3 read nearly every word wrong.
  parameter RD_LAT = 2;

  wire clk, locked;
  CY2_PLL #(
    .INCLK0_INPUT_FREQUENCY(20000),
    .CLK0_MULTIPLY_BY(2),
    .CLK0_DIVIDE_BY(1)
  ) pll (
    .INCLK(CLOCK_50),
    .CLK0(clk),
    .LOCKED(locked)
  );
  assign SRAM_CE_N = 1'b0;
  assign SRAM_UB_N = 1'b0;
  assign SRAM_LB_N = 1'b0;
  assign LEDG = locked;

  // The controller's outputs, which the manifest copies into the pins'
  // I/O-cell registers.
  reg [17:0] c_addr = 0;
  reg [15:0] c_dq = 0;
  reg c_drive = 0, c_we_n = 1, c_oe_n = 1;
  reg [15:0] dq_in = 0;
  assign SRAM_ADDR = c_addr;
  assign SRAM_WE_N = c_we_n;
  assign SRAM_OE_N = c_oe_n;
  genvar i;
  generate
    for (i = 0; i < 16; i = i + 1) begin : dq
      assign SRAM_DQ[i] = c_drive ? c_dq[i] : 1'bz;
    end
  endgenerate
  always @(posedge clk) dq_in <= SRAM_DQ;

  reg [1:0] lock_q = 0;
  always @(posedge clk) lock_q <= {lock_q[0], locked};
  wire run = lock_q[1];

  localparam S_WAIT = 2'd0, S_WRITE = 2'd1, S_READ = 2'd2, S_TURN = 2'd3;
  reg [1:0] state = S_WAIT;
  reg [1:0] ph = 0;       // clock within a write or read
  reg [17:0] a = 0;       // the word
  reg [3:0] t = 0;        // idle clocks between phases
  reg reading = 0;        // the phase after S_TURN
  reg [31:0] pass = 0;
  reg [31:0] wl = 1;      // write pattern LFSR
  reg issue = 0;          // a read address went out on this edge

  function [31:0] lfsr(input [31:0] l);
    lfsr = (l >> 1) ^ (l[0] ? 32'h80200003 : 32'h0);
  endfunction
  function [31:0] seed(input [15:0] p);
    seed = {p, ~p} | 32'h1;
  endfunction

  always @(posedge clk) begin
    issue <= 1'b0;
    if (!run) begin
      state <= S_WAIT;
      c_drive <= 1'b0;
      c_we_n <= 1'b1;
      c_oe_n <= 1'b1;
    end else case (state)
      S_WAIT: begin
        state <= S_WRITE;
        a <= 0;
        ph <= 0;
        reading <= 0;
        wl <= seed(pass[15:0]);
      end
      // Address and data change a clock after WE# rises, not with it.
      S_WRITE: begin
        ph <= ph + 1'b1;
        case (ph)
          2'd0: begin
            c_addr <= a;
            c_dq <= wl[15:0];
            c_drive <= 1'b1;
          end
          2'd1: c_we_n <= 1'b0;
          default: begin
            c_we_n <= 1'b1;
            ph <= 0;
            wl <= lfsr(wl);
            a <= a + 1'b1;
            if (&a) begin
              state <= S_TURN;
              t <= 0;
              reading <= 1'b1;
            end
          end
        endcase
      end
      // The bus released (the last write's data held a clock past its
      // WE# rising edge), then OE# turned on or off, and the last reads
      // checked.
      S_TURN: begin
        t <= t + 1'b1;
        if (t == 4'd0) c_drive <= 1'b0;
        if (t == 4'd2) c_oe_n <= ~reading;
        if (t == 4'd7) begin
          state <= reading ? S_READ : S_WRITE;
          ph <= 0;
          a <= 0;
        end
      end
      S_READ: begin
        ph <= ph + 1'b1;
        if (ph == 2'd0) begin
          c_addr <= a;
          issue <= 1'b1;
        end else begin
          ph <= 0;
          a <= a + 1'b1;
          if (&a) begin
            state <= S_TURN;
            t <= 0;
            reading <= 1'b0;
            pass <= pass + 1'b1;
            wl <= seed(pass[15:0] + 1'b1);
          end
        end
      end
    endcase
  end

  // The checker: each read's data is in dq_in RD_LAT edges after its
  // address went out (`due[RD_LAT-1]` is set from that edge to the next),
  // and the pattern is regenerated in the same order. A pass's read phase
  // starts with the seed its write phase had.
  reg [RD_LAT-1:0] due = 0;
  reg [31:0] rl = 1;
  reg [31:0] errors = 0;
  reg seen = 0;
  reg reading_q = 0;
  always @(posedge clk) begin
    due <= {due[RD_LAT-2:0], issue};
    reading_q <= reading;
    if (reading & ~reading_q) rl <= seed(pass[15:0]);
    else if (due[RD_LAT-1]) begin
      rl <= lfsr(rl);
      if (dq_in != rl[15:0]) begin
        if (errors != 32'hffffffff) errors <= errors + 1'b1;
        seen <= 1'b1;
      end
    end
  end
  assign LEDR = {pass[2:0], seen};

  reg [31:0] g_pass = 0, g_err = 0;
  always @(posedge clk) begin
    g_pass <= pass ^ (pass >> 1);
    g_err <= errors ^ (errors >> 1);
  end
  reg [31:0] n50 = 0, g50 = 0;
  always @(posedge CLOCK_50) begin
    n50 <= n50 + 1'b1;
    g50 <= n50 ^ (n50 >> 1);
  end

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
    else dr <= {"SRM!", g_pass, g_err, g50};
endmodule
