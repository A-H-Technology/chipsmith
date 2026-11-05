// A 27-bit counter on the Zybo's 125 MHz clock; its top four bits drive
// led[3:0], so led[0] (LD0) toggles about twice a second.
module blinky(input clk, output [3:0] led);
  reg [26:0] counter = 0;
  always @(posedge clk) counter <= counter + 1'b1;
  assign led = {counter[23], counter[24], counter[25], counter[26]};
endmodule
