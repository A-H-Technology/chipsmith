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

(* blackbox *)
module BSCANE2 (output CAPTURE, output DRCK, output RESET, output RUNTEST,
                output SEL, output SHIFT, output TCK, output TDI, output TMS,
                output UPDATE, input TDO);
  parameter DISABLE_JTAG = "FALSE";
  parameter integer JTAG_CHAIN = 1;
endmodule
