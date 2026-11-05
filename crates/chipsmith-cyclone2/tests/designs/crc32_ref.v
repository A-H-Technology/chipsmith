// examples/crc32 with Ethernet's CRC-32 written the textbook way, a bit at
// a time (reflected, polynomial 0xedb88320), eight bits a cycle: the
// reference its lfsr.v instance is proven against.
module top(input CLOCK_50, input [3:0] KEY, input [17:0] SW,
           output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [7:0] count = 0;
  reg [31:0] crc = 32'hffffffff;
  reg [31:0] shown = 0;

  function [31:0] crc_byte(input [31:0] c, input [7:0] d);
    integer i;
    begin
      crc_byte = c ^ {24'b0, d};
      for (i = 0; i < 8; i = i + 1)
        crc_byte = crc_byte[0] ? (crc_byte >> 1) ^ 32'hedb88320 : crc_byte >> 1;
    end
  endfunction

  always @(posedge clk) begin
    count <= count + 1'b1;
    crc <= KEY[0] ? crc_byte(crc, count ^ SW[7:0]) : 32'hffffffff;
    if (&count) shown <= ~crc;
  end
  assign LEDR = shown[17:0];
  assign LEDG = shown[26:18];
endmodule
