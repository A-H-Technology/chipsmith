// M4K memories shallower than their block. A 64 x 8 RAM uses 6 of a x9
// block's 9 address pins; synthesis repeats its top address bit on the
// other three, so every word has an address of its own.
//
//   s8   64 x 8 single port
//   r36  32 x 36 simple dual port
//   o4   16 x 4 ROM
module top(input CLOCK_50, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [31:0] cnt = 0;
  always @(posedge clk) cnt <= cnt + 1'b1;
  // Write data: counter bits mixed with the switches.
  wire [35:0] d = {cnt[31:14], cnt[17:0]} ^ {SW, SW};
  wire we = SW[17] & cnt[2];
  wire [5:0] wa = cnt[31:26];
  wire [5:0] ra = cnt[30:25] ^ SW[5:0];

  reg [7:0] s8 [0:63];
  reg [5:0] s8_a;
  always @(posedge clk) begin
    if (we) s8[wa] <= d[7:0];
    s8_a <= wa;
  end
  wire [7:0] q8 = s8[s8_a];

  reg [35:0] r36 [0:31];
  reg [35:0] q36;
  always @(posedge clk) begin
    if (we) r36[wa[4:0]] <= d;
    q36 <= r36[ra[4:0]];
  end

  reg [3:0] o4 [0:15];
  integer i;
  initial for (i = 0; i < 16; i = i + 1) o4[i] = i ^ (i >> 1);
  reg [3:0] q4;
  always @(posedge clk) q4 <= o4[ra[3:0]];

  localparam N = 48;
  wire [N-1:0] all = {q8, q36, q4};

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
