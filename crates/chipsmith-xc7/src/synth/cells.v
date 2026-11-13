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

// The Zynq's processing system, as prjxray-db's PS7 site has it: each site
// pin NAMEk is bit k of port NAME (a pin with no index is a scalar). The
// dedicated MIO, DDR and PS* pins (INOUT there) aren't the fabric's and are
// left out. An input left open is tied to 0, as is every input of the PS7
// chipsmith adds to a design without one (IRQF2P[19:16] are the cores' FIQ
// and IRQ). Generated from site_type_PS7.json; 669 ports.
(* blackbox *) module PS7 (
    input [3:0] DDRARB, output [199:0] DEBUGDATA, input [15:0] DEBUGSELECT, input DMA0ACLK,
    input DMA0DAREADY, output [1:0] DMA0DATYPE, output DMA0DAVALID, input DMA0DRLAST,
    output DMA0DRREADY, input [1:0] DMA0DRTYPE, input DMA0DRVALID, output DMA0RSTN,
    input DMA1ACLK, input DMA1DAREADY, output [1:0] DMA1DATYPE, output DMA1DAVALID,
    input DMA1DRLAST, output DMA1DRREADY, input [1:0] DMA1DRTYPE, input DMA1DRVALID,
    output DMA1RSTN, input DMA2ACLK, input DMA2DAREADY, output [1:0] DMA2DATYPE,
    output DMA2DAVALID, input DMA2DRLAST, output DMA2DRREADY, input [1:0] DMA2DRTYPE,
    input DMA2DRVALID, output DMA2RSTN, input DMA3ACLK, input DMA3DAREADY,
    output [1:0] DMA3DATYPE, output DMA3DAVALID, input DMA3DRLAST, output DMA3DRREADY,
    input [1:0] DMA3DRTYPE, input DMA3DRVALID, output DMA3RSTN, input EMIOCAN0PHYRX,
    output EMIOCAN0PHYTX, input EMIOCAN1PHYRX, output EMIOCAN1PHYTX, input EMIOENET0EXTINTIN,
    input EMIOENET0GMIICOL, input EMIOENET0GMIICRS, input EMIOENET0GMIIRXCLK,
    input [7:0] EMIOENET0GMIIRXD, input EMIOENET0GMIIRXDV, input EMIOENET0GMIIRXER,
    input EMIOENET0GMIITXCLK, output [7:0] EMIOENET0GMIITXD, output EMIOENET0GMIITXEN,
    output EMIOENET0GMIITXER, input EMIOENET0MDIOI, output EMIOENET0MDIOMDC,
    output EMIOENET0MDIOO, output EMIOENET0MDIOTN, output EMIOENET0PTPDELAYREQRX,
    output EMIOENET0PTPDELAYREQTX, output EMIOENET0PTPPDELAYREQRX,
    output EMIOENET0PTPPDELAYREQTX, output EMIOENET0PTPPDELAYRESPRX,
    output EMIOENET0PTPPDELAYRESPTX, output EMIOENET0PTPSYNCFRAMERX,
    output EMIOENET0PTPSYNCFRAMETX, output EMIOENET0SOFRX, output EMIOENET0SOFTX,
    input EMIOENET1EXTINTIN, input EMIOENET1GMIICOL, input EMIOENET1GMIICRS,
    input EMIOENET1GMIIRXCLK, input [7:0] EMIOENET1GMIIRXD, input EMIOENET1GMIIRXDV,
    input EMIOENET1GMIIRXER, input EMIOENET1GMIITXCLK, output [7:0] EMIOENET1GMIITXD,
    output EMIOENET1GMIITXEN, output EMIOENET1GMIITXER, input EMIOENET1MDIOI,
    output EMIOENET1MDIOMDC, output EMIOENET1MDIOO, output EMIOENET1MDIOTN,
    output EMIOENET1PTPDELAYREQRX, output EMIOENET1PTPDELAYREQTX,
    output EMIOENET1PTPPDELAYREQRX, output EMIOENET1PTPPDELAYREQTX,
    output EMIOENET1PTPPDELAYRESPRX, output EMIOENET1PTPPDELAYRESPTX,
    output EMIOENET1PTPSYNCFRAMERX, output EMIOENET1PTPSYNCFRAMETX, output EMIOENET1SOFRX,
    output EMIOENET1SOFTX, input [63:0] EMIOGPIOI, output [63:0] EMIOGPIOO,
    output [63:0] EMIOGPIOTN, input EMIOI2C0SCLI, output EMIOI2C0SCLO, output EMIOI2C0SCLTN,
    input EMIOI2C0SDAI, output EMIOI2C0SDAO, output EMIOI2C0SDATN, input EMIOI2C1SCLI,
    output EMIOI2C1SCLO, output EMIOI2C1SCLTN, input EMIOI2C1SDAI, output EMIOI2C1SDAO,
    output EMIOI2C1SDATN, input EMIOPJTAGTCK, input EMIOPJTAGTDI, output EMIOPJTAGTDO,
    output EMIOPJTAGTDTN, input EMIOPJTAGTMS, output EMIOSDIO0BUSPOW,
    output [2:0] EMIOSDIO0BUSVOLT, input EMIOSDIO0CDN, output EMIOSDIO0CLK,
    input EMIOSDIO0CLKFB, input EMIOSDIO0CMDI, output EMIOSDIO0CMDO, output EMIOSDIO0CMDTN,
    input [3:0] EMIOSDIO0DATAI, output [3:0] EMIOSDIO0DATAO, output [3:0] EMIOSDIO0DATATN,
    output EMIOSDIO0LED, input EMIOSDIO0WP, output EMIOSDIO1BUSPOW,
    output [2:0] EMIOSDIO1BUSVOLT, input EMIOSDIO1CDN, output EMIOSDIO1CLK,
    input EMIOSDIO1CLKFB, input EMIOSDIO1CMDI, output EMIOSDIO1CMDO, output EMIOSDIO1CMDTN,
    input [3:0] EMIOSDIO1DATAI, output [3:0] EMIOSDIO1DATAO, output [3:0] EMIOSDIO1DATATN,
    output EMIOSDIO1LED, input EMIOSDIO1WP, input EMIOSPI0MI, output EMIOSPI0MO,
    output EMIOSPI0MOTN, input EMIOSPI0SCLKI, output EMIOSPI0SCLKO, output EMIOSPI0SCLKTN,
    input EMIOSPI0SI, output EMIOSPI0SO, input EMIOSPI0SSIN, output EMIOSPI0SSNTN,
    output [2:0] EMIOSPI0SSON, output EMIOSPI0STN, input EMIOSPI1MI, output EMIOSPI1MO,
    output EMIOSPI1MOTN, input EMIOSPI1SCLKI, output EMIOSPI1SCLKO, output EMIOSPI1SCLKTN,
    input EMIOSPI1SI, output EMIOSPI1SO, input EMIOSPI1SSIN, output EMIOSPI1SSNTN,
    output [2:0] EMIOSPI1SSON, output EMIOSPI1STN, input EMIOSRAMINTIN, input EMIOTRACECLK,
    output EMIOTRACECTL, output [31:0] EMIOTRACEDATA, input [2:0] EMIOTTC0CLKI,
    output [2:0] EMIOTTC0WAVEO, input [2:0] EMIOTTC1CLKI, output [2:0] EMIOTTC1WAVEO,
    input EMIOUART0CTSN, input EMIOUART0DCDN, input EMIOUART0DSRN, output EMIOUART0DTRN,
    input EMIOUART0RIN, output EMIOUART0RTSN, input EMIOUART0RX, output EMIOUART0TX,
    input EMIOUART1CTSN, input EMIOUART1DCDN, input EMIOUART1DSRN, output EMIOUART1DTRN,
    input EMIOUART1RIN, output EMIOUART1RTSN, input EMIOUART1RX, output EMIOUART1TX,
    output [1:0] EMIOUSB0PORTINDCTL, input EMIOUSB0VBUSPWRFAULT, output EMIOUSB0VBUSPWRSELECT,
    output [1:0] EMIOUSB1PORTINDCTL, input EMIOUSB1VBUSPWRFAULT, output EMIOUSB1VBUSPWRSELECT,
    input EMIOWDTCLKI, output EMIOWDTRSTO, input EVENTEVENTI, output EVENTEVENTO,
    output [1:0] EVENTSTANDBYWFE, output [1:0] EVENTSTANDBYWFI, output [3:0] FCLKCLK,
    input [3:0] FCLKCLKTRIGN, output [3:0] FCLKRESETN, input FPGAIDLEN,
    input [3:0] FTMDTRACEINATID, input FTMDTRACEINCLOCK, input [31:0] FTMDTRACEINDATA,
    input FTMDTRACEINVALID, input [31:0] FTMTF2PDEBUG, input [3:0] FTMTF2PTRIG,
    output [3:0] FTMTF2PTRIGACK, output [31:0] FTMTP2FDEBUG, output [3:0] FTMTP2FTRIG,
    input [3:0] FTMTP2FTRIGACK, input [19:0] IRQF2P, output [28:0] IRQP2F, input MAXIGP0ACLK,
    output [31:0] MAXIGP0ARADDR, output [1:0] MAXIGP0ARBURST, output [3:0] MAXIGP0ARCACHE,
    output MAXIGP0ARESETN, output [11:0] MAXIGP0ARID, output [3:0] MAXIGP0ARLEN,
    output [1:0] MAXIGP0ARLOCK, output [2:0] MAXIGP0ARPROT, output [3:0] MAXIGP0ARQOS,
    input MAXIGP0ARREADY, output [1:0] MAXIGP0ARSIZE, output MAXIGP0ARVALID,
    output [31:0] MAXIGP0AWADDR, output [1:0] MAXIGP0AWBURST, output [3:0] MAXIGP0AWCACHE,
    output [11:0] MAXIGP0AWID, output [3:0] MAXIGP0AWLEN, output [1:0] MAXIGP0AWLOCK,
    output [2:0] MAXIGP0AWPROT, output [3:0] MAXIGP0AWQOS, input MAXIGP0AWREADY,
    output [1:0] MAXIGP0AWSIZE, output MAXIGP0AWVALID, input [11:0] MAXIGP0BID,
    output MAXIGP0BREADY, input [1:0] MAXIGP0BRESP, input MAXIGP0BVALID,
    input [31:0] MAXIGP0RDATA, input [11:0] MAXIGP0RID, input MAXIGP0RLAST,
    output MAXIGP0RREADY, input [1:0] MAXIGP0RRESP, input MAXIGP0RVALID,
    output [31:0] MAXIGP0WDATA, output [11:0] MAXIGP0WID, output MAXIGP0WLAST,
    input MAXIGP0WREADY, output [3:0] MAXIGP0WSTRB, output MAXIGP0WVALID, input MAXIGP1ACLK,
    output [31:0] MAXIGP1ARADDR, output [1:0] MAXIGP1ARBURST, output [3:0] MAXIGP1ARCACHE,
    output MAXIGP1ARESETN, output [11:0] MAXIGP1ARID, output [3:0] MAXIGP1ARLEN,
    output [1:0] MAXIGP1ARLOCK, output [2:0] MAXIGP1ARPROT, output [3:0] MAXIGP1ARQOS,
    input MAXIGP1ARREADY, output [1:0] MAXIGP1ARSIZE, output MAXIGP1ARVALID,
    output [31:0] MAXIGP1AWADDR, output [1:0] MAXIGP1AWBURST, output [3:0] MAXIGP1AWCACHE,
    output [11:0] MAXIGP1AWID, output [3:0] MAXIGP1AWLEN, output [1:0] MAXIGP1AWLOCK,
    output [2:0] MAXIGP1AWPROT, output [3:0] MAXIGP1AWQOS, input MAXIGP1AWREADY,
    output [1:0] MAXIGP1AWSIZE, output MAXIGP1AWVALID, input [11:0] MAXIGP1BID,
    output MAXIGP1BREADY, input [1:0] MAXIGP1BRESP, input MAXIGP1BVALID,
    input [31:0] MAXIGP1RDATA, input [11:0] MAXIGP1RID, input MAXIGP1RLAST,
    output MAXIGP1RREADY, input [1:0] MAXIGP1RRESP, input MAXIGP1RVALID,
    output [31:0] MAXIGP1WDATA, output [11:0] MAXIGP1WID, output MAXIGP1WLAST,
    input MAXIGP1WREADY, output [3:0] MAXIGP1WSTRB, output MAXIGP1WVALID, input SAXIACPACLK,
    input [31:0] SAXIACPARADDR, input [1:0] SAXIACPARBURST, input [3:0] SAXIACPARCACHE,
    output SAXIACPARESETN, input [2:0] SAXIACPARID, input [3:0] SAXIACPARLEN,
    input [1:0] SAXIACPARLOCK, input [2:0] SAXIACPARPROT, input [3:0] SAXIACPARQOS,
    output SAXIACPARREADY, input [1:0] SAXIACPARSIZE, input [4:0] SAXIACPARUSER,
    input SAXIACPARVALID, input [31:0] SAXIACPAWADDR, input [1:0] SAXIACPAWBURST,
    input [3:0] SAXIACPAWCACHE, input [2:0] SAXIACPAWID, input [3:0] SAXIACPAWLEN,
    input [1:0] SAXIACPAWLOCK, input [2:0] SAXIACPAWPROT, input [3:0] SAXIACPAWQOS,
    output SAXIACPAWREADY, input [1:0] SAXIACPAWSIZE, input [4:0] SAXIACPAWUSER,
    input SAXIACPAWVALID, output [2:0] SAXIACPBID, input SAXIACPBREADY,
    output [1:0] SAXIACPBRESP, output SAXIACPBVALID, output [63:0] SAXIACPRDATA,
    output [2:0] SAXIACPRID, output SAXIACPRLAST, input SAXIACPRREADY,
    output [1:0] SAXIACPRRESP, output SAXIACPRVALID, input [63:0] SAXIACPWDATA,
    input [2:0] SAXIACPWID, input SAXIACPWLAST, output SAXIACPWREADY, input [7:0] SAXIACPWSTRB,
    input SAXIACPWVALID, input SAXIGP0ACLK, input [31:0] SAXIGP0ARADDR,
    input [1:0] SAXIGP0ARBURST, input [3:0] SAXIGP0ARCACHE, output SAXIGP0ARESETN,
    input [5:0] SAXIGP0ARID, input [3:0] SAXIGP0ARLEN, input [1:0] SAXIGP0ARLOCK,
    input [2:0] SAXIGP0ARPROT, input [3:0] SAXIGP0ARQOS, output SAXIGP0ARREADY,
    input [1:0] SAXIGP0ARSIZE, input SAXIGP0ARVALID, input [31:0] SAXIGP0AWADDR,
    input [1:0] SAXIGP0AWBURST, input [3:0] SAXIGP0AWCACHE, input [5:0] SAXIGP0AWID,
    input [3:0] SAXIGP0AWLEN, input [1:0] SAXIGP0AWLOCK, input [2:0] SAXIGP0AWPROT,
    input [3:0] SAXIGP0AWQOS, output SAXIGP0AWREADY, input [1:0] SAXIGP0AWSIZE,
    input SAXIGP0AWVALID, output [5:0] SAXIGP0BID, input SAXIGP0BREADY,
    output [1:0] SAXIGP0BRESP, output SAXIGP0BVALID, output [31:0] SAXIGP0RDATA,
    output [5:0] SAXIGP0RID, output SAXIGP0RLAST, input SAXIGP0RREADY,
    output [1:0] SAXIGP0RRESP, output SAXIGP0RVALID, input [31:0] SAXIGP0WDATA,
    input [5:0] SAXIGP0WID, input SAXIGP0WLAST, output SAXIGP0WREADY, input [3:0] SAXIGP0WSTRB,
    input SAXIGP0WVALID, input SAXIGP1ACLK, input [31:0] SAXIGP1ARADDR,
    input [1:0] SAXIGP1ARBURST, input [3:0] SAXIGP1ARCACHE, output SAXIGP1ARESETN,
    input [5:0] SAXIGP1ARID, input [3:0] SAXIGP1ARLEN, input [1:0] SAXIGP1ARLOCK,
    input [2:0] SAXIGP1ARPROT, input [3:0] SAXIGP1ARQOS, output SAXIGP1ARREADY,
    input [1:0] SAXIGP1ARSIZE, input SAXIGP1ARVALID, input [31:0] SAXIGP1AWADDR,
    input [1:0] SAXIGP1AWBURST, input [3:0] SAXIGP1AWCACHE, input [5:0] SAXIGP1AWID,
    input [3:0] SAXIGP1AWLEN, input [1:0] SAXIGP1AWLOCK, input [2:0] SAXIGP1AWPROT,
    input [3:0] SAXIGP1AWQOS, output SAXIGP1AWREADY, input [1:0] SAXIGP1AWSIZE,
    input SAXIGP1AWVALID, output [5:0] SAXIGP1BID, input SAXIGP1BREADY,
    output [1:0] SAXIGP1BRESP, output SAXIGP1BVALID, output [31:0] SAXIGP1RDATA,
    output [5:0] SAXIGP1RID, output SAXIGP1RLAST, input SAXIGP1RREADY,
    output [1:0] SAXIGP1RRESP, output SAXIGP1RVALID, input [31:0] SAXIGP1WDATA,
    input [5:0] SAXIGP1WID, input SAXIGP1WLAST, output SAXIGP1WREADY, input [3:0] SAXIGP1WSTRB,
    input SAXIGP1WVALID, input SAXIHP0ACLK, input [31:0] SAXIHP0ARADDR,
    input [1:0] SAXIHP0ARBURST, input [3:0] SAXIHP0ARCACHE, output SAXIHP0ARESETN,
    input [5:0] SAXIHP0ARID, input [3:0] SAXIHP0ARLEN, input [1:0] SAXIHP0ARLOCK,
    input [2:0] SAXIHP0ARPROT, input [3:0] SAXIHP0ARQOS, output SAXIHP0ARREADY,
    input [1:0] SAXIHP0ARSIZE, input SAXIHP0ARVALID, input [31:0] SAXIHP0AWADDR,
    input [1:0] SAXIHP0AWBURST, input [3:0] SAXIHP0AWCACHE, input [5:0] SAXIHP0AWID,
    input [3:0] SAXIHP0AWLEN, input [1:0] SAXIHP0AWLOCK, input [2:0] SAXIHP0AWPROT,
    input [3:0] SAXIHP0AWQOS, output SAXIHP0AWREADY, input [1:0] SAXIHP0AWSIZE,
    input SAXIHP0AWVALID, output [5:0] SAXIHP0BID, input SAXIHP0BREADY,
    output [1:0] SAXIHP0BRESP, output SAXIHP0BVALID, output [2:0] SAXIHP0RACOUNT,
    output [7:0] SAXIHP0RCOUNT, output [63:0] SAXIHP0RDATA, input SAXIHP0RDISSUECAP1EN,
    output [5:0] SAXIHP0RID, output SAXIHP0RLAST, input SAXIHP0RREADY,
    output [1:0] SAXIHP0RRESP, output SAXIHP0RVALID, output [5:0] SAXIHP0WACOUNT,
    output [7:0] SAXIHP0WCOUNT, input [63:0] SAXIHP0WDATA, input [5:0] SAXIHP0WID,
    input SAXIHP0WLAST, output SAXIHP0WREADY, input SAXIHP0WRISSUECAP1EN,
    input [7:0] SAXIHP0WSTRB, input SAXIHP0WVALID, input SAXIHP1ACLK,
    input [31:0] SAXIHP1ARADDR, input [1:0] SAXIHP1ARBURST, input [3:0] SAXIHP1ARCACHE,
    output SAXIHP1ARESETN, input [5:0] SAXIHP1ARID, input [3:0] SAXIHP1ARLEN,
    input [1:0] SAXIHP1ARLOCK, input [2:0] SAXIHP1ARPROT, input [3:0] SAXIHP1ARQOS,
    output SAXIHP1ARREADY, input [1:0] SAXIHP1ARSIZE, input SAXIHP1ARVALID,
    input [31:0] SAXIHP1AWADDR, input [1:0] SAXIHP1AWBURST, input [3:0] SAXIHP1AWCACHE,
    input [5:0] SAXIHP1AWID, input [3:0] SAXIHP1AWLEN, input [1:0] SAXIHP1AWLOCK,
    input [2:0] SAXIHP1AWPROT, input [3:0] SAXIHP1AWQOS, output SAXIHP1AWREADY,
    input [1:0] SAXIHP1AWSIZE, input SAXIHP1AWVALID, output [5:0] SAXIHP1BID,
    input SAXIHP1BREADY, output [1:0] SAXIHP1BRESP, output SAXIHP1BVALID,
    output [2:0] SAXIHP1RACOUNT, output [7:0] SAXIHP1RCOUNT, output [63:0] SAXIHP1RDATA,
    input SAXIHP1RDISSUECAP1EN, output [5:0] SAXIHP1RID, output SAXIHP1RLAST,
    input SAXIHP1RREADY, output [1:0] SAXIHP1RRESP, output SAXIHP1RVALID,
    output [5:0] SAXIHP1WACOUNT, output [7:0] SAXIHP1WCOUNT, input [63:0] SAXIHP1WDATA,
    input [5:0] SAXIHP1WID, input SAXIHP1WLAST, output SAXIHP1WREADY,
    input SAXIHP1WRISSUECAP1EN, input [7:0] SAXIHP1WSTRB, input SAXIHP1WVALID,
    input SAXIHP2ACLK, input [31:0] SAXIHP2ARADDR, input [1:0] SAXIHP2ARBURST,
    input [3:0] SAXIHP2ARCACHE, output SAXIHP2ARESETN, input [5:0] SAXIHP2ARID,
    input [3:0] SAXIHP2ARLEN, input [1:0] SAXIHP2ARLOCK, input [2:0] SAXIHP2ARPROT,
    input [3:0] SAXIHP2ARQOS, output SAXIHP2ARREADY, input [1:0] SAXIHP2ARSIZE,
    input SAXIHP2ARVALID, input [31:0] SAXIHP2AWADDR, input [1:0] SAXIHP2AWBURST,
    input [3:0] SAXIHP2AWCACHE, input [5:0] SAXIHP2AWID, input [3:0] SAXIHP2AWLEN,
    input [1:0] SAXIHP2AWLOCK, input [2:0] SAXIHP2AWPROT, input [3:0] SAXIHP2AWQOS,
    output SAXIHP2AWREADY, input [1:0] SAXIHP2AWSIZE, input SAXIHP2AWVALID,
    output [5:0] SAXIHP2BID, input SAXIHP2BREADY, output [1:0] SAXIHP2BRESP,
    output SAXIHP2BVALID, output [2:0] SAXIHP2RACOUNT, output [7:0] SAXIHP2RCOUNT,
    output [63:0] SAXIHP2RDATA, input SAXIHP2RDISSUECAP1EN, output [5:0] SAXIHP2RID,
    output SAXIHP2RLAST, input SAXIHP2RREADY, output [1:0] SAXIHP2RRESP, output SAXIHP2RVALID,
    output [5:0] SAXIHP2WACOUNT, output [7:0] SAXIHP2WCOUNT, input [63:0] SAXIHP2WDATA,
    input [5:0] SAXIHP2WID, input SAXIHP2WLAST, output SAXIHP2WREADY,
    input SAXIHP2WRISSUECAP1EN, input [7:0] SAXIHP2WSTRB, input SAXIHP2WVALID,
    input SAXIHP3ACLK, input [31:0] SAXIHP3ARADDR, input [1:0] SAXIHP3ARBURST,
    input [3:0] SAXIHP3ARCACHE, output SAXIHP3ARESETN, input [5:0] SAXIHP3ARID,
    input [3:0] SAXIHP3ARLEN, input [1:0] SAXIHP3ARLOCK, input [2:0] SAXIHP3ARPROT,
    input [3:0] SAXIHP3ARQOS, output SAXIHP3ARREADY, input [1:0] SAXIHP3ARSIZE,
    input SAXIHP3ARVALID, input [31:0] SAXIHP3AWADDR, input [1:0] SAXIHP3AWBURST,
    input [3:0] SAXIHP3AWCACHE, input [5:0] SAXIHP3AWID, input [3:0] SAXIHP3AWLEN,
    input [1:0] SAXIHP3AWLOCK, input [2:0] SAXIHP3AWPROT, input [3:0] SAXIHP3AWQOS,
    output SAXIHP3AWREADY, input [1:0] SAXIHP3AWSIZE, input SAXIHP3AWVALID,
    output [5:0] SAXIHP3BID, input SAXIHP3BREADY, output [1:0] SAXIHP3BRESP,
    output SAXIHP3BVALID, output [2:0] SAXIHP3RACOUNT, output [7:0] SAXIHP3RCOUNT,
    output [63:0] SAXIHP3RDATA, input SAXIHP3RDISSUECAP1EN, output [5:0] SAXIHP3RID,
    output SAXIHP3RLAST, input SAXIHP3RREADY, output [1:0] SAXIHP3RRESP, output SAXIHP3RVALID,
    output [5:0] SAXIHP3WACOUNT, output [7:0] SAXIHP3WCOUNT, input [63:0] SAXIHP3WDATA,
    input [5:0] SAXIHP3WID, input SAXIHP3WLAST, output SAXIHP3WREADY,
    input SAXIHP3WRISSUECAP1EN, input [7:0] SAXIHP3WSTRB, input SAXIHP3WVALID,
    input TESTA9MBISTDATAIN, input TESTA9MBISTDSHIFT, input TESTA9MBISTENABLEN,
    input TESTA9MBISTRESET, output [5:0] TESTA9MBISTRESULT, input TESTA9MBISTRUN,
    input TESTA9MBISTSHIFT, input TESTAMUXENABLEB, input [4:0] TESTBGAMUXSEL,
    input TESTBGPOWERDOWN, input TESTBSCENN, input TESTDFTRAMBYPN, output [20:0] TESTDIVCLKOUT,
    input TESTDIVCLKOUTPREOPCGENABLEN, input TESTDIVIDERRESETN, input TESTDIVIDERUPDATETOG,
    input TESTEDTBYPASS, input [6:0] TESTEDTCHANNELSIN, output [6:0] TESTEDTCHANNELSOUT,
    input TESTEDTCLOCK, input TESTEDTUPDATE, output TESTMBISTCOMPSTAT, input TESTMBISTMODEN,
    input TESTMBISTTAPTCK, input TESTMBISTTAPTDI, output TESTMBISTTAPTDO,
    output TESTMBISTTAPTDOENABLE, input TESTMBISTTAPTMS, input TESTMBISTTAPTRST,
    output [2:0] TESTPLLCLKOUT, output [2:0] TESTPLLCONFIGREADY,
    input [2:0] TESTPLLCONFIGUPDATE, input [2:0] TESTPLLFBTESTN,
    output [2:0] TESTPLLFEEDBACKDIV, output [2:0] TESTPLLLOCK, output [2:0] TESTPLLNEWCLK,
    input TESTPLLPOWERDOWNN, input TESTPLLREFCLKCPU, input TESTPLLREFCLKDDR,
    input [2:0] TESTPLLREFCLKENN, input TESTPLLREFCLKIOU, input TESTPLLRESET,
    input TESTPSSCLOCKDR, input TESTPSSEXTEST, input TESTPSSEXTESTSMPL, input TESTPSSINTEST,
    input TESTPSSRESETTAPB, input TESTPSSSHIFTDR, input TESTPSSTDI, output TESTPSSTDO,
    input TESTPSSUPDATEDR, input TESTRESETMUXN, input TESTSCANCLOCKCLOCKGEN,
    input [23:0] TESTSCANCLOCKOPCG, input [4:0] TESTSCANCLOCKPAD,
    input TESTSCANENABLEATSPEEDNONSCANFLOPSN, input TESTSCANENABLEN,
    input TESTSCANMODEATSPEEDN, input [23:0] TESTSCANMODEATSPEEDOPCGN, input TESTSCANMODEN,
    input TESTSCANRESETN, input TESTSLCRCONFIGCLOCK, input TESTSLCRCONFIGIN,
    output TESTSLCRCONFIGOUT, input TESTSLCRCONFIGRESETN, input [6:0] TESTSPAREIN,
    output [6:0] TESTSPAREOUT, input TESTTRIGGEROPCGN
);
endmodule

// A clock management tile's PLL or MMCM asked for by ratio, as the DE2's
// CY2_PLL (ADR-0008): the input period in picoseconds, per output
// f_in * MULTIPLY_BY / DIVIDE_BY, a phase shift in picoseconds (negative:
// earlier) and a duty cycle in percent. COMPENSATION "ZHOLD" feeds back
// through a BUFG, "INTERNAL" inside the tile (`crate::cmt`).
(* blackbox *) module XC7_PLL (input INCLK, input ARESET, output CLK0, CLK1, CLK2, CLK3, CLK4, CLK5, output LOCKED);
  parameter INCLK0_INPUT_FREQUENCY = 8000;
  parameter COMPENSATION = "ZHOLD";
  parameter CLK0_MULTIPLY_BY = 1;
  parameter CLK0_DIVIDE_BY = 1;
  parameter CLK0_PHASE_SHIFT = 0;
  parameter CLK0_DUTY_CYCLE = 50;
  parameter CLK1_MULTIPLY_BY = 1;
  parameter CLK1_DIVIDE_BY = 1;
  parameter CLK1_PHASE_SHIFT = 0;
  parameter CLK1_DUTY_CYCLE = 50;
  parameter CLK2_MULTIPLY_BY = 1;
  parameter CLK2_DIVIDE_BY = 1;
  parameter CLK2_PHASE_SHIFT = 0;
  parameter CLK2_DUTY_CYCLE = 50;
  parameter CLK3_MULTIPLY_BY = 1;
  parameter CLK3_DIVIDE_BY = 1;
  parameter CLK3_PHASE_SHIFT = 0;
  parameter CLK3_DUTY_CYCLE = 50;
  parameter CLK4_MULTIPLY_BY = 1;
  parameter CLK4_DIVIDE_BY = 1;
  parameter CLK4_PHASE_SHIFT = 0;
  parameter CLK4_DUTY_CYCLE = 50;
  parameter CLK5_MULTIPLY_BY = 1;
  parameter CLK5_DIVIDE_BY = 1;
  parameter CLK5_PHASE_SHIFT = 0;
  parameter CLK5_DUTY_CYCLE = 50;
endmodule
(* blackbox *) module XC7_MMCM (input INCLK, input ARESET, output CLK0, CLK1, CLK2, CLK3, CLK4, CLK5, CLK6, output LOCKED);
  parameter INCLK0_INPUT_FREQUENCY = 8000;
  parameter COMPENSATION = "ZHOLD";
  parameter CLK0_MULTIPLY_BY = 1;
  parameter CLK0_DIVIDE_BY = 1;
  parameter CLK0_PHASE_SHIFT = 0;
  parameter CLK0_DUTY_CYCLE = 50;
  parameter CLK1_MULTIPLY_BY = 1;
  parameter CLK1_DIVIDE_BY = 1;
  parameter CLK1_PHASE_SHIFT = 0;
  parameter CLK1_DUTY_CYCLE = 50;
  parameter CLK2_MULTIPLY_BY = 1;
  parameter CLK2_DIVIDE_BY = 1;
  parameter CLK2_PHASE_SHIFT = 0;
  parameter CLK2_DUTY_CYCLE = 50;
  parameter CLK3_MULTIPLY_BY = 1;
  parameter CLK3_DIVIDE_BY = 1;
  parameter CLK3_PHASE_SHIFT = 0;
  parameter CLK3_DUTY_CYCLE = 50;
  parameter CLK4_MULTIPLY_BY = 1;
  parameter CLK4_DIVIDE_BY = 1;
  parameter CLK4_PHASE_SHIFT = 0;
  parameter CLK4_DUTY_CYCLE = 50;
  parameter CLK5_MULTIPLY_BY = 1;
  parameter CLK5_DIVIDE_BY = 1;
  parameter CLK5_PHASE_SHIFT = 0;
  parameter CLK5_DUTY_CYCLE = 50;
  parameter CLK6_MULTIPLY_BY = 1;
  parameter CLK6_DIVIDE_BY = 1;
  parameter CLK6_PHASE_SHIFT = 0;
  parameter CLK6_DUTY_CYCLE = 50;
endmodule

// What synthesis makes of them (`crate::cmt::Config::params`), and what the
// decoder reads back: the pins chipsmith connects.
(* blackbox *) module PLLE2_ADV (input CLKIN1, input CLKFBIN, input RST, output CLKFBOUT,
                                 output CLKOUT0, CLKOUT1, CLKOUT2, CLKOUT3, CLKOUT4, CLKOUT5,
                                 output LOCKED);
endmodule
(* blackbox *) module MMCME2_ADV (input CLKIN1, input CLKFBIN, input RST, output CLKFBOUT,
                                  output CLKOUT0, CLKOUT1, CLKOUT2, CLKOUT3, CLKOUT4, CLKOUT5,
                                  CLKOUT6, output LOCKED);
endmodule
