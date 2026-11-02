// Behavioural models of the 7-series hard blocks chipsmith's netlists hold
// (RAMB18E1, RAMB36E1), for co-simulating a synthesized or decoded netlist
// against its RTL with iverilog. Written from UG473 and checked against
// Vivado's own unisim models on random configurations and stimulus
// (crates/chipsmith-xc7/tests/models.rs). Not synthesizable: both ports of
// a block are handled together once per time step, after every clock edge
// of that step, so a read can tell whether the other port wrote its word on
// the same edge; writes are nonblocking, so every read of the step sees the
// memory as it was before.
//
// What the model leaves x, the hardware doesn't define (UG473, "Conflict
// Avoidance"): a read of a word the other port writes on the same edge,
// unless the writing port is READ_FIRST; a bit both ports write on one
// edge, and any read of it on that edge.

// A block's core: ports A and B, each up to 64 data and 8 parity bits wide
// ({parity, data} as one 72-bit word). In SDP mode the wrappers hand the
// whole write to port B and take the whole read from port A.
module CS_RAMB #(
    parameter integer ABITS = 14,
    parameter integer DATA_BITS = 16384,
    parameter integer PARITY_BITS = 2048,
    // 0 for no read or no write on that port.
    parameter integer RW_A = 0,
    parameter integer RW_B = 0,
    parameter integer WW_A = 0,
    parameter integer WW_B = 0,
    parameter WRITE_MODE_A = "WRITE_FIRST",
    parameter WRITE_MODE_B = "WRITE_FIRST",
    parameter integer REG_A = 0,
    parameter integer REG_B = 0,
    parameter RSTREG_PRIORITY_A = "RSTREG",
    parameter RSTREG_PRIORITY_B = "RSTREG",
    // As the primitive has them: a word of the port's read width,
    // {parity, data} from bit 0 (SDP's wide read: both halves', B's above).
    parameter [71:0] INIT_A = 0,
    parameter [71:0] INIT_B = 0,
    parameter [71:0] SRVAL_A = 0,
    parameter [71:0] SRVAL_B = 0,
    parameter [32767:0] CONTENTS = 0,
    parameter [4095:0] CONTENTS_P = 0
) (
    input CLKA, CLKB, ENA, ENB, REGCEA, REGCEB, RSTRAMA, RSTRAMB, RSTREGA, RSTREGB,
    input [15:0] ADDRA, ADDRB,
    input [71:0] DIA, DIB,
    input [7:0] WEA, WEB,
    output [71:0] DOA, DOB
);
    reg mem [0:DATA_BITS-1];
    reg memp [0:PARITY_BITS-1];
    integer i;
    // Copied first: iverilog takes seconds per bit selected from a
    // parameter this wide.
    reg [32767:0] contents;
    reg [4095:0] contents_p;
    initial begin
        contents = CONTENTS;
        contents_p = CONTENTS_P;
        for (i = 0; i < DATA_BITS; i = i + 1) mem[i] = contents[i];
        for (i = 0; i < PARITY_BITS; i = i + 1) memp[i] = contents_p[i];
    end

    function integer dbits(input integer w);
        dbits = w >= 9 ? w / 9 * 8 : w;
    endfunction
    function integer pbits(input integer w);
        pbits = w >= 9 ? w / 9 : 0;
    endfunction
    function integer lg(input integer n);
        integer k;
        begin
            lg = 0;
            for (k = 1; k < n; k = k * 2) lg = lg + 1;
        end
    endfunction

    // A read width's word as {parity, data} at bits 64 and 0.
    function [71:0] word(input [71:0] v, input integer w);
        integer k;
        begin
            word = 0;
            for (k = 0; k < dbits(w); k = k + 1) word[k] = v[k];
            for (k = 0; k < pbits(w); k = k + 1) word[64 + k] = v[dbits(w) + k];
        end
    endfunction
    localparam [71:0] IA = word(INIT_A, RW_A), IB = word(INIT_B, RW_B);
    localparam [71:0] SA = word(SRVAL_A, RW_A), SB = word(SRVAL_B, RW_B);

    // The read data latches and output registers.
    reg [71:0] latch_a = IA, latch_b = IB, reg_a = IA, reg_b = IB;
    assign DOA = REG_A ? reg_a : latch_a;
    assign DOB = REG_B ? reg_b : latch_b;

    reg edge_a = 0, edge_b = 0;
    always @(posedge CLKA) edge_a = 1;
    always @(posedge CLKB) edge_b = 1;

    // What each port's write covers this edge: its word's first data and
    // parity bits (with its width and lane enables).
    integer ws_a = 0, wps_a = 0, ws_b = 0, wps_b = 0;

    task write(input integer w, input [15:0] addr, input [71:0] di, input [7:0] we,
               output integer ws, output integer wps);
        integer word, k;
        begin
            word = addr[ABITS-1:0] >> lg(dbits(w));
            ws = word * dbits(w);
            wps = word * pbits(w);
            for (k = 0; k < dbits(w); k = k + 1)
                if (we[w >= 9 ? k / 8 : 0]) mem[ws + k] <= di[k];
            for (k = 0; k < pbits(w); k = k + 1)
                if (we[k]) memp[wps + k] <= di[64 + k];
        end
    endtask

    // Whether a write of width w from ws/wps with lanes we covers data
    // (parity, `p`) bit b.
    function covers(input integer w, input integer ws, input integer wps, input [7:0] we,
                    input p, input integer b);
        begin
            if (p) covers = w >= 9 && b >= wps && b < wps + pbits(w) && we[b - wps];
            else covers = b >= ws && b < ws + dbits(w) && we[w >= 9 ? (b - ws) / 8 : 0];
        end
    endfunction

    // The word of read width w at addr, {parity, data}: as stored before
    // the edge, with this port's own write (width ow, lanes owe, data di)
    // in it if `own` (WRITE_FIRST), x where the other port's write (width
    // xw, lanes xwe) clashes if `clash`, and x where both write (`both`).
    function [71:0] read(input integer w, input [15:0] addr,
                         input own, input integer ow, input integer ows, input integer owps,
                         input [7:0] owe, input [71:0] di,
                         input clash, input integer xw, input integer xws, input integer xwps,
                         input [7:0] xwe, input both);
        integer word, k, b;
        begin
            word = addr[ABITS-1:0] >> lg(dbits(w));
            read = 0;
            for (k = 0; k < dbits(w); k = k + 1) begin
                b = word * dbits(w) + k;
                read[k] = mem[b];
                if (own && covers(ow, ows, owps, owe, 0, b)) read[k] = di[b - ows];
                if (clash && covers(xw, xws, xwps, xwe, 0, b)) read[k] = 1'bx;
                if (both && covers(ow, ows, owps, owe, 0, b) && covers(xw, xws, xwps, xwe, 0, b)) read[k] = 1'bx;
            end
            for (k = 0; k < pbits(w); k = k + 1) begin
                b = word * pbits(w) + k;
                read[64 + k] = memp[b];
                if (own && covers(ow, ows, owps, owe, 1, b)) read[64 + k] = di[64 + b - owps];
                if (clash && covers(xw, xws, xwps, xwe, 1, b)) read[64 + k] = 1'bx;
                if (both && covers(ow, ows, owps, owe, 1, b) && covers(xw, xws, xwps, xwe, 1, b)) read[64 + k] = 1'bx;
            end
        end
    endfunction

    function [71:0] reg_next(input [71:0] q, input [71:0] d, input [71:0] srval, input rst,
                             input ce, input rst_first);
        begin
            reg_next = q;
            if (rst && (rst_first || ce)) reg_next = srval;
            else if (ce) reg_next = d;
        end
    endfunction

    reg wrote_a, wrote_b, hold_a, hold_b;

    // Whether any byte enable of a write of width w is on: the first only
    // below x9 (and on a port with no write, where NO_CHANGE still reads
    // it).
    function lanes_on(input integer w, input [7:0] we);
        integer k;
        begin
            lanes_on = we[0];
            for (k = 1; k < pbits(w); k = k + 1) lanes_on = lanes_on | we[k];
        end
    endfunction

    reg [71:0] dia, dib;
    reg [7:0] wea, web;
    reg [15:0] addra, addrb;
    always @(posedge CLKA or posedge CLKB) begin
        #0;
        dia = DIA;
        dib = DIB;
        wea = WEA;
        web = WEB;
        addra = ADDRA;
        addrb = ADDRB;
        hold_a = edge_a && ENA && lanes_on(WW_A, wea);
        hold_b = edge_b && ENB && lanes_on(WW_B, web);
        wrote_a = hold_a && WW_A != 0;
        wrote_b = hold_b && WW_B != 0;
        if (wrote_a) write(WW_A, addra, dia, wea, ws_a, wps_a);
        if (wrote_b) write(WW_B, addrb, dib, web, ws_b, wps_b);
        // Both ports writing a bit leave it x.
        if (wrote_a && wrote_b) begin
            for (i = 0; i < dbits(WW_A); i = i + 1)
                if (covers(WW_A, ws_a, wps_a, wea, 0, ws_a + i) && covers(WW_B, ws_b, wps_b, web, 0, ws_a + i))
                    mem[ws_a + i] <= 1'bx;
            for (i = 0; i < pbits(WW_A); i = i + 1)
                if (covers(WW_A, ws_a, wps_a, wea, 1, wps_a + i) && covers(WW_B, ws_b, wps_b, web, 1, wps_a + i))
                    memp[wps_a + i] <= 1'bx;
        end
        if (edge_a && REG_A)
            reg_a <= reg_next(reg_a, latch_a, SA, RSTREGA, REGCEA, RSTREG_PRIORITY_A == "RSTREG");
        if (edge_b && REG_B)
            reg_b <= reg_next(reg_b, latch_b, SB, RSTREGB, REGCEB, RSTREG_PRIORITY_B == "RSTREG");
        if (edge_a && ENA && RW_A != 0) begin
            if (RSTRAMA)
                latch_a <= SA;
            else if (!(hold_a && WRITE_MODE_A == "NO_CHANGE"))
                latch_a <= read(RW_A, addra, wrote_a && WRITE_MODE_A == "WRITE_FIRST", WW_A, ws_a, wps_a, wea, dia,
                                wrote_b && WRITE_MODE_B != "READ_FIRST", WW_B, ws_b, wps_b, web, wrote_a && wrote_b);
        end
        if (edge_b && ENB && RW_B != 0) begin
            if (RSTRAMB)
                latch_b <= SB;
            else if (!(hold_b && WRITE_MODE_B == "NO_CHANGE"))
                latch_b <= read(RW_B, addrb, wrote_b && WRITE_MODE_B == "WRITE_FIRST", WW_B, ws_b, wps_b, web, dib,
                                wrote_a && WRITE_MODE_A != "READ_FIRST", WW_A, ws_a, wps_a, wea, wrote_a && wrote_b);
        end
        edge_a = 0;
        edge_b = 0;
    end
endmodule

// A port's {parity, data} as the core's 72-bit word.
`define CS_WORD(p, d, pw, dw) {{(8 - pw){1'b0}}, p, {(64 - dw){1'b0}}, d}

module RAMB18E1 (
    input CLKARDCLK, CLKBWRCLK, ENARDEN, ENBWREN, REGCEAREGCE, REGCEB,
    input RSTRAMARSTRAM, RSTRAMB, RSTREGARSTREG, RSTREGB,
    input [13:0] ADDRARDADDR, ADDRBWRADDR,
    input [15:0] DIADI, DIBDI,
    input [1:0] DIPADIP, DIPBDIP,
    input [1:0] WEA,
    input [3:0] WEBWE,
    output [15:0] DOADO, DOBDO,
    output [1:0] DOPADOP, DOPBDOP
);
    parameter integer DOA_REG = 0;
    parameter integer DOB_REG = 0;
    parameter [17:0] INIT_A = 0;
    parameter [17:0] INIT_B = 0;
    parameter [17:0] SRVAL_A = 0;
    parameter [17:0] SRVAL_B = 0;
    parameter RAM_MODE = "TDP";
    parameter RDADDR_COLLISION_HWCONFIG = "DELAYED_WRITE";
    parameter integer READ_WIDTH_A = 0;
    parameter integer READ_WIDTH_B = 0;
    parameter integer WRITE_WIDTH_A = 0;
    parameter integer WRITE_WIDTH_B = 0;
    parameter WRITE_MODE_A = "WRITE_FIRST";
    parameter WRITE_MODE_B = "WRITE_FIRST";
    parameter RSTREG_PRIORITY_A = "RSTREG";
    parameter RSTREG_PRIORITY_B = "RSTREG";
    parameter SIM_COLLISION_CHECK = "ALL";
    parameter SIM_DEVICE = "7SERIES";
    parameter INIT_FILE = "NONE";
    parameter [0:0] IS_CLKARDCLK_INVERTED = 0;
    parameter [0:0] IS_CLKBWRCLK_INVERTED = 0;
    parameter [0:0] IS_ENARDEN_INVERTED = 0;
    parameter [0:0] IS_ENBWREN_INVERTED = 0;
    parameter [0:0] IS_RSTRAMARSTRAM_INVERTED = 0;
    parameter [0:0] IS_RSTRAMB_INVERTED = 0;
    parameter [0:0] IS_RSTREGARSTREG_INVERTED = 0;
    parameter [0:0] IS_RSTREGB_INVERTED = 0;
    parameter [255:0] INIT_00 = 0, INIT_01 = 0, INIT_02 = 0, INIT_03 = 0, INIT_04 = 0, INIT_05 = 0, INIT_06 = 0, INIT_07 = 0,
        INIT_08 = 0, INIT_09 = 0, INIT_0A = 0, INIT_0B = 0, INIT_0C = 0, INIT_0D = 0, INIT_0E = 0, INIT_0F = 0,
        INIT_10 = 0, INIT_11 = 0, INIT_12 = 0, INIT_13 = 0, INIT_14 = 0, INIT_15 = 0, INIT_16 = 0, INIT_17 = 0,
        INIT_18 = 0, INIT_19 = 0, INIT_1A = 0, INIT_1B = 0, INIT_1C = 0, INIT_1D = 0, INIT_1E = 0, INIT_1F = 0,
        INIT_20 = 0, INIT_21 = 0, INIT_22 = 0, INIT_23 = 0, INIT_24 = 0, INIT_25 = 0, INIT_26 = 0, INIT_27 = 0,
        INIT_28 = 0, INIT_29 = 0, INIT_2A = 0, INIT_2B = 0, INIT_2C = 0, INIT_2D = 0, INIT_2E = 0, INIT_2F = 0,
        INIT_30 = 0, INIT_31 = 0, INIT_32 = 0, INIT_33 = 0, INIT_34 = 0, INIT_35 = 0, INIT_36 = 0, INIT_37 = 0,
        INIT_38 = 0, INIT_39 = 0, INIT_3A = 0, INIT_3B = 0, INIT_3C = 0, INIT_3D = 0, INIT_3E = 0, INIT_3F = 0;
    parameter [255:0] INITP_00 = 0, INITP_01 = 0, INITP_02 = 0, INITP_03 = 0, INITP_04 = 0, INITP_05 = 0,
        INITP_06 = 0, INITP_07 = 0;

    localparam SDP = RAM_MODE == "SDP";
    // SDP: port A reads all 36 bits ({DOB, DOA}), port B writes them
    // ({DIB, DIA}, WEBWE a byte each); either side may be narrower.
    localparam SDP_R = SDP && READ_WIDTH_A == 36;
    localparam SDP_W = SDP && WRITE_WIDTH_B == 36;
    localparam [71:0] IA = SDP_R ? {INIT_B[17:16], INIT_A[17:16], INIT_B[15:0], INIT_A[15:0]} : INIT_A;
    localparam [71:0] SA = SDP_R ? {SRVAL_B[17:16], SRVAL_A[17:16], SRVAL_B[15:0], SRVAL_A[15:0]} : SRVAL_A;
    wire [71:0] doa, dob;
    CS_RAMB #(
        .ABITS(14), .DATA_BITS(16384), .PARITY_BITS(2048),
        .RW_A(READ_WIDTH_A), .RW_B(SDP ? 0 : READ_WIDTH_B),
        .WW_A(SDP ? 0 : WRITE_WIDTH_A), .WW_B(WRITE_WIDTH_B),
        .WRITE_MODE_A(WRITE_MODE_A), .WRITE_MODE_B(WRITE_MODE_B),
        .REG_A(DOA_REG), .REG_B(SDP ? 0 : DOB_REG),
        .RSTREG_PRIORITY_A(RSTREG_PRIORITY_A), .RSTREG_PRIORITY_B(RSTREG_PRIORITY_B),
        .INIT_A(IA), .INIT_B(INIT_B), .SRVAL_A(SA), .SRVAL_B(SRVAL_B),
        .CONTENTS({INIT_3F, INIT_3E, INIT_3D, INIT_3C, INIT_3B, INIT_3A, INIT_39, INIT_38,
                   INIT_37, INIT_36, INIT_35, INIT_34, INIT_33, INIT_32, INIT_31, INIT_30,
                   INIT_2F, INIT_2E, INIT_2D, INIT_2C, INIT_2B, INIT_2A, INIT_29, INIT_28,
                   INIT_27, INIT_26, INIT_25, INIT_24, INIT_23, INIT_22, INIT_21, INIT_20,
                   INIT_1F, INIT_1E, INIT_1D, INIT_1C, INIT_1B, INIT_1A, INIT_19, INIT_18,
                   INIT_17, INIT_16, INIT_15, INIT_14, INIT_13, INIT_12, INIT_11, INIT_10,
                   INIT_0F, INIT_0E, INIT_0D, INIT_0C, INIT_0B, INIT_0A, INIT_09, INIT_08,
                   INIT_07, INIT_06, INIT_05, INIT_04, INIT_03, INIT_02, INIT_01, INIT_00}),
        .CONTENTS_P({INITP_07, INITP_06, INITP_05, INITP_04, INITP_03, INITP_02, INITP_01, INITP_00})
    ) core (
        .CLKA(CLKARDCLK ^ IS_CLKARDCLK_INVERTED), .CLKB(CLKBWRCLK ^ IS_CLKBWRCLK_INVERTED),
        .ENA(ENARDEN ^ IS_ENARDEN_INVERTED), .ENB(ENBWREN ^ IS_ENBWREN_INVERTED),
        .REGCEA(REGCEAREGCE), .REGCEB(REGCEB),
        .RSTRAMA(RSTRAMARSTRAM ^ IS_RSTRAMARSTRAM_INVERTED), .RSTRAMB(RSTRAMB ^ IS_RSTRAMB_INVERTED),
        .RSTREGA(RSTREGARSTREG ^ IS_RSTREGARSTREG_INVERTED), .RSTREGB(RSTREGB ^ IS_RSTREGB_INVERTED),
        .ADDRA({2'b0, ADDRARDADDR}), .ADDRB({2'b0, ADDRBWRADDR}),
        .DIA(`CS_WORD(DIPADIP, DIADI, 2, 16)),
        .DIB(SDP_W ? `CS_WORD({DIPBDIP, DIPADIP}, {DIBDI, DIADI}, 4, 32) : `CS_WORD(DIPBDIP, DIBDI, 2, 16)),
        .WEA({6'b0, WEA}), .WEB(SDP_W ? {4'b0, WEBWE} : {6'b0, WEBWE[1:0]}),
        .DOA(doa), .DOB(dob)
    );
    assign DOADO = doa[15:0];
    assign DOPADOP = doa[65:64];
    assign DOBDO = SDP_R ? doa[31:16] : dob[15:0];
    assign DOPBDOP = SDP_R ? doa[67:66] : dob[65:64];
endmodule

module RAMB36E1 (
    input CLKARDCLK, CLKBWRCLK, ENARDEN, ENBWREN, REGCEAREGCE, REGCEB,
    input RSTRAMARSTRAM, RSTRAMB, RSTREGARSTREG, RSTREGB,
    input CASCADEINA, CASCADEINB, INJECTDBITERR, INJECTSBITERR,
    input [15:0] ADDRARDADDR, ADDRBWRADDR,
    input [31:0] DIADI, DIBDI,
    input [3:0] DIPADIP, DIPBDIP,
    input [3:0] WEA,
    input [7:0] WEBWE,
    output [31:0] DOADO, DOBDO,
    output [3:0] DOPADOP, DOPBDOP,
    output CASCADEOUTA, CASCADEOUTB, DBITERR, SBITERR,
    output [7:0] ECCPARITY,
    output [8:0] RDADDRECC
);
    parameter integer DOA_REG = 0;
    parameter integer DOB_REG = 0;
    parameter EN_ECC_READ = "FALSE";
    parameter EN_ECC_WRITE = "FALSE";
    parameter [35:0] INIT_A = 0;
    parameter [35:0] INIT_B = 0;
    parameter [35:0] SRVAL_A = 0;
    parameter [35:0] SRVAL_B = 0;
    parameter RAM_EXTENSION_A = "NONE";
    parameter RAM_EXTENSION_B = "NONE";
    parameter RAM_MODE = "TDP";
    parameter RDADDR_COLLISION_HWCONFIG = "DELAYED_WRITE";
    parameter integer READ_WIDTH_A = 0;
    parameter integer READ_WIDTH_B = 0;
    parameter integer WRITE_WIDTH_A = 0;
    parameter integer WRITE_WIDTH_B = 0;
    parameter WRITE_MODE_A = "WRITE_FIRST";
    parameter WRITE_MODE_B = "WRITE_FIRST";
    parameter RSTREG_PRIORITY_A = "RSTREG";
    parameter RSTREG_PRIORITY_B = "RSTREG";
    parameter SIM_COLLISION_CHECK = "ALL";
    parameter SIM_DEVICE = "7SERIES";
    parameter INIT_FILE = "NONE";
    parameter [0:0] IS_CLKARDCLK_INVERTED = 0;
    parameter [0:0] IS_CLKBWRCLK_INVERTED = 0;
    parameter [0:0] IS_ENARDEN_INVERTED = 0;
    parameter [0:0] IS_ENBWREN_INVERTED = 0;
    parameter [0:0] IS_RSTRAMARSTRAM_INVERTED = 0;
    parameter [0:0] IS_RSTRAMB_INVERTED = 0;
    parameter [0:0] IS_RSTREGARSTREG_INVERTED = 0;
    parameter [0:0] IS_RSTREGB_INVERTED = 0;
    parameter [255:0] INIT_00 = 0, INIT_01 = 0, INIT_02 = 0, INIT_03 = 0, INIT_04 = 0, INIT_05 = 0, INIT_06 = 0, INIT_07 = 0,
        INIT_08 = 0, INIT_09 = 0, INIT_0A = 0, INIT_0B = 0, INIT_0C = 0, INIT_0D = 0, INIT_0E = 0, INIT_0F = 0,
        INIT_10 = 0, INIT_11 = 0, INIT_12 = 0, INIT_13 = 0, INIT_14 = 0, INIT_15 = 0, INIT_16 = 0, INIT_17 = 0,
        INIT_18 = 0, INIT_19 = 0, INIT_1A = 0, INIT_1B = 0, INIT_1C = 0, INIT_1D = 0, INIT_1E = 0, INIT_1F = 0,
        INIT_20 = 0, INIT_21 = 0, INIT_22 = 0, INIT_23 = 0, INIT_24 = 0, INIT_25 = 0, INIT_26 = 0, INIT_27 = 0,
        INIT_28 = 0, INIT_29 = 0, INIT_2A = 0, INIT_2B = 0, INIT_2C = 0, INIT_2D = 0, INIT_2E = 0, INIT_2F = 0,
        INIT_30 = 0, INIT_31 = 0, INIT_32 = 0, INIT_33 = 0, INIT_34 = 0, INIT_35 = 0, INIT_36 = 0, INIT_37 = 0,
        INIT_38 = 0, INIT_39 = 0, INIT_3A = 0, INIT_3B = 0, INIT_3C = 0, INIT_3D = 0, INIT_3E = 0, INIT_3F = 0,
        INIT_40 = 0, INIT_41 = 0, INIT_42 = 0, INIT_43 = 0, INIT_44 = 0, INIT_45 = 0, INIT_46 = 0, INIT_47 = 0,
        INIT_48 = 0, INIT_49 = 0, INIT_4A = 0, INIT_4B = 0, INIT_4C = 0, INIT_4D = 0, INIT_4E = 0, INIT_4F = 0,
        INIT_50 = 0, INIT_51 = 0, INIT_52 = 0, INIT_53 = 0, INIT_54 = 0, INIT_55 = 0, INIT_56 = 0, INIT_57 = 0,
        INIT_58 = 0, INIT_59 = 0, INIT_5A = 0, INIT_5B = 0, INIT_5C = 0, INIT_5D = 0, INIT_5E = 0, INIT_5F = 0,
        INIT_60 = 0, INIT_61 = 0, INIT_62 = 0, INIT_63 = 0, INIT_64 = 0, INIT_65 = 0, INIT_66 = 0, INIT_67 = 0,
        INIT_68 = 0, INIT_69 = 0, INIT_6A = 0, INIT_6B = 0, INIT_6C = 0, INIT_6D = 0, INIT_6E = 0, INIT_6F = 0,
        INIT_70 = 0, INIT_71 = 0, INIT_72 = 0, INIT_73 = 0, INIT_74 = 0, INIT_75 = 0, INIT_76 = 0, INIT_77 = 0,
        INIT_78 = 0, INIT_79 = 0, INIT_7A = 0, INIT_7B = 0, INIT_7C = 0, INIT_7D = 0, INIT_7E = 0, INIT_7F = 0;
    parameter [255:0] INITP_00 = 0, INITP_01 = 0, INITP_02 = 0, INITP_03 = 0, INITP_04 = 0, INITP_05 = 0, INITP_06 = 0, INITP_07 = 0,
        INITP_08 = 0, INITP_09 = 0, INITP_0A = 0, INITP_0B = 0, INITP_0C = 0, INITP_0D = 0, INITP_0E = 0, INITP_0F = 0;

    localparam SDP = RAM_MODE == "SDP";
    // SDP: port A reads all 72 bits ({DOB, DOA}), port B writes them
    // ({DIB, DIA}, WEBWE a byte each); either side may be narrower.
    localparam SDP_R = SDP && READ_WIDTH_A == 72;
    localparam SDP_W = SDP && WRITE_WIDTH_B == 72;
    localparam [71:0] IA = SDP_R ? {INIT_B[35:32], INIT_A[35:32], INIT_B[31:0], INIT_A[31:0]} : INIT_A;
    localparam [71:0] SA = SDP_R ? {SRVAL_B[35:32], SRVAL_A[35:32], SRVAL_B[31:0], SRVAL_A[31:0]} : SRVAL_A;
    wire [71:0] doa, dob;
    CS_RAMB #(
        .ABITS(15), .DATA_BITS(32768), .PARITY_BITS(4096),
        .RW_A(READ_WIDTH_A), .RW_B(SDP ? 0 : READ_WIDTH_B),
        .WW_A(SDP ? 0 : WRITE_WIDTH_A), .WW_B(WRITE_WIDTH_B),
        .WRITE_MODE_A(WRITE_MODE_A), .WRITE_MODE_B(WRITE_MODE_B),
        .REG_A(DOA_REG), .REG_B(SDP ? 0 : DOB_REG),
        .RSTREG_PRIORITY_A(RSTREG_PRIORITY_A), .RSTREG_PRIORITY_B(RSTREG_PRIORITY_B),
        .INIT_A(IA), .INIT_B(INIT_B), .SRVAL_A(SA), .SRVAL_B(SRVAL_B),
        .CONTENTS({INIT_7F, INIT_7E, INIT_7D, INIT_7C, INIT_7B, INIT_7A, INIT_79, INIT_78,
                   INIT_77, INIT_76, INIT_75, INIT_74, INIT_73, INIT_72, INIT_71, INIT_70,
                   INIT_6F, INIT_6E, INIT_6D, INIT_6C, INIT_6B, INIT_6A, INIT_69, INIT_68,
                   INIT_67, INIT_66, INIT_65, INIT_64, INIT_63, INIT_62, INIT_61, INIT_60,
                   INIT_5F, INIT_5E, INIT_5D, INIT_5C, INIT_5B, INIT_5A, INIT_59, INIT_58,
                   INIT_57, INIT_56, INIT_55, INIT_54, INIT_53, INIT_52, INIT_51, INIT_50,
                   INIT_4F, INIT_4E, INIT_4D, INIT_4C, INIT_4B, INIT_4A, INIT_49, INIT_48,
                   INIT_47, INIT_46, INIT_45, INIT_44, INIT_43, INIT_42, INIT_41, INIT_40,
                   INIT_3F, INIT_3E, INIT_3D, INIT_3C, INIT_3B, INIT_3A, INIT_39, INIT_38,
                   INIT_37, INIT_36, INIT_35, INIT_34, INIT_33, INIT_32, INIT_31, INIT_30,
                   INIT_2F, INIT_2E, INIT_2D, INIT_2C, INIT_2B, INIT_2A, INIT_29, INIT_28,
                   INIT_27, INIT_26, INIT_25, INIT_24, INIT_23, INIT_22, INIT_21, INIT_20,
                   INIT_1F, INIT_1E, INIT_1D, INIT_1C, INIT_1B, INIT_1A, INIT_19, INIT_18,
                   INIT_17, INIT_16, INIT_15, INIT_14, INIT_13, INIT_12, INIT_11, INIT_10,
                   INIT_0F, INIT_0E, INIT_0D, INIT_0C, INIT_0B, INIT_0A, INIT_09, INIT_08,
                   INIT_07, INIT_06, INIT_05, INIT_04, INIT_03, INIT_02, INIT_01, INIT_00}),
        .CONTENTS_P({INITP_0F, INITP_0E, INITP_0D, INITP_0C, INITP_0B, INITP_0A, INITP_09, INITP_08, INITP_07, INITP_06, INITP_05, INITP_04, INITP_03, INITP_02, INITP_01, INITP_00})
    ) core (
        .CLKA(CLKARDCLK ^ IS_CLKARDCLK_INVERTED), .CLKB(CLKBWRCLK ^ IS_CLKBWRCLK_INVERTED),
        .ENA(ENARDEN ^ IS_ENARDEN_INVERTED), .ENB(ENBWREN ^ IS_ENBWREN_INVERTED),
        .REGCEA(REGCEAREGCE), .REGCEB(REGCEB),
        .RSTRAMA(RSTRAMARSTRAM ^ IS_RSTRAMARSTRAM_INVERTED), .RSTRAMB(RSTRAMB ^ IS_RSTRAMB_INVERTED),
        .RSTREGA(RSTREGARSTREG ^ IS_RSTREGARSTREG_INVERTED), .RSTREGB(RSTREGB ^ IS_RSTREGB_INVERTED),
        .ADDRA(ADDRARDADDR), .ADDRB(ADDRBWRADDR),
        .DIA(`CS_WORD(DIPADIP, DIADI, 4, 32)),
        .DIB(SDP_W ? `CS_WORD({DIPBDIP, DIPADIP}, {DIBDI, DIADI}, 8, 64) : `CS_WORD(DIPBDIP, DIBDI, 4, 32)),
        .WEA({4'b0, WEA}), .WEB(SDP_W ? WEBWE : {4'b0, WEBWE[3:0]}),
        .DOA(doa), .DOB(dob)
    );
    assign DOADO = doa[31:0];
    assign DOPADOP = doa[67:64];
    assign DOBDO = SDP_R ? doa[63:32] : dob[31:0];
    assign DOPBDOP = SDP_R ? doa[71:68] : dob[67:64];
    assign {CASCADEOUTA, CASCADEOUTB, DBITERR, SBITERR} = 0;
    assign ECCPARITY = 0;
    assign RDADDRECC = 0;
endmodule
