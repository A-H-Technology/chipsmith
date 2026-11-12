// a5pnr's cells (README (w), (y)). A5_ARITH is one half of an ALM in
// arithmetic mode, as Quartus' tennm_lcell_comb models it (quartus/eda/
// fv_lib/verilog/tennm/tennm_atoms.sv): p = P[{D,C,B,A}], g = G[{D,C,B,A}],
// SO = CI ^ p, CO = p ? CI : g. A5_M20K is an M20K as a 512 x 32 simple
// dual-port RAM: write enable, four byte enables, a read enable, an optional
// output register and initial contents (INIT bit 32a + k is word a, bit k).
(* blackbox *)
module A5_ARITH #(parameter [15:0] P = 16'h0, parameter [15:0] G = 16'h0) (
    input A, B, C, D, CI,
    output SO, CO
);
endmodule

(* blackbox *)
module A5_M20K #(parameter [16383:0] INIT = 16384'h0, parameter OUTREG = 0) (
    input CLK,
    input WE,
    input [3:0] BE,
    input RE,
    input [8:0] WADDR,
    input [31:0] WDATA,
    input [8:0] RADDR,
    output [31:0] RDATA
);
endmodule

// An IOPLL (README (y)): the PLL of the reference clock pin's I/O bank,
// output 0 only, frequencies in kHz (tools/pll/iopll.py picks N, M, C).
(* blackbox *)
module A5_IOPLL #(parameter REF_KHZ = 50000, parameter OUT_KHZ = 100000) (
    input REFCLK,
    output OUTCLK
);
endmodule

// A multiplier of a DSP block (README (z11)): 18x18 (WIDE = 0: A[17:0],
// B[17:0], P[35:0]) or 27x27 (WIDE = 1), both operands signed or both
// unsigned. a5pnr registers P in the block's output register: every
// product bit must go straight into a flop (yosys/mul_map.v makes these).
(* blackbox *)
module A5_MUL #(parameter SIGNED = 0, parameter WIDE = 0) (
    input [26:0] A, B,
    output [53:0] P
);
endmodule
