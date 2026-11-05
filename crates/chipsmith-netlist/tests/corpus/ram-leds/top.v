// M4K blocks on the DE2's LEDs.
//
// LEDR: a ROM (256 x 18, initial contents) stepped through about 12 times
// a second: a light bouncing end to end, then a bar filling up.
//
// LEDG: a RAM (512 x 9, simple dual port, initial contents a Gray code)
// read at the same step, so the green LEDs count in Gray code. Press
// KEY[0] to write SW[8:0] at the address being shown. Hold KEY[1] to stop
// the step and see the value come back; otherwise it returns every 512
// steps, about 43 s later.
//
// Each memory fills its whole block (cyclone2-re synth/m4k.txt: shallower
// memories stay in logic), and reads are synchronous.
module top(input CLOCK_50, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  // KEY[n] reads 0 while pressed. A press writes once: the key is
  // synchronised and its falling edge is the write enable, a register. Its
  // inversion sits in that register's LUT: an inverter straight from a pin
  // into the M4K's write enable or into a register (also what a register
  // initialised to 1 becomes) is a one-input LUT that quartus_map removes
  // and re-creates its own way, so the build wouldn't validate
  // byte-identical. Starting at 0, k1 & ~k0 stays low until a real press.
  reg k0 = 0, k1 = 0, write = 0;
  always @(posedge clk) begin
    k0 <= KEY[0];
    k1 <= k0;
    write <= k1 & ~k0;
  end
  wire hold = ~KEY[1];

  reg [21:0] tick = 0;
  reg [8:0] step = 0;
  always @(posedge clk) begin
    tick <= tick + 1'b1;
    if (&tick && !hold) step <= step + 1'b1;
  end

  reg [17:0] rom [0:255];
  reg [8:0] ram [0:511];
  integer i, p;
  initial begin
    for (i = 0; i < 256; i = i + 1) begin
      if (i < 136) begin
        // 34 steps there and back.
        p = i % 34;
        rom[i] = 18'b1 << (p < 18 ? p : 34 - p);
      end else begin
        rom[i] = ~(18'h3ffff << ((i - 136) % 19));
      end
    end
    for (i = 0; i < 512; i = i + 1) ram[i] = i ^ (i >> 1);
  end

  reg [17:0] r;
  reg [8:0] g;
  always @(posedge clk) begin
    r <= rom[step[7:0]];
    if (write) ram[step] <= SW[8:0];
    g <= ram[step];
  end
  assign LEDR = r;
  assign LEDG = g;
endmodule
