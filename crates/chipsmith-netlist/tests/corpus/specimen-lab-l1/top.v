module top(input CLOCK_50, input CLOCK_27, input [17:0] SW, input [3:0] KEY, output [17:0] LEDR);
  wire c3; wire co1;
  cycloneii_lcell_comb #(.lut_mask(16'h06E8), .sum_lutc_input("cin")) l2 (
    .dataa(SW[13]), .datab(SW[13]), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(c3), .cout(co1));
  wire c6; wire co4;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l5 (
    .dataa(SW[15]), .datab(SW[12]), .datac(1'b1), .datad(1'b1), .cin(co1), .combout(c6), .cout(co4));
  wire q7;
  cycloneii_lcell_ff f8 (.datain(c6), .clk(CLOCK_50), .ena(SW[0]), .regout(q7));
  wire c10;
  cycloneii_lcell_comb #(.lut_mask(16'h8035), .sum_lutc_input("datac")) l9 (
    .dataa(KEY[2]), .datab(SW[10]), .datac(SW[14]), .datad(c3), .cin(1'b0), .combout(c10));
  wire q11;
  cycloneii_lcell_ff f12 (.datain(c10), .clk(CLOCK_50), .ena(SW[0]), .sclr(KEY[0]), .regout(q11));
  wire c14;
  cycloneii_lcell_comb #(.lut_mask(16'hBE66), .sum_lutc_input("datac")) l13 (
    .dataa(q11), .datab(SW[16]), .datac(q7), .datad(SW[4]), .cin(1'b0), .combout(c14));
  wire c17; wire co15;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l16 (
    .dataa(q11), .datab(SW[11]), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(c17), .cout(co15));
  wire q18;
  cycloneii_lcell_ff f19 (.datain(c17), .clk(CLOCK_50), .ena(SW[0]), .regout(q18));
  wire c22; wire co20;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l21 (
    .dataa(SW[9]), .datab(SW[15]), .datac(1'b1), .datad(1'b1), .cin(co15), .combout(c22), .cout(co20));
  wire q23;
  cycloneii_lcell_ff f24 (.datain(c22), .clk(CLOCK_50), .ena(SW[0]), .sclr(KEY[0]), .regout(q23));
  wire c27; wire co25;
  cycloneii_lcell_comb #(.lut_mask(16'hD6E8), .sum_lutc_input("cin")) l26 (
    .dataa(q11), .datab(KEY[1]), .datac(1'b1), .datad(1'b1), .cin(co20), .combout(c27), .cout(co25));
  wire q28;
  cycloneii_lcell_ff f29 (.datain(c27), .clk(CLOCK_50), .ena(SW[0]), .regout(q28));
  wire c32; wire co30;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l31 (
    .dataa(SW[6]), .datab(KEY[1]), .datac(1'b1), .datad(1'b1), .cin(co25), .combout(c32), .cout(co30));
  wire c35; wire co33;
  cycloneii_lcell_comb #(.lut_mask(16'h46E8), .sum_lutc_input("cin")) l34 (
    .dataa(q28), .datab(SW[16]), .datac(1'b1), .datad(1'b1), .cin(co30), .combout(c35), .cout(co33));
  wire c37;
  cycloneii_lcell_comb #(.lut_mask(16'h69D5), .sum_lutc_input("datac")) l36 (
    .dataa(SW[17]), .datab(SW[15]), .datac(KEY[0]), .datad(q18), .cin(1'b0), .combout(c37));
  wire q38;
  cycloneii_lcell_ff f39 (.datain(c37), .clk(CLOCK_50), .ena(SW[0]), .regout(q38));
  wire c41;
  cycloneii_lcell_comb #(.lut_mask(16'hDC6C), .sum_lutc_input("datac")) l40 (
    .dataa(c37), .datab(c32), .datac(SW[5]), .datad(SW[11]), .cin(1'b0), .combout(c41));
  wire c43;
  cycloneii_lcell_comb #(.lut_mask(16'h3FE4), .sum_lutc_input("datac")) l42 (
    .dataa(SW[8]), .datab(SW[2]), .datac(SW[1]), .datad(q23), .cin(1'b0), .combout(c43));
  wire c45;
  cycloneii_lcell_comb #(.lut_mask(16'hA81B), .sum_lutc_input("datac")) l44 (
    .dataa(SW[11]), .datab(SW[7]), .datac(c14), .datad(KEY[3]), .cin(1'b0), .combout(c45));
  wire q46;
  cycloneii_lcell_ff f47 (.datain(c45), .clk(CLOCK_50), .ena(SW[0]), .sclr(KEY[0]), .regout(q46));
  wire q48;
  cycloneii_lcell_ff f49 (.datain(SW[3]), .clk(CLOCK_50), .ena(SW[0]), .sclr(KEY[0]), .regout(q48));
  wire q50;
  cycloneii_lcell_ff f51 (.datain(q48), .clk(CLOCK_27), .ena(c45), .sclr(SW[1]), .sload(c35), .sdata(q46), .regout(q50));
  wire c53;
  cycloneii_lcell_comb #(.lut_mask(16'h202D), .sum_lutc_input("datac")) l52 (
    .dataa(c14), .datab(KEY[2]), .datac(q23), .datad(c43), .cin(1'b0), .combout(c53));
  wire c56; wire co54;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l55 (
    .dataa(q50), .datab(q18), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(c56), .cout(co54));
  wire q57;
  cycloneii_lcell_ff f58 (.datain(c56), .clk(CLOCK_27), .ena(c45), .sclr(SW[1]), .sload(c35), .sdata(SW[2]), .regout(q57));
  wire c61; wire co59;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l60 (
    .dataa(q57), .datab(c53), .datac(1'b1), .datad(1'b1), .cin(co54), .combout(c61), .cout(co59));
  wire q62;
  cycloneii_lcell_ff f63 (.datain(c61), .clk(CLOCK_27), .ena(c45), .sclr(SW[1]), .sload(c35), .sdata(q38), .regout(q62));
  wire c66; wire co64;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l65 (
    .dataa(q48), .datab(c41), .datac(1'b1), .datad(1'b1), .cin(co59), .combout(c66), .cout(co64));
  wire c69; wire co67;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l68 (
    .dataa(q62), .datab(c66), .datac(1'b1), .datad(1'b1), .cin(co64), .combout(c69), .cout(co67));
  wire c71;
  cycloneii_lcell_comb #(.lut_mask(16'h6DE3), .sum_lutc_input("datac")) l70 (
    .dataa(c69), .datab(c45), .datac(SW[8]), .datad(KEY[3]), .cin(1'b0), .combout(c71));
  wire c73;
  cycloneii_lcell_comb #(.lut_mask(16'h10B9), .sum_lutc_input("datac")) l72 (
    .dataa(q11), .datab(c3), .datac(c69), .datad(c71), .cin(1'b0), .combout(c73));
  wire q74;
  cycloneii_lcell_ff f75 (.datain(c73), .clk(CLOCK_27), .ena(c45), .regout(q74));
  wire c77;
  cycloneii_lcell_comb #(.lut_mask(16'h571D), .sum_lutc_input("datac")) l76 (
    .dataa(q50), .datab(q74), .datac(q7), .datad(KEY[3]), .cin(1'b0), .combout(c77));
  wire c79;
  cycloneii_lcell_comb #(.lut_mask(16'hC530), .sum_lutc_input("datac")) l78 (
    .dataa(c77), .datab(SW[8]), .datac(q48), .datad(q46), .cin(1'b0), .combout(c79));
  wire c81;
  cycloneii_lcell_comb #(.lut_mask(16'h5746), .sum_lutc_input("datac")) l80 (
    .dataa(c79), .datab(q11), .datac(SW[9]), .datad(SW[8]), .cin(1'b0), .combout(c81));
  wire c83;
  cycloneii_lcell_comb #(.lut_mask(16'h3946), .sum_lutc_input("datac")) l82 (
    .dataa(c81), .datab(SW[4]), .datac(c45), .datad(q46), .cin(1'b0), .combout(c83));
  wire c85;
  cycloneii_lcell_comb #(.lut_mask(16'hE593), .sum_lutc_input("datac")) l84 (
    .dataa(q7), .datab(c45), .datac(c83), .datad(c32), .cin(1'b0), .combout(c85));
  wire q86;
  cycloneii_lcell_ff f87 (.datain(c85), .clk(CLOCK_27), .ena(c45), .sclr(SW[1]), .sload(c35), .sdata(c83), .regout(q86));
  wire c89;
  cycloneii_lcell_comb #(.lut_mask(16'hC918), .sum_lutc_input("datac")) l88 (
    .dataa(SW[0]), .datab(q86), .datac(c83), .datad(c85), .cin(1'b0), .combout(c89));
  wire q90;
  cycloneii_lcell_ff f91 (.datain(c89), .clk(CLOCK_27), .ena(c45), .sclr(SW[1]), .sload(c35), .sdata(c83), .regout(q90));
  wire c93;
  cycloneii_lcell_comb #(.lut_mask(16'hC970), .sum_lutc_input("datac")) l92 (
    .dataa(q90), .datab(SW[2]), .datac(SW[1]), .datad(SW[0]), .cin(1'b0), .combout(c93));
  wire q94;
  cycloneii_lcell_ff f95 (.datain(c93), .clk(CLOCK_27), .ena(q86), .regout(q94));
  wire c97;
  cycloneii_lcell_comb #(.lut_mask(16'hA96E), .sum_lutc_input("datac")) l96 (
    .dataa(q94), .datab(q50), .datac(SW[7]), .datad(c93), .cin(1'b0), .combout(c97));
  wire q98;
  cycloneii_lcell_ff f99 (.datain(c97), .clk(CLOCK_27), .ena(q86), .sload(c89), .sdata(SW[7]), .regout(q98));
  wire c101;
  cycloneii_lcell_comb #(.lut_mask(16'hFBBF), .sum_lutc_input("datac")) l100 (
    .dataa(q98), .datab(c85), .datac(SW[17]), .datad(SW[5]), .cin(1'b0), .combout(c101));
  wire q102;
  cycloneii_lcell_ff f103 (.datain(c101), .clk(CLOCK_27), .ena(q86), .sload(c89), .sdata(SW[17]), .regout(q102));
  wire c105;
  cycloneii_lcell_comb #(.lut_mask(16'h5116), .sum_lutc_input("datac")) l104 (
    .dataa(q102), .datab(SW[2]), .datac(KEY[2]), .datad(SW[11]), .cin(1'b0), .combout(c105));
  wire q106;
  cycloneii_lcell_ff f107 (.datain(c105), .clk(CLOCK_27), .ena(q86), .sload(c89), .sdata(KEY[2]), .regout(q106));
  wire q108;
  cycloneii_lcell_ff f109 (.datain(SW[5]), .clk(CLOCK_50), .ena(1'b1), .sload(c89), .sdata(c14), .regout(q108));
  wire c111;
  cycloneii_lcell_comb #(.lut_mask(16'h7E47), .sum_lutc_input("datac")) l110 (
    .dataa(q106), .datab(SW[1]), .datac(KEY[0]), .datad(c3), .cin(1'b0), .combout(c111));
  wire c113;
  cycloneii_lcell_comb #(.lut_mask(16'hFC23), .sum_lutc_input("datac")) l112 (
    .dataa(SW[6]), .datab(q108), .datac(c105), .datad(c111), .cin(1'b0), .combout(c113));
  wire q114;
  cycloneii_lcell_ff f115 (.datain(c113), .clk(CLOCK_50), .ena(1'b1), .regout(q114));
  wire c117;
  cycloneii_lcell_comb #(.lut_mask(16'hC79E), .sum_lutc_input("datac")) l116 (
    .dataa(c113), .datab(q114), .datac(KEY[0]), .datad(SW[4]), .cin(1'b0), .combout(c117));
  wire c119;
  cycloneii_lcell_comb #(.lut_mask(16'hB6FF), .sum_lutc_input("datac")) l118 (
    .dataa(q18), .datab(c117), .datac(KEY[2]), .datad(SW[6]), .cin(1'b0), .combout(c119));
  wire q120;
  cycloneii_lcell_ff f121 (.datain(c119), .clk(CLOCK_50), .ena(1'b1), .sload(c89), .sdata(KEY[2]), .regout(q120));
  wire c124; wire co122;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l123 (
    .dataa(q102), .datab(c119), .datac(1'b1), .datad(1'b1), .cin(1'b1), .combout(c124), .cout(co122));
  wire q125;
  cycloneii_lcell_ff f126 (.datain(c124), .clk(CLOCK_27), .ena(q86), .sload(c89), .sdata(q90), .regout(q125));
  wire c129; wire co127;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l128 (
    .dataa(q125), .datab(q120), .datac(1'b1), .datad(1'b1), .cin(co122), .combout(c129), .cout(co127));
  wire q130;
  cycloneii_lcell_ff f131 (.datain(c129), .clk(CLOCK_27), .ena(q86), .sload(c89), .sdata(q86), .regout(q130));
  wire c134; wire co132;
  cycloneii_lcell_comb #(.lut_mask(16'hB6E8), .sum_lutc_input("cin")) l133 (
    .dataa(q130), .datab(c3), .datac(1'b1), .datad(1'b1), .cin(co127), .combout(c134), .cout(co132));
  wire q135;
  cycloneii_lcell_ff f136 (.datain(c134), .clk(CLOCK_27), .ena(q86), .regout(q135));
  wire c139; wire co137;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l138 (
    .dataa(q135), .datab(q114), .datac(1'b1), .datad(1'b1), .cin(co132), .combout(c139), .cout(co137));
  wire q140;
  cycloneii_lcell_ff f141 (.datain(c139), .clk(CLOCK_50), .ena(1'b1), .regout(q140));
  wire c144; wire co142;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l143 (
    .dataa(q46), .datab(q28), .datac(1'b1), .datad(1'b1), .cin(co137), .combout(c144), .cout(co142));
  wire q145;
  cycloneii_lcell_ff f146 (.datain(c144), .clk(CLOCK_50), .ena(1'b1), .regout(q145));
  wire c149; wire co147;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l148 (
    .dataa(q140), .datab(q145), .datac(1'b1), .datad(1'b1), .cin(co142), .combout(c149), .cout(co147));
  wire c151;
  cycloneii_lcell_comb #(.lut_mask(16'hB838), .sum_lutc_input("datac")) l150 (
    .dataa(c37), .datab(c89), .datac(q23), .datad(q145), .cin(1'b0), .combout(c151));
  wire c154; wire co152;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l153 (
    .dataa(c71), .datab(c151), .datac(1'b1), .datad(1'b1), .cin(1'b1), .combout(c154), .cout(co152));
  wire q155;
  cycloneii_lcell_ff f156 (.datain(c154), .clk(CLOCK_27), .ena(1'b1), .regout(q155));
  wire c159; wire co157;
  cycloneii_lcell_comb #(.lut_mask(16'hC6E8), .sum_lutc_input("cin")) l158 (
    .dataa(c43), .datab(SW[10]), .datac(1'b1), .datad(1'b1), .cin(co152), .combout(c159), .cout(co157));
  wire c161;
  cycloneii_lcell_comb #(.lut_mask(16'h65B2), .sum_lutc_input("datac")) l160 (
    .dataa(SW[10]), .datab(q155), .datac(c79), .datad(q94), .cin(1'b0), .combout(c161));
  wire c163;
  cycloneii_lcell_comb #(.lut_mask(16'hE8C8), .sum_lutc_input("datac")) l162 (
    .dataa(c159), .datab(c111), .datac(SW[15]), .datad(c161), .cin(1'b0), .combout(c163));
  wire c165;
  cycloneii_lcell_comb #(.lut_mask(16'hF5B6), .sum_lutc_input("datac")) l164 (
    .dataa(c163), .datab(c79), .datac(q140), .datad(SW[0]), .cin(1'b0), .combout(c165));
  wire c167;
  cycloneii_lcell_comb #(.lut_mask(16'hD5D9), .sum_lutc_input("datac")) l166 (
    .dataa(c165), .datab(SW[3]), .datac(c161), .datad(q38), .cin(1'b0), .combout(c167));
  wire c169;
  cycloneii_lcell_comb #(.lut_mask(16'h4FA7), .sum_lutc_input("datac")) l168 (
    .dataa(c167), .datab(q130), .datac(c14), .datad(c43), .cin(1'b0), .combout(c169));
  wire q170;
  cycloneii_lcell_ff f171 (.datain(c169), .clk(CLOCK_50), .ena(c149), .regout(q170));
  wire c174; wire co172;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l173 (
    .dataa(q18), .datab(c169), .datac(1'b1), .datad(1'b1), .cin(1'b1), .combout(c174), .cout(co172));
  wire q175;
  cycloneii_lcell_ff f176 (.datain(c174), .clk(CLOCK_27), .ena(1'b1), .regout(q175));
  wire c179; wire co177;
  cycloneii_lcell_comb #(.lut_mask(16'h36E8), .sum_lutc_input("cin")) l178 (
    .dataa(q130), .datab(q170), .datac(1'b1), .datad(1'b1), .cin(co172), .combout(c179), .cout(co177));
  wire q180;
  cycloneii_lcell_ff f181 (.datain(c179), .clk(CLOCK_50), .ena(c149), .regout(q180));
  wire c184; wire co182;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l183 (
    .dataa(q180), .datab(q62), .datac(1'b1), .datad(1'b1), .cin(co177), .combout(c184), .cout(co182));
  wire q185;
  cycloneii_lcell_ff f186 (.datain(c184), .clk(CLOCK_50), .ena(c149), .regout(q185));
  wire c188;
  cycloneii_lcell_comb #(.lut_mask(16'h3434), .sum_lutc_input("datac")) l187 (
    .dataa(q185), .datab(c149), .datac(q175), .datad(c79), .cin(1'b0), .combout(c188));
  wire q189;
  cycloneii_lcell_ff f190 (.datain(c188), .clk(CLOCK_27), .ena(1'b1), .regout(q189));
  wire q191;
  cycloneii_lcell_ff f192 (.datain(c117), .clk(CLOCK_27), .ena(1'b1), .regout(q191));
  wire c194;
  cycloneii_lcell_comb #(.lut_mask(16'h3287), .sum_lutc_input("datac")) l193 (
    .dataa(c85), .datab(c53), .datac(q98), .datad(c119), .cin(1'b0), .combout(c194));
  wire q195;
  cycloneii_lcell_ff f196 (.datain(c194), .clk(CLOCK_50), .ena(c149), .regout(q195));
  wire c198;
  cycloneii_lcell_comb #(.lut_mask(16'h3F65), .sum_lutc_input("datac")) l197 (
    .dataa(c69), .datab(c151), .datac(SW[8]), .datad(q195), .cin(1'b0), .combout(c198));
  wire q199;
  cycloneii_lcell_ff f200 (.datain(c198), .clk(CLOCK_50), .ena(c149), .regout(q199));
  wire q201;
  cycloneii_lcell_ff f202 (.datain(c83), .clk(CLOCK_50), .ena(c149), .regout(q201));
  wire c204;
  cycloneii_lcell_comb #(.lut_mask(16'h11DC), .sum_lutc_input("datac")) l203 (
    .dataa(c111), .datab(q189), .datac(q191), .datad(c89), .cin(1'b0), .combout(c204));
  wire c206;
  cycloneii_lcell_comb #(.lut_mask(16'h9AE1), .sum_lutc_input("datac")) l205 (
    .dataa(SW[2]), .datab(c204), .datac(q195), .datad(q201), .cin(1'b0), .combout(c206));
  wire q207;
  cycloneii_lcell_ff f208 (.datain(c206), .clk(CLOCK_50), .ena(1'b1), .aclr(c198), .regout(q207));
  wire c210;
  cycloneii_lcell_comb #(.lut_mask(16'hA730), .sum_lutc_input("datac")) l209 (
    .dataa(q207), .datab(q199), .datac(KEY[3]), .datad(q50), .cin(1'b0), .combout(c210));
  wire q211;
  cycloneii_lcell_ff f212 (.datain(c210), .clk(CLOCK_50), .ena(1'b1), .aclr(c198), .regout(q211));
  wire c214;
  cycloneii_lcell_comb #(.lut_mask(16'h3E86), .sum_lutc_input("datac")) l213 (
    .dataa(c210), .datab(c66), .datac(c41), .datad(c198), .cin(1'b0), .combout(c214));
  wire q215;
  cycloneii_lcell_ff f216 (.datain(c214), .clk(CLOCK_50), .ena(1'b1), .regout(q215));
  wire c218;
  cycloneii_lcell_comb #(.lut_mask(16'h9B34), .sum_lutc_input("datac")) l217 (
    .dataa(q211), .datab(KEY[3]), .datac(c151), .datad(q215), .cin(1'b0), .combout(c218));
  wire c220;
  cycloneii_lcell_comb #(.lut_mask(16'h1DA8), .sum_lutc_input("datac")) l219 (
    .dataa(c214), .datab(c93), .datac(c218), .datad(c35), .cin(1'b0), .combout(c220));
  wire q221;
  cycloneii_lcell_ff f222 (.datain(c220), .clk(CLOCK_50), .ena(1'b1), .regout(q221));
  wire c224;
  cycloneii_lcell_comb #(.lut_mask(16'h3771), .sum_lutc_input("datac")) l223 (
    .dataa(q221), .datab(SW[5]), .datac(c220), .datad(q215), .cin(1'b0), .combout(c224));
  wire q225;
  cycloneii_lcell_ff f226 (.datain(c224), .clk(CLOCK_50), .ena(1'b1), .aclr(c198), .regout(q225));
  wire c229; wire co227;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l228 (
    .dataa(q225), .datab(SW[12]), .datac(1'b1), .datad(1'b1), .cin(1'b1), .combout(c229), .cout(co227));
  wire q230;
  cycloneii_lcell_ff f231 (.datain(c229), .clk(CLOCK_50), .ena(1'b1), .aclr(c198), .regout(q230));
  wire c234; wire co232;
  cycloneii_lcell_comb #(.lut_mask(16'hD6E8), .sum_lutc_input("cin")) l233 (
    .dataa(q230), .datab(q106), .datac(1'b1), .datad(1'b1), .cin(co227), .combout(c234), .cout(co232));
  wire q235;
  cycloneii_lcell_ff f236 (.datain(c234), .clk(CLOCK_50), .ena(1'b1), .aclr(c198), .regout(q235));
  wire c239; wire co237;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l238 (
    .dataa(q235), .datab(SW[10]), .datac(1'b1), .datad(1'b1), .cin(co232), .combout(c239), .cout(co237));
  wire q240;
  cycloneii_lcell_ff f241 (.datain(c239), .clk(CLOCK_50), .ena(1'b1), .regout(q240));
  wire c244; wire co242;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l243 (
    .dataa(q240), .datab(q86), .datac(1'b1), .datad(1'b1), .cin(co237), .combout(c244), .cout(co242));
  wire q245;
  cycloneii_lcell_ff f246 (.datain(c244), .clk(CLOCK_50), .ena(1'b1), .regout(q245));
  wire c249; wire co247;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l248 (
    .dataa(q130), .datab(q245), .datac(1'b1), .datad(1'b1), .cin(co242), .combout(c249), .cout(co247));
  wire q250;
  cycloneii_lcell_ff f251 (.datain(c249), .clk(CLOCK_50), .ena(1'b1), .aclr(c198), .regout(q250));
  wire c254; wire co252;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l253 (
    .dataa(q250), .datab(q221), .datac(1'b1), .datad(1'b1), .cin(1'b0), .combout(c254), .cout(co252));
  wire q255;
  cycloneii_lcell_ff f256 (.datain(c254), .clk(CLOCK_50), .ena(1'b1), .aclr(c198), .regout(q255));
  wire q257;
  cycloneii_lcell_ff f258 (.datain(q255), .clk(CLOCK_50), .ena(1'b1), .sload(c41), .sdata(c161), .regout(q257));
  wire q259;
  cycloneii_lcell_ff f260 (.datain(SW[7]), .clk(CLOCK_50), .ena(1'b1), .sload(c41), .sdata(q257), .regout(q259));
  wire c262;
  cycloneii_lcell_comb #(.lut_mask(16'h26A9), .sum_lutc_input("datac")) l261 (
    .dataa(q259), .datab(c37), .datac(c3), .datad(SW[12]), .cin(1'b0), .combout(c262));
  wire c264;
  cycloneii_lcell_comb #(.lut_mask(16'hA31B), .sum_lutc_input("datac")) l263 (
    .dataa(c262), .datab(q259), .datac(SW[5]), .datad(SW[6]), .cin(1'b0), .combout(c264));
  wire c266;
  cycloneii_lcell_comb #(.lut_mask(16'h206B), .sum_lutc_input("datac")) l265 (
    .dataa(c105), .datab(c264), .datac(q57), .datad(SW[5]), .cin(1'b0), .combout(c266));
  wire c268;
  cycloneii_lcell_comb #(.lut_mask(16'hE616), .sum_lutc_input("datac")) l267 (
    .dataa(c266), .datab(c83), .datac(c214), .datad(c198), .cin(1'b0), .combout(c268));
  wire q269;
  cycloneii_lcell_ff f270 (.datain(c268), .clk(CLOCK_50), .ena(1'b1), .sload(c41), .sdata(c214), .regout(q269));
  wire c272;
  cycloneii_lcell_comb #(.lut_mask(16'h42D2), .sum_lutc_input("datac")) l271 (
    .dataa(SW[10]), .datab(q269), .datac(SW[7]), .datad(q207), .cin(1'b0), .combout(c272));
  wire c274;
  cycloneii_lcell_comb #(.lut_mask(16'hAD84), .sum_lutc_input("datac")) l273 (
    .dataa(c272), .datab(q235), .datac(SW[14]), .datad(q259), .cin(1'b0), .combout(c274));
  wire q275;
  cycloneii_lcell_ff f276 (.datain(c274), .clk(CLOCK_50), .ena(1'b1), .sload(c41), .sdata(SW[14]), .regout(q275));
  wire c278;
  cycloneii_lcell_comb #(.lut_mask(16'hF4D0), .sum_lutc_input("datac")) l277 (
    .dataa(q275), .datab(c117), .datac(c220), .datad(c83), .cin(1'b0), .combout(c278));
  wire q279;
  cycloneii_lcell_ff f280 (.datain(c278), .clk(CLOCK_50), .ena(1'b1), .sload(c41), .sdata(c220), .regout(q279));
  wire c282;
  cycloneii_lcell_comb #(.lut_mask(16'hBE41), .sum_lutc_input("datac")) l281 (
    .dataa(c204), .datab(c151), .datac(q125), .datad(q279), .cin(1'b0), .combout(c282));
  wire q283;
  cycloneii_lcell_ff f284 (.datain(c282), .clk(CLOCK_50), .ena(1'b1), .sload(c41), .sdata(q125), .regout(q283));
  wire c286;
  cycloneii_lcell_comb #(.lut_mask(16'hCC89), .sum_lutc_input("datac")) l285 (
    .dataa(q283), .datab(c282), .datac(q50), .datad(q240), .cin(1'b0), .combout(c286));
  wire q287;
  cycloneii_lcell_ff f288 (.datain(c286), .clk(CLOCK_50), .ena(1'b1), .sload(c41), .sdata(q50), .regout(q287));
  wire c290;
  cycloneii_lcell_comb #(.lut_mask(16'h53DC), .sum_lutc_input("datac")) l289 (
    .dataa(q120), .datab(q287), .datac(c286), .datad(c220), .cin(1'b0), .combout(c290));
  wire q291;
  cycloneii_lcell_ff f292 (.datain(c290), .clk(CLOCK_50), .ena(1'b1), .sload(c41), .sdata(c286), .regout(q291));
  wire c294;
  cycloneii_lcell_comb #(.lut_mask(16'h0C7A), .sum_lutc_input("datac")) l293 (
    .dataa(c290), .datab(c282), .datac(q291), .datad(SW[17]), .cin(1'b0), .combout(c294));
  wire q295;
  cycloneii_lcell_ff f296 (.datain(c294), .clk(CLOCK_50), .ena(1'b1), .sload(c41), .sdata(q291), .regout(q295));
  wire c299; wire co297;
  cycloneii_lcell_comb #(.lut_mask(16'h96E8), .sum_lutc_input("cin")) l298 (
    .dataa(c169), .datab(q295), .datac(1'b1), .datad(1'b1), .cin(1'b1), .combout(c299), .cout(co297));
  wire q300;
  cycloneii_lcell_ff f301 (.datain(c299), .clk(CLOCK_50), .ena(1'b1), .sload(c41), .sdata(KEY[3]), .regout(q300));
  wire c304; wire co302;
  cycloneii_lcell_comb #(.lut_mask(16'hC6E8), .sum_lutc_input("cin")) l303 (
    .dataa(q145), .datab(q300), .datac(1'b1), .datad(1'b1), .cin(co297), .combout(c304), .cout(co302));
  wire q305;
  cycloneii_lcell_ff f306 (.datain(c304), .clk(CLOCK_50), .ena(1'b1), .regout(q305));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led0 (.dataa(q305), .datab(c165), .datac(c165), .datad(c165), .cin(1'b0), .combout(LEDR[0]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led1 (.dataa(SW[6]), .datab(SW[6]), .datac(SW[6]), .datad(SW[6]), .cin(1'b0), .combout(LEDR[1]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led2 (.dataa(q235), .datab(q235), .datac(q235), .datad(q235), .cin(1'b0), .combout(LEDR[2]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led3 (.dataa(SW[12]), .datab(SW[12]), .datac(SW[12]), .datad(SW[12]), .cin(1'b0), .combout(LEDR[3]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led4 (.dataa(q305), .datab(q305), .datac(q305), .datad(q305), .cin(1'b0), .combout(LEDR[4]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led5 (.dataa(q130), .datab(q130), .datac(q130), .datad(q130), .cin(1'b0), .combout(LEDR[5]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led6 (.dataa(q191), .datab(q191), .datac(q191), .datad(q191), .cin(1'b0), .combout(LEDR[6]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led7 (.dataa(c43), .datab(c43), .datac(c43), .datad(c43), .cin(1'b0), .combout(LEDR[7]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led8 (.dataa(q201), .datab(q201), .datac(q201), .datad(q201), .cin(1'b0), .combout(LEDR[8]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led9 (.dataa(SW[13]), .datab(SW[13]), .datac(SW[13]), .datad(SW[13]), .cin(1'b0), .combout(LEDR[9]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led10 (.dataa(q18), .datab(q18), .datac(q18), .datad(q18), .cin(1'b0), .combout(LEDR[10]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led11 (.dataa(c41), .datab(c41), .datac(c41), .datad(c41), .cin(1'b0), .combout(LEDR[11]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led12 (.dataa(SW[8]), .datab(SW[8]), .datac(SW[8]), .datad(SW[8]), .cin(1'b0), .combout(LEDR[12]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led13 (.dataa(c167), .datab(c167), .datac(c167), .datad(c167), .cin(1'b0), .combout(LEDR[13]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led14 (.dataa(c149), .datab(c149), .datac(c149), .datad(c149), .cin(1'b0), .combout(LEDR[14]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led15 (.dataa(c119), .datab(c119), .datac(c119), .datad(c119), .cin(1'b0), .combout(LEDR[15]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led16 (.dataa(c169), .datab(c169), .datac(c169), .datad(c169), .cin(1'b0), .combout(LEDR[16]));
  cycloneii_lcell_comb #(.lut_mask(16'h6996), .sum_lutc_input("datac")) led17 (.dataa(SW[10]), .datab(SW[10]), .datac(SW[10]), .datad(SW[10]), .cin(1'b0), .combout(LEDR[17]));
endmodule
