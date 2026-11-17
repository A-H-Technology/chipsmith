// I/O registers on the DE25-Nano, through a jumper between GPIO0's D0 and
// D1 (header pins 1 and 2), read from Linux over the lightweight bridge
// (/dev/uio0 at 0x20000000; busybox devmem):
//
//   0x00  read: 0x10E61057, what this core is
//   0x04  read: checks where D1 read back what D0 drove
//   0x08  read: checks where it didn't
//   0x0C  read: checks made while D0 was released (not compared)
//
// D0 is driven by its output register and enabled by its output-enable
// register, D1 read by its input register, all three in the pins' I/O
// blocks (`registers` in chipsmith.toml). The data changes every 16 cycles
// and D0 is released for 64 cycles in every 256; while driven, each value
// is checked 12 cycles after it's set, through both registers and the wire.
module ioreg (
    input  wire        CLOCK1_50,
    output wire        lw_clk,
    output wire        gpo,
    input  wire        gpi,

    input  wire [3:0]  awid,
    input  wire [28:0] awaddr,
    input  wire [7:0]  awlen,
    input  wire [2:0]  awsize,
    input  wire [1:0]  awburst,
    input  wire        awlock,
    input  wire [3:0]  awcache,
    input  wire [2:0]  awprot,
    input  wire        awvalid,
    output wire        awready,
    input  wire [31:0] wdata,
    input  wire [3:0]  wstrb,
    input  wire        wlast,
    input  wire        wvalid,
    output wire        wready,
    output reg  [3:0]  bid,
    output wire [1:0]  bresp,
    output wire        bvalid,
    input  wire        bready,

    input  wire [3:0]  arid,
    input  wire [28:0] araddr,
    input  wire [7:0]  arlen,
    input  wire [2:0]  arsize,
    input  wire [1:0]  arburst,
    input  wire        arlock,
    input  wire [3:0]  arcache,
    input  wire [2:0]  arprot,
    input  wire        arvalid,
    output wire        arready,
    output reg  [3:0]  rid,
    output reg  [31:0] rdata,
    output wire [1:0]  rresp,
    output wire        rlast,
    output wire        rvalid,
    input  wire        rready
);
    wire clk = CLOCK1_50;
    assign lw_clk = clk;
    assign bresp = 2'b00;
    assign rresp = 2'b00;

    reg [15:0] n = 16'd0;
    always @(posedge clk) n <= n + 16'd1;

    wire data, driven;

    // The I/O registers: output data and enable for D0, input for D1.
    reg o_r = 1'b0, oe_r = 1'b0, i_r = 1'b0;
    always @(posedge clk) begin
        o_r  <= data;                 // a new value every 16 cycles
        oe_r <= driven;               // released 64 cycles in 256
        i_r  <= gpi;
    end
    assign gpo = oe_r ? o_r : 1'bz;

    // The checker works from the counter: an I/O block's registers drive
    // only their pins. With n[3:0] at 12, n - 1 and n - 2 (when D0's
    // registers took what D1's input register now holds) agree with n on
    // every bit used here.
    assign data = n[4] ^ n[7] ^ n[9];
    assign driven = n[7:6] != 2'b11;
    reg [31:0] good = 0, bad = 0, released = 0;
    always @(posedge clk)
        if (n[3:0] == 4'd12 && n > 16'd1024) begin
            if (!driven) released <= released + 1'b1;
            else if (i_r == data) good <= good + 1'b1;
            else bad <= bad + 1'b1;
        end

    // The bridge: writes ignored, single reads of four registers.
    reg       aw_busy = 1'b0, b_pend = 1'b0;
    assign awready = !aw_busy && !b_pend;
    assign wready  = aw_busy;
    assign bvalid  = b_pend;
    always @(posedge clk) begin
        if (awvalid && awready) begin
            aw_busy <= 1'b1;
            bid     <= awid;
        end
        if (wvalid && wready && wlast) begin
            aw_busy <= 1'b0;
            b_pend  <= 1'b1;
        end
        if (b_pend && bready) b_pend <= 1'b0;
    end
    reg       r_busy = 1'b0;
    reg [7:0] r_left = 8'd0;
    reg [1:0] r_reg = 2'd0;
    assign arready = !r_busy;
    assign rvalid  = r_busy;
    assign rlast   = r_busy && r_left == 8'd0;
    always @(*)
        case (r_reg)
            2'd0: rdata = 32'h10E6_1057;
            2'd1: rdata = good;
            2'd2: rdata = bad;
            default: rdata = released;
        endcase
    always @(posedge clk) begin
        if (arvalid && arready) begin
            r_busy <= 1'b1;
            rid    <= arid;
            r_left <= arlen;
            r_reg  <= araddr[3:2];
        end else if (rvalid && rready) begin
            if (rlast) r_busy <= 1'b0;
            else begin
                r_left <= r_left - 8'd1;
                r_reg  <= r_reg + 2'd1;
            end
        end
    end
endmodule
