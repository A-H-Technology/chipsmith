// M4K clock enables and read enables.
//
//   e8   512 x 8 single port, everything under one enable: the block's
//        clock enable (ENA0)
//   r18  256 x 18 simple dual port with a read enable: the read port on
//        clock 1 (the same clock) under ENA1
//   r4   1024 x 4 ROM with a read enable
//   h9   512 x 9, a write the read enable doesn't cover: LE registers hold
//        the read data
module top(input CLOCK_50, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [31:0] cnt = 0;
  always @(posedge clk) cnt <= cnt + 1'b1;
  // Write data: counter bits mixed with the switches.
  wire [35:0] d = {cnt[31:14], cnt[17:0]} ^ {SW, SW};
  wire en = cnt[3] ^ SW[16];
  wire re = cnt[4] & SW[15];
  wire we = SW[17] & cnt[2];
  wire [11:0] wa = cnt[31:20];
  wire [11:0] ra = cnt[30:19] ^ SW[11:0];

  reg [7:0] e8 [0:511];
  reg [7:0] q8;
  always @(posedge clk)
    if (en) begin
      if (we) e8[wa[8:0]] <= d[7:0];
      q8 <= e8[wa[8:0]];
    end

  reg [17:0] r18 [0:255];
  reg [17:0] q18;
  always @(posedge clk) begin
    if (we) r18[wa[7:0]] <= d[17:0];
    if (re) q18 <= r18[ra[7:0]];
  end

  reg [3:0] r4 [0:1023];
  integer i;
  initial for (i = 0; i < 1024; i = i + 1) r4[i] = (i * 32'h9E3779B1) >> 28;
  reg [3:0] q4;
  always @(posedge clk) if (re) q4 <= r4[ra[9:0]];

  reg [8:0] h9 [0:511];
  reg [8:0] q9;
  always @(posedge clk) begin
    if (we) h9[ra[8:0]] <= d[8:0];
    if (en) q9 <= h9[ra[8:0]];
  end

  localparam N = 39;
  wire [N-1:0] all = {q8, q18, q4, q9};

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
