// nandland's SPI master with a single chip select, mode 0, 6.25 MHz SCLK,
// on expansion header GPIO_0's first four pins: SCLK, MOSI, CS_n, MISO.
// It sends two-byte transfers back to back, a running counter XOR SW[7:0],
// and shows the bytes it reads from MISO on LEDR (jumper MOSI to MISO for
// a loopback). KEY[0] (active low) resets.
module top(input CLOCK_50, input [3:0] KEY, input [17:0] SW,
           output SPI_SCLK, output SPI_MOSI, output SPI_CS_N, input SPI_MISO,
           output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [1:0] rst_sync = 2'b00;
  always @(posedge clk) rst_sync <= {rst_sync[0], KEY[0]};
  wire rst_l = rst_sync[1];

  wire tx_ready, rx_dv, sclk, mosi, cs_n;
  wire [1:0] rx_count;
  wire [7:0] rx_byte;

  reg [7:0] counter = 0;
  reg tx_dv = 0;
  always @(posedge clk) begin
    tx_dv <= 0;
    if (tx_ready && !tx_dv) begin
      tx_dv <= 1;
      counter <= counter + 1'b1;
    end
  end

  SPI_Master_With_Single_CS #(.SPI_MODE(0), .CLKS_PER_HALF_BIT(4),
                              .MAX_BYTES_PER_CS(2), .CS_INACTIVE_CLKS(1)) spi (
    .i_Rst_L(rst_l), .i_Clk(clk),
    .i_TX_Count(2'd2), .i_TX_Byte(counter ^ SW[7:0]), .i_TX_DV(tx_dv),
    .o_TX_Ready(tx_ready),
    .o_RX_Count(rx_count), .o_RX_DV(rx_dv), .o_RX_Byte(rx_byte),
    .o_SPI_Clk(sclk), .i_SPI_MISO(SPI_MISO), .o_SPI_MOSI(mosi), .o_SPI_CS_n(cs_n));

  assign SPI_SCLK = sclk;
  assign SPI_MOSI = mosi;
  assign SPI_CS_N = cs_n;

  reg [15:0] received = 0;
  reg [1:0] which = 0;
  always @(posedge clk)
    if (rx_dv) begin
      received <= {received[7:0], rx_byte};
      which <= rx_count;
    end

  assign LEDR = {which, received};
  assign LEDG = {6'b0, cs_n, tx_ready, rx_dv};
endmodule
