// M4K on two clocks: simple dual ports written on CLOCK_50 and read on
// CLOCK_27 (the block's clock 1).
//
//   x9   512 x 9
//   x36  128 x 36, its read data registered again on CLOCK_27 (the
//        output register, clock 1)
module top(input CLOCK_50, input CLOCK_27, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [31:0] cnt = 0;
  always @(posedge clk) cnt <= cnt + 1'b1;
  // Write data: counter bits mixed with the switches.
  wire [35:0] d = {cnt[31:14], cnt[17:0]} ^ {SW, SW};
  reg [15:0] rcnt = 0;
  always @(posedge CLOCK_27) rcnt <= rcnt + 1'b1;
  wire we = SW[17] & cnt[2];
  wire [11:0] wa = cnt[31:20];
  wire [8:0] ra = rcnt[15:7] ^ SW[8:0];

  reg [8:0] x9 [0:511];
  reg [8:0] q9;
  always @(posedge clk) if (we) x9[wa[8:0]] <= d[8:0];
  always @(posedge CLOCK_27) q9 <= x9[ra];

  reg [35:0] x36 [0:127];
  reg [35:0] q36, p36;
  always @(posedge clk) if (we) x36[wa[6:0]] <= d;
  always @(posedge CLOCK_27) begin
    q36 <= x36[ra[8:2]];
    p36 <= q36;
  end

  localparam N = 45;
  wire [N-1:0] all = {q9, p36};

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
