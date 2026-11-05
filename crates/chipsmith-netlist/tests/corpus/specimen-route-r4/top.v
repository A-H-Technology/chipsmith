module top(input [5:0] SW, output [5:0] LEDR);
  (* keep *) wire w0_0;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c0_0 (
    .dataa(SW[0]), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w0_0));
  (* keep *) wire w0_1;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c0_1 (
    .dataa(w0_0), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w0_1));
  (* keep *) wire w0_2;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c0_2 (
    .dataa(w0_1), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w0_2));
  (* keep *) wire w0_3;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c0_3 (
    .dataa(w0_2), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w0_3));
  (* keep *) wire w0_4;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c0_4 (
    .dataa(w0_3), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w0_4));
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c0_5 (
    .dataa(w0_4), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(LEDR[0]));
  (* keep *) wire w1_0;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c1_0 (
    .dataa(SW[1]), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w1_0));
  (* keep *) wire w1_1;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c1_1 (
    .dataa(w1_0), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w1_1));
  (* keep *) wire w1_2;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c1_2 (
    .dataa(w1_1), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w1_2));
  (* keep *) wire w1_3;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c1_3 (
    .dataa(w1_2), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w1_3));
  (* keep *) wire w1_4;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c1_4 (
    .dataa(w1_3), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w1_4));
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c1_5 (
    .dataa(w1_4), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(LEDR[1]));
  (* keep *) wire w2_0;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c2_0 (
    .dataa(SW[2]), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w2_0));
  (* keep *) wire w2_1;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c2_1 (
    .dataa(w2_0), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w2_1));
  (* keep *) wire w2_2;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c2_2 (
    .dataa(w2_1), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w2_2));
  (* keep *) wire w2_3;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c2_3 (
    .dataa(w2_2), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w2_3));
  (* keep *) wire w2_4;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c2_4 (
    .dataa(w2_3), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w2_4));
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c2_5 (
    .dataa(w2_4), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(LEDR[2]));
  (* keep *) wire w3_0;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c3_0 (
    .dataa(SW[3]), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w3_0));
  (* keep *) wire w3_1;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c3_1 (
    .dataa(w3_0), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w3_1));
  (* keep *) wire w3_2;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c3_2 (
    .dataa(w3_1), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w3_2));
  (* keep *) wire w3_3;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c3_3 (
    .dataa(w3_2), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w3_3));
  (* keep *) wire w3_4;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c3_4 (
    .dataa(w3_3), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w3_4));
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c3_5 (
    .dataa(w3_4), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(LEDR[3]));
  (* keep *) wire w4_0;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c4_0 (
    .dataa(SW[4]), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w4_0));
  (* keep *) wire w4_1;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c4_1 (
    .dataa(w4_0), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w4_1));
  (* keep *) wire w4_2;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c4_2 (
    .dataa(w4_1), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w4_2));
  (* keep *) wire w4_3;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c4_3 (
    .dataa(w4_2), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w4_3));
  (* keep *) wire w4_4;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c4_4 (
    .dataa(w4_3), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w4_4));
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c4_5 (
    .dataa(w4_4), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(LEDR[4]));
  (* keep *) wire w5_0;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c5_0 (
    .dataa(SW[5]), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w5_0));
  (* keep *) wire w5_1;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c5_1 (
    .dataa(w5_0), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w5_1));
  (* keep *) wire w5_2;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c5_2 (
    .dataa(w5_1), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w5_2));
  (* keep *) wire w5_3;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c5_3 (
    .dataa(w5_2), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w5_3));
  (* keep *) wire w5_4;
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c5_4 (
    .dataa(w5_3), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(w5_4));
  cycloneii_lcell_comb #(.lut_mask(16'hAAAA), .sum_lutc_input("datac")) c5_5 (
    .dataa(w5_4), .datab(1'b1), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(LEDR[5]));
endmodule
