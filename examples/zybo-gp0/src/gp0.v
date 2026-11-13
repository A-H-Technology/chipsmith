// A register block on the PS7's M_AXI_GP0, which Linux sees at 0x40000000
// (nixos-fpga's /dev/uio "gp0"), as examples/de25-lw is on the DE25-Nano's
// lightweight bridge:
//
//   0x00  read: 0xC5B1D6E1, what this core is
//   0x04  read/write: scratch
//   0x08  read: a counter at 100 MHz
//   0x0C  read/write: bit 0 drives IRQF2P[0], an interrupt software raises
//         and clears
//
// GP0 is AXI3 (32-bit data, 12-bit IDs, 4-bit lengths), clocked from the
// fabric: 100 MHz from the 125 MHz pin, below where Vivado's timer puts the
// PS7's GP0 outputs. Any beat of a read burst reads the register its
// address picks; a write burst writes its first beat.
module gp0 (input wire clk);
    wire aclk, locked;
    XC7_PLL #(
        .INCLK0_INPUT_FREQUENCY(8000),
        .CLK0_MULTIPLY_BY(4), .CLK0_DIVIDE_BY(5)
    ) pll (.INCLK(clk), .ARESET(1'b0), .CLK0(aclk), .LOCKED(locked));

    wire [31:0] araddr, awaddr, wdata;
    wire [11:0] arid, awid;
    wire [3:0]  arlen, wstrb;
    wire        arvalid, awvalid, wvalid, wlast, bready, rready;
    reg  [11:0] bid = 12'd0, rid = 12'd0;
    reg  [31:0] rdata;
    wire        awready, wready, bvalid, arready, rvalid, rlast;
    wire        irq;

    PS7 ps7 (
        .MAXIGP0ACLK(aclk),
        .MAXIGP0ARADDR(araddr), .MAXIGP0ARID(arid), .MAXIGP0ARLEN(arlen),
        .MAXIGP0ARVALID(arvalid), .MAXIGP0ARREADY(arready),
        .MAXIGP0RDATA(rdata), .MAXIGP0RID(rid), .MAXIGP0RLAST(rlast),
        .MAXIGP0RRESP(2'b00), .MAXIGP0RVALID(rvalid), .MAXIGP0RREADY(rready),
        .MAXIGP0AWADDR(awaddr), .MAXIGP0AWID(awid), .MAXIGP0AWVALID(awvalid),
        .MAXIGP0AWREADY(awready),
        .MAXIGP0WDATA(wdata), .MAXIGP0WSTRB(wstrb), .MAXIGP0WLAST(wlast),
        .MAXIGP0WVALID(wvalid), .MAXIGP0WREADY(wready),
        .MAXIGP0BID(bid), .MAXIGP0BRESP(2'b00), .MAXIGP0BVALID(bvalid),
        .MAXIGP0BREADY(bready),
        // [19:16] are the cores' FIQ and IRQ: held at 0.
        .IRQF2P({19'd0, irq})
    );

    reg [31:0] count = 32'd0;
    reg [31:0] scratch = 32'd0;
    reg        irq_r = 1'b0;
    assign irq = irq_r;
    always @(posedge aclk) count <= count + 32'd1;

    // Writes: an address, then its beats up to wlast; the first one lands.
    reg       aw_busy = 1'b0, w_first = 1'b0, b_pend = 1'b0;
    reg [1:0] w_reg = 2'd0;
    assign awready = locked && !aw_busy && !b_pend;
    assign wready  = aw_busy;
    assign bvalid  = b_pend;
    always @(posedge aclk) begin
        if (awvalid && awready) begin
            aw_busy <= 1'b1;
            w_first <= 1'b1;
            bid     <= awid;
            w_reg   <= awaddr[3:2];
        end
        if (wvalid && wready) begin
            w_first <= 1'b0;
            if (w_first && wstrb != 4'd0)
                case (w_reg)
                    2'd1: scratch <= wdata;
                    2'd3: irq_r <= wdata[0];
                    default: ;
                endcase
            if (wlast) begin
                aw_busy <= 1'b0;
                b_pend  <= 1'b1;
            end
        end
        if (b_pend && bready) b_pend <= 1'b0;
    end

    // Reads: one beat per cycle the PS7 takes, arlen + 1 of them.
    reg       r_busy = 1'b0;
    reg [3:0] r_left = 4'd0;
    reg [1:0] r_reg = 2'd0;
    assign arready = locked && !r_busy;
    assign rvalid  = r_busy;
    assign rlast   = r_busy && r_left == 4'd0;
    always @(*)
        case (r_reg)
            2'd0: rdata = 32'hC5B1_D6E1;
            2'd1: rdata = scratch;
            2'd2: rdata = count;
            default: rdata = {31'd0, irq_r};
        endcase
    always @(posedge aclk) begin
        if (arvalid && arready) begin
            r_busy <= 1'b1;
            rid    <= arid;
            r_left <= arlen;
            r_reg  <= araddr[3:2];
        end else if (rvalid && rready) begin
            if (rlast) r_busy <= 1'b0;
            else begin
                r_left <= r_left - 4'd1;
                r_reg  <= r_reg + 2'd1;
            end
        end
    end
endmodule
