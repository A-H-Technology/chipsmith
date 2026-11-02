// The 7-series primitives chipsmith's xc7 synthesis ends in and its packer
// reads (cells.rs builds them), and the hard blocks a design may
// instantiate, declared for the Verilog front end with Xilinx's port names
// and parameters (UG953). Read as a library, so only ports and parameters
// count: behaviour is UG953's, and the proofs use yosys' models of them
// (techlibs/xilinx/cells_sim.v).

(* blackbox *) module LUT1 (output O, input I0);
  parameter [1:0] INIT = 0;
endmodule
(* blackbox *) module LUT2 (output O, input I0, I1);
  parameter [3:0] INIT = 0;
endmodule
(* blackbox *) module LUT3 (output O, input I0, I1, I2);
  parameter [7:0] INIT = 0;
endmodule
(* blackbox *) module LUT4 (output O, input I0, I1, I2, I3);
  parameter [15:0] INIT = 0;
endmodule
(* blackbox *) module LUT5 (output O, input I0, I1, I2, I3, I4);
  parameter [31:0] INIT = 0;
endmodule
(* blackbox *) module LUT6 (output O, input I0, I1, I2, I3, I4, I5);
  parameter [63:0] INIT = 0;
endmodule

// O = S ? CI : DI per bit up the chain from CI (the first CARRY4 of a
// chain: CYINIT), O the sums S ^ carry, CO the carries out.
(* blackbox *) module CARRY4 (output [3:0] CO, output [3:0] O, input CI, input CYINIT,
                              input [3:0] DI, input [3:0] S);
endmodule

(* blackbox *) module MUXF7 (output O, input I0, I1, S);
endmodule
(* blackbox *) module MUXF8 (output O, input I0, I1, S);
endmodule

// Registers: R synchronous reset to 0 and S set to 1, both over CE; CLR
// asynchronous clear, PRE preset. INIT is the power-up value.
(* blackbox *) module FDRE (output Q, input C, CE, D, R);
  parameter [0:0] INIT = 0;
  parameter [0:0] IS_C_INVERTED = 0;
endmodule
(* blackbox *) module FDSE (output Q, input C, CE, D, S);
  parameter [0:0] INIT = 1;
  parameter [0:0] IS_C_INVERTED = 0;
endmodule
(* blackbox *) module FDCE (output Q, input C, CE, CLR, D);
  parameter [0:0] INIT = 0;
  parameter [0:0] IS_C_INVERTED = 0;
endmodule
(* blackbox *) module FDPE (output Q, input C, CE, PRE, D);
  parameter [0:0] INIT = 1;
  parameter [0:0] IS_C_INVERTED = 0;
endmodule

// Four 32 x 2 RAMs of one SLICEM, all written at ADDRD from DIA..DID,
// each read at its own address, asynchronously.
(* blackbox *) module RAM32M (output [1:0] DOA, DOB, DOC, DOD,
                              input [4:0] ADDRA, ADDRB, ADDRC, ADDRD,
                              input [1:0] DIA, DIB, DIC, DID, input WCLK, WE);
  parameter [63:0] INIT_A = 0;
  parameter [63:0] INIT_B = 0;
  parameter [63:0] INIT_C = 0;
  parameter [63:0] INIT_D = 0;
  parameter [0:0] IS_WCLK_INVERTED = 0;
endmodule

(* blackbox *) module IBUF (output O, input I);
endmodule
(* blackbox *) module OBUF (output O, input I);
endmodule
// T high releases the pin.
(* blackbox *) module OBUFT (output O, input I, T);
endmodule
(* blackbox *) module IOBUF (output O, inout IO, input I, T);
endmodule
(* blackbox *) module BUFG (output O, input I);
endmodule

(* blackbox *) module BSCANE2 (output CAPTURE, DRCK, RESET, RUNTEST, SEL, SHIFT, TCK, TDI,
                               TMS, UPDATE, input TDO);
  parameter DISABLE_JTAG = "FALSE";
  parameter integer JTAG_CHAIN = 1;
endmodule
