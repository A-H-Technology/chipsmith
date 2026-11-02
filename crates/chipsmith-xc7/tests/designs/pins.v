// A tri-state output, a bidirectional bus read back, an output always
// released and one tied to a constant.
module pins(input clk, input oe, input [3:0] d, output [3:0] q, output [3:0] t,
            inout [3:0] bus, output z, output k);
  reg [3:0] r;
  always @(posedge clk) r <= bus ^ d;
  assign bus = oe ? r : 4'bz;
  assign t = oe ? d : 4'bz;
  assign q = r;
  assign z = 1'bz;
  assign k = 1'b1;
endmodule
