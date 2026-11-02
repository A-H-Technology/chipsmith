// True dual port, 1024 x 16, one clock: each port reads its own write
// (new data) and the other's old data; both writing one word, port B's
// write wins (it comes last).
module tdp (input clk, input wea, input web, input [9:0] aa, input [9:0] ab,
            input [15:0] da, input [15:0] db, output reg [15:0] qa, output reg [15:0] qb);
  reg [15:0] mem [0:1023];
  always @(posedge clk) begin
    if (wea) mem[aa] <= da;
    if (web) mem[ab] <= db;
    qa <= wea ? da : mem[aa];
    qb <= web ? db : mem[ab];
  end
endmodule
