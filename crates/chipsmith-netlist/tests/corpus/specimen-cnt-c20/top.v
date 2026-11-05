module top(input CLOCK_50, output [17:0] LEDR);
  reg [19:0] cnt;
  always @(posedge CLOCK_50) cnt <= cnt + 1'b1;
  assign LEDR = cnt[19:2];
endmodule
