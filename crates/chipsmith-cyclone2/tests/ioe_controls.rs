//! I/O-cell registers with the cell's own controls through the whole flow
//! on the shipped chip database: synthesis, pack, place, route, bitstream.
//! Quartus packs a register's clock enable, asynchronous clear or preset
//! and falling edge into the I/O cell (cyclone2-re README, "I/O-cell
//! register controls"); these check that chipsmith's build does, down to
//! what the bitstream writes for each pin. `tests/synth.rs` proves the
//! synthesis; the bidir here, whose three registers share one reset, is
//! out of its co-simulation's reach. Needs `CHIPSMITH_CYCLONE2_DB`: run in
//! the devshell.

mod support;

use std::collections::BTreeMap;

use chipsmith_cyclone2::bitstream::{self, io::IoControl};
use chipsmith_cyclone2::pin::IoRegisters;

#[test]
fn registers_take_the_cells_controls() {
    let body = r#"
  reg [1:0] iq;
  always @(posedge CLK or negedge RST_N)
    if (!RST_N) iq <= 0; else if (!EN_N) iq <= D;
  assign Q = iq;
  reg o;
  always @(negedge CLK or posedge RST) if (RST) o <= 1; else o <= D[0] & D[1];
  assign O = o;
  reg gi, gd, ge;
  always @(posedge CLK or posedge RST)
    if (RST) begin gi <= 0; gd <= 0; ge <= 0; end
    else begin gi <= G; if (!EN_N) gd <= D[1]; ge <= D[0] ^ D[1]; end
  assign G = ge ? gd : 1'bz;
  assign GI = gi;
"#;
    let only = |input, output, output_enable| IoRegisters {
        input,
        output,
        output_enable,
    };
    let regs = BTreeMap::from([
        ("D[0]".to_string(), only(true, false, false)),
        ("D[1]".to_string(), only(true, false, false)),
        ("O".to_string(), only(false, true, false)),
        ("G".to_string(), only(true, true, true)),
    ]);
    let (dev, r) = support::flow(
        "ioe-controls",
        "input CLK, input RST, input RST_N, input EN_N, input [1:0] D, output [1:0] Q, \
         output O, inout G, output GI",
        body,
        &[
            ("CLK", "PIN_N2"),
            ("RST", "PIN_V2"),
            ("RST_N", "PIN_W26"),
            ("EN_N", "PIN_G26"),
            ("D[0]", "PIN_T7"),
            ("D[1]", "PIN_U3"),
            ("Q[0]", "PIN_AE23"),
            ("Q[1]", "PIN_AF23"),
            ("O", "PIN_AB21"),
            ("G", "PIN_F24"),
            ("GI", "PIN_AC22"),
        ],
        &regs,
    );
    let db = bitstream::load_db().unwrap();
    let phys = r.to_phys(&dev, &db.io).unwrap();
    let pin = |p: &str| {
        phys.pins
            .iter()
            .find(|u| u.pin == p)
            .unwrap_or_else(|| panic!("no pin {p}"))
    };
    let inverted = |c: Option<IoControl>| c.map(|c| c.inverted);

    // Input registers: the enable's inverter and the active-low reset
    // fold into the cell.
    for p in ["T7", "U3"] {
        let [i, o, e] = pin(p).dir.regs();
        let i = i.unwrap_or_else(|| panic!("{p}: no input register"));
        assert!(o.is_none() && e.is_none());
        assert_eq!(inverted(i.clock.enable), Some(true), "{p}: enable");
        assert_eq!(inverted(i.areset), Some(true), "{p}: reset");
        assert!(!i.clock.inverted && !i.powers_up_high);
    }
    // A preset on the falling edge.
    let [_, o, _] = pin("AB21").dir.regs();
    let o = o.expect("AB21: output register");
    assert!(o.clock.inverted && o.powers_up_high && o.clock.enable.is_none());
    assert_eq!(inverted(o.areset), Some(false));
    // The bidir: three registers on one reset, the enable for the output
    // register only.
    let [i, d, e] = pin("F24").dir.regs().map(|r| r.expect("F24: a register"));
    let (areset, _) = pin("F24").dir.resets().unwrap();
    assert!(areset.is_some_and(|a| !a.inverted));
    for r in [i, d, e] {
        assert_eq!(r.areset, areset);
        assert!(!r.clock.inverted && !r.powers_up_high);
    }
    assert_eq!(inverted(d.clock.enable), Some(true));
    assert!(i.clock.enable.is_none() && e.clock.enable.is_none());
}
