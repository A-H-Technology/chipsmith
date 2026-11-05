//! Register packing through the whole flow on the shipped chip database:
//! a register on the sync load path (its data on SDATA, the LE's input C)
//! shares an LE with a LUT that leaves input C to it, as Quartus' fitter
//! packs them, so the design takes fewer LEs than LUTs plus registers.
//! Needs `CHIPSMITH_CYCLONE2_DB`: run in the devshell.

mod support;

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use chipsmith_cyclone2::bitstream::nextpnr::Bel;

#[test]
fn sync_load_registers_share_les_with_luts() {
    // A shift register: each stage's data is the previous stage's output,
    // so all but none take SDATA. Beside each stage a 3-input XOR, which
    // leaves input C free.
    let body = r#"
  reg [15:0] sh;
  always @(posedge CLK) sh <= {sh[14:0], D};
  assign Q = sh[15:12];
  genvar i;
  generate for (i = 0; i < 15; i = i + 1) begin : x
    assign X[i] = sh[i] ^ A[i % 4] ^ B[i % 3];
  end endgenerate
"#;
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let pins: Vec<(String, &str)> = [("CLK", "PIN_N2"), ("D", "PIN_N25")]
        .into_iter()
        .map(|(p, n)| (p.to_string(), n))
        .chain((0..4).map(|i| {
            (
                format!("Q[{i}]"),
                ["PIN_AE23", "PIN_AF23", "PIN_AB21", "PIN_AC22"][i],
            )
        }))
        .chain((0..4).map(|i| {
            (
                format!("A[{i}]"),
                ["PIN_N26", "PIN_P25", "PIN_AE14", "PIN_AF14"][i],
            )
        }))
        .chain((0..3).map(|i| (format!("B[{i}]"), ["PIN_AD13", "PIN_AC13", "PIN_C13"][i])))
        .chain((0..15).map(|i| {
            let p = [
                "PIN_AE22", "PIN_AF22", "PIN_W19", "PIN_V18", "PIN_U18", "PIN_U17", "PIN_AA20",
                "PIN_Y18", "PIN_Y12", "PIN_AD21", "PIN_AC21", "PIN_AA14", "PIN_Y13", "PIN_AA13",
                "PIN_AC14",
            ][i];
            (format!("X[{i}]"), p)
        }))
        .collect();
    let pins: Vec<(&str, &str)> = pins.iter().map(|(p, n)| (p.as_str(), *n)).collect();
    let (_, r) = support::flow(
        base,
        "register-packing",
        "input CLK, input D, input [3:0] A, input [2:0] B, output [3:0] Q, output [14:0] X",
        body,
        &pins,
        &BTreeMap::new(),
    );
    let mut luts = 0;
    let mut sdata = 0;
    let mut les = HashSet::new();
    for c in r.cells.values() {
        match c.bel {
            Bel::Lut { x, y, k } => {
                luts += 1;
                les.insert((x, y, k));
            }
            Bel::Ff { x, y, k } => {
                sdata += c.conns.contains_key("SDATA") as usize;
                les.insert((x, y, k));
            }
            _ => {}
        }
    }
    assert!(sdata >= 15, "{sdata} registers on SDATA");
    // Without packing each SDATA register is an LE of its own.
    assert!(
        les.len() + 10 <= luts + sdata,
        "{} LEs for {luts} LUTs and {sdata} SDATA registers",
        les.len()
    );
}
