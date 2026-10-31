// Asynchronous resets as DE2 code writes them: active low from a pin
// (`negedge rst_n`), clearing and presetting, with an enable and on a
// falling edge, beside an active-high reset and the same pin as an
// active-high clear.
module top(input clk, input rst_n, input rst, input en, input a, input [3:0] d,
           output [5:0] c, output [3:0] p, output [3:0] h, output n, output b);
  reg [5:0] cnt;
  always @(posedge clk or negedge rst_n)
    if (!rst_n) cnt <= 0;
    else if (en) cnt <= cnt + 1'b1;
  reg [3:0] pr;
  always @(posedge clk or negedge rst_n)
    if (!rst_n) pr <= 4'hf;
    else pr <= d ^ cnt[3:0];
  reg [3:0] hi;
  always @(posedge clk or posedge rst)
    if (rst) hi <= 0;
    else hi <= {hi[2:0], a};
  reg ne;
  always @(negedge clk or negedge rst_n)
    if (!rst_n) ne <= 0;
    else ne <= a ^ d[0];
  reg both;
  always @(posedge clk or posedge rst_n)
    if (rst_n) both <= 0;
    else both <= d[1];
  assign c = cnt;
  assign p = pr;
  assign h = hi;
  assign n = ne;
  assign b = both;
endmodule
