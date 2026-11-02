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

// Block RAM (UG473): two ports A and B, each with its clock, enable, byte
// write enables, register enable and resets; RAM_MODE "SDP" reads on A and
// writes on B, the data over both ports' pins.
(* blackbox *) module RAMB18E1 (output [15:0] DOADO, DOBDO, output [1:0] DOPADOP, DOPBDOP,
                               input CLKARDCLK, CLKBWRCLK, ENARDEN, ENBWREN, REGCEAREGCE, REGCEB,
                               input RSTRAMARSTRAM, RSTRAMB, RSTREGARSTREG, RSTREGB,
                               input [13:0] ADDRARDADDR, ADDRBWRADDR, input [15:0] DIADI, DIBDI,
                               input [1:0] DIPADIP, DIPBDIP, input [1:0] WEA, input [3:0] WEBWE);
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
  parameter [255:0] INIT_00 = 0, INIT_01 = 0, INIT_02 = 0, INIT_03 = 0, INIT_04 = 0, INIT_05 = 0, INIT_06 = 0, INIT_07 = 0;
  parameter [255:0] INIT_08 = 0, INIT_09 = 0, INIT_0A = 0, INIT_0B = 0, INIT_0C = 0, INIT_0D = 0, INIT_0E = 0, INIT_0F = 0;
  parameter [255:0] INIT_10 = 0, INIT_11 = 0, INIT_12 = 0, INIT_13 = 0, INIT_14 = 0, INIT_15 = 0, INIT_16 = 0, INIT_17 = 0;
  parameter [255:0] INIT_18 = 0, INIT_19 = 0, INIT_1A = 0, INIT_1B = 0, INIT_1C = 0, INIT_1D = 0, INIT_1E = 0, INIT_1F = 0;
  parameter [255:0] INIT_20 = 0, INIT_21 = 0, INIT_22 = 0, INIT_23 = 0, INIT_24 = 0, INIT_25 = 0, INIT_26 = 0, INIT_27 = 0;
  parameter [255:0] INIT_28 = 0, INIT_29 = 0, INIT_2A = 0, INIT_2B = 0, INIT_2C = 0, INIT_2D = 0, INIT_2E = 0, INIT_2F = 0;
  parameter [255:0] INIT_30 = 0, INIT_31 = 0, INIT_32 = 0, INIT_33 = 0, INIT_34 = 0, INIT_35 = 0, INIT_36 = 0, INIT_37 = 0;
  parameter [255:0] INIT_38 = 0, INIT_39 = 0, INIT_3A = 0, INIT_3B = 0, INIT_3C = 0, INIT_3D = 0, INIT_3E = 0, INIT_3F = 0;
  parameter [255:0] INITP_00 = 0, INITP_01 = 0, INITP_02 = 0, INITP_03 = 0, INITP_04 = 0, INITP_05 = 0, INITP_06 = 0, INITP_07 = 0;
endmodule

// Block RAM (UG473): two ports A and B, each with its clock, enable, byte
// write enables, register enable and resets; RAM_MODE "SDP" reads on A and
// writes on B, the data over both ports' pins.
(* blackbox *) module RAMB36E1 (output [31:0] DOADO, DOBDO, output [3:0] DOPADOP, DOPBDOP,
                               output CASCADEOUTA, CASCADEOUTB, DBITERR, SBITERR, output [7:0] ECCPARITY,
                               output [8:0] RDADDRECC, input CASCADEINA, CASCADEINB, INJECTDBITERR, INJECTSBITERR,
                               input CLKARDCLK, CLKBWRCLK, ENARDEN, ENBWREN, REGCEAREGCE, REGCEB,
                               input RSTRAMARSTRAM, RSTRAMB, RSTREGARSTREG, RSTREGB,
                               input [15:0] ADDRARDADDR, ADDRBWRADDR, input [31:0] DIADI, DIBDI,
                               input [3:0] DIPADIP, DIPBDIP, input [3:0] WEA, input [7:0] WEBWE);
  parameter integer DOA_REG = 0;
  parameter integer DOB_REG = 0;
  parameter [35:0] INIT_A = 0;
  parameter [35:0] INIT_B = 0;
  parameter [35:0] SRVAL_A = 0;
  parameter [35:0] SRVAL_B = 0;
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
  parameter EN_ECC_READ = "FALSE";
  parameter EN_ECC_WRITE = "FALSE";
  parameter RAM_EXTENSION_A = "NONE";
  parameter RAM_EXTENSION_B = "NONE";
  parameter [0:0] IS_CLKARDCLK_INVERTED = 0;
  parameter [0:0] IS_CLKBWRCLK_INVERTED = 0;
  parameter [0:0] IS_ENARDEN_INVERTED = 0;
  parameter [0:0] IS_ENBWREN_INVERTED = 0;
  parameter [0:0] IS_RSTRAMARSTRAM_INVERTED = 0;
  parameter [0:0] IS_RSTRAMB_INVERTED = 0;
  parameter [0:0] IS_RSTREGARSTREG_INVERTED = 0;
  parameter [0:0] IS_RSTREGB_INVERTED = 0;
  parameter [255:0] INIT_00 = 0, INIT_01 = 0, INIT_02 = 0, INIT_03 = 0, INIT_04 = 0, INIT_05 = 0, INIT_06 = 0, INIT_07 = 0;
  parameter [255:0] INIT_08 = 0, INIT_09 = 0, INIT_0A = 0, INIT_0B = 0, INIT_0C = 0, INIT_0D = 0, INIT_0E = 0, INIT_0F = 0;
  parameter [255:0] INIT_10 = 0, INIT_11 = 0, INIT_12 = 0, INIT_13 = 0, INIT_14 = 0, INIT_15 = 0, INIT_16 = 0, INIT_17 = 0;
  parameter [255:0] INIT_18 = 0, INIT_19 = 0, INIT_1A = 0, INIT_1B = 0, INIT_1C = 0, INIT_1D = 0, INIT_1E = 0, INIT_1F = 0;
  parameter [255:0] INIT_20 = 0, INIT_21 = 0, INIT_22 = 0, INIT_23 = 0, INIT_24 = 0, INIT_25 = 0, INIT_26 = 0, INIT_27 = 0;
  parameter [255:0] INIT_28 = 0, INIT_29 = 0, INIT_2A = 0, INIT_2B = 0, INIT_2C = 0, INIT_2D = 0, INIT_2E = 0, INIT_2F = 0;
  parameter [255:0] INIT_30 = 0, INIT_31 = 0, INIT_32 = 0, INIT_33 = 0, INIT_34 = 0, INIT_35 = 0, INIT_36 = 0, INIT_37 = 0;
  parameter [255:0] INIT_38 = 0, INIT_39 = 0, INIT_3A = 0, INIT_3B = 0, INIT_3C = 0, INIT_3D = 0, INIT_3E = 0, INIT_3F = 0;
  parameter [255:0] INIT_40 = 0, INIT_41 = 0, INIT_42 = 0, INIT_43 = 0, INIT_44 = 0, INIT_45 = 0, INIT_46 = 0, INIT_47 = 0;
  parameter [255:0] INIT_48 = 0, INIT_49 = 0, INIT_4A = 0, INIT_4B = 0, INIT_4C = 0, INIT_4D = 0, INIT_4E = 0, INIT_4F = 0;
  parameter [255:0] INIT_50 = 0, INIT_51 = 0, INIT_52 = 0, INIT_53 = 0, INIT_54 = 0, INIT_55 = 0, INIT_56 = 0, INIT_57 = 0;
  parameter [255:0] INIT_58 = 0, INIT_59 = 0, INIT_5A = 0, INIT_5B = 0, INIT_5C = 0, INIT_5D = 0, INIT_5E = 0, INIT_5F = 0;
  parameter [255:0] INIT_60 = 0, INIT_61 = 0, INIT_62 = 0, INIT_63 = 0, INIT_64 = 0, INIT_65 = 0, INIT_66 = 0, INIT_67 = 0;
  parameter [255:0] INIT_68 = 0, INIT_69 = 0, INIT_6A = 0, INIT_6B = 0, INIT_6C = 0, INIT_6D = 0, INIT_6E = 0, INIT_6F = 0;
  parameter [255:0] INIT_70 = 0, INIT_71 = 0, INIT_72 = 0, INIT_73 = 0, INIT_74 = 0, INIT_75 = 0, INIT_76 = 0, INIT_77 = 0;
  parameter [255:0] INIT_78 = 0, INIT_79 = 0, INIT_7A = 0, INIT_7B = 0, INIT_7C = 0, INIT_7D = 0, INIT_7E = 0, INIT_7F = 0;
  parameter [255:0] INITP_00 = 0, INITP_01 = 0, INITP_02 = 0, INITP_03 = 0, INITP_04 = 0, INITP_05 = 0, INITP_06 = 0, INITP_07 = 0;
  parameter [255:0] INITP_08 = 0, INITP_09 = 0, INITP_0A = 0, INITP_0B = 0, INITP_0C = 0, INITP_0D = 0, INITP_0E = 0, INITP_0F = 0;
endmodule

// DSP48E1 (UG479): P = (A [* B] ...) by OPMODE, ALUMODE and INMODE, with
// optional register stages (A, B, C, D, AD, M, P and the controls').
(* blackbox *) module DSP48E1 (output [29:0] ACOUT, output [17:0] BCOUT, output CARRYCASCOUT,
                               output [3:0] CARRYOUT, output MULTSIGNOUT, OVERFLOW,
                               output [47:0] P, output PATTERNBDETECT, PATTERNDETECT,
                               output [47:0] PCOUT, output UNDERFLOW,
                               input [29:0] A, ACIN, input [3:0] ALUMODE, input [17:0] B, BCIN,
                               input [47:0] C, input CARRYCASCIN, CARRYIN, input [2:0] CARRYINSEL,
                               input CEA1, CEA2, CEAD, CEALUMODE, CEB1, CEB2, CEC, CECARRYIN,
                               CECTRL, CED, CEINMODE, CEM, CEP, CLK, input [24:0] D,
                               input [4:0] INMODE, input MULTSIGNIN, input [6:0] OPMODE,
                               input [47:0] PCIN, input RSTA, RSTALLCARRYIN, RSTALUMODE, RSTB,
                               RSTC, RSTCTRL, RSTD, RSTINMODE, RSTM, RSTP);
  parameter integer ACASCREG = 1;
  parameter integer ADREG = 1;
  parameter integer ALUMODEREG = 1;
  parameter integer AREG = 1;
  parameter AUTORESET_PATDET = "NO_RESET";
  parameter A_INPUT = "DIRECT";
  parameter integer BCASCREG = 1;
  parameter integer BREG = 1;
  parameter B_INPUT = "DIRECT";
  parameter integer CARRYINREG = 1;
  parameter integer CARRYINSELREG = 1;
  parameter integer CREG = 1;
  parameter integer DREG = 1;
  parameter integer INMODEREG = 1;
  parameter integer MREG = 1;
  parameter integer OPMODEREG = 1;
  parameter integer PREG = 1;
  parameter SEL_MASK = "MASK";
  parameter SEL_PATTERN = "PATTERN";
  parameter USE_DPORT = "FALSE";
  parameter USE_MULT = "MULTIPLY";
  parameter USE_PATTERN_DETECT = "NO_PATDET";
  parameter USE_SIMD = "ONE48";
  parameter [47:0] MASK = 48'h3fffffffffff;
  parameter [47:0] PATTERN = 48'h000000000000;
  parameter [3:0] IS_ALUMODE_INVERTED = 4'b0;
  parameter [0:0] IS_CARRYIN_INVERTED = 1'b0;
  parameter [0:0] IS_CLK_INVERTED = 1'b0;
  parameter [4:0] IS_INMODE_INVERTED = 5'b0;
  parameter [6:0] IS_OPMODE_INVERTED = 7'b0;
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
