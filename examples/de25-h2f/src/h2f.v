// A burst-capable memory on the DE25-Nano's HPS-to-FPGA bridge, which Linux
// sees at 0x40000000 (/dev/mem):
//
//   0x0000-0x01FF  read/write: 32 words of 128 bits, byte strobes, in MLABs
//                  (a burst past them wraps)
//   0x1000         read: 0x48324600 ("H2F"), what this core is
//   0x1010         read: write bursts of more than one beat
//   0x1020         read: read bursts of more than one beat
//   0x1030         read: the longest write (bits 15:8) and read (7:0) burst,
//                  beats - 1
//   0x1040         write: clears 0x1010 to 0x1030
//
// The bridge's ports are the HPS's own AXI4 (128-bit data, 32 address bits
// routed), connected straight to the atom. Bursts are INCR (a FIXED one
// stays on its first word); reads take two cycles a beat.
module h2f (
    input  wire        CLOCK1_50,
    output wire        lw_clk,

    input  wire [3:0]  awid,
    input  wire [31:0] awaddr,
    input  wire [7:0]  awlen,
    input  wire [2:0]  awsize,
    input  wire [1:0]  awburst,
    input  wire        awlock,
    input  wire [3:0]  awcache,
    input  wire [2:0]  awprot,
    input  wire        awvalid,
    output wire        awready,
    input  wire [127:0] wdata,
    input  wire [15:0] wstrb,
    input  wire        wlast,
    input  wire        wvalid,
    output wire        wready,
    output reg  [3:0]  bid,
    output wire [1:0]  bresp,
    output wire        bvalid,
    input  wire        bready,

    input  wire [3:0]  arid,
    input  wire [31:0] araddr,
    input  wire [7:0]  arlen,
    input  wire [2:0]  arsize,
    input  wire [1:0]  arburst,
    input  wire        arlock,
    input  wire [3:0]  arcache,
    input  wire [2:0]  arprot,
    input  wire        arvalid,
    output wire        arready,
    output reg  [3:0]  rid,
    output reg  [127:0] rdata,
    output wire [1:0]  rresp,
    output wire        rlast,
    output wire        rvalid,
    input  wire        rready
);
    wire clk = CLOCK1_50;
    assign lw_clk = clk;
    assign bresp = 2'b00;
    assign rresp = 2'b00;

    reg [31:0] w_bursts = 0, r_bursts = 0;
    reg [7:0]  w_max = 0, r_max = 0;
    reg        clear = 1'b0;   // a write to 0x1040, for the read side too

    // Writes: the address, then its beats up to wlast, each to the next word.
    reg       aw_busy = 1'b0, b_pend = 1'b0, w_mem = 1'b0, w_incr = 1'b0;
    reg [4:0] waddr = 5'd0;
    reg [3:0] w_reg = 4'd0;
    assign awready = !aw_busy && !b_pend;
    assign wready  = aw_busy;
    assign bvalid  = b_pend;
    wire w_beat = wvalid && wready;

    // Reads: arlen + 1 beats, each read the cycle before it's offered.
    reg       r_busy = 1'b0, r_have = 1'b0, r_mem = 1'b0, r_incr = 1'b0;
    reg [7:0] r_left = 8'd0;
    reg [4:0] raddr = 5'd0;
    wire [127:0] q;

    // The memory: a 32 x 8 RAM per byte lane, so a strobe writes its own
    // bytes.
    genvar i;
    generate for (i = 0; i < 16; i = i + 1) begin : lane
        reg [7:0] mem [0:31];
        reg [7:0] out = 8'd0;
        always @(posedge clk) begin
            if (w_beat && w_mem && wstrb[i]) mem[waddr] <= wdata[8 * i +: 8];
            out <= mem[raddr];
        end
        assign q[8 * i +: 8] = out;
    end endgenerate

    always @(posedge clk) begin
        if (awvalid && awready) begin
            aw_busy <= 1'b1;
            bid     <= awid;
            waddr   <= awaddr[8:4];
            w_mem   <= !awaddr[12];
            w_incr  <= awburst == 2'b01;
            w_reg   <= awaddr[7:4];
            if (awlen != 8'd0) w_bursts <= w_bursts + 1'b1;
            if (awlen > w_max) w_max <= awlen;
        end
        clear <= w_beat && !w_mem && w_reg == 4'd4 && wstrb != 16'd0;
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

    assign arready = !r_busy;
    assign rvalid  = r_busy && r_have;
    assign rlast   = r_left == 8'd0;
    always @(*)
        if (r_mem) rdata = q;
        else
            case (raddr[3:0])
                4'd0: rdata = 128'h4832_4600;
                4'd1: rdata = w_bursts;
                4'd2: rdata = r_bursts;
                4'd3: rdata = {112'd0, w_max, r_max};
                default: rdata = 128'd0;
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
            raddr  <= araddr[8:4];
            r_mem  <= !araddr[12];
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
