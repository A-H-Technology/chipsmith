// Bruno Levy's FemtoRV32 "Quark" (RV32I) running a four-instruction loop
// out of a ROM: it counts in x2 and stores it to 0x00400000, which drives
// LEDR (bits 31:14) and LEDG (bits 8:0), as examples/picorv32 does. KEY[0]
// (active low) resets.
module top(input CLOCK_50, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [3:0] rst_cnt = 0;
  wire resetn = &rst_cnt;
  always @(posedge clk)
    if (!KEY[0]) rst_cnt <= 0;
    else if (!resetn) rst_cnt <= rst_cnt + 1'b1;

  wire [31:0] mem_addr, mem_wdata;
  wire [3:0] mem_wmask;
  wire mem_rstrb;
  reg [31:0] mem_rdata = 0;

  FemtoRV32 cpu (
    .clk(clk), .mem_addr(mem_addr), .mem_wdata(mem_wdata), .mem_wmask(mem_wmask),
    .mem_rdata(mem_rdata), .mem_rstrb(mem_rstrb), .mem_rbusy(1'b0), .mem_wbusy(1'b0),
    .reset(resetn));

  reg [31:0] rom;
  always @*
    case (mem_addr[4:2])
      0: rom = 32'h004000b7; // lui  x1, 0x400
      1: rom = 32'h00000113; // li   x2, 0
      2: rom = 32'h00110113; // addi x2, x2, 1
      3: rom = 32'h0020a023; // sw   x2, 0(x1)
      4: rom = 32'hff9ff06f; // j    -8
      default: rom = 32'h00000013;
    endcase

  reg [31:0] leds = 0;
  always @(posedge clk) begin
    if (mem_rstrb) mem_rdata <= rom;
    if (|mem_wmask && mem_addr[22]) leds <= mem_wdata;
  end
  assign LEDR = leds[31:14];
  assign LEDG = leds[8:0];
endmodule
