// M4K output registers.
//
//   o9   512 x 9 simple dual port, its read data registered again: the
//        block's output register
//   c18  256 x 18 ROM, output register cleared asynchronously (CLR0)
//   n8   512 x 8 simple dual port, its output register under an enable of
//        its own (clock 1, the same clock, under ENA1)
//   s16  256 x 16, the read data reset synchronously: LE logic after the
//        block
module top(input CLOCK_50, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [31:0] cnt = 0;
  always @(posedge clk) cnt <= cnt + 1'b1;
  // Write data: counter bits mixed with the switches.
  wire [35:0] d = {cnt[31:14], cnt[17:0]} ^ {SW, SW};
  wire we = SW[17] & cnt[2];
  wire [11:0] wa = cnt[31:20];
  wire [11:0] ra = cnt[30:19] ^ SW[11:0];
  // Clears and resets from registers, not straight from a pin.
  reg clr = 0, rst = 0, oen = 0;
  always @(posedge clk) begin
    clr <= SW[16] & cnt[9];
    rst <= SW[15] & cnt[8];
    oen <= cnt[5] | SW[14];
  end

  reg [8:0] o9 [0:511];
  reg [8:0] q9, p9;
  always @(posedge clk) begin
    if (we) o9[wa[8:0]] <= d[8:0];
    q9 <= o9[ra[8:0]];
    p9 <= q9;
  end

  reg [17:0] c18 [0:255];
  integer i;
  initial for (i = 0; i < 256; i = i + 1) c18[i] = (i * 32'h9E3779B1) >> 13;
  reg [17:0] q18, p18;
  always @(posedge clk) q18 <= c18[ra[7:0]];
  always @(posedge clk or posedge clr)
    if (clr) p18 <= 0;
    else p18 <= q18;

  reg [7:0] n8 [0:511];
  reg [7:0] q8, p8;
  always @(posedge clk) begin
    if (we) n8[wa[8:0]] <= d[7:0];
    q8 <= n8[ra[8:0]];
    if (oen) p8 <= q8;
  end

  reg [15:0] s16 [0:255];
  reg [15:0] q16;
  always @(posedge clk) begin
    if (we) s16[wa[7:0]] <= d[15:0];
    if (rst) q16 <= 16'h0;
    else q16 <= s16[ra[7:0]];
  end

  localparam N = 51;
  wire [N-1:0] all = {p9, p18, p8, q16};

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
