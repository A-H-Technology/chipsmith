//! One product per multiplier configuration class, alone in a design with
//! its operands on the design's ports: what the per-configuration proofs
//! check (the mapping of a `$mul` onto blocks, against the RTL's `$mul`).
//! Operands are registered first so the proof has a clock (a product of
//! bare inputs is a one-cycle combinational check, which `equiv_induct`
//! does not do). The operand registers are read by an output too and the
//! product isn't registered, so no register moves into a block: a block's
//! register has no RTL net to be matched to, and a proof through it is
//! SAT on the whole product (a 9 x 9 takes minutes, 12 x 7 didn't finish
//! in 40). Moving registers in is the `reg_*` shapes' (9 x 9).

/// (name, Verilog, blocks it maps to).
pub fn shapes() -> Vec<(&'static str, String, usize)> {
    let mut v = Vec::new();
    // (name, a width, b width, signed, product width, blocks)
    for (name, wa, wb, signed, wp, blocks) in [
        // One block, nine-bit mode.
        ("u9x9", 9, 9, false, 18, 1),
        ("s9x9", 9, 9, true, 18, 1),
        ("s9x3", 9, 3, true, 12, 1),
        ("u4x7", 4, 7, false, 11, 1),
        // One block, 18-bit mode.
        ("u18x18", 18, 18, false, 36, 1),
        ("s18x18", 18, 18, true, 36, 1),
        ("s12x7", 12, 7, true, 19, 1),
        ("u18x10", 18, 10, false, 28, 1),
        // The product cut to fewer bits than the operands make.
        ("u16x16_16", 16, 16, false, 16, 1),
    ] {
        let sg = if signed { "signed " } else { "" };
        let verilog = format!(
            "module top(input clk, input [{wa1}:0] a, input [{wb1}:0] b, output [{wp1}:0] p,
           output [{wa1}:0] oa, output [{wb1}:0] ob);
  reg {sg}[{wa1}:0] ra; reg {sg}[{wb1}:0] rb;
  always @(posedge clk) begin ra <= a; rb <= b; end
  assign p = ra * rb; assign oa = ra; assign ob = rb;
endmodule
",
            wa1 = wa - 1,
            wb1 = wb - 1,
            wp1 = wp - 1,
        );
        v.push((name, verilog, blocks));
    }
    // Limbs. A dense product over several blocks is beyond SAT (a 20 x 20
    // limb sum against a flat multiplier doesn't finish in an hour), so
    // the operands are sparse: a few variable bits at the top and at the
    // bottom of each, zeros between. The limbs are still cut at bit 18,
    // every partial product is still built and summed by the real code,
    // and the proof finishes in seconds. Dense operands are covered by
    // co-simulation.
    for (name, wa, wb, signed, wp, blocks) in [
        // 22 x 22 to 44: 2 x 2 limbs, all four products land.
        ("u22x22_sparse", 22, 22, false, 44, 4),
        ("s22x22_sparse", 22, 22, true, 44, 4),
        // 32 x 32 to 32 (C's `*`): the high x high product starts at bit
        // 36, past the result.
        ("u32x32_32_sparse", 32, 32, false, 32, 3),
        // Three limbs by one.
        ("u40x18_sparse", 40, 18, false, 58, 3),
        ("s40x18_sparse", 40, 18, true, 58, 3),
    ] {
        let sg = if signed { "signed " } else { "" };
        // Top 4 and bottom 4 bits of each operand are inputs (the top
        // limb's sign bit among them); the sparse b for a narrow one.
        let sparse = |w: usize, i: &str| {
            if w <= 8 {
                format!("{i}[{}:0]", w - 1)
            } else {
                format!("{{{i}[7:4], {}'b0, {i}[3:0]}}", w - 8)
            }
        };
        let verilog = format!(
            "module top(input clk, input [7:0] a, input [7:0] b, output [{wp1}:0] p,
           output [7:0] oa, output [7:0] ob);
  reg [7:0] ia, ib;
  wire {sg}[{wa1}:0] ra = {sa};
  wire {sg}[{wb1}:0] rb = {sb};
  always @(posedge clk) begin ia <= a; ib <= b; end
  assign p = ra * rb; assign oa = ia; assign ob = ib;
endmodule
",
            wa1 = wa - 1,
            wb1 = wb - 1,
            wp1 = wp - 1,
            sa = sparse(wa, "ia"),
            sb = sparse(wb, "ib"),
        );
        v.push((name, verilog, blocks));
    }
    // Registers that move into the block: on the operands (read by the
    // product alone), on the product, both, with an enable, with a clear;
    // and ones that can't: an operand register read elsewhere too, a
    // product register with a different clock enable than the operands'.
    let regs = |name: &'static str, body: &str, blocks: usize| {
        (
            name,
            format!(
                "module top(input clk, input en, input clr, input [8:0] a, input [8:0] b, output [17:0] p, output [8:0] o);\n{body}\nendmodule\n"
            ),
            blocks,
        )
    };
    v.push(regs(
        "reg_in",
        "  reg [8:0] ra, rb; always @(posedge clk) begin ra <= a; rb <= b; end\n  assign p = ra * rb; assign o = a;",
        1,
    ));
    v.push(regs(
        "reg_out",
        "  reg [17:0] q; always @(posedge clk) q <= a * b;\n  assign p = q; assign o = a;",
        1,
    ));
    v.push(regs(
        "reg_in_out",
        "  reg [8:0] ra, rb; reg [17:0] q; always @(posedge clk) begin ra <= a; rb <= b; q <= ra * rb; end\n  assign p = q; assign o = a;",
        1,
    ));
    v.push(regs(
        "reg_enable_clear",
        "  reg [8:0] ra, rb; reg [17:0] q;\n  always @(posedge clk or posedge clr) if (clr) begin ra <= 0; rb <= 0; q <= 0; end else if (en) begin ra <= a; rb <= b; q <= ra * rb; end\n  assign p = q; assign o = a;",
        1,
    ));
    v.push(regs(
        "reg_in_shared",
        "  reg [8:0] ra, rb; always @(posedge clk) begin ra <= a; rb <= b; end\n  assign p = ra * rb; assign o = ra;",
        1,
    ));
    v.push(regs(
        "reg_out_other_enable",
        "  reg [8:0] ra, rb; reg [17:0] q; always @(posedge clk) begin ra <= a; rb <= b; if (en) q <= ra * rb; end\n  assign p = q; assign o = a;",
        1,
    ));
    // Mixed signedness: Verilog's rule makes the product unsigned when
    // either operand is, so a signed operand is extended by hand.
    v.push((
        "s12xu8",
        "module top(input clk, input [11:0] a, input [7:0] b, output [19:0] p,
           output [11:0] oa, output [7:0] ob);
  reg signed [11:0] ra; reg [7:0] rb;
  always @(posedge clk) begin ra <= a; rb <= b; end
  assign p = ra * $signed({1'b0, rb}); assign oa = ra; assign ob = rb;
endmodule
"
        .into(),
        1,
    ));
    // A constant operand stays logic; a one-bit one too.
    v.push((
        "const_k",
        "module top(input clk, input [11:0] a, output reg [19:0] p);
  reg [11:0] ra;
  always @(posedge clk) begin ra <= a; p <= ra * 12'd1234; end
endmodule
"
        .into(),
        0,
    ));
    v.push((
        "one_bit",
        "module top(input clk, input [11:0] a, input b, output reg [12:0] p);
  reg [11:0] ra; reg rb;
  always @(posedge clk) begin ra <= a; rb <= b; p <= ra * rb; end
endmodule
"
        .into(),
        0,
    ));
    // Two products sharing an operand, and a product of a sum: the
    // adders around the blocks map as any other.
    v.push((
        "two_products",
        "module top(input clk, input [8:0] a, input [8:0] b, input [8:0] c, output [18:0] p,
           output [8:0] oa, output [8:0] ob, output [8:0] oc);
  reg [8:0] ra, rb, rc;
  always @(posedge clk) begin ra <= a; rb <= b; rc <= c; end
  assign p = ra * rb + ra * rc; assign oa = ra; assign ob = rb; assign oc = rc;
endmodule
"
        .into(),
        2,
    ));
    v
}
