// M4K memories over several blocks.
//
//   d9   2048 x 9 simple dual port: four 512 x 9 blocks, the write enable
//        decoded from the top address bits, the read data multiplexed by
//        them, registered
//   d2   8192 x 2 ROM: deeper than any block
//   w40  256 x 40 single port: three blocks side by side
module top(input CLOCK_50, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [31:0] cnt = 0;
  always @(posedge clk) cnt <= cnt + 1'b1;
  // Write data: counter bits mixed with the switches.
  wire [35:0] d = {cnt[31:14], cnt[17:0]} ^ {SW, SW};
  wire we = SW[17] & cnt[2];
  wire [12:0] wa = cnt[31:19];
  wire [12:0] ra = cnt[30:18] ^ {SW[12:0]};

  reg [8:0] d9 [0:2047];
  reg [8:0] q9;
  always @(posedge clk) begin
    if (we) d9[wa[10:0]] <= d[8:0];
    q9 <= d9[ra[10:0]];
  end

  reg [1:0] d2 [0:8191];
  integer i;
  initial for (i = 0; i < 8192; i = i + 1) d2[i] = (i * 32'h9E3779B1) >> 30;
  reg [1:0] q2;
  always @(posedge clk) q2 <= d2[ra];

  reg [39:0] w40 [0:255];
  reg [7:0] w40_a;
  always @(posedge clk) begin
    if (we) w40[wa[7:0]] <= {d[3:0], d};
    w40_a <= wa[7:0];
  end
  wire [39:0] q40 = w40[w40_a];

  localparam N = 51;
  wire [N-1:0] all = {q9, q2, q40};

  // Every read bit reaches a LED: bit n folded into LED n % 27.
  reg [26:0] f;
  integer n;
  always @* begin
    f = 27'h0;
    for (n = 0; n < N; n = n + 1) f[n % 27] = f[n % 27] ^ all[n];
  end
  assign LEDR = f[17:0];
  assign LEDG = f[26:18];
endmodule
