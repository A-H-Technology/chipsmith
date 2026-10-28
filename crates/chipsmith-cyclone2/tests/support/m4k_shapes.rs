//! One memory per M4K configuration class, alone in a design with its
//! ports on the design's: what the per-configuration proofs check (the
//! block's model against yosys' memory, word by word).

/// (name, Verilog, blocks it maps to).
pub fn shapes() -> Vec<(&'static str, &'static str, usize)> {
    vec![
        (
            "sp_x1",
            "module top(input clk, input we, input [11:0] a, input d, output q);
  reg m [0:4095];
  reg [11:0] a_q;
  always @(posedge clk) begin if (we) m[a] <= d; a_q <= a; end
  assign q = m[a_q];
endmodule",
            1,
        ),
        (
            "sp_x2",
            "module top(input clk, input we, input [10:0] a, input [1:0] d, output [1:0] q);
  reg [1:0] m [0:2047];
  reg [10:0] a_q;
  always @(posedge clk) begin if (we) m[a] <= d; a_q <= a; end
  assign q = m[a_q];
endmodule",
            1,
        ),
        (
            "sdp_x4",
            "module top(input clk, input we, input [9:0] wa, input [9:0] ra, input [3:0] d, output reg [3:0] q);
  reg [3:0] m [0:1023];
  always @(posedge clk) begin if (we) m[wa] <= d; q <= m[ra]; end
endmodule",
            1,
        ),
        (
            "sdp_x9",
            "module top(input clk, input we, input [8:0] wa, input [8:0] ra, input [8:0] d, output reg [8:0] q);
  reg [8:0] m [0:511];
  always @(posedge clk) begin if (we) m[wa] <= d; q <= m[ra]; end
endmodule",
            1,
        ),
        (
            "sp_x18",
            "module top(input clk, input we, input [7:0] a, input [17:0] d, output [17:0] q);
  reg [17:0] m [0:255];
  reg [7:0] a_q;
  always @(posedge clk) begin if (we) m[a] <= d; a_q <= a; end
  assign q = m[a_q];
endmodule",
            1,
        ),
        (
            "sdp_x36",
            "module top(input clk, input we, input [6:0] wa, input [6:0] ra, input [35:0] d, output reg [35:0] q);
  reg [35:0] m [0:127];
  always @(posedge clk) begin if (we) m[wa] <= d; q <= m[ra]; end
endmodule",
            1,
        ),
        (
            "rom_x36",
            "module top(input clk, input [6:0] a, output reg [35:0] q);
  reg [35:0] m [0:127];
  integer i;
  initial for (i = 0; i < 128; i = i + 1) m[i] = (i * 64'h9E3779B97F4A7C15) >> 28;
  always @(posedge clk) q <= m[a];
endmodule",
            1,
        ),
        (
            "rom_x8_read_enable",
            "module top(input clk, input re, input [8:0] a, output reg [7:0] q);
  reg [7:0] m [0:511];
  integer i;
  initial for (i = 0; i < 512; i = i + 1) m[i] = (i * 32'h9E3779B1) >> 24;
  always @(posedge clk) if (re) q <= m[a];
endmodule",
            1,
        ),
        (
            "bytes_x16",
            "module top(input clk, input [1:0] be, input [7:0] a, input [15:0] d, output reg [15:0] q);
  reg [15:0] m [0:255];
  always @(posedge clk) begin
    if (be[0]) m[a][7:0] <= d[7:0];
    if (be[1]) m[a][15:8] <= d[15:8];
    q <= m[a];
  end
endmodule",
            1,
        ),
        (
            "bytes_x32",
            "module top(input clk, input [3:0] be, input [6:0] wa, input [6:0] ra, input [31:0] d, output reg [31:0] q);
  reg [31:0] m [0:127];
  always @(posedge clk) begin
    if (be[0]) m[wa][7:0] <= d[7:0];
    if (be[1]) m[wa][15:8] <= d[15:8];
    if (be[2]) m[wa][23:16] <= d[23:16];
    if (be[3]) m[wa][31:24] <= d[31:24];
    q <= m[ra];
  end
endmodule",
            1,
        ),
        (
            "mixed_w32_r8",
            "module top(input clk, input we, input [6:0] wa, input [8:0] ra, input [31:0] d, output reg [7:0] q);
  reg [7:0] m [0:511];
  always @(posedge clk) begin
    if (we) begin
      m[{wa, 2'd0}] <= d[7:0];
      m[{wa, 2'd1}] <= d[15:8];
      m[{wa, 2'd2}] <= d[23:16];
      m[{wa, 2'd3}] <= d[31:24];
    end
    q <= m[ra];
  end
endmodule",
            1,
        ),
        (
            "mixed_w8_r32",
            "module top(input clk, input we, input [8:0] wa, input [6:0] ra, input [7:0] d, output reg [31:0] q);
  reg [7:0] m [0:511];
  always @(posedge clk) begin
    if (we) m[wa] <= d;
    q <= {m[{ra, 2'd3}], m[{ra, 2'd2}], m[{ra, 2'd1}], m[{ra, 2'd0}]};
  end
endmodule",
            1,
        ),
        (
            "tdp_x9",
            "module top(input clk, input wea, input web, input [8:0] aa, input [8:0] ab, input [8:0] da, input [8:0] db,
           output reg [8:0] qa, output reg [8:0] qb);
  reg [8:0] m [0:511];
  wire webx = web & (ab != aa);
  always @(posedge clk) begin if (wea) m[aa] <= da; qa <= wea ? da : m[aa]; end
  always @(posedge clk) begin if (webx) m[ab] <= db; qb <= webx ? db : m[ab]; end
endmodule",
            1,
        ),
        (
            "tdp_x16_x4",
            "module top(input clk, input wea, input web, input [7:0] aa, input [9:0] ab, input [15:0] da, input [3:0] db,
           output reg [15:0] qa, output reg [3:0] qb);
  reg [3:0] m [0:1023];
  wire webx = web & (ab[9:2] != aa);
  always @(posedge clk) begin
    if (wea) begin
      m[{aa, 2'd0}] <= da[3:0];
      m[{aa, 2'd1}] <= da[7:4];
      m[{aa, 2'd2}] <= da[11:8];
      m[{aa, 2'd3}] <= da[15:12];
    end
    qa <= wea ? da : {m[{aa, 2'd3}], m[{aa, 2'd2}], m[{aa, 2'd1}], m[{aa, 2'd0}]};
  end
  always @(posedge clk) begin if (webx) m[ab] <= db; qb <= webx ? db : m[ab]; end
endmodule",
            1,
        ),
        // Both ports writing one word on one edge, in one process so the
        // later write wins in either front end; each port still reads its
        // own write's data (the block gives the masked one the old word).
        (
            "tdp_collide_x9",
            "module top(input clk, input wea, input web, input [8:0] aa, input [8:0] ab, input [8:0] da, input [8:0] db,
           output reg [8:0] qa, output reg [8:0] qb);
  reg [8:0] m [0:511];
  always @(posedge clk) begin
    if (wea) m[aa] <= da;
    if (web) m[ab] <= db;
    qa <= wea ? da : m[aa];
    qb <= web ? db : m[ab];
  end
endmodule",
            1,
        ),
        (
            "tdp_collide_x16_x4",
            "module top(input clk, input wea, input web, input [7:0] aa, input [9:0] ab, input [15:0] da, input [3:0] db,
           output reg [15:0] qa, output reg [3:0] qb);
  reg [3:0] m [0:1023];
  always @(posedge clk) begin
    if (wea) begin
      m[{aa, 2'd0}] <= da[3:0];
      m[{aa, 2'd1}] <= da[7:4];
      m[{aa, 2'd2}] <= da[11:8];
      m[{aa, 2'd3}] <= da[15:12];
    end
    if (web) m[ab] <= db;
    qa <= wea ? da : {m[{aa, 2'd3}], m[{aa, 2'd2}], m[{aa, 2'd1}], m[{aa, 2'd0}]};
    qb <= web ? db : m[ab];
  end
endmodule",
            1,
        ),
        (
            "two_writes_one_read_x9",
            "module top(input clk, input a, input b, input [8:0] x, input [8:0] y, input [8:0] d1, input [8:0] d2,
           output reg [8:0] q);
  reg [8:0] m [0:511];
  always @(posedge clk) begin
    if (a) m[x] <= d1;
    if (b) m[y] <= d2;
    q <= a ? d1 : m[x];
  end
endmodule",
            1,
        ),
        (
            "two_clocks_x9",
            "module top(input clk, input clk2, input we, input [8:0] wa, input [8:0] ra, input [8:0] d, output reg [8:0] q);
  reg [8:0] m [0:511];
  always @(posedge clk) if (we) m[wa] <= d;
  always @(posedge clk2) q <= m[ra];
endmodule",
            1,
        ),
        (
            "clock_enable_x9",
            "module top(input clk, input en, input we, input [8:0] a, input [8:0] d, output reg [8:0] q);
  reg [8:0] m [0:511];
  always @(posedge clk) if (en) begin if (we) m[a] <= d; q <= m[a]; end
endmodule",
            1,
        ),
        (
            "read_enable_x18",
            "module top(input clk, input re, input we, input [7:0] wa, input [7:0] ra, input [17:0] d, output reg [17:0] q);
  reg [17:0] m [0:255];
  always @(posedge clk) begin if (we) m[wa] <= d; if (re) q <= m[ra]; end
endmodule",
            1,
        ),
        (
            "read_enable_held_x9",
            "module top(input clk, input en, input we, input [8:0] a, input [8:0] d, output reg [8:0] q);
  reg [8:0] m [0:511];
  always @(posedge clk) begin if (we) m[a] <= d; if (en) q <= m[a]; end
endmodule",
            1,
        ),
        (
            "outreg_clear_x9",
            "module top(input clk, input clr, input oen, input we, input [8:0] wa, input [8:0] ra, input [8:0] d,
           output reg [8:0] p);
  reg [8:0] m [0:511];
  reg [8:0] q;
  always @(posedge clk) begin if (we) m[wa] <= d; q <= m[ra]; end
  always @(posedge clk or posedge clr) if (clr) p <= 0; else if (oen) p <= q;
endmodule",
            1,
        ),
        (
            "sync_reset_x18",
            "module top(input clk, input rst, input we, input [7:0] wa, input [7:0] ra, input [17:0] d, output reg [17:0] q);
  reg [17:0] m [0:255];
  always @(posedge clk) begin if (we) m[wa] <= d; if (rst) q <= 0; else q <= m[ra]; end
endmodule",
            1,
        ),
        (
            "shallow_x8",
            "module top(input clk, input we, input [5:0] a, input [7:0] d, output [7:0] q);
  reg [7:0] m [0:63];
  reg [5:0] a_q;
  always @(posedge clk) begin if (we) m[a] <= d; a_q <= a; end
  assign q = m[a_q];
endmodule",
            1,
        ),
    ]
}
