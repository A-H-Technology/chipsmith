//! `$mul` onto `A5_MUL`, a DSP block's multiplier (README (z11)), as
//! tools/a5pnr/yosys/mul_map.v maps it for yosys: a product of two operands
//! of up to 27 bits, both signed or both unsigned, 18x18 when both fit, the
//! operands extended to the multiplier's width (every pin carries a signal)
//! and the product cut or extended to Y. Small and mixed-sign products stay
//! for the gates. a5pnr then takes the flops the product goes straight into
//! as the block's output register.

use std::collections::BTreeMap;

use chipsmith_netlist::{
    BinaryOp, CellKind, Const, Dir, Generic, GenericPort, Module, ParamValue, SigBit, SigSpec,
    State,
};
use chipsmith_synth::rewrite::Subst;
use chipsmith_synth::techmap::extend;

pub const MUL: &str = "A5_MUL";

pub fn map(m: &mut Module) -> usize {
    let cells = std::mem::take(&mut m.cells);
    let mut subst = Subst::new(m);
    let mut n = 0;
    for c in cells {
        let CellKind::Binary(b) = &c.kind else {
            m.cells.push(c);
            continue;
        };
        let (wa, wb) = (b.a.sig.len(), b.b.sig.len());
        if b.op != BinaryOp::Mul
            || b.a.signed != b.b.signed
            || wa > 27
            || wb > 27
            || wa < 3
            || wb < 3
            || wa * wb < 36
        {
            m.cells.push(c);
            continue;
        }
        let signed = b.a.signed;
        let wide = wa > 18 || wb > 18;
        let w = if wide { 27 } else { 18 };
        let pad = |s: &SigSpec| -> SigSpec {
            let mut v: Vec<SigBit> = extend(s, w, signed).to_vec();
            v.resize(27, SigBit::ZERO);
            v
        };
        let p: SigSpec = (0..54).map(|_| SigBit::Net(m.new_net())).collect();
        let flag = |v: bool| ParamValue::Bits(Const(vec![State::from(v)]));
        let port = |name: &str, dir, sig: SigSpec| GenericPort {
            name: name.into(),
            dir: Some(dir),
            sig,
        };
        let kind = CellKind::Other(Generic {
            ty: MUL.into(),
            params: BTreeMap::from([("SIGNED".into(), flag(signed)), ("WIDE".into(), flag(wide))]),
            ports: vec![
                port("A", Dir::In, pad(&b.a.sig)),
                port("B", Dir::In, pad(&b.b.sig)),
                port("P", Dir::Out, p.clone()),
            ],
        });
        let y = extend(&p[..2 * w], b.y.len(), signed);
        for (o, v) in b.y.iter().zip(y.iter()) {
            if let SigBit::Net(net) = o {
                subst.set(*net, *v);
            }
        }
        m.cells.push(chipsmith_netlist::Cell { kind, ..c });
        n += 1;
    }
    subst.apply(m);
    n
}
