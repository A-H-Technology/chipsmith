// Twenty-four of examples/picorv32's cores, about 28K of the EP2C35's
// 33K LEs: a synthetic near-full-chip design for timing the placer and
// router. Each core runs the same loop against its own ROM and LED
// register; the LEDs show the XOR of every core's register. KEY[0]
// (active low) resets.
module top(input CLOCK_50, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  localparam N = 24;
  wire clk = CLOCK_50;
  reg [3:0] rst_cnt = 0;
  wire resetn = &rst_cnt;
  always @(posedge clk)
    if (!KEY[0]) rst_cnt <= 0;
    else if (!resetn) rst_cnt <= rst_cnt + 1'b1;

  wire [32*N-1:0] all_leds;
  genvar i;
  generate for (i = 0; i < N; i = i + 1) begin : core
    wire mem_valid, mem_instr;
    reg mem_ready = 0;
    wire [31:0] mem_addr, mem_wdata;
    wire [3:0] mem_wstrb;
    reg [31:0] mem_rdata = 0;

    picorv32 #(
      .ENABLE_COUNTERS(0), .ENABLE_COUNTERS64(0), .CATCH_MISALIGN(0), .CATCH_ILLINSN(0),
      .TWO_STAGE_SHIFT(0), .ENABLE_REGS_DUALPORT(1)
    ) cpu (
      .clk(clk), .resetn(resetn), .mem_valid(mem_valid), .mem_instr(mem_instr), .mem_ready(mem_ready),
      .mem_addr(mem_addr), .mem_wdata(mem_wdata), .mem_wstrb(mem_wstrb), .mem_rdata(mem_rdata));

    reg [31:0] rom;
    always @*
      case (mem_addr[4:2])
        0: rom = 32'h100000b7; // lui  x1, 0x10000
        1: rom = 32'h00000113 ^ (i << 20); // li   x2, i
        2: rom = 32'h00110113; // addi x2, x2, 1
        3: rom = 32'h0020a023; // sw   x2, 0(x1)
        4: rom = 32'hff9ff06f; // j    -8
        default: rom = 32'h00000013;
      endcase

    reg [31:0] leds = 0;
    always @(posedge clk) begin
      mem_ready <= 0;
      if (mem_valid && !mem_ready) begin
        mem_ready <= 1;
        if (mem_addr[28]) begin
          if (|mem_wstrb) leds <= mem_wdata;
        end else
          mem_rdata <= rom;
      end
    end
    assign all_leds[32*i +: 32] = leds;
  end endgenerate

  reg [31:0] folded;
  integer k;
  always @* begin
    folded = 0;
    for (k = 0; k < N; k = k + 1) folded = folded ^ all_leds[32*k +: 32];
  end
  assign LEDR = folded[31:14];
  assign LEDG = folded[8:0];
endmodule
