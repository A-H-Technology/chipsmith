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
  // 1: clocked on CLK's falling edge (the LAB inverts the clock).
  parameter CLK_INV = 0;
endmodule

// Pin buffers. Each has its I/O cell's registers: INPUT_REGISTER samples
// the pad on INCLK into REGOUT; OUTPUT_REGISTER samples DATAOUT and
// OE_REGISTER samples OE on OUTCLK, and the pad takes what they hold. A
// register is used when its parameter is "low" or "high", its power-up
// value; "none" (the default) bypasses it. <register>_ENA = 1: it samples
// only while its side's enable (INCLKENA; OUTCLKENA, which the output and
// OE registers share) is active; <register>_ARESET = 1: ARESET, which all
// three share, returns it to its power-up value. INCLK_INVERT /
// OUTCLK_INVERT clock a side on the falling edge; INCLKENA_INVERT,
// OUTCLKENA_INVERT, ARESET_INVERT make those controls active low
// (pin.rs: register_param, control_param).
(* blackbox *)
module CY2_IBUF (input PAD, input INCLK, input INCLKENA, input ARESET,
                 output COMBOUT, output REGOUT);
  parameter INPUT_REGISTER = "none";
endmodule

(* blackbox *)
module CY2_OBUF (input DATAOUT, input OUTCLK, input OUTCLKENA, input ARESET,
                 output PAD);
  parameter OUTPUT_REGISTER = "none";
endmodule

// Tri-state output or bidir: PAD = OE ? DATAOUT : 1'bz, COMBOUT = PAD.
(* blackbox *)
module CY2_IOBUF (input DATAOUT, input OE, input INCLK, input OUTCLK,
                  input INCLKENA, input OUTCLKENA, input ARESET,
                  output COMBOUT, output REGOUT, inout PAD);
  parameter INPUT_REGISTER = "none";
  parameter OUTPUT_REGISTER = "none";
  parameter OE_REGISTER = "none";
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

// An embedded multiplier (synth/mult.rs builds it from a `$mul`):
// P = A * B over the low A_WIDTH / B_WIDTH bits, each operand two's
// complement when its *_SIGNED parameter is 1; P's low A_WIDTH+B_WIDTH
// bits are the product. Widths 1 to 18; one block of the 35. With A_REG
// or B_REG the operand is registered on CLK's rising edge, with P_REG the
// product is; the registers share CLK, the enable (active high, on when
// unconnected) and the asynchronous clear (active high, to 0).
(* blackbox *)
module CY2_MULT (input [17:0] A, input [17:0] B, input CLK, input ENA, input ACLR,
                 output [35:0] P);
  parameter A_WIDTH = 18;
  parameter B_WIDTH = 18;
  parameter A_SIGNED = 0;
  parameter B_SIGNED = 0;
  parameter A_REG = 0;
  parameter B_REG = 0;
  parameter P_REG = 0;
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

// A PLL (four per device, crate::pll): clocks CLK0-CLK2 at
// f(INCLK) * CLKk_MULTIPLY_BY / CLKk_DIVIDE_BY, as altpll names it. INCLK
// must come straight from a dedicated clock pin (CLOCK_50 feeds PLL_1).
// INCLK0_INPUT_FREQUENCY is its period in ps; CLKk_PHASE_SHIFT is in ps,
// CLKk_DUTY_CYCLE in percent. OPERATION_MODE "NORMAL" compensates the
// global clock network's delay, "NO_COMPENSATION" doesn't. LOCKED goes high
// once the loop is locked; ARESET (active high) resets it, PFDENA (active
// high, default on) lets its phase detector run.
(* blackbox *)
module CY2_PLL (input INCLK, input ARESET, input PFDENA, output CLK0, output CLK1, output CLK2,
                output LOCKED);
  parameter INCLK0_INPUT_FREQUENCY = 20000;
  parameter OPERATION_MODE = "NORMAL";
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
endmodule
