// Every RTLIL cell type chipsmith-netlist models as a typed cell, instantiated
// directly with mixed widths, signedness, polarities and reset values, so
// decoding and re-encoding each one is checked against yosys.
module top(input clk, input en, input rst, input [7:0] a, input [7:0] b, input [2:0] s,
           output [1:0] t, output [300:0] o);
  wire [4:0] w1;
  \$not #(.A_SIGNED(0), .A_WIDTH(4), .Y_WIDTH(5)) u_not (.A(a[3:0]), .Y(w1));
  wire [4:0] w2;
  \$pos #(.A_SIGNED(1), .A_WIDTH(4), .Y_WIDTH(5)) u_pos (.A(a[3:0]), .Y(w2));
  wire [4:0] w3;
  \$neg #(.A_SIGNED(0), .A_WIDTH(4), .Y_WIDTH(5)) u_neg (.A(a[3:0]), .Y(w3));
  wire [4:0] w4;
  \$reduce_and #(.A_SIGNED(1), .A_WIDTH(4), .Y_WIDTH(5)) u_reduce_and (.A(a[3:0]), .Y(w4));
  wire [4:0] w5;
  \$reduce_or #(.A_SIGNED(0), .A_WIDTH(4), .Y_WIDTH(5)) u_reduce_or (.A(a[3:0]), .Y(w5));
  wire [4:0] w6;
  \$reduce_xor #(.A_SIGNED(1), .A_WIDTH(4), .Y_WIDTH(5)) u_reduce_xor (.A(a[3:0]), .Y(w6));
  wire [4:0] w7;
  \$reduce_xnor #(.A_SIGNED(0), .A_WIDTH(4), .Y_WIDTH(5)) u_reduce_xnor (.A(a[3:0]), .Y(w7));
  wire [4:0] w8;
  \$reduce_bool #(.A_SIGNED(1), .A_WIDTH(4), .Y_WIDTH(5)) u_reduce_bool (.A(a[3:0]), .Y(w8));
  wire [4:0] w9;
  \$logic_not #(.A_SIGNED(0), .A_WIDTH(4), .Y_WIDTH(5)) u_logic_not (.A(a[3:0]), .Y(w9));
  wire [5:0] w10;
  \$and #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_and (.A(a[3:0]), .B(b[3:0]), .Y(w10));
  wire [5:0] w11;
  \$or #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_or (.A(a[3:0]), .B(b[3:0]), .Y(w11));
  wire [5:0] w12;
  \$xor #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_xor (.A(a[3:0]), .B(b[3:0]), .Y(w12));
  wire [5:0] w13;
  \$xnor #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_xnor (.A(a[3:0]), .B(b[3:0]), .Y(w13));
  wire [5:0] w14;
  \$shl #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_shl (.A(a[3:0]), .B(b[3:0]), .Y(w14));
  wire [5:0] w15;
  \$shr #(.A_SIGNED(1), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_shr (.A(a[3:0]), .B(b[3:0]), .Y(w15));
  wire [5:0] w16;
  \$sshl #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_sshl (.A(a[3:0]), .B(b[3:0]), .Y(w16));
  wire [5:0] w17;
  \$sshr #(.A_SIGNED(1), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_sshr (.A(a[3:0]), .B(b[3:0]), .Y(w17));
  wire [5:0] w18;
  \$shift #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_shift (.A(a[3:0]), .B(b[3:0]), .Y(w18));
  wire [5:0] w19;
  \$shiftx #(.A_SIGNED(0), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_shiftx (.A(a[3:0]), .B(b[3:0]), .Y(w19));
  wire [5:0] w20;
  \$logic_and #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_logic_and (.A(a[3:0]), .B(b[3:0]), .Y(w20));
  wire [5:0] w21;
  \$logic_or #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_logic_or (.A(a[3:0]), .B(b[3:0]), .Y(w21));
  wire [5:0] w22;
  \$eq #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_eq (.A(a[3:0]), .B(b[3:0]), .Y(w22));
  wire [5:0] w23;
  \$ne #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_ne (.A(a[3:0]), .B(b[3:0]), .Y(w23));
  wire [5:0] w24;
  \$eqx #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_eqx (.A(a[3:0]), .B(b[3:0]), .Y(w24));
  wire [5:0] w25;
  \$nex #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_nex (.A(a[3:0]), .B(b[3:0]), .Y(w25));
  wire [5:0] w26;
  \$lt #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_lt (.A(a[3:0]), .B(b[3:0]), .Y(w26));
  wire [5:0] w27;
  \$le #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_le (.A(a[3:0]), .B(b[3:0]), .Y(w27));
  wire [5:0] w28;
  \$ge #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_ge (.A(a[3:0]), .B(b[3:0]), .Y(w28));
  wire [5:0] w29;
  \$gt #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_gt (.A(a[3:0]), .B(b[3:0]), .Y(w29));
  wire [5:0] w30;
  \$add #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_add (.A(a[3:0]), .B(b[3:0]), .Y(w30));
  wire [5:0] w31;
  \$sub #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_sub (.A(a[3:0]), .B(b[3:0]), .Y(w31));
  wire [5:0] w32;
  \$mul #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_mul (.A(a[3:0]), .B(b[3:0]), .Y(w32));
  wire [5:0] w33;
  \$div #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_div (.A(a[3:0]), .B(b[3:0]), .Y(w33));
  wire [5:0] w34;
  \$mod #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_mod (.A(a[3:0]), .B(b[3:0]), .Y(w34));
  wire [5:0] w35;
  \$divfloor #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_divfloor (.A(a[3:0]), .B(b[3:0]), .Y(w35));
  wire [5:0] w36;
  \$modfloor #(.A_SIGNED(0), .B_SIGNED(0), .A_WIDTH(4), .B_WIDTH(4), .Y_WIDTH(6)) b_modfloor (.A(a[3:0]), .B(b[3:0]), .Y(w36));
  wire [5:0] w37;
  \$pow #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(3), .Y_WIDTH(6)) b_pow (.A(a[3:0]), .B(b[2:0]), .Y(w37));
  wire [3:0] w38;
  \$mux #(.WIDTH(4)) mux (.A(a[3:0]), .B(b[3:0]), .S(s[0]), .Y(w38));
  wire [1:0] w39;
  \$pmux #(.WIDTH(2), .S_WIDTH(3)) pmux (.A(a[1:0]), .B(b[5:0]), .S(s), .Y(w39));
  wire [1:0] w40;
  \$bmux #(.WIDTH(2), .S_WIDTH(2)) bmux (.A(a), .S(s[1:0]), .Y(w40));
  wire [7:0] w41;
  \$demux #(.WIDTH(2), .S_WIDTH(2)) demux (.A(a[1:0]), .S(s[1:0]), .Y(w41));
  \$tribuf #(.WIDTH(2)) tribuf (.A(a[1:0]), .EN(en), .Y(t));
  wire [4:0] w42;
  wire [4:0] w43;
  wire [4:0] w44;
  \$alu #(.A_SIGNED(1), .B_SIGNED(1), .A_WIDTH(4), .B_WIDTH(3), .Y_WIDTH(5)) alu (.A(a[3:0]), .B(b[2:0]), .CI(s[0]), .BI(s[1]), .X(w42), .Y(w43), .CO(w44));
  wire [0:0] w45;
  \$lut #(.WIDTH(4), .LUT(16'h6996)) lut (.A(a[3:0]), .Y(w45));
  wire [2:0] w46;
  \$ff #(.WIDTH(3)) ff_ff (.D(a[2:0]), .Q(w46));
  wire [2:0] w47;
  \$dff #(.WIDTH(3), .CLK_POLARITY(0)) ff_dff (.CLK(clk), .D(a[2:0]), .Q(w47));
  wire [2:0] w48;
  \$dffe #(.WIDTH(3), .CLK_POLARITY(1), .EN_POLARITY(0)) ff_dffe (.CLK(clk), .EN(en), .D(a[2:0]), .Q(w48));
  wire [2:0] w49;
  \$adff #(.WIDTH(3), .CLK_POLARITY(1), .ARST_POLARITY(0), .ARST_VALUE(3'b1x0)) ff_adff (.CLK(clk), .ARST(rst), .D(a[2:0]), .Q(w49));
  wire [2:0] w50;
  \$adffe #(.WIDTH(3), .CLK_POLARITY(0), .ARST_POLARITY(1), .ARST_VALUE(3'b101), .EN_POLARITY(1)) ff_adffe (.CLK(clk), .ARST(rst), .EN(en), .D(a[2:0]), .Q(w50));
  wire [2:0] w51;
  \$sdff #(.WIDTH(3), .CLK_POLARITY(1), .SRST_POLARITY(1), .SRST_VALUE(3'b011)) ff_sdff (.CLK(clk), .SRST(rst), .D(a[2:0]), .Q(w51));
  wire [2:0] w52;
  \$sdffe #(.WIDTH(3), .CLK_POLARITY(1), .SRST_POLARITY(0), .SRST_VALUE(3'b110), .EN_POLARITY(1)) ff_sdffe (.CLK(clk), .SRST(rst), .EN(en), .D(a[2:0]), .Q(w52));
  wire [2:0] w53;
  \$sdffce #(.WIDTH(3), .CLK_POLARITY(0), .SRST_POLARITY(1), .SRST_VALUE(3'b001), .EN_POLARITY(0)) ff_sdffce (.CLK(clk), .SRST(rst), .EN(en), .D(a[2:0]), .Q(w53));
  wire [2:0] w54;
  \$aldff #(.WIDTH(3), .CLK_POLARITY(1), .ALOAD_POLARITY(0)) ff_aldff (.CLK(clk), .ALOAD(rst), .AD(b[2:0]), .D(a[2:0]), .Q(w54));
  wire [2:0] w55;
  \$aldffe #(.WIDTH(3), .CLK_POLARITY(1), .ALOAD_POLARITY(1), .EN_POLARITY(1)) ff_aldffe (.CLK(clk), .ALOAD(rst), .AD(b[2:0]), .EN(en), .D(a[2:0]), .Q(w55));
  wire [2:0] w56;
  \$dffsr #(.WIDTH(3), .CLK_POLARITY(1), .SET_POLARITY(1), .CLR_POLARITY(0)) ff_dffsr (.CLK(clk), .SET(b[2:0]), .CLR(b[5:3]), .D(a[2:0]), .Q(w56));
  wire [2:0] w57;
  \$dffsre #(.WIDTH(3), .CLK_POLARITY(0), .SET_POLARITY(0), .CLR_POLARITY(1), .EN_POLARITY(1)) ff_dffsre (.CLK(clk), .SET(b[2:0]), .CLR(b[5:3]), .EN(en), .D(a[2:0]), .Q(w57));
  wire [2:0] w58;
  \$dlatch #(.WIDTH(3), .EN_POLARITY(1)) ff_dlatch (.EN(en), .D(a[2:0]), .Q(w58));
  wire [2:0] w59;
  \$adlatch #(.WIDTH(3), .EN_POLARITY(0), .ARST_POLARITY(1), .ARST_VALUE(3'b010)) ff_adlatch (.EN(en), .ARST(rst), .D(a[2:0]), .Q(w59));
  wire [2:0] w60;
  \$dlatchsr #(.WIDTH(3), .EN_POLARITY(1), .SET_POLARITY(0), .CLR_POLARITY(0)) ff_dlatchsr (.EN(en), .SET(b[2:0]), .CLR(b[5:3]), .D(a[2:0]), .Q(w60));
  wire [2:0] w61;
  \$sr #(.WIDTH(3), .SET_POLARITY(1), .CLR_POLARITY(0)) ff_sr (.SET(b[2:0]), .CLR(b[5:3]), .Q(w61));
  wire [7:0] w62;
  \$mem_v2 #(.MEMID("\\mem"), .SIZE(8), .OFFSET(0), .ABITS(3), .WIDTH(4), .INIT(32'h0123x5zx),
    .RD_PORTS(2), .RD_CLK_ENABLE(2'b10), .RD_CLK_POLARITY(2'b10), .RD_TRANSPARENCY_MASK(4'b0100),
    .RD_COLLISION_X_MASK(4'b1000), .RD_WIDE_CONTINUATION(2'b00), .RD_CE_OVER_SRST(2'b10),
    .RD_ARST_VALUE(8'b1010xxxx), .RD_SRST_VALUE(8'b0101xxxx), .RD_INIT_VALUE(8'b0011xxxx),
    .WR_PORTS(2), .WR_CLK_ENABLE(2'b11), .WR_CLK_POLARITY(2'b11), .WR_PRIORITY_MASK(4'b0100),
    .WR_WIDE_CONTINUATION(2'b00))
    mem (.RD_CLK({clk, 1'bx}), .RD_EN({en, 1'b1}), .RD_ARST({1'b0, 1'b0}), .RD_SRST({s[2], 1'b0}),
      .RD_ADDR({a[5:3], a[2:0]}), .RD_DATA(w62),
      .WR_CLK({clk, clk}), .WR_EN({b[7:4], {4{en}}}), .WR_ADDR({b[2:0], a[2:0]}), .WR_DATA({a[7:4], b[3:0]}));
  assign o = {w62, w61, w60, w59, w58, w57, w56, w55, w54, w53, w52, w51, w50, w49, w48, w47, w46, w45, w44, w43, w42, w41, w40, w39, w38, w37, w36, w35, w34, w33, w32, w31, w30, w29, w28, w27, w26, w25, w24, w23, w22, w21, w20, w19, w18, w17, w16, w15, w14, w13, w12, w11, w10, w9, w8, w7, w6, w5, w4, w3, w2, w1};
endmodule
