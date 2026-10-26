// The Cyclone II device cells chipsmith's synthesis ends in and its packer
// reads (cells.rs builds them), declared for the Verilog front end: a design
// may instantiate the hard blocks (CY2_JTAG, CY2_ASMI) or any other of them
// itself. Read as a library, so only ports and parameters count.
//
// Behaviour follows Quartus' cycloneii_lcell_comb/_ff: COMBOUT is a LUT4 over
// (DATAA, DATAB, C, DATAD) with C = CIN when CIN is connected, else DATAC;
// COUT is the same LUT with C = CIN and DATAD = 0. Unconnected LUT inputs
// read 1, as Quartus ties them to vcc; synthesis ties unused inputs to 1.

(* blackbox *)
module LCELL_COMB (input DATAA, input DATAB, input DATAC, input DATAD, input CIN,
                   output COMBOUT, output COUT);
  parameter [15:0] LUT_MASK = 16'hFFFF;
endmodule

// ENA gates SCLR (only an enabled clock edge clears); ACLR is asynchronous.
// Unconnected (synthesis ties them so): ENA 1, ACLR and SCLR 0.
(* blackbox *)
module LCELL_FF (input DATAIN, input CLK, input ENA, input ACLR, input SCLR, output REGOUT);
endmodule

(* blackbox *)
module CY2_IBUF (input PAD, output COMBOUT);
endmodule

(* blackbox *)
module CY2_OBUF (input DATAOUT, output PAD);
endmodule

// Tri-state output or bidir: PAD = OE ? DATAOUT : 1'bz, COMBOUT = PAD.
(* blackbox *)
module CY2_IOBUF (input DATAOUT, input OE, output COMBOUT, inout PAD);
endmodule

// M4K RAM block (synth/m4k.rs builds it from a memory; crate::m4k is the
// reference). MODE "SINGLE": port A reads and writes. "SIMPLE_DUAL": port A
// writes, port B reads (a read of the word being written returns the old
// data). "TRUE_DUAL": both read and write. A port whose write enable is
// never driven doesn't write (a ROM). WIDTH, B_WIDTH: the ports' physical
// widths, 1, 2, 4, 9, 18 or 36; an n-bit address uses bits 12 - n ..= 11.
// Port A's input registers are on clock 0 (CLK), port B's on B_CLOCK; each
// clock k is gated by ENAk. A_OUTREG/B_OUTREG: "NONE", "CLOCK0", "CLOCK1"
// register the read data, cleared asynchronously by A_OUTCLR/B_OUTCLR
// ("NONE", "CLEAR0", "CLEAR1"). Data in and out are by physical column
// (port B's read data from DATAOUT[18] in true dual port); BASE, SLICE and
// PACKING say which columns hold data (crate::m4k::Shape). INIT: the 4,608
// content bits in physical order.
(* blackbox *)
module CY2_M4K (input CLK, input CLK1, input ENA0, input ENA1, input CLR0, input CLR1,
                input [11:0] A_ADDR, input [35:0] A_DATAIN, input A_WE, input [3:0] A_BYTEENA,
                input [11:0] B_ADDR, input [17:0] B_DATAIN, input B_WE, input [1:0] B_BYTEENA,
                output [35:0] DATAOUT);
  parameter MODE = "SINGLE";
  parameter WIDTH = 1;
  parameter B_WIDTH = 1;
  parameter B_CLOCK = 0;
  parameter A_OUTREG = "NONE";
  parameter A_OUTCLR = "NONE";
  parameter B_OUTREG = "NONE";
  parameter B_OUTCLR = "NONE";
  parameter BASE = 1;
  parameter SLICE = 1;
  parameter PACKING = "DENSE";
  parameter [4607:0] INIT = 4608'b0;
endmodule

// The JTAG block (one per device): TAP signals out to the core during a
// user instruction (USER0 IR 0x00c, USER1 0x00e), TDOUSER shifted out on TDO.
// cycloneii_jtag minus the pin ports, which the device wires itself.
(* blackbox *)
module CY2_JTAG (input TDOUSER, output TMSUTAP, output TCKUTAP, output TDIUTAP,
                 output SHIFTUSER, output CLKDRUSER, output UPDATEUSER,
                 output RUNIDLEUSER, output USR1USER);
endmodule

// The serial-flash block (one per device): drives DCLK, nCSO and ASDO of
// the configuration flash from the core and returns DATA0. Always driving
// (cycloneii_asmiblock with oe tied low, the only mode modelled).
(* blackbox *)
module CY2_ASMI (input DCLKIN, input SCEIN, input SDOIN, output DATA0OUT);
endmodule
