//! An active-low asynchronous reset on LE registers through the whole flow
//! on the shipped chip database: synthesis, pack, place, route, bitstream.
//! `always @(posedge clk or negedge rst_n)` is how Terasic's and most
//! university DE2 code resets; Quartus keeps one clear net and each LAB
//! inverts it (`LAB_CLR_INVERT_PAIR`; cyclone2-re README, "Asynchronous
//! resets"). This checks that chipsmith's build does, down to the LAB's
//! inversion bit in the RBF. `tests/synth.rs`
//! (`active_low_clears_fold_into_the_lab`) co-simulates the packed netlist.
//! Needs `CHIPSMITH_CYCLONE2_DB`: run in the devshell.

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use chipsmith_cyclone2::bitstream::{
    self, addr,
    asmdb::arch,
    logic::{Aclr, Clear},
    rbf::Rbf,
};

#[test]
fn a_counter_with_an_active_low_reset_shares_one_inverted_clear() {
    // A 24-bit counter: its carry chain spans two LABs, which need the
    // same clear. Beside it, the same pin as an active-high clear: a
    // different LAB clear, so another LAB.
    let body = r#"
  reg [23:0] cnt;
  always @(posedge CLK or negedge RST_N)
    if (!RST_N) cnt <= 0; else cnt <= cnt + 1'b1;
  assign Q = cnt[23:20];
  reg [3:0] h;
  always @(posedge CLK or posedge RST_N)
    if (RST_N) h <= 0; else h <= {h[2:0], D};
  assign H = h;
"#;
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let name = "lab-clears";
    let (dev, r) = support::flow(
        base,
        name,
        "input CLK, input RST_N, input D, output [3:0] Q, output [3:0] H",
        body,
        &[
            ("CLK", "PIN_N2"),
            ("RST_N", "PIN_G26"),
            ("D", "PIN_N25"),
            ("Q[0]", "PIN_AE23"),
            ("Q[1]", "PIN_AF23"),
            ("Q[2]", "PIN_AB21"),
            ("Q[3]", "PIN_AC22"),
            ("H[0]", "PIN_AE22"),
            ("H[1]", "PIN_AF22"),
            ("H[2]", "PIN_W19"),
            ("H[3]", "PIN_V18"),
        ],
        &BTreeMap::new(),
    );

    // One clear net: the pin's own, into every register; no LUT reads it
    // (an inverter in front of the clears would).
    let rst_n = r
        .cells
        .values()
        .find(|c| c.attrs.get("PORT").map(String::as_str) == Some("RST_N"))
        .map(|c| c.conns["COMBOUT"])
        .expect("RST_N's buffer");
    let ffs: Vec<_> = r.cells.values().filter(|c| c.ty == "LCELL_FF").collect();
    assert_eq!(ffs.len(), 28);
    assert!(ffs.iter().all(|c| c.conns.get("ACLR") == Some(&rst_n)));
    assert!(r
        .cells
        .values()
        .filter(|c| c.ty == "LCELL_COMB")
        .all(|c| c.conns.values().all(|&n| n != rst_n)));

    let db = bitstream::load_db().unwrap();
    let phys = r.to_phys(&dev, &db.io).unwrap();
    let rbf = Rbf::read(&base.join("synth").join(name).join("top.rbf")).unwrap();
    let invert = db.lab.feature(arch::LAB_CLR_INVERT_PAIR, 6).unwrap().paths[0][0][0];
    let (mut inverted, mut plain) = (0, 0);
    for (&(x, y), lab) in &phys.logic.labs {
        let [first, second] = lab.aclr;
        assert_eq!(second, None, "LAB X{x} Y{y}: clear line 1 in use");
        let Some(Clear { src, inverted: inv }) = first else {
            continue;
        };
        // The cleared registers of a LAB all take clear line 0.
        let regs: Vec<_> = phys
            .logic
            .les
            .iter()
            .filter(|l| (l.x, l.y) == (x, y))
            .filter_map(|l| l.reg)
            .collect();
        assert!(regs.iter().all(|r| r.aclr == Some(0)), "LAB X{x} Y{y}");
        if inv {
            inverted += regs.len();
        } else {
            plain += regs.len();
        }
        // The bit: set for an active-low clear from a LAB line, and for an
        // active-high one from a global.
        let bit = addr::frame_bit(db.lab.flat(x, y, invert).unwrap());
        assert_eq!(
            rbf.get(bit),
            inv != matches!(src, Aclr::Global(_)),
            "LAB X{x} Y{y}: {src:?}, inverted {inv}"
        );
    }
    assert_eq!((inverted, plain), (24, 4));
}
