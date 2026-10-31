// Every register next to a pin goes into that pin's I/O cell with its
// controls: KEY[0] and KEY[3] (held: active low) are the cells'
// asynchronous resets, `tick` (about 3 Hz) and KEY[1] their clock enables.
// KEY[0] resets the core registers too, written as Terasic's DE2 code
// writes it: `always @(posedge clk or negedge rst_n)`.
module top (
    input        CLOCK_50,
    input  [3:0] KEY,
    input  [2:0] SW,
    input  [3:0] SW_IN,
    output [9:0] LEDR,
    output [4:0] LEDG,
    inout        GPIO_IO
);
    // An active-low reset: the LAB inverts its clear line, as it does for
    // Quartus (`.aclr(!KEY[0])`), so the 24-bit divider's carry chain shares
    // one clear across its two LABs.
    wire rst_n = KEY[0];
    wire rst_in_n = KEY[3];
    reg [23:0] div;
    always @(posedge CLOCK_50 or negedge rst_n)
        if (!rst_n) div <= 0;
        else div <= div + 1'b1;
    wire tick = div == 0;

    // LEDR[3:0]: a counter, cleared by KEY[0] and stepped on `tick`.
    reg [3:0] cnt;
    always @(posedge CLOCK_50 or negedge rst_n)
        if (!rst_n) cnt <= 0;
        else if (tick) cnt <= cnt + 1'b1;
    // LEDR[7:4]: a shift register of SW[0], all lit by KEY[0] (a preset).
    reg [3:0] sh;
    always @(posedge CLOCK_50 or negedge rst_n)
        if (!rst_n) sh <= 4'hf;
        else if (tick) sh <= {sh[2:0], SW[0]};
    // LEDR[9:8]: SW[2:1] on the falling edge.
    reg [1:0] ne;
    always @(negedge CLOCK_50) ne <= SW[2:1];
    assign LEDR = {ne, sh, cnt};

    // SW_IN sampled while KEY[1] is held, cleared by KEY[3] (active low:
    // the cells invert it).
    reg [3:0] swq;
    always @(posedge CLOCK_50 or negedge rst_in_n)
        if (!rst_in_n) swq <= 0;
        else if (!KEY[1]) swq <= SW_IN;

    // GPIO_IO toggles on `tick` while KEY[2] is held; its input register
    // reads it back. All three registers clear with KEY[0].
    reg gi, gd, ge;
    always @(posedge CLOCK_50 or negedge rst_n)
        if (!rst_n) begin
            gi <= 0;
            gd <= 0;
            ge <= 0;
        end else begin
            gi <= GPIO_IO;
            if (tick) gd <= ~gd;
            ge <= ~KEY[2];
        end
    assign GPIO_IO = ge ? gd : 1'bz;
    assign LEDG = {gi, swq};
endmodule
