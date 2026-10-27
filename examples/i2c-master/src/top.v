// Alex Forencich's I2C master on expansion header GPIO_0's first two pins
// (SCL, SDA, open drain; add pull-ups), 100 kHz (prescale = 50 MHz /
// (100 kHz * 4)). It loops a one-byte transaction to the 7-bit address on
// SW[6:0]: a read, or with SW[17] up a write of SW[15:8]. LEDR shows the
// count and last byte read, LEDG the core's status and a missed ACK.
// KEY[0] (active low) resets.
module top(input CLOCK_50, input [3:0] KEY, input [17:0] SW,
           inout I2C_SCL, inout I2C_SDA, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [1:0] rst_sync = 2'b11;
  always @(posedge clk) rst_sync <= {rst_sync[0], ~KEY[0]};
  wire rst = rst_sync[1];

  wire scl_o, scl_t, sda_o, sda_t;
  wire cmd_ready, data_ready, rx_valid, rx_last, busy, bus_control, bus_active, missed_ack;
  wire [7:0] rx_data;

  i2c_master i2c (
    .clk(clk), .rst(rst),
    .s_axis_cmd_address(SW[6:0]),
    .s_axis_cmd_start(1'b1), .s_axis_cmd_read(~SW[17]), .s_axis_cmd_write(SW[17]),
    .s_axis_cmd_write_multiple(1'b0), .s_axis_cmd_stop(1'b1),
    .s_axis_cmd_valid(1'b1), .s_axis_cmd_ready(cmd_ready),
    .s_axis_data_tdata(SW[15:8]), .s_axis_data_tvalid(1'b1),
    .s_axis_data_tready(data_ready), .s_axis_data_tlast(1'b1),
    .m_axis_data_tdata(rx_data), .m_axis_data_tvalid(rx_valid),
    .m_axis_data_tready(1'b1), .m_axis_data_tlast(rx_last),
    .scl_i(I2C_SCL), .scl_o(scl_o), .scl_t(scl_t),
    .sda_i(I2C_SDA), .sda_o(sda_o), .sda_t(sda_t),
    .busy(busy), .bus_control(bus_control), .bus_active(bus_active),
    .missed_ack(missed_ack),
    .prescale(16'd125), .stop_on_idle(1'b1));

  assign I2C_SCL = scl_t ? 1'bz : scl_o;
  assign I2C_SDA = sda_t ? 1'bz : sda_o;

  reg [7:0] last = 0;
  reg [7:0] reads = 0;
  reg nack = 0;
  always @(posedge clk)
    if (rst) begin
      last <= 0; reads <= 0; nack <= 0;
    end else begin
      if (rx_valid) begin
        last <= rx_data;
        reads <= reads + 1'b1;
      end
      if (missed_ack) nack <= 1;
    end

  assign LEDR = {2'b0, reads, last};
  assign LEDG = {2'b0, cmd_ready, data_ready, rx_last, nack, bus_active, bus_control, busy};
endmodule
