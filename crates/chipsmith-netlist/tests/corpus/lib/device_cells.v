// Interfaces of the device primitives the corpus instantiates directly,
// for `read_verilog -lib`: Quartus' Cyclone II cells (as cyclone2-re's
// specimens use them) and the 7-series BSCANE2 (yosys' xilinx/cells_xtra.v).

(* blackbox *)
module cycloneii_lcell_comb (input dataa, input datab, input datac, input datad,
                             input cin, output combout, output cout);
  parameter lut_mask = 16'hFFFF;
  parameter sum_lutc_input = "datac";
endmodule

(* blackbox *)
module cycloneii_lcell_ff (input datain, input clk, input ena, input aclr,
                           input sclr, input sdata, input sload, output regout);
endmodule

// The embedded multiplier's two atoms (cyclone2-re's multiplier fuzz
// designs instantiate them): the multiplier proper and the output stage
// its product leaves through.
(* blackbox *)
module cycloneii_mac_mult (input [17:0] dataa, input [17:0] datab, input signa, input signb,
                           input clk, input aclr, input ena, output [35:0] dataout);
  parameter dataa_width = 18;
  parameter datab_width = 18;
  parameter dataa_clock = "none";
  parameter datab_clock = "none";
  parameter signa_clock = "none";
  parameter signb_clock = "none";
endmodule

(* blackbox *)
module cycloneii_mac_out (input [35:0] dataa, input clk, input aclr, input ena,
                          output [35:0] dataout);
  parameter dataa_width = 36;
  parameter output_clock = "none";
endmodule

(* blackbox *)
module BSCANE2 (output CAPTURE, output DRCK, output RESET, output RUNTEST,
                output SEL, output SHIFT, output TCK, output TDI, output TMS,
                output UPDATE, input TDO);
  parameter DISABLE_JTAG = "FALSE";
  parameter integer JTAG_CHAIN = 1;
endmodule
