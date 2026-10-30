// 640x480 at 60 Hz (25 MHz pixels, 800 x 525 per frame: 59.52 Hz) on the
// DE2's VGA DAC: eight colour bars over the top half, red, green, blue
// and grey ramps over the bottom, a white border. PLL_1 makes the 25 MHz
// pixel clock (CLK0) and 50 MHz (CLK1) from CLOCK_50.
//
// VGA_CLK, the DAC's latch clock, can't be the PLL clock itself: a PLL
// clock reaches only its PLL<n>_OUT pin, and VGA_CLK (PIN_B8) isn't one.
// So a register on the 50 MHz clock makes it, rising halfway through each
// pixel: `t` toggles on every pixel clock, and VGA_CLK is t, sampled on
// 50 MHz, XORed with its previous sample (high after the 50 MHz edge
// between pixel edges, low after the one on a pixel edge). The two clocks
// come from one VCO, so their edges keep that order.
//
// USER1 holds "VGA!" and Gray-coded counters of CLOCK_50, the pixel
// clock and the frames: `cargo run --release -p chipsmith-cyclone2
// --example de2_user1 -- --words 4` prints 0.500000 (25 MHz) and
// 0.00000119 (59.52 Hz) times CLOCK_50. LEDG[0] is the PLL's lock.
module top(
  input CLOCK_50,
  output reg [9:0] VGA_R,
  output reg [9:0] VGA_G,
  output reg [9:0] VGA_B,
  output reg VGA_HS,
  output reg VGA_VS,
  output reg VGA_BLANK,
  output VGA_SYNC,
  output reg VGA_CLK,
  output LEDG
);
  wire pclk, clk50, locked;
  CY2_PLL #(
    .INCLK0_INPUT_FREQUENCY(20000),
    .CLK0_MULTIPLY_BY(1),
    .CLK0_DIVIDE_BY(2),
    .CLK1_MULTIPLY_BY(1),
    .CLK1_DIVIDE_BY(1)
  ) pll (
    .INCLK(CLOCK_50),
    .CLK0(pclk),
    .CLK1(clk50),
    .LOCKED(locked)
  );
  assign LEDG = locked;
  assign VGA_SYNC = 1'b0;

  reg t = 0;
  always @(posedge pclk) t <= ~t;
  reg s = 0;
  always @(posedge clk50) begin
    s <= t;
    VGA_CLK <= t ^ s;
  end

  // 800 x 525: 640 visible, front porch 16, sync 96, back porch 48; 480
  // lines, 10, 2, 33. Both syncs active low.
  reg [9:0] x = 0, y = 0;
  reg [31:0] frames = 0, pixels = 0;
  always @(posedge pclk) begin
    pixels <= pixels + 1'b1;
    if (x == 10'd799) begin
      x <= 0;
      if (y == 10'd524) begin
        y <= 0;
        frames <= frames + 1'b1;
      end else y <= y + 1'b1;
    end else x <= x + 1'b1;
  end

  wire visible = x < 10'd640 && y < 10'd480;
  wire border = x == 0 || x == 10'd639 || y == 0 || y == 10'd479;
  // Top half: bars white, yellow, cyan, green, magenta, red, blue, black.
  wire [2:0] bar = ~x[9:7];
  // Bottom half: four 60-line bands, red, green, blue and grey ramps
  // (x * 1.5: 0 to 958 of 1023).
  wire [9:0] ramp = x + (x >> 1);
  wire [1:0] band = y < 10'd300 ? 2'd0 : y < 10'd360 ? 2'd1 : y < 10'd420 ? 2'd2 : 2'd3;
  reg [9:0] r, g, b;
  always @* begin
    if (border) {r, g, b} = {30{1'b1}};
    else if (y < 10'd240) begin
      r = {10{bar[1]}};
      g = {10{bar[2]}};
      b = {10{bar[0]}};
    end else begin
      r = band == 2'd0 || band == 2'd3 ? ramp : 10'd0;
      g = band == 2'd1 || band == 2'd3 ? ramp : 10'd0;
      b = band == 2'd2 || band == 2'd3 ? ramp : 10'd0;
    end
  end

  // A pixel's outputs, in the pins' I/O-cell registers (synthesis finds
  // a reset in `visible ? r : 0`; the I/O cell's register samples it as
  // logic on its D).
  initial begin
    VGA_HS = 1'b1;
    VGA_VS = 1'b1;
  end
  always @(posedge pclk) begin
    VGA_R <= visible ? r : 10'd0;
    VGA_G <= visible ? g : 10'd0;
    VGA_B <= visible ? b : 10'd0;
    VGA_HS <= ~(x >= 10'd656 && x < 10'd752);
    VGA_VS <= ~(y >= 10'd490 && y < 10'd492);
    VGA_BLANK <= visible;
  end

  reg [31:0] gp = 0, gf = 0, n50 = 0, g50 = 0;
  always @(posedge pclk) begin
    gp <= pixels ^ (pixels >> 1);
    gf <= frames ^ (frames >> 1);
  end
  always @(posedge CLOCK_50) begin
    n50 <= n50 + 1'b1;
    g50 <= n50 ^ (n50 >> 1);
  end

  wire tck, tdi, shift, usr1;
  reg [127:0] dr = 0;
  CY2_JTAG jtag (
    .TDOUSER(dr[0]),
    .TCKUTAP(tck),
    .TDIUTAP(tdi),
    .SHIFTUSER(shift),
    .USR1USER(usr1)
  );
  always @(posedge tck)
    if (shift & usr1) dr <= {tdi, dr[127:1]};
    else dr <= {"VGA!", gf, gp, g50};
endmodule
