// The DE2's pin settings from chipsmith.toml at work: I/O-cell registers
// (one powering up high), weak pull-ups and bus hold, drive strengths, a
// 3.3-V LVCMOS display and a 2.5-V input. It drives only LEDs, the HEX0
// digit and, while KEY[0] is held, expansion-header pin GPIO_0[4].
module top(
    input CLOCK_50,
    input [3:0] SW,        // SW[3:0]: the HEX0 digit
    input [4:0] SW_HI,     // SW[17:13]; SW[12:0] are dedicated clock inputs, which have no I/O registers
    input [1:0] KEY,
    input [1:0] GPIO_PU,   // GPIO_0[1:0], weak pull-ups: 1 with nothing plugged in
    input [1:0] GPIO_BH,   // GPIO_0[3:2], bus hold: keeps the last level driven
    inout GPIO_IO,         // GPIO_0[4], driven through its I/O registers
    output [4:0] LEDR,     // LEDR[4:0], SW[17:13] two clocks late
    output [5:0] LEDR_IN,  // LEDR[13:8], the inputs above
    output [4:0] LEDG,     // LEDG[4:0], dimmed: 4 mA
    output [6:0] HEX0
);
  reg [25:0] c = 0;
  always @(posedge CLOCK_50) c <= c + 1'b1;

  // Switches through their input registers, onto the LEDs through output
  // registers.
  reg [4:0] sw_q, led_q;
  always @(posedge CLOCK_50) begin
    sw_q <= SW_HI;
    led_q <= sw_q;
  end
  assign LEDR = led_q;

  // The counter's top bits; LEDG[4] powers up lit.
  reg [3:0] slow;
  reg lit = 1'b1;
  always @(posedge CLOCK_50) begin
    slow <= c[25:22];
    lit <= ~c[24];
  end
  assign LEDG = {lit, slow};

  // GPIO_0[4]: drives a 3 Hz square wave while KEY[0] is pressed, else let
  // go and pulled up; read back through its input register.
  reg oe_q = 0, d_q = 0, io_q = 0;
  always @(posedge CLOCK_50) begin
    oe_q <= ~KEY[0];
    d_q <= c[23];
    io_q <= GPIO_IO;
  end
  assign GPIO_IO = oe_q ? d_q : 1'bz;
  assign LEDR_IN = {~KEY[1], io_q, GPIO_BH, GPIO_PU};

  // SW[3:0] as a hex digit (segments active low).
  reg [6:0] seg;
  always @*
    case (SW[3:0])
      4'h0: seg = 7'b1000000; 4'h1: seg = 7'b1111001; 4'h2: seg = 7'b0100100;
      4'h3: seg = 7'b0110000; 4'h4: seg = 7'b0011001; 4'h5: seg = 7'b0010010;
      4'h6: seg = 7'b0000010; 4'h7: seg = 7'b1111000; 4'h8: seg = 7'b0000000;
      4'h9: seg = 7'b0010000; 4'ha: seg = 7'b0001000; 4'hb: seg = 7'b0000011;
      4'hc: seg = 7'b1000110; 4'hd: seg = 7'b0100001; 4'he: seg = 7'b0000110;
      default: seg = 7'b0001110;
    endcase
  assign HEX0 = seg;
endmodule
