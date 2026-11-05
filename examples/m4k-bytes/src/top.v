// M4K byte enables.
//
//   b16  256 x 16, two byte enables, read and written at one address (the
//        read returns the old data: a simple dual port on one address)
//   b32  128 x 32, four byte enables, simple dual port
module top(input CLOCK_50, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [31:0] cnt = 0;
  always @(posedge clk) cnt <= cnt + 1'b1;
  // Write data: counter bits mixed with the switches.
  wire [35:0] d = {cnt[31:14], cnt[17:0]} ^ {SW, SW};
  wire [3:0] be = cnt[7:4] & {4{SW[17]}};
  wire [7:0] a16 = cnt[27:20];
  wire [6:0] wa32 = cnt[26:20];
  wire [6:0] ra32 = cnt[31:25] ^ SW[6:0];

  reg [15:0] b16 [0:255];
  reg [15:0] q16;
  always @(posedge clk) begin
    if (be[0]) b16[a16][7:0] <= d[7:0];
    if (be[1]) b16[a16][15:8] <= d[15:8];
    q16 <= b16[a16];
  end

  reg [31:0] b32 [0:127];
  reg [31:0] q32;
  always @(posedge clk) begin
    if (be[0]) b32[wa32][7:0] <= d[7:0];
    if (be[1]) b32[wa32][15:8] <= d[15:8];
    if (be[2]) b32[wa32][23:16] <= d[23:16];
    if (be[3]) b32[wa32][31:24] <= d[31:24];
    q32 <= b32[ra32];
  end

  localparam N = 48;
  wire [N-1:0] all = {q16, q32};

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
