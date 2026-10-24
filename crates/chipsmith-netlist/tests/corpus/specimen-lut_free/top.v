module top(input [3:0] SW, output LEDR0);
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) lut (
    .dataa(SW[0]), .datab(SW[1]), .datac(SW[2]), .datad(SW[3]), .cin(1'b0), .combout(LEDR0));
endmodule
