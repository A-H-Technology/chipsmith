// The DE2's SDRAM (IS42S16400: 4 banks x 4096 rows x 256 columns x 16 bits,
// 8 MB) written with a pseudo-random pattern and read back, over and over.
// A 100 MHz controller on PLL_1's CLK0; the SDRAM's clock is CLK2 on
// DRAM_CLK, PLL_1's clock output pin, shifted to lead the commands by 3 ns
// (+7 ns of a 10 ns period), as Terasic's DE2 designs clock it.
//
// Each pass writes all 4M words in 8-word bursts (ACTIVE, WRITE with auto
// precharge), the data a 32-bit LFSR seeded from the pass number, then
// reads them back in the same order and compares. USER1 (a 128-bit
// register clocked by TCK) holds from the top "SDR!", then the passes, the
// words that read back wrong and a CLOCK_50 counter, each Gray-coded:
// `cargo run --release -p chipsmith-cyclone2 --example de2_user1 --
// --words 4` prints counter 1 as the errors (0 on a good board) and counter
// 2 as the passes, rising about 6 a second.
//
// LEDG[0] is the PLL's lock, LEDR[0] lights on any error, LEDR[3:1] count
// passes.
module top(
  input CLOCK_50,
  output reg [11:0] DRAM_ADDR,
  output reg [1:0] DRAM_BA,
  output [1:0] DRAM_DQM,
  output DRAM_CS_N,
  output reg DRAM_RAS_N,
  output reg DRAM_CAS_N,
  output reg DRAM_WE_N,
  output DRAM_CKE,
  output DRAM_CLK,
  inout [15:0] DRAM_DQ,
  output [3:0] LEDR,
  output LEDG
);
  // FPGA clock edges from the controller's READ cycle to the input
  // registers holding its first word: one into the pins' output registers,
  // CAS latency 3, the SDRAM's access time and the board's delays, less
  // the clock's lead.
  parameter RD_LAT = 4;

  wire clk, locked;
  CY2_PLL #(
    .INCLK0_INPUT_FREQUENCY(20000),
    .CLK0_MULTIPLY_BY(2),
    .CLK0_DIVIDE_BY(1),
    .CLK2_MULTIPLY_BY(2),
    .CLK2_DIVIDE_BY(1),
    .CLK2_PHASE_SHIFT(7000)
  ) pll (
    .INCLK(CLOCK_50),
    .CLK0(clk),
    .CLK2(DRAM_CLK),
    .LOCKED(locked)
  );
  assign DRAM_CKE = 1'b1;
  assign DRAM_CS_N = 1'b0;
  assign DRAM_DQM = 2'b00;
  assign LEDG = locked;

  // The controller's outputs, then the pins' I/O-cell registers, plain
  // copies of them a cycle later: an I/O-cell register has no reset or
  // enable, which synthesis would find in the controller's muxes.
  reg [11:0] c_addr = 0;
  reg [1:0] c_ba = 0;
  // {RAS#, CAS#, WE#}; the chip is always selected, no byte masked.
  reg [2:0] c_cmd = 3'b111;
  reg [15:0] c_dq = 0, c_oe = 0;
  reg [15:0] dq_out = 0, dq_oe = 0, dq_in = 0;
  always @(posedge clk) begin
    DRAM_ADDR <= c_addr;
    DRAM_BA <= c_ba;
    {DRAM_RAS_N, DRAM_CAS_N, DRAM_WE_N} <= c_cmd;
    dq_out <= c_dq;
    dq_oe <= c_oe;
  end
  genvar i;
  generate
    for (i = 0; i < 16; i = i + 1) begin : dq
      assign DRAM_DQ[i] = dq_oe[i] ? dq_out[i] : 1'bz;
    end
  endgenerate
  always @(posedge clk) dq_in <= DRAM_DQ;

  reg [1:0] lock_q = 0;
  always @(posedge clk) lock_q <= {lock_q[0], locked};
  wire run = lock_q[1];

  // {RAS#, CAS#, WE#}
  localparam NOP = 3'b111, ACT = 3'b011, READ = 3'b101, WRITE = 3'b100,
             PRE = 3'b010, REF = 3'b001, MRS = 3'b000;
  // Burst length 8, sequential, CAS latency 3, bursts on writes too.
  localparam MODE = 12'b00_0_00_011_0_011;

  localparam S_WAIT = 3'd0, S_PRE = 3'd1, S_IREF = 3'd2, S_MRS = 3'd3,
             S_BURST = 3'd4, S_REF = 3'd5;
  reg [2:0] state = S_WAIT;
  reg [14:0] t = 0;      // wait counter
  reg [3:0] n = 0;       // init refreshes
  reg [3:0] step = 0;    // cycle within a burst or refresh slot
  reg [18:0] burst = 0;  // {bank, row, column[7:3]}
  reg reading = 0;
  reg [9:0] since_ref = 0;
  reg [15:0] pass = 0;
  reg [31:0] wl = 1;     // write pattern LFSR
  reg rd_word = 0;       // a read word is due RD_LAT edges on

  function [31:0] lfsr(input [31:0] l);
    lfsr = (l >> 1) ^ (l[0] ? 32'h80200003 : 32'h0);
  endfunction
  function [31:0] seed(input [15:0] p);
    seed = {p, ~p} | 32'h1;
  endfunction

  always @(posedge clk) begin
    c_cmd <= NOP;
    c_oe <= 16'h0;
    rd_word <= 1'b0;
    // Refresh every 7 us (4096 rows in 64 ms is one per 15.6 us).
    if (state != S_REF && since_ref != 10'd1023) since_ref <= since_ref + 1'b1;
    if (!run) begin
      state <= S_WAIT;
      t <= 0;
    end else case (state)
      // 200 us of NOPs after power-up, then precharge all, 8 refreshes,
      // the mode register.
      S_WAIT: begin
        t <= t + 1'b1;
        if (t == 15'd20000) begin
          state <= S_PRE;
          t <= 0;
        end
      end
      S_PRE: begin
        t <= t + 1'b1;
        if (t == 0) begin
          c_cmd <= PRE;
          c_addr[10] <= 1'b1;
        end
        if (t == 15'd4) begin
          state <= S_IREF;
          t <= 0;
          n <= 0;
        end
      end
      S_IREF: begin
        t <= t + 1'b1;
        if (t == 0) c_cmd <= REF;
        if (t == 15'd9) begin
          t <= 0;
          n <= n + 1'b1;
          if (n == 4'd7) state <= S_MRS;
        end
      end
      S_MRS: begin
        t <= t + 1'b1;
        if (t == 0) begin
          c_cmd <= MRS;
          c_ba <= 2'b00;
          c_addr <= MODE;
        end
        if (t == 15'd4) begin
          state <= S_BURST;
          step <= 0;
          burst <= 0;
          reading <= 0;
          since_ref <= 0;
          wl <= seed(pass);
        end
      end
      // 16 cycles a burst: ACTIVE at 0, READ/WRITE with auto precharge at
      // 3 (tRCD 20 ns), write data 3-10, the precharge done by 14 (tWR +
      // tRP), the next ACTIVE 16 cycles on (tRC 70 ns).
      S_BURST: begin
        step <= step + 1'b1;
        case (step)
          4'd0: begin
            c_cmd <= ACT;
            c_ba <= burst[18:17];
            c_addr <= burst[16:5];
          end
          4'd3: begin
            c_cmd <= reading ? READ : WRITE;
            c_addr <= {1'b0, 1'b1, 2'b00, burst[4:0], 3'b000};
          end
          default: ;
        endcase
        if (step >= 4'd3 && step <= 4'd10) begin
          if (reading) begin
            rd_word <= 1'b1;
          end else begin
            c_oe <= 16'hffff;
            c_dq <= wl[15:0];
            wl <= lfsr(wl);
          end
        end
        if (step == 4'd15) begin
          burst <= burst + 1'b1;
          if (burst == 19'h7ffff) begin
            reading <= ~reading;
            if (reading) begin
              pass <= pass + 1'b1;
              wl <= seed(pass + 1'b1);
            end
          end
          if (since_ref >= 10'd700) begin
            state <= S_REF;
            t <= 0;
          end
        end
      end
      // All banks are precharged between bursts.
      S_REF: begin
        t <= t + 1'b1;
        if (t == 0) c_cmd <= REF;
        if (t == 15'd9) begin
          state <= S_BURST;
          since_ref <= 0;
        end
      end
      default: state <= S_WAIT;
    endcase
  end

  // The checker: each read word's slot arrives RD_LAT edges after its
  // READ-burst cycle, and the pattern is regenerated in the same order.
  // A pass's read phase starts with the seed its write phase had.
  reg [RD_LAT:0] due = 0;
  reg [31:0] rl = 1;
  reg [31:0] errors = 0;
  reg seen = 0;
  reg reading_q = 0;
  always @(posedge clk) begin
    due <= {due[RD_LAT-1:0], rd_word};
    reading_q <= reading;
    if (reading & ~reading_q) rl <= seed(pass);
    else if (due[RD_LAT]) begin
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
    g_pass <= {16'h0, pass} ^ ({16'h0, pass} >> 1);
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
    else dr <= {"SDR!", g_pass, g_err, g50};
endmodule
