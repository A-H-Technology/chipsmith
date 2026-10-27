// Alex Forencich's AXI-stream UART on the DE2's RS-232 port, 115200 8N1
// (prescale = 50 MHz / (115200 * 8)). It echoes every byte it receives
// with SW[7:0] XORed in, and KEY[1] sends SW[7:0]. SW[17] loops TX back
// into RX inside the FPGA. LEDR shows the last byte received and a count
// of them, LEDG the busy and error flags. KEY[0] (active low) resets.
module top(input CLOCK_50, input [3:0] KEY, input [17:0] SW, input UART_RXD,
           output UART_TXD, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [1:0] rst_sync = 2'b11;
  always @(posedge clk) rst_sync <= {rst_sync[0], ~KEY[0]};
  wire rst = rst_sync[1];

  reg [2:0] key1 = 3'b111;
  always @(posedge clk) key1 <= {key1[1:0], KEY[1]};
  wire press = key1[2] & ~key1[1];

  wire [7:0] rx_data;
  wire rx_valid, tx_ready, tx_busy, rx_busy, overrun, frame;
  wire rxd = SW[17] ? UART_TXD : UART_RXD;

  uart #(.DATA_WIDTH(8)) u (
    .clk(clk), .rst(rst),
    .s_axis_tdata(rx_valid ? rx_data ^ SW[7:0] : SW[7:0]),
    .s_axis_tvalid(rx_valid | press), .s_axis_tready(tx_ready),
    .m_axis_tdata(rx_data), .m_axis_tvalid(rx_valid), .m_axis_tready(1'b1),
    .rxd(rxd), .txd(UART_TXD),
    .tx_busy(tx_busy), .rx_busy(rx_busy),
    .rx_overrun_error(overrun), .rx_frame_error(frame),
    .prescale(16'd54));

  reg [7:0] last = 0;
  reg [9:0] count = 0;
  reg [1:0] errors = 0;
  always @(posedge clk)
    if (rst) begin
      last <= 0; count <= 0; errors <= 0;
    end else begin
      if (rx_valid) begin
        last <= rx_data;
        count <= count + 1'b1;
      end
      errors <= errors | {overrun, frame};
    end

  assign LEDR = {count, last};
  assign LEDG = {4'b0, tx_ready, errors, rx_busy, tx_busy};
endmodule
