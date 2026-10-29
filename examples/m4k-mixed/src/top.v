// M4K simple dual ports with different write and read widths.
//
//   w32  written 32 bits at a time (128 words), read 8 (512 words)
//   r32  written 8 bits at a time (512 words), read 32 (128 words)
//   w2   written 2 bits at a time (2048 words), read 1 (4096 words)
module top(input CLOCK_50, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [31:0] cnt = 0;
  always @(posedge clk) cnt <= cnt + 1'b1;
  // Write data: counter bits mixed with the switches.
  wire [35:0] d = {cnt[31:14], cnt[17:0]} ^ {SW, SW};
  wire we = SW[17] & cnt[2];
  wire [11:0] wa = cnt[31:20];
  wire [11:0] ra = cnt[30:19] ^ SW[11:0];

  reg [7:0] w32 [0:511];
  reg [7:0] q8;
  always @(posedge clk) begin
    if (we) begin
      w32[{wa[6:0], 2'd0}] <= d[7:0];
      w32[{wa[6:0], 2'd1}] <= d[15:8];
      w32[{wa[6:0], 2'd2}] <= d[23:16];
      w32[{wa[6:0], 2'd3}] <= d[31:24];
    end
    q8 <= w32[ra[8:0]];
  end

  reg [7:0] r32 [0:511];
  reg [31:0] q32;
  always @(posedge clk) begin
    if (we) r32[wa[8:0]] <= d[7:0];
    q32 <= {r32[{ra[6:0], 2'd3}], r32[{ra[6:0], 2'd2}], r32[{ra[6:0], 2'd1}], r32[{ra[6:0], 2'd0}]};
  end

  reg w2 [0:4095];
  reg q1;
  always @(posedge clk) begin
    if (we) begin
      w2[{wa[10:0], 1'b0}] <= d[0];
      w2[{wa[10:0], 1'b1}] <= d[1];
    end
    q1 <= w2[ra];
  end

  localparam N = 41;
  wire [N-1:0] all = {q8, q32, q1};

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
