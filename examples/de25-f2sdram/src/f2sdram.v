// The DE25-Nano's fabric writing and reading the HPS's DDR through the
// FPGA-to-SDRAM bridge, driven from Linux over the lightweight bridge
// (/dev/uio0 at 0x20000000; busybox devmem):
//
//   0x00  read: 0xF25D0000, what this core is
//   0x04  read/write: the region's physical address (32-byte aligned)
//   0x08  read/write: its length in 256-bit beats (a multiple of 16)
//   0x0C  read/write: the seed of the pattern
//   0x10  write: 1 writes the pattern, 2 reads the region back and checks it
//   0x14  read: bit 31 busy, bits 15:0 beats written
//   0x24  read: beats read
//   0x28  read: the last bad write and read response, {BUSER, BRESP} and
//         {RUSER, RRESP}
//   0x18  read: beats that read back wrong
//   0x1C  read: write responses that weren't OKAY, read responses that weren't
//   0x20  read/write: AxPROT (bits 2:0), AxCACHE (bits 7:4)
//
// The pattern's 32-bit lane k of beat i is seed + 8 i + k. Bursts are 16
// beats (512 bytes), INCR, all strobes; one write burst in flight at a time,
// and one read burst.
module f2sdram (
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
    input  wire        rready,
    output wire        f2s_clk,
    output wire [4:0]   f2s_awid,
    output reg  [31:0]  f2s_awaddr,
    output wire [3:0]   f2s_awregion,
    output wire [7:0]   f2s_awlen,
    output wire [2:0]   f2s_awsize,
    output wire [1:0]   f2s_awburst,
    output wire         f2s_awlock,
    output wire [3:0]   f2s_awcache,
    output wire [2:0]   f2s_awprot,
    output wire [3:0]   f2s_awqos,
    output wire [24:0]  f2s_awuser,
    output reg          f2s_awvalid,
    input  wire         f2s_awready,
    output wire [255:0] f2s_wdata,
    output wire [31:0]  f2s_wstrb,
    output wire         f2s_wlast,
    output wire [7:0]   f2s_wuser,
    output reg          f2s_wvalid,
    input  wire         f2s_wready,
    input  wire [4:0]   f2s_bid,
    input  wire [1:0]   f2s_bresp,
    input  wire [7:0]   f2s_buser,
    input  wire         f2s_bvalid,
    output wire         f2s_bready,
    output wire [4:0]   f2s_arid,
    output reg  [31:0]  f2s_araddr,
    output wire [3:0]   f2s_arregion,
    output wire [7:0]   f2s_arlen,
    output wire [2:0]   f2s_arsize,
    output wire [1:0]   f2s_arburst,
    output wire         f2s_arlock,
    output wire [3:0]   f2s_arcache,
    output wire [2:0]   f2s_arprot,
    output wire [3:0]   f2s_arqos,
    output wire [24:0]  f2s_aruser,
    output reg          f2s_arvalid,
    input  wire         f2s_arready,
    input  wire [4:0]   f2s_rid,
    input  wire [255:0] f2s_rdata,
    input  wire [1:0]   f2s_rresp,
    input  wire         f2s_rlast,
    input  wire [7:0]   f2s_ruser,
    input  wire         f2s_rvalid,
    output wire         f2s_rready
);
    wire clk = CLOCK1_50;
    assign lw_clk = clk;
    assign f2s_clk = clk;
    assign bresp = 2'b00;
    assign rresp = 2'b00;

    // Control registers.
    reg [31:0] base = 32'd0, len = 32'd0, seed = 32'd0;
    reg [7:0]  attr = 8'h00;
    reg [31:0] written = 0, readn = 0, wrong = 0;
    reg [15:0] bad_b = 0, bad_r = 0;
    reg [15:0] last_b = 0, last_r = 0;  // the last bad response: {user, resp}
    reg        busy = 1'b0;
    reg        go_w = 1'b0, go_r = 1'b0;   // one-cycle starts from a write

    // The F2SDRAM side's constant fields.
    assign f2s_awid = 5'd0;    assign f2s_arid = 5'd0;
    assign f2s_awregion = 4'd0; assign f2s_arregion = 4'd0;
    assign f2s_awlen = 8'd15;   assign f2s_arlen = 8'd15;
    assign f2s_awsize = 3'd5;   assign f2s_arsize = 3'd5;     // 32 bytes
    assign f2s_awburst = 2'b01; assign f2s_arburst = 2'b01;   // INCR
    assign f2s_awlock = 1'b0;   assign f2s_arlock = 1'b0;
    assign f2s_awcache = attr[7:4]; assign f2s_arcache = attr[7:4];
    assign f2s_awprot = attr[2:0];  assign f2s_arprot = attr[2:0];
    assign f2s_awqos = 4'd0;    assign f2s_arqos = 4'd0;
    assign f2s_awuser = 25'd0;  assign f2s_aruser = 25'd0;
    assign f2s_wuser = 8'd0;
    assign f2s_wstrb = 32'hFFFF_FFFF;
    assign f2s_bready = 1'b1;
    assign f2s_rready = 1'b1;

    // The pattern of beat `i`.
    function [255:0] beat(input [31:0] s, input [31:0] i);
        integer k;
        for (k = 0; k < 8; k = k + 1) beat[32 * k +: 32] = s + 8 * i + k;
    endfunction

    // Writes: AW, 16 W beats, then wait for B; until `len` beats are done.
    reg [1:0]  wst = 2'd0;       // 0 idle, 1 address, 2 data, 3 response
    reg [31:0] wbeat = 0;        // beat index of the next W
    reg [3:0]  wk = 4'd0;        // beat within the burst
    assign f2s_wdata = beat(seed, wbeat);
    assign f2s_wlast = wk == 4'd15;
    // Reads: AR, then 16 R beats checked as they come.
    reg [1:0]  rst = 2'd0;       // 0 idle, 1 address, 2 data
    reg [31:0] rbeat = 0;
    always @(posedge clk) begin
        case (wst)
            2'd0: if (go_w) begin
                wst <= 2'd1; wbeat <= 0; written <= 0; bad_b <= 0;
                f2s_awaddr <= base; f2s_awvalid <= 1'b1;
            end
            2'd1: if (f2s_awready) begin
                f2s_awvalid <= 1'b0; f2s_wvalid <= 1'b1; wk <= 4'd0; wst <= 2'd2;
            end
            2'd2: if (f2s_wready) begin
                wbeat <= wbeat + 1; written <= written + 1; wk <= wk + 4'd1;
                if (wk == 4'd15) begin f2s_wvalid <= 1'b0; wst <= 2'd3; end
            end
            2'd3: if (f2s_bvalid) begin
                if (f2s_bresp != 2'b00) begin
                    bad_b <= bad_b + 1'b1;
                    last_b <= {6'd0, f2s_buser, f2s_bresp};
                end
                if (wbeat >= len) wst <= 2'd0;
                else begin
                    f2s_awaddr <= f2s_awaddr + 32'd512; f2s_awvalid <= 1'b1; wst <= 2'd1;
                end
            end
        endcase
        case (rst)
            2'd0: if (go_r) begin
                rst <= 2'd1; rbeat <= 0; readn <= 0; wrong <= 0; bad_r <= 0;
                f2s_araddr <= base; f2s_arvalid <= 1'b1;
            end
            2'd1: if (f2s_arready) begin f2s_arvalid <= 1'b0; rst <= 2'd2; end
            2'd2: if (f2s_rvalid) begin
                rbeat <= rbeat + 1; readn <= readn + 1;
                if (f2s_rdata != beat(seed, rbeat)) wrong <= wrong + 1;
                if (f2s_rresp != 2'b00) begin
                    bad_r <= bad_r + 1'b1;
                    last_r <= {6'd0, f2s_ruser, f2s_rresp};
                end
                if (f2s_rlast) begin
                    if (rbeat + 1 >= len) rst <= 2'd0;
                    else begin
                        f2s_araddr <= f2s_araddr + 32'd512; f2s_arvalid <= 1'b1; rst <= 2'd1;
                    end
                end
            end
            default: rst <= 2'd0;
        endcase
        busy <= wst != 2'd0 || rst != 2'd0 || go_w || go_r;
    end
    initial begin f2s_awvalid = 1'b0; f2s_wvalid = 1'b0; f2s_arvalid = 1'b0; end

    // The lightweight bridge: single-beat register reads and writes.
    reg       aw_busy = 1'b0, b_pend = 1'b0;
    reg [3:0] w_reg = 4'd0;
    assign awready = !aw_busy && !b_pend;
    assign wready  = aw_busy;
    assign bvalid  = b_pend;
    always @(posedge clk) begin
        go_w <= 1'b0;
        go_r <= 1'b0;
        if (awvalid && awready) begin
            aw_busy <= 1'b1;
            bid     <= awid;
            w_reg   <= awaddr[5:2];
        end
        if (wvalid && wready) begin
            case (w_reg)
                4'd1: base <= wdata;
                4'd2: len <= wdata;
                4'd3: seed <= wdata;
                4'd4: begin go_w <= wdata == 32'd1 && !busy; go_r <= wdata == 32'd2 && !busy; end
                4'd8: attr <= wdata[7:0];
                default: ;
            endcase
            if (wlast) begin
                aw_busy <= 1'b0;
                b_pend  <= 1'b1;
            end
        end
        if (b_pend && bready) b_pend <= 1'b0;
    end
    reg       r_busy = 1'b0;
    reg [7:0] r_left = 8'd0;
    reg [3:0] r_reg = 4'd0;
    assign arready = !r_busy;
    assign rvalid  = r_busy;
    assign rlast   = r_busy && r_left == 8'd0;
    always @(*)
        case (r_reg)
            4'd0: rdata = 32'hF25D_0000;
            4'd1: rdata = base;
            4'd2: rdata = len;
            4'd3: rdata = seed;
            4'd5: rdata = {busy, 15'd0, written[15:0]} ;
            4'd6: rdata = wrong;
            4'd7: rdata = {bad_b, bad_r};
            4'd8: rdata = {24'd0, attr};
            4'd9: rdata = readn;
            4'd10: rdata = {last_b, last_r};
            default: rdata = 32'd0;
        endcase
    always @(posedge clk) begin
        if (arvalid && arready) begin
            r_busy <= 1'b1;
            rid    <= arid;
            r_left <= arlen;
            r_reg  <= araddr[5:2];
        end else if (rvalid && rready) begin
            if (rlast) r_busy <= 1'b0;
            else begin
                r_left <= r_left - 8'd1;
                r_reg  <= r_reg + 4'd1;
            end
        end
    end
endmodule
