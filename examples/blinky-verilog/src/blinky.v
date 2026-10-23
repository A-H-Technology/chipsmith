// A 26-bit counter on CLOCK_50; its top four bits drive LEDR[3:0], so
// LEDR[0] toggles about once a second.
module blinky(input CLOCK_50, output [3:0] LEDR);
  reg [25:0] counter = 0;
  always @(posedge CLOCK_50) counter <= counter + 1'b1;
  assign LEDR = {counter[22], counter[23], counter[24], counter[25]};
endmodule
