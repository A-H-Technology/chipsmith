// Every register next to a pin goes into that pin's I/O cell with its
// controls: RST (SW[17] up) and KEY[3] (held: active low) are the cells'
// asynchronous resets, `tick` (about 3 Hz) and KEY[1] their clock enables.
module top (
    input        CLOCK_50,
    input        RST,
    input  [3:0] KEY,
    input  [2:0] SW,
    input  [3:0] SW_IN,
    output [9:0] LEDR,
    output [4:0] LEDG,
    inout        GPIO_IO
);
    reg [23:0] div = 0;
    always @(posedge CLOCK_50) div <= div + 1'b1;
    wire tick = div == 0;
    // An active-high reset straight from a pin: an LE register's clear is
    // active high, and an inverter in front would be one per register in
    // chipsmith (an inverted LAB clear isn't modelled) and folded into the
    // clears by quartus_map.
    wire rst = RST;
    wire rst_in_n = KEY[3];

    // LEDR[3:0]: a counter, cleared by RST and stepped on `tick`.
    reg [3:0] cnt;
    always @(posedge CLOCK_50 or posedge rst)
        if (rst) cnt <= 0;
        else if (tick) cnt <= cnt + 1'b1;
    // LEDR[7:4]: a shift register of SW[0], all lit by RST (a preset).
    reg [3:0] sh;
    always @(posedge CLOCK_50 or posedge rst)
        if (rst) sh <= 4'hf;
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
    // reads it back. All three registers clear with RST.
    reg gi, gd, ge;
    always @(posedge CLOCK_50 or posedge rst)
        if (rst) begin
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
