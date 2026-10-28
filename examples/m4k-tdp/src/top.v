// M4K true dual port: each port reads what it writes (new data, written
// as Quartus' template has it: `q <= we ? d : mem[a]`) and the other
// port's words as they were.
//
//   t9   512 x 9, both ports x9
//   t16  256 x 16 on port A, 1024 x 4 on port B (mixed widths)
module top(input CLOCK_50, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [31:0] cnt = 0;
  always @(posedge clk) cnt <= cnt + 1'b1;
  // Write data: counter bits mixed with the switches.
  wire [35:0] d = {cnt[31:14], cnt[17:0]} ^ {SW, SW};
  wire wea = SW[17] & cnt[2];
  wire web = SW[16] & cnt[3];
  // Port B doesn't write the word port A writes on the same edge: from two
  // always blocks, which write lands is a race in Verilog (yosys and
  // chipsmith order them differently). Collisions written in one process
  // map too (README, tests/support/m4k_shapes.rs tdp_collide_*).
  wire web9 = web & (ab[8:0] != aa[8:0]);
  wire web4 = web & (ab[9:2] != aa[7:0]);
  wire [11:0] aa = cnt[31:20];
  wire [11:0] ab = cnt[30:19] ^ SW[11:0];

  reg [8:0] t9 [0:511];
  reg [8:0] qa9, qb9;
  always @(posedge clk) begin
    if (wea) t9[aa[8:0]] <= d[8:0];
    qa9 <= wea ? d[8:0] : t9[aa[8:0]];
  end
  always @(posedge clk) begin
    if (web9) t9[ab[8:0]] <= d[17:9];
    qb9 <= web9 ? d[17:9] : t9[ab[8:0]];
  end

  reg [3:0] t4 [0:1023];
  reg [15:0] qa16;
  reg [3:0] qb4;
  always @(posedge clk) begin
    if (wea) begin
      t4[{aa[7:0], 2'd0}] <= d[3:0];
      t4[{aa[7:0], 2'd1}] <= d[7:4];
      t4[{aa[7:0], 2'd2}] <= d[11:8];
      t4[{aa[7:0], 2'd3}] <= d[15:12];
    end
    qa16 <= wea ? d[15:0] : {t4[{aa[7:0], 2'd3}], t4[{aa[7:0], 2'd2}], t4[{aa[7:0], 2'd1}], t4[{aa[7:0], 2'd0}]};
  end
  always @(posedge clk) begin
    if (web4) t4[ab[9:0]] <= d[35:32];
    qb4 <= web4 ? d[35:32] : t4[ab[9:0]];
  end

  localparam N = 38;
  wire [N-1:0] all = {qa9, qb9, qa16, qb4};

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
