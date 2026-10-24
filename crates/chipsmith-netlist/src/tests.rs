use crate::ff::{Bit, DffControl, Ff, FfKind, LatchReset, SetClr, SyncEnable};
use crate::json::{read_json, write_json};
use crate::param::ParamValue;
use crate::sig::{Const, Ctrl, Polarity, SigBit, State};
use crate::{CellKind, Connectivity, Design, Dir, Endpoint, Gate, GateOp, Module, Mux, Wire};

/// Every gate-level storage cell the IR can build, as yosys names it.
fn all_bit_ffs(m: &mut Module) -> Vec<Ff<Bit>> {
    let mut sig = || SigBit::Net(m.new_net());
    let mut ctrls = Vec::new();
    for p in [Polarity::Pos, Polarity::Neg] {
        ctrls.push(Ctrl {
            sig: sig(),
            polarity: p,
        });
    }
    let sr = |s: SigBit, r: SigBit, sp, rp| SetClr::<Bit> {
        set: s,
        set_polarity: sp,
        clr: r,
        clr_polarity: rp,
    };
    let (a, b, c) = (sig(), sig(), sig());
    let mut kinds = vec![FfKind::Global { d: a }];
    for &clk in &ctrls {
        for &en in [None]
            .iter()
            .chain(ctrls.iter().map(Some).collect::<Vec<_>>().iter())
        {
            let en = en.copied();
            let mut controls = vec![DffControl::Plain { en }];
            for &r in &ctrls {
                for value in [false, true] {
                    controls.push(DffControl::Async { arst: r, value, en });
                    controls.push(DffControl::Sync {
                        srst: r,
                        value,
                        en: match en {
                            None => SyncEnable::Always,
                            Some(e) => SyncEnable::ResetOverEnable(e),
                        },
                    });
                    if let Some(e) = en {
                        controls.push(DffControl::Sync {
                            srst: r,
                            value,
                            en: SyncEnable::EnableOverReset(e),
                        });
                    }
                }
                controls.push(DffControl::Load {
                    aload: r,
                    ad: b,
                    en,
                });
                for &sp in &[Polarity::Pos, Polarity::Neg] {
                    controls.push(DffControl::SetClr {
                        sr: sr(b, c, sp, r.polarity),
                        en,
                    });
                }
            }
            kinds.extend(
                controls
                    .into_iter()
                    .map(|control| FfKind::Dff { clk, d: a, control }),
            );
        }
        let mut resets = vec![LatchReset::None];
        for &r in &ctrls {
            for value in [false, true] {
                resets.push(LatchReset::Async { arst: r, value });
            }
            for &sp in &[Polarity::Pos, Polarity::Neg] {
                resets.push(LatchReset::SetClr(sr(b, c, sp, r.polarity)));
            }
            kinds.push(FfKind::Sr(sr(b, c, clk.polarity, r.polarity)));
        }
        kinds.extend(resets.into_iter().map(|reset| FfKind::Dlatch {
            en: clk,
            d: a,
            reset,
        }));
    }
    let q = sig();
    kinds.into_iter().map(|kind| Ff { q, kind }).collect()
}

#[test]
fn every_gate_level_register_survives_its_name() {
    let mut m = Module::new("m");
    let ffs = all_bit_ffs(&mut m);
    let mut names = std::collections::HashSet::new();
    for ff in ffs {
        let kind = CellKind::FfGate(ff);
        names.insert(kind.type_name().into_owned());
        let back = CellKind::from_generic(kind.to_generic()).unwrap();
        assert_eq!(back, kind, "{}", kind.type_name());
    }
    // yosys' simcells.v has 129 storage cells: all reachable, no two
    // kinds sharing a name.
    assert_eq!(names.len(), 129);
}

#[test]
fn string_parameters_that_look_like_bits_get_a_space() {
    let cases = [
        (ParamValue::Str("1".into()), "\"1 \""),
        (ParamValue::Str(String::new()), "\" \""),
        (ParamValue::Str("datac".into()), "\"datac\""),
        (ParamValue::Str("01 x".into()), "\"01 x\""),
        (ParamValue::Bits(Const::parse("10x").unwrap()), "\"10x\""),
        (ParamValue::Bits(Const::default()), "\"\""),
    ];
    for (value, text) in cases {
        let mut m = Module::new("m");
        let y = m.new_sig(1);
        let mut w = Wire::new("y", y.clone());
        w.port = Some(Dir::Out);
        m.wires.push(w);
        m.attributes.insert("p".into(), value.clone());
        let d = Design { modules: vec![m] };
        let mut out = Vec::new();
        write_json(&d, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains(&format!("\"p\": {text}")), "{value:?}: {s}");
        assert_eq!(read_json(s.as_bytes()).unwrap(), d);
    }
}

#[test]
fn const_ints_round_trip() {
    for v in [0i64, 1, -1, 5, -6, i32::MAX as i64, i32::MIN as i64] {
        assert_eq!(Const::from_int(v, 32).as_i64(), Some(v));
    }
    assert_eq!(Const::parse("0101").unwrap().as_u64(), Some(5));
    assert_eq!(Const::parse("01x1").unwrap().as_u64(), None);
    assert_eq!(Const::parse("0110").unwrap().to_string(), "0110");
}

#[test]
fn connectivity_finds_drivers_and_users() {
    let mut m = Module::new("m");
    let (a, b, s) = (m.new_sig(1), m.new_sig(1), m.new_sig(1));
    let y = m.new_sig(1);
    for (name, bits, dir) in [
        ("a", &a, Dir::In),
        ("b", &b, Dir::In),
        ("s", &s, Dir::In),
        ("y", &y, Dir::Out),
    ] {
        let mut w = Wire::new(name, bits.clone());
        w.port = Some(dir);
        m.wires.push(w);
    }
    let n = m.new_sig(1);
    let not = Gate::new(GateOp::Not, &[a[0]], n[0]).unwrap();
    m.add_cell("not", CellKind::Gate(not));
    m.add_cell(
        "mux",
        CellKind::Mux(Mux {
            a: n.clone(),
            b: b.clone(),
            s: s[0],
            y: y.clone(),
        }),
    );
    m.check().unwrap();
    let c = Connectivity::new(&m);
    let net = |s: &[SigBit]| s[0].net().unwrap();
    assert_eq!(
        c.driver(net(&n)),
        Some(Endpoint::Cell {
            cell: 0,
            port: 1,
            bit: 0
        })
    );
    assert_eq!(
        c.users(net(&n)),
        &[Endpoint::Cell {
            cell: 1,
            port: 0,
            bit: 0
        }]
    );
    assert_eq!(c.driver(net(&a)), Some(Endpoint::Port { wire: 0, bit: 0 }));
    assert_eq!(c.users(net(&y)), &[Endpoint::Port { wire: 3, bit: 0 }]);
    let (port, bit) = Connectivity::port(&m, c.users(net(&s))[0]).unwrap();
    assert_eq!((port.name, bit), ("S", 0));
}

#[test]
fn width_mismatches_are_rejected_on_write() {
    let mut m = Module::new("m");
    let (a, b, y) = (m.new_sig(2), m.new_sig(3), m.new_sig(2));
    let s = m.new_net().into();
    m.add_cell("mux", CellKind::Mux(Mux { a, b, s, y }));
    let d = Design { modules: vec![m] };
    let err = write_json(&d, Vec::new()).unwrap_err().to_string();
    assert!(err.contains("B is 3 bits"), "{err}");
}

#[test]
fn constants_and_x_bits_are_signals() {
    let text = br#"{"modules": {"m": {
        "ports": {"y": {"direction": "output", "bits": ["0", "x", 5, "z"]}},
        "cells": {}, "netnames": {}}}}"#;
    let d = read_json(text).unwrap();
    let w = &d.modules[0].wires[0];
    assert_eq!(w.bits[0], SigBit::Const(State::Zero));
    assert_eq!(w.bits[1], SigBit::X);
    assert!(w.bits[2].net().is_some());
    assert_eq!(w.bits[3], SigBit::Z);
}
