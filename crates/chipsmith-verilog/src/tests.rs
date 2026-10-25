//! Unit tests that need no yosys: constant results of the sizing and
//! signedness rules (IEEE 1364-2005 §5.4/5.5, values worked out by hand),
//! the shape of what comes out, and error messages.
//! tests/semantics.rs and tests/yosys.rs check the rest against yosys.

use chipsmith_netlist::{CellKind, Const, Design, DffControl, FfKind, SigBit, State};

use crate::Frontend;

fn elaborate(src: &str) -> Result<Design, crate::Error> {
    let mut fe = Frontend::new();
    fe.read_source("t.v", src)?;
    fe.elaborate("top")
}

/// The constant an output port is tied to.
fn port_const(d: &Design, port: &str) -> String {
    let w = d.top().unwrap().wire(port).unwrap();
    let bits: Option<Vec<State>> = w
        .bits
        .iter()
        .map(|b| match b {
            SigBit::Const(s) => Some(*s),
            SigBit::Net(_) => None,
        })
        .collect();
    Const(bits.unwrap_or_else(|| panic!("{port} is not constant"))).to_string()
}

#[test]
fn sizing_and_signedness_of_constants() {
    let d = elaborate(
        "module top(output [7:0] a, b, c, d, e, f, g, h, output [3:0] i, j);
           assign a = -4'sd3;                  // sign-extended: 11111101
           assign b = 4'd13 + 4'd5;            // 8-bit context: 18, no wrap
           assign c = (4'd13 + 4'd5) >> 1;     // still 8 bits wide: 9
           assign d = 4'sd15 >>> 1;            // 4'sd15 is -1: 11111111
           assign e = 4'sb1000 + 1'b1;         // unsigned context: 8 + 1
           assign f = $signed(4'b1000) + 1'sb1;// signed: -8 + -1 = -9
           assign g = {4'b1010} >>> 1;         // a concatenation is unsigned
           assign h = 8'sd200 / 8'sd3;         // -56 / 3 = -18 (toward zero)
           assign i = 5'd31 == 4'd15 ? 1 : 2;  // compared at 5 bits: false
           assign j = 3'b111 < 3'sb111;        // unsigned compare: 7 < 7 false
         endmodule",
    )
    .unwrap();
    assert_eq!(port_const(&d, "a"), "11111101");
    assert_eq!(port_const(&d, "b"), "00010010");
    assert_eq!(port_const(&d, "c"), "00001001");
    assert_eq!(port_const(&d, "d"), "11111111");
    assert_eq!(port_const(&d, "e"), "00001001");
    assert_eq!(port_const(&d, "f"), "11110111");
    assert_eq!(port_const(&d, "g"), "00000101");
    assert_eq!(port_const(&d, "h"), "11101110");
    assert_eq!(port_const(&d, "i"), "0010");
    assert_eq!(port_const(&d, "j"), "0000");
}

#[test]
fn parameters_take_their_declared_type() {
    let d = elaborate(
        "module sub #(parameter [3:0] P = 0, parameter Q = 0, parameter integer R = 0)
                    (output [7:0] p, q, r);
           assign p = P; assign q = Q; assign r = R;
         endmodule
         module top(output [7:0] p, q, r);
           // P truncates to 4 bits; Q takes the override's type (signed,
           // 3 bits); R is a 32-bit integer.
           sub #(.P(8'hab), .Q(3'sb101), .R(-2)) u (.p(p), .q(q), .r(r));
         endmodule",
    )
    .unwrap();
    let sub = d
        .modules
        .iter()
        .find(|m| m.name.starts_with("$paramod\\sub"))
        .unwrap();
    let c = |port: &str| {
        let w = sub.wire(port).unwrap();
        Const(
            w.bits
                .iter()
                .map(|b| match b {
                    SigBit::Const(s) => *s,
                    SigBit::Net(_) => panic!("{port} isn't constant"),
                })
                .collect(),
        )
        .to_string()
    };
    assert_eq!(c("p"), "00001011");
    assert_eq!(c("q"), "11111101");
    assert_eq!(c("r"), "11111110");
}

#[test]
fn constant_functions_and_generate() {
    let d = elaborate(
        "module top(output [7:0] y);
           function integer log2(input integer v);
             integer k;
             begin
               log2 = 0;
               for (k = v - 1; k > 0; k = k >> 1) log2 = log2 + 1;
             end
           endfunction
           localparam N = log2(200);           // 8
           genvar i;
           for (i = 0; i < 8; i = i + 1) begin : g
             if (i < N - 4) assign y[i] = 1'b1; else assign y[i] = 1'b0;
           end
         endmodule",
    )
    .unwrap();
    assert_eq!(port_const(&d, "y"), "00001111");
}

#[test]
fn registers_and_initial_values() {
    let d = elaborate(
        "module top(input clk, rst, input [3:0] d, output reg [3:0] q, output reg [3:0] r);
           initial q = 4'd9;
           always @(posedge clk) q <= d;
           always @(posedge clk or posedge rst) if (rst) r <= 4'd3; else r <= d;
         endmodule",
    )
    .unwrap();
    let top = d.top().unwrap();
    assert_eq!(
        top.wire("q").unwrap().attributes.get("init"),
        Some(&chipsmith_netlist::ParamValue::Bits(Const::from_int(9, 4)))
    );
    let mut kinds = Vec::new();
    for c in &top.cells {
        if let CellKind::Ff(ff) = &c.kind {
            match &ff.kind {
                FfKind::Dff {
                    control: DffControl::Plain { en: None },
                    ..
                } => kinds.push("dff"),
                FfKind::Dff {
                    control: DffControl::Async { value, .. },
                    ..
                } => {
                    assert_eq!(*value, Const::from_int(3, 4));
                    kinds.push("adff");
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }
    kinds.sort();
    assert_eq!(kinds, ["adff", "dff"]);
}

/// Memories come out as one `$mem_v2` with asynchronous reads (yosys'
/// `memory_dff` makes them synchronous later) and tri-states as `$tribuf`.
#[test]
fn memories_and_tristates() {
    let d = elaborate(
        "module top(input clk, we, oe, input [3:0] a, input [7:0] wd, output reg [7:0] q, inout [7:0] io);
           reg [7:0] m [0:15];
           initial m[3] = 8'h42;
           always @(posedge clk) begin
             if (we) m[a] <= wd;
             q <= m[a];
           end
           assign io = oe ? q : 8'bz;
         endmodule",
    )
    .unwrap();
    let top = d.top().unwrap();
    let mem = top
        .cells
        .iter()
        .find_map(|c| match &c.kind {
            CellKind::Mem(m) => Some(m),
            _ => None,
        })
        .unwrap();
    assert_eq!((mem.memid.as_str(), mem.size, mem.width), ("\\m", 16, 8));
    assert_eq!((mem.rd.len(), mem.wr.len()), (1, 1));
    assert!(mem.rd[0].sync.is_none());
    assert_eq!(mem.init.0[24..32], Const::from_int(0x42, 8).0[..]);
    assert!(top
        .cells
        .iter()
        .any(|c| matches!(c.kind, CellKind::Tribuf(_))));
}

fn error(src: &str) -> String {
    elaborate(src).unwrap_err().to_string()
}

#[test]
fn errors_point_at_the_source() {
    assert_eq!(
        error("module top(output y);\n  assign y = 1'b1 +;\nendmodule"),
        "t.v:2:20: expected an expression, found `;`"
    );
    assert_eq!(
        error("module top(output y);\n  assign y = nope;\nendmodule"),
        "t.v:2:14: `nope` is not declared"
    );
    assert_eq!(
        error("`default_nettype none\nmodule top(input a);\n  assign b = a;\nendmodule"),
        "t.v:3:10: `b` is not declared (`default_nettype none)"
    );
    assert!(error(
        "module top(input a, output reg y);\n  always @* begin y = a; y <= 1; end\nendmodule"
    )
    .starts_with("t.v:2:26: `y` is assigned with both `=` and `<=`"));
    assert!(error(
        "module top(input [3:0] n, output reg y);\n  integer i;\n  always @* for (i = 0; i < n; i = i + 1) y = i;\nendmodule"
    )
    .starts_with("t.v:3:25: loop condition doesn't fold to a constant"));
    assert!(
        error("module top(input a, output y);\n  assign y = a;\n  assign y = ~a;\nendmodule")
            .contains("`y` has more than one driver")
    );
    assert!(error("module top(output y);\n  sub u(.o(y));\nendmodule")
        .starts_with("t.v:2:3: module sub is not defined"));
    assert!(error("module top(input clk, d, output reg q);\n  always @(posedge clk or posedge d) q <= d;\nendmodule")
        .contains("must start with `if` testing the asynchronous ones"));
    assert!(
        error("module top(input a, output y);\n  assign y = $random;\nendmodule")
            .contains("system function $random is not supported")
    );
}

#[test]
fn errors_inside_macros_report_the_use() {
    let e = error("`define BAD (1 +)\nmodule top(output y);\n  assign y = `BAD;\nendmodule");
    assert!(e.starts_with("t.v:3:14:"), "{e}");
}
