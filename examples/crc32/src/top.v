// Alex Forencich's parametrizable parallel LFSR/CRC (lfsr.v, from
// verilog-ethernet) as Ethernet's CRC-32 (polynomial 0x04c11db7, Galois,
// bit-reversed, 8 bits a cycle), the configuration his eth_mac cores use.
// It folds a byte stream (a counter XOR SW[7:0]) into the CRC every cycle;
// KEY[0] (active low) restarts it at 0xffffffff. LEDR and LEDG show the
// inverted CRC, sampled when the counter wraps.
module top(input CLOCK_50, input [3:0] KEY, input [17:0] SW,
           output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [7:0] count = 0;
  reg [31:0] crc = 32'hffffffff;
  reg [31:0] shown = 0;
  wire [31:0] next;

  lfsr #(.LFSR_WIDTH(32), .LFSR_POLY(32'h4c11db7), .LFSR_CONFIG("GALOIS"),
         .LFSR_FEED_FORWARD(0), .REVERSE(1), .DATA_WIDTH(8), .STYLE("AUTO")) crc32 (
    .data_in(count ^ SW[7:0]), .state_in(crc), .data_out(), .state_out(next));

  always @(posedge clk) begin
    count <= count + 1'b1;
    crc <= KEY[0] ? next : 32'hffffffff;
    if (&count) shown <= ~crc;
  end
  assign LEDR = shown[17:0];
  assign LEDG = shown[26:18];
endmodule
