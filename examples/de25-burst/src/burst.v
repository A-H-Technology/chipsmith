// A burst-capable memory on the DE25-Nano's lightweight HPS-to-FPGA bridge,
// which Linux sees at 0x20000000 (nixos-fpga's /dev/uio "lwhps2fpga"):
//
//   0x000-0x07F  read/write: 32 words, byte strobes, in MLABs (a burst
//                past them wraps)
//   0x400        read: 0xB0857E57, what this core is
//   0x404        read: write bursts of more than one beat
//   0x408        read: read bursts of more than one beat
//   0x40C        read: the longest write (bits 15:8) and read (7:0) burst,
//                beats - 1
//   0x410        write: clears 0x404 to 0x40C
//
// The bridge's ports are the HPS's own AXI4 (32-bit data, so a 64- or
// 128-bit access from the CPU comes as a burst). Bursts are INCR (a FIXED
// one stays on its first word): each beat writes or reads the next word.
// Reads take two cycles a beat (the read is registered).
module burst (
    input  wire        CLOCK1_50,
    output wire        lw_clk,

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

    // The memory, a byte lane each so a strobe writes its own bytes.
    reg [7:0] m0 [0:31];
    reg [7:0] m1 [0:31];
    reg [7:0] m2 [0:31];
    reg [7:0] m3 [0:31];

    reg [31:0] w_bursts = 0, r_bursts = 0;
    reg [7:0]  w_max = 0, r_max = 0;
    reg        clear = 1'b0;   // a write to 0x410, for the read side too

    // Writes: the address, then its beats up to wlast, each to the next word.
    reg       aw_busy = 1'b0, b_pend = 1'b0, w_mem = 1'b0, w_incr = 1'b0;
    reg [4:0] waddr = 5'd0;
    reg [3:0] w_reg = 4'd0;
    assign awready = !aw_busy && !b_pend;
    assign wready  = aw_busy;
    assign bvalid  = b_pend;
    wire w_beat = wvalid && wready;
    always @(posedge clk) begin
        if (w_beat && w_mem) begin
            if (wstrb[0]) m0[waddr] <= wdata[7:0];
            if (wstrb[1]) m1[waddr] <= wdata[15:8];
            if (wstrb[2]) m2[waddr] <= wdata[23:16];
            if (wstrb[3]) m3[waddr] <= wdata[31:24];
        end
    end
    always @(posedge clk) begin
        if (awvalid && awready) begin
            aw_busy <= 1'b1;
            bid     <= awid;
            waddr   <= awaddr[6:2];
            w_mem   <= !awaddr[10];
            w_incr  <= awburst == 2'b01;
            w_reg   <= awaddr[5:2];
            if (awlen != 8'd0) w_bursts <= w_bursts + 1'b1;
            if (awlen > w_max) w_max <= awlen;
        end
        clear <= w_beat && !w_mem && w_reg == 4'd4 && wstrb != 4'd0;
        if (clear) begin
            w_bursts <= 0;
            w_max <= 0;
        end
        if (w_beat) begin
            if (w_incr) waddr <= waddr + 5'd1;
            if (wlast) begin
                aw_busy <= 1'b0;
                b_pend  <= 1'b1;
            end
        end
        if (b_pend && bready) b_pend <= 1'b0;
    end

    // Reads: arlen + 1 beats, each read the cycle before it's offered.
    reg       r_busy = 1'b0, r_have = 1'b0, r_mem = 1'b0, r_incr = 1'b0;
    reg [7:0] r_left = 8'd0;
    reg [4:0] raddr = 5'd0;
    reg [31:0] q = 32'd0;
    always @(posedge clk) q <= {m3[raddr], m2[raddr], m1[raddr], m0[raddr]};
    assign arready = !r_busy;
    assign rvalid  = r_busy && r_have;
    assign rlast   = r_left == 8'd0;
    always @(*)
        if (r_mem) rdata = q;
        else
            case (raddr[3:0])
                4'd0: rdata = 32'hB085_7E57;
                4'd1: rdata = w_bursts;
                4'd2: rdata = r_bursts;
                4'd3: rdata = {16'd0, w_max, r_max};
                default: rdata = 32'd0;
            endcase
    always @(posedge clk) begin
        if (clear) begin
            r_bursts <= 0;
            r_max <= 0;
        end
        if (arvalid && arready) begin
            r_busy <= 1'b1;
            r_have <= 1'b0;
            rid    <= arid;
            r_left <= arlen;
            raddr  <= araddr[6:2];
            r_mem  <= !araddr[10];
            r_incr <= arburst == 2'b01;
            if (arlen != 8'd0) r_bursts <= r_bursts + 1'b1;
            if (arlen > r_max) r_max <= arlen;
        end else if (r_busy && !r_have) begin
            r_have <= 1'b1;
        end else if (rvalid && rready) begin
            if (rlast) r_busy <= 1'b0;
            else begin
                r_left <= r_left - 8'd1;
                if (r_incr) raddr <= raddr + 5'd1;
                r_have <= 1'b0;
            end
        end
    end
endmodule
