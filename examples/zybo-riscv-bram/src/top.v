// PicoRV32 (RV32I) running a self-checking program out of block RAM: one
// 1,024 x 32 RAM (two RAMB18E1s) holds the program (prog/selftest.S,
// assembled into prog.hex) and the words it writes, with byte enables. Each
// pass the program writes 64 words with a pattern of the pass number,
// overwrites a byte of each, reads them back and counts what differs; it
// stores the passes and the errors to the I/O register at 0x10000000.
// USER1 (BSCANE2, as zybo-jtag-counter) holds "RV32", the passes and the
// errors: a good board shows the passes rising and 0 errors (`cargo run
// --release -p chipsmith-jtag --example xc7_user1`). led[0] lights on an
// error, led[3:1] show the pass count's top bits.
module top(input clk, output [3:0] led);
  // The CPU runs at 112.5 MHz, 9/10 of the Zybo's clock, from a PLL;
  // reset holds until it has locked. chipsmith's placement and routing
  // of it made 114.8 to 128.9 MHz at Vivado's slow corner over five seeds
  // (bench/zybo-timing.sh), so 125 failed on four (README, "The Zybo
  // flow").
  wire cclk, locked;
  XC7_PLL #(.INCLK0_INPUT_FREQUENCY(8000), .CLK0_MULTIPLY_BY(9), .CLK0_DIVIDE_BY(10))
    pll (.INCLK(clk), .ARESET(1'b0), .CLK0(cclk), .LOCKED(locked));
  reg [3:0] rst_cnt = 0;
  wire resetn = &rst_cnt;
  always @(posedge cclk)
    if (!locked) rst_cnt <= 0;
    else if (!resetn) rst_cnt <= rst_cnt + 1'b1;

  wire mem_valid, mem_instr;
  reg mem_ready = 0;
  wire [31:0] mem_addr, mem_wdata;
  wire [3:0] mem_wstrb;
  wire [31:0] mem_rdata;

  picorv32 #(
    .ENABLE_COUNTERS(0), .ENABLE_COUNTERS64(0), .CATCH_MISALIGN(0), .CATCH_ILLINSN(0),
    .TWO_STAGE_SHIFT(0), .ENABLE_REGS_DUALPORT(1)
  ) cpu (
    .clk(cclk), .resetn(resetn), .mem_valid(mem_valid), .mem_instr(mem_instr), .mem_ready(mem_ready),
    .mem_addr(mem_addr), .mem_wdata(mem_wdata), .mem_wstrb(mem_wstrb), .mem_rdata(mem_rdata));

  // The RAM reads every cycle; a request is answered on the next, when the
  // read data is out.
  reg [31:0] ram [0:1023];
  initial $readmemh("prog.hex", ram);
  reg [31:0] rdata = 0;
  wire [9:0] word = mem_addr[11:2];
  wire to_ram = mem_valid && !mem_ready && !mem_addr[28];
  always @(posedge cclk) begin
    if (to_ram && mem_wstrb[0]) ram[word][7:0] <= mem_wdata[7:0];
    if (to_ram && mem_wstrb[1]) ram[word][15:8] <= mem_wdata[15:8];
    if (to_ram && mem_wstrb[2]) ram[word][23:16] <= mem_wdata[23:16];
    if (to_ram && mem_wstrb[3]) ram[word][31:24] <= mem_wdata[31:24];
    rdata <= ram[word];
  end
  assign mem_rdata = rdata;

  reg [15:0] passes = 0, errors = 0;
  always @(posedge cclk) begin
    mem_ready <= mem_valid && !mem_ready;
    if (mem_valid && !mem_ready && mem_addr[28] && |mem_wstrb) begin
      if (mem_addr[2]) errors <= mem_wdata[15:0];
      else passes <= mem_wdata[15:0];
    end
  end
  assign led = {passes[15:13], errors != 0};

  wire tck, sel, capture, shift, tdi;
  reg [63:0] dr = 0;
  BSCANE2 #(.JTAG_CHAIN(1)) bscan (
    .TCK(tck), .SEL(sel), .CAPTURE(capture), .SHIFT(shift), .TDI(tdi),
    .TDO(dr[0])
  );
  always @(posedge tck)
    if (sel) begin
      if (capture) dr <= {"RV32", passes, errors};
      else if (shift) dr <= {tdi, dr[63:1]};
    end
endmodule
