// Every register shape the 7-series slice has, and the carry chain's
// arithmetic.
module regs_arith(input clk, input en, input rst, input rst_n, input [11:0] a, input [11:0] b,
                  input ci, output [11:0] sum, output [11:0] diff, output lt, output ge,
                  output eq, output [12:0] wide, output [7:0] regs, output [5:0] cnt);
  reg [11:0] s, d;
  reg l, g, e;
  reg [12:0] w;
  reg [7:0] r;
  reg [5:0] c = 6'd37;
  always @(posedge clk) begin
    s <= a + b + ci;           // carry-in from logic
    d <= a - b;
    l <= $signed(a) < $signed(b);
    g <= a >= 12'd100;
    e <= a == b;
    w <= {1'b0, a} + {b, 1'b1};
  end
  // Synchronous reset over the enable (FDRE), set (FDSE), reset under the
  // enable, asynchronous clear (FDCE) and preset (FDPE), a falling edge,
  // active-low controls, power-up values of both kinds.
  always @(posedge clk) if (rst) r[0] <= 0; else if (en) r[0] <= a[0];
  always @(posedge clk) if (rst) r[1] <= 1; else if (en) r[1] <= a[1];
  always @(posedge clk) if (en) begin if (rst) r[2] <= 0; else r[2] <= a[2]; end
  always @(posedge clk or posedge rst) if (rst) r[3] <= 0; else if (en) r[3] <= a[3];
  always @(posedge clk or posedge rst) if (rst) r[4] <= 1; else r[4] <= a[4];
  always @(negedge clk) r[5] <= a[5] ^ b[5];
  always @(posedge clk or negedge rst_n) if (!rst_n) r[6] <= 0; else if (!en) r[6] <= a[6];
  initial r[7] = 1;
  always @(posedge clk) r[7] <= a[7] | r[7] & b[7];
  // A down counter with a power-up value and a load.
  always @(posedge clk) if (en) c <= c - 1'b1; else if (rst) c <= a[5:0];
  assign {sum, diff, lt, ge, eq, wide, regs, cnt} = {s, d, l, g, e, w, r, c};
endmodule
