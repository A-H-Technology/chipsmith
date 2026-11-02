// examples/picorv32 (its top renamed picorv32_top by the test) with its
// reset key released: out of power-up reset the
// core runs its loop, so the LEDs show its stores (its register file's
// contents) for the co-simulation to compare. Under random keys it would
// rarely leave reset.
module run(input CLOCK_50, output [17:0] LEDR, output [8:0] LEDG);
  picorv32_top t(.CLOCK_50(CLOCK_50), .KEY(4'b1111), .LEDR(LEDR), .LEDG(LEDG));
endmodule
