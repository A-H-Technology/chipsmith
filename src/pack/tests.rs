//! The packing rules on small hand-made yosys netlists.

use serde_json::{json, Map, Value};

use super::*;

/// A yosys netlist of `cells`: (name, type, LUT mask or None, connections).
/// Signals are numbers; "0"/"1" constants. Bit 2 is the CLOCK_50 pad, 3
/// the LEDR pad and 4 the SW pad (one-bit ports); pin buffers are added.
fn design(cells: &[(&str, &str, Option<u16>, Value)]) -> Value {
    let mut all = Map::new();
    let pins = [
        (
            "clk_buf",
            "CY2_IBUF",
            None,
            json!({"PAD": [2], "COMBOUT": [10]}),
        ),
        (
            "led_buf",
            "CY2_OBUF",
            None,
            json!({"DATAOUT": [11], "PAD": [3]}),
        ),
        (
            "sw_buf",
            "CY2_IBUF",
            None,
            json!({"PAD": [4], "COMBOUT": [12]}),
        ),
    ];
    for (name, ty, mask, conns) in pins.iter().chain(cells) {
        let outputs = ["COMBOUT", "COUT", "REGOUT"];
        let dirs: Map<String, Value> = conns
            .as_object()
            .unwrap()
            .keys()
            .map(|p| {
                let out = outputs.contains(&p.as_str()) || (*ty == "CY2_OBUF" && p == "PAD");
                (p.clone(), json!(if out { "output" } else { "input" }))
            })
            .collect();
        let params = match mask {
            Some(m) => json!({"LUT_MASK": format!("{m:016b}")}),
            None => json!({}),
        };
        all.insert(
            name.to_string(),
            json!({"type": ty, "parameters": params, "attributes": {},
                   "port_directions": dirs, "connections": conns}),
        );
    }
    json!({"modules": {"top": {
        "attributes": {"top": "00000000000000000000000000000001"},
        "ports": {
            "CLOCK_50": {"direction": "input", "bits": [2]},
            "LEDR": {"direction": "output", "bits": [3]},
            "SW": {"direction": "input", "bits": [4]},
        },
        "cells": all,
        "netnames": {"clk": {"bits": [10]}, "led": {"bits": [11]}, "sw": {"bits": [12]}},
    }}})
}

fn packed(cells: &[(&str, &str, Option<u16>, Value)]) -> (Netlist, Stats) {
    let mut nl = yosys::import(&design(cells)).unwrap();
    let stats = pack(&mut nl).unwrap();
    (nl, stats)
}

fn find(nl: &Netlist, name: &str) -> Option<CellId> {
    nl.cells().into_iter().find(|&c| nl.cell(c).name == name)
}

fn cell<'a>(nl: &'a Netlist, name: &str) -> &'a netlist::Cell {
    nl.cell(find(nl, name).unwrap_or_else(|| panic!("no cell {name}")))
}

fn net_name(nl: &Netlist, name: &str, port: Port) -> Option<String> {
    cell(nl, name).net(port).map(|n| nl.net(n).name.clone())
}

#[test]
fn constant_zero_folds_and_a_constant_one_on_c_or_d_only_disconnects() {
    let (nl, _) = packed(&[(
        "lut",
        "LCELL_COMB",
        Some(0x8000), // A & B & C & D
        json!({"DATAA": [12], "DATAB": ["1"], "DATAC": ["1"], "DATAD": ["1"], "COMBOUT": [11]}),
    )]);
    let lut = cell(&nl, "lut");
    // B folded (to A & C & D), C and D just disconnected: they read 1 anyway.
    assert_eq!(lut.mask(), mask::fix(0x8000, 1, true));
    assert_eq!(lut.net(Port::DataB), None);
    assert_eq!(lut.net(Port::DataC), None);
    assert_eq!(net_name(&nl, "lut", Port::DataA).as_deref(), Some("sw"));
}

#[test]
fn a_stuck_register_is_removed_and_its_zero_propagates() {
    let (nl, stats) = packed(&[
        (
            "ff",
            "LCELL_FF",
            None,
            json!({"DATAIN": ["0"], "CLK": [10], "ENA": ["1"], "ACLR": ["0"], "SCLR": ["0"], "REGOUT": [20]}),
        ),
        (
            "lut",
            "LCELL_COMB",
            Some(0x6666), // A ^ B
            json!({"DATAA": [20], "DATAB": [12], "DATAC": ["1"], "DATAD": ["1"], "COMBOUT": [11]}),
        ),
    ]);
    assert_eq!(stats.stuck_ffs, 1);
    assert!(find(&nl, "ff").is_none());
    // A ^ B with A = 0 is B: a buffer, so it folds into the pin too.
    assert!(find(&nl, "lut").is_none());
    assert_eq!(
        net_name(&nl, "led_buf", Port::Dataout).as_deref(),
        Some("sw")
    );
    // No register left to clock: no clock control.
    assert_eq!(stats.clocks, 0);
}

#[test]
fn an_input_less_lut_is_a_constant_for_its_loads() {
    let (nl, stats) = packed(&[(
        "lut",
        "LCELL_COMB",
        Some(0x0000),
        json!({"DATAA": ["0"], "DATAB": ["1"], "DATAC": ["1"], "DATAD": ["1"], "COMBOUT": [11]}),
    )]);
    assert_eq!(stats.const_luts, 1);
    assert_eq!(
        cell(&nl, "led_buf").kind,
        Kind::Obuf {
            port: Some("LEDR".into()),
            data: IoeIn::Const(false)
        }
    );
}

#[test]
fn an_inverter_folds_into_lut_masks_and_the_pin() {
    let (nl, stats) = packed(&[
        (
            "inv",
            "LCELL_COMB",
            Some(!mask::BUFFERS[0]),
            json!({"DATAA": [12], "DATAB": ["1"], "DATAC": ["1"], "DATAD": ["1"], "COMBOUT": [20]}),
        ),
        (
            "and",
            "LCELL_COMB",
            Some(0x8888), // A & B
            json!({"DATAA": [20], "DATAB": [21], "DATAC": ["1"], "DATAD": ["1"], "COMBOUT": [22]}),
        ),
        (
            "ff",
            "LCELL_FF",
            None,
            json!({"DATAIN": [22], "CLK": [10], "ENA": [20], "ACLR": ["0"], "SCLR": ["0"], "REGOUT": [21]}),
        ),
        (
            "buf",
            "LCELL_COMB",
            Some(mask::BUFFERS[1]),
            json!({"DATAA": ["1"], "DATAB": [20], "DATAC": ["1"], "DATAD": ["1"], "COMBOUT": [11]}),
        ),
    ]);
    assert!(find(&nl, "inv").is_none());
    assert!(find(&nl, "buf").is_none());
    assert_eq!(stats.removed_luts, 2);
    // (!A) & B
    assert_eq!(cell(&nl, "and").mask(), 0x4444);
    assert_eq!(net_name(&nl, "and", Port::DataA).as_deref(), Some("sw"));
    assert_eq!(
        cell(&nl, "ff").kind,
        Kind::Ff(netlist::FfConfig {
            ena_inv: true,
            sclr_inv: false
        })
    );
    // The pin was fed by a buffer of the inverter: one inversion, in the IOE.
    assert_eq!(
        cell(&nl, "led_buf").kind,
        Kind::Obuf {
            port: Some("LEDR".into()),
            data: IoeIn::Signal { invert: true }
        }
    );
}

#[test]
fn an_inverter_in_front_of_a_register_is_pushed_into_its_driver() {
    let (nl, stats) = packed(&[
        (
            "xor",
            "LCELL_COMB",
            Some(0x6666),
            json!({"DATAA": [12], "DATAB": [21], "DATAC": ["1"], "DATAD": ["1"], "COMBOUT": [20]}),
        ),
        (
            "inv",
            "LCELL_COMB",
            Some(!mask::BUFFERS[0]),
            json!({"DATAA": [20], "DATAB": ["1"], "DATAC": ["1"], "DATAD": ["1"], "COMBOUT": [22]}),
        ),
        (
            "ff",
            "LCELL_FF",
            None,
            json!({"DATAIN": [22], "CLK": [10], "ENA": ["1"], "ACLR": ["0"], "SCLR": ["0"], "REGOUT": [21]}),
        ),
        (
            "out",
            "LCELL_COMB",
            Some(0x8888),
            json!({"DATAA": [20], "DATAB": [21], "DATAC": ["1"], "DATAD": ["1"], "COMBOUT": [11]}),
        ),
    ]);
    assert!(find(&nl, "inv").is_none());
    assert_eq!(stats.removed_luts, 1);
    // The driver computes the inverse, its other load undoes it.
    assert_eq!(cell(&nl, "xor").mask(), !0x6666);
    assert_eq!(cell(&nl, "out").mask(), mask::invert_input(0x8888, 0));
    // The register now sits beside the inverted driver.
    assert_eq!(
        net_name(&nl, "ff", Port::Datain),
        net_name(&nl, "xor", Port::Combout)
    );
}

#[test]
fn a_register_fed_by_a_register_takes_the_sync_load_path() {
    let (nl, stats) = packed(&[
        (
            "a",
            "LCELL_FF",
            None,
            json!({"DATAIN": [12], "CLK": [10], "ENA": ["1"], "ACLR": ["0"], "SCLR": ["0"], "REGOUT": [20]}),
        ),
        (
            "b",
            "LCELL_FF",
            None,
            json!({"DATAIN": [20], "CLK": [10], "ENA": ["1"], "ACLR": ["0"], "SCLR": ["0"], "REGOUT": [11]}),
        ),
    ]);
    assert_eq!(stats.sdata, 2);
    assert_eq!(
        net_name(&nl, "b", Port::Sdata),
        net_name(&nl, "a", Port::Regout)
    );
    assert_eq!(cell(&nl, "b").net(Port::Datain), None);
    // Both clocked through one clock control.
    assert_eq!(stats.clocks, 1);
    let cc = cell(&nl, "clk$clkctrl");
    assert_eq!(cc.kind, Kind::ClkCtrl);
    assert_eq!(net_name(&nl, "a", Port::Clk).as_deref(), Some("clk$global"));
}

#[test]
fn a_register_loading_a_constant_one_gets_a_lut_for_it() {
    let (nl, _) = packed(&[(
        "ff",
        "LCELL_FF",
        None,
        json!({"DATAIN": ["1"], "CLK": [10], "ENA": [12], "ACLR": ["0"], "SCLR": ["0"], "REGOUT": [11]}),
    )]);
    assert_eq!(cell(&nl, "ff$const").mask(), 0xFFFF);
    assert_eq!(
        net_name(&nl, "ff", Port::Datain).as_deref(),
        Some("ff$const_net")
    );
}

#[test]
fn carry_chains_get_quartus_polarity() {
    let m = 0x96E8; // sum A ^ B ^ cin, carry majority
    let le = |a: u64, cin: Value, cout: u64, sum: u64| {
        json!({"DATAA": [a], "DATAB": [12], "DATAC": ["1"], "DATAD": ["1"],
               "CIN": cin, "COUT": [cout], "COMBOUT": [sum]})
    };
    let (nl, stats) = packed(&[
        ("c0", "LCELL_COMB", Some(m), le(30, json!(["1"]), 40, 50)),
        ("c1", "LCELL_COMB", Some(m), le(31, json!([40]), 41, 51)),
        ("c2", "LCELL_COMB", Some(m), le(32, json!([41]), 42, 52)),
        (
            "x",
            "LCELL_COMB",
            Some(0x6996),
            json!({"DATAA": [50], "DATAB": [51], "DATAC": [52], "DATAD": [10], "COMBOUT": [11]}),
        ),
        (
            "r0",
            "LCELL_FF",
            None,
            json!({"DATAIN": [12], "CLK": [10], "ENA": ["1"], "ACLR": ["0"], "SCLR": ["0"], "REGOUT": [30]}),
        ),
        (
            "r1",
            "LCELL_FF",
            None,
            json!({"DATAIN": [12], "CLK": [10], "ENA": ["1"], "ACLR": ["0"], "SCLR": ["0"], "REGOUT": [31]}),
        ),
        (
            "r2",
            "LCELL_FF",
            None,
            json!({"DATAIN": [12], "CLK": [10], "ENA": ["1"], "ACLR": ["0"], "SCLR": ["0"], "REGOUT": [32]}),
        ),
    ]);
    assert_eq!(stats.chains, 1);
    assert_eq!(
        cell(&nl, "c0").mask(),
        mask::normalise_carry(m, 0, false, true)
    );
    assert_eq!(
        cell(&nl, "c1").mask(),
        mask::normalise_carry(m, 1, false, true)
    );
    // The last LE's COUT is unused: its carry half holds the sum.
    assert_eq!(
        cell(&nl, "c2").mask(),
        mask::normalise_carry(m, 2, true, true)
    );
    assert_ne!(cell(&nl, "c1").mask(), m);
}

/// SW in, IO inout, T a tri-state output and LEDR reading IO back.
fn bidir_design() -> Value {
    let cell = |ty: &str, mask: Option<u16>, conns: Value, dirs: Value| {
        let params = match mask {
            Some(m) => json!({"LUT_MASK": format!("{m:016b}")}),
            None => json!({}),
        };
        json!({"type": ty, "parameters": params, "attributes": {},
               "port_directions": dirs, "connections": conns})
    };
    json!({"modules": {"top": {
        "attributes": {"top": "00000000000000000000000000000001"},
        "ports": {
            "IO": {"direction": "inout", "bits": [5]},
            "LEDR": {"direction": "output", "bits": [3]},
            "SW": {"direction": "input", "bits": [4]},
            "T": {"direction": "output", "bits": [6]},
        },
        "cells": {
            "sw_buf": cell("CY2_IBUF", None, json!({"PAD": [4], "COMBOUT": [12]}),
                json!({"PAD": "input", "COMBOUT": "output"})),
            "inv": cell("LCELL_COMB", Some(!mask::BUFFERS[0]),
                json!({"DATAA": [12], "DATAB": ["1"], "DATAC": ["1"], "DATAD": ["1"], "COMBOUT": [20]}),
                json!({"DATAA": "input", "DATAB": "input", "DATAC": "input", "DATAD": "input",
                       "COMBOUT": "output"})),
            "io_buf": cell("CY2_IOBUF", None,
                json!({"PAD": [5], "DATAOUT": ["0"], "OE": [20], "COMBOUT": [30]}),
                json!({"PAD": "inout", "DATAOUT": "input", "OE": "input", "COMBOUT": "output"})),
            "led_buf": cell("CY2_OBUF", None, json!({"DATAOUT": [30], "PAD": [3]}),
                json!({"DATAOUT": "input", "PAD": "output"})),
            "t_buf": cell("CY2_IOBUF", None,
                json!({"PAD": [6], "DATAOUT": [12], "OE": ["1"], "COMBOUT": [31]}),
                json!({"PAD": "inout", "DATAOUT": "input", "OE": "input", "COMBOUT": "output"})),
        },
        "netnames": {"io_in": {"bits": [30]}, "sw": {"bits": [12]}, "t_in": {"bits": [31]}},
    }}})
}

#[test]
fn tri_state_and_bidir_pins_fold_constants_and_inverters_into_the_ioe() {
    let mut nl = yosys::import(&bidir_design()).unwrap();
    pack(&mut nl).unwrap();
    assert!(find(&nl, "inv").is_none());
    assert_eq!(
        cell(&nl, "io_buf").kind,
        Kind::Iobuf {
            port: Some("IO".into()),
            data: IoeIn::Const(false),
            oe: IoeIn::Signal { invert: true },
        }
    );
    assert_eq!(net_name(&nl, "io_buf", Port::Oe).as_deref(), Some("sw"));
    assert_eq!(
        net_name(&nl, "io_buf", Port::Combout).as_deref(),
        Some("io_in")
    );
    // Nothing reads T's pad: its input buffer is off.
    assert_eq!(
        cell(&nl, "t_buf").kind,
        Kind::Iobuf {
            port: Some("T".into()),
            data: IoeIn::Signal { invert: false },
            oe: IoeIn::Const(true),
        }
    );
    assert_eq!(cell(&nl, "t_buf").net(Port::Combout), None);
    assert!(!cell(&nl, "io_buf").has_port(Port::Pad));

    let json = to_json(&nl).unwrap();
    let top = &json["modules"]["top"];
    assert_eq!(top["ports"]["IO"]["direction"], "inout");
    assert_eq!(top["ports"]["T"]["direction"], "output");
    let io = &top["cells"]["io_buf"];
    assert_eq!(io["type"], "CY2_IOBUF");
    assert_eq!(io["parameters"]["OUT_CONST"], "0");
    assert_eq!(io["parameters"]["OE_INVERT"], "1");
    assert_eq!(top["cells"]["t_buf"]["parameters"]["OE_CONST"], "1");
}

#[test]
fn an_inout_without_a_tri_state_driver_is_an_error() {
    let mut j = bidir_design();
    let cells = j["modules"]["top"]["cells"].as_object_mut().unwrap();
    cells.remove("io_buf");
    cells.remove("led_buf");
    let mut nl = yosys::import(&j).unwrap();
    let err = pack(&mut nl).unwrap_err().to_string();
    assert!(
        err.contains("inout port IO has no tri-state driver"),
        "{err}"
    );
}
