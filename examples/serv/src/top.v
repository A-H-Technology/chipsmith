// Olof Kindgren's SERV, the bit-serial RISC-V (RV32I), with its register
// file in RAM (serv_rf_top), running a four-instruction loop out of a ROM
// on its Wishbone instruction bus: it counts in x2 and stores it to
// 0x10000000 over the data bus, which drives LEDR (bits 31:14) and LEDG
// (bits 8:0), as examples/picorv32 does. KEY[0] (active low) resets.
module top(input CLOCK_50, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [3:0] rst_cnt = 0;
  wire rst = ~&rst_cnt;
  always @(posedge clk)
    if (!KEY[0]) rst_cnt <= 0;
    else if (rst) rst_cnt <= rst_cnt + 1'b1;

  wire [31:0] ibus_adr, dbus_adr, dbus_dat;
  wire [3:0] dbus_sel;
  wire ibus_cyc, dbus_we, dbus_cyc;
  reg [31:0] ibus_rdt = 0;
  reg ibus_ack = 0, dbus_ack = 0;

  serv_rf_top #(.RESET_PC(32'd0)) cpu (
    .clk(clk), .i_rst(rst), .i_timer_irq(1'b0),
    .o_ibus_adr(ibus_adr), .o_ibus_cyc(ibus_cyc), .i_ibus_rdt(ibus_rdt), .i_ibus_ack(ibus_ack),
    .o_dbus_adr(dbus_adr), .o_dbus_dat(dbus_dat), .o_dbus_sel(dbus_sel), .o_dbus_we(dbus_we),
    .o_dbus_cyc(dbus_cyc), .i_dbus_rdt(32'd0), .i_dbus_ack(dbus_ack),
    .o_ext_rs1(), .o_ext_rs2(), .o_ext_funct3(), .i_ext_rd(32'd0), .i_ext_ready(1'b0),
    .o_mdu_valid());

  reg [31:0] rom;
  always @*
    case (ibus_adr[4:2])
      0: rom = 32'h100000b7; // lui  x1, 0x10000
      1: rom = 32'h00000113; // li   x2, 0
      2: rom = 32'h00110113; // addi x2, x2, 1
      3: rom = 32'h0020a023; // sw   x2, 0(x1)
      4: rom = 32'hff9ff06f; // j    -8
      default: rom = 32'h00000013;
    endcase

  reg [31:0] leds = 0;
  always @(posedge clk) begin
    ibus_ack <= ibus_cyc & !ibus_ack;
    ibus_rdt <= rom;
    dbus_ack <= dbus_cyc & !dbus_ack;
    if (dbus_cyc && !dbus_ack && dbus_we && dbus_adr[28]) leds <= dbus_dat;
    if (rst) begin
      ibus_ack <= 0;
      dbus_ack <= 0;
    end
  end
  assign LEDR = leds[31:14];
  assign LEDG = leds[8:0];
endmodule
