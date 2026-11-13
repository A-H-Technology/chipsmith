// A register block on the DE25-Nano's lightweight HPS-to-FPGA bridge, which
// Linux sees at 0x20000000 (nixos-fpga's /dev/uio "lwhps2fpga"):
//
//   0x00  read: 0xC5B1D6E0, what this core is
//   0x04  read/write: scratch
//   0x08  read: a counter at CLOCK1_50 (50 MHz)
//   0x0C  read/write: bit 0 drives f2h_irq0[0], an interrupt software raises
//         and clears
//
// The bridge's ports are the HPS's own AXI4 (32-bit data, 29-bit addresses,
// 4-bit IDs), with no adapter between: any beat of a burst reads the register
// its address picks, and a write burst writes its first beat.
module lw (
    input  wire        CLOCK1_50,
    output wire        lw_clk,
    output wire        irq,

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

    reg [31:0] count = 32'd0;
    reg [31:0] scratch = 32'd0;
    reg        irq_r = 1'b0;
    assign irq = irq_r;
    always @(posedge clk) count <= count + 32'd1;

    // Writes: an address, then its beats up to wlast; the first one lands.
    reg       aw_busy = 1'b0, w_first = 1'b0, b_pend = 1'b0;
    reg [1:0] w_reg = 2'd0;
    assign awready = !aw_busy && !b_pend;
    assign wready  = aw_busy;
    assign bvalid  = b_pend;
    always @(posedge clk) begin
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

    // Reads: one beat per cycle the HPS takes, arlen + 1 of them.
    reg       r_busy = 1'b0;
    reg [7:0] r_left = 8'd0;
    reg [1:0] r_reg = 2'd0;
    assign arready = !r_busy;
    assign rvalid  = r_busy;
    assign rlast   = r_busy && r_left == 8'd0;
    always @(*)
        case (r_reg)
            2'd0: rdata = 32'hC5B1_D6E0;
            2'd1: rdata = scratch;
            2'd2: rdata = count;
            default: rdata = {31'd0, irq_r};
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
