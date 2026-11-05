//! `wreduce` for arithmetic: operands lose top bits that don't change
//! their value (zeros of an unsigned operand, copies of a signed one's
//! sign), results lose top bits nothing reads, and a sum's top bits that
//! can only be its carry's extension are wired instead of computed. A
//! carry chain is then only as long as the arithmetic needs.

use chipsmith_netlist::{Binary, BinaryOp, CellKind, Module, Operand, SigBit, UnaryOp};

use crate::rewrite::Subst;

/// Drops the top bits of an operand that its extension would give back.
fn trim(op: &mut Operand) -> bool {
    let before = op.sig.len();
    if op.signed {
        while op.sig.len() >= 2 && op.sig[op.sig.len() - 1] == op.sig[op.sig.len() - 2] {
            op.sig.pop();
        }
    } else {
        while op.sig.last() == Some(&SigBit::ZERO) {
            op.sig.pop();
        }
    }
    op.sig.len() != before
}

fn readers(m: &Module) -> Vec<bool> {
    let mut read = vec![false; m.net_count()];
    for c in &m.cells {
        c.kind.for_each_port(|p| {
            if p.dir.is_none_or(|d| d.reads()) {
                for b in p.bits {
                    if let SigBit::Net(n) = b {
                        read[n.index()] = true;
                    }
                }
            }
        });
    }
    for (_, w, dir) in m.ports() {
        if dir.drives() {
            for b in &w.bits {
                if let SigBit::Net(n) = b {
                    read[n.index()] = true;
                }
            }
        }
    }
    read
}

/// Returns whether anything changed.
pub fn wreduce(m: &mut Module) -> bool {
    let read = readers(m);
    let mut subst = Subst::new(m);
    let mut changed = false;
    let unread = |b: &SigBit| b.net().is_none_or(|n| !read[n.index()]);
    for c in &mut m.cells {
        match &mut c.kind {
            CellKind::Binary(b @ Binary { .. })
                if matches!(b.op, BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul) =>
            {
                // Nobody reads the top of the result: don't compute it.
                while b.y.len() > 1 && unread(b.y.last().expect("nonempty")) {
                    b.y.pop();
                    changed = true;
                }
                let signed = b.a.signed && b.b.signed;
                if !signed {
                    b.a.signed = false;
                    b.b.signed = false;
                }
                changed |= trim(&mut b.a);
                changed |= trim(&mut b.b);
                // Low bits where one operand is 0 make no carry (`A - B`
                // is `A + ~B + 1`: a 0 in B there adds 1 + 1, the carry it
                // gives is the +1 the next bit needed): they are the other
                // operand's bits, and the chain can start above them.
                if b.op != BinaryOp::Mul {
                    while !b.y.is_empty() {
                        let bit = |o: &Operand| {
                            o.sig.first().copied().unwrap_or(if o.signed {
                                SigBit::X
                            } else {
                                SigBit::ZERO
                            })
                        };
                        let (a0, b0) = (bit(&b.a), bit(&b.b));
                        let through = if b0 == SigBit::ZERO {
                            a0
                        } else if a0 == SigBit::ZERO && b.op == BinaryOp::Add {
                            b0
                        } else {
                            break;
                        };
                        if through == SigBit::X
                            || b.a.sig.is_empty() && b.a.signed
                            || b.b.sig.is_empty() && b.b.signed
                        {
                            break;
                        }
                        let y0 = b.y.remove(0);
                        if let SigBit::Net(n) = y0 {
                            subst.set(n, through);
                        }
                        if !b.a.sig.is_empty() {
                            b.a.sig.remove(0);
                        }
                        if !b.b.sig.is_empty() {
                            b.b.sig.remove(0);
                        }
                        changed = true;
                    }
                    // Plus nothing: the other operand.
                    let zero = |o: &Operand| o.sig.iter().all(|&x| x == SigBit::ZERO);
                    if zero(&b.b) || (b.op == BinaryOp::Add && zero(&b.a)) {
                        let other = if zero(&b.b) { &b.a } else { &b.b };
                        let v = crate::techmap::extend(&other.sig, b.y.len(), other.signed);
                        for (y, v) in b.y.drain(..).zip(v) {
                            if let SigBit::Net(n) = y {
                                subst.set(n, v);
                            }
                        }
                        changed = true;
                        continue;
                    }
                }
                // An addition of narrower operands: the bits above its
                // carry are the carry's extension.
                if b.op == BinaryOp::Add {
                    let top = b.a.sig.len().max(b.b.sig.len()) + 1;
                    if b.y.len() > top {
                        let ext = if signed { None } else { Some(SigBit::ZERO) };
                        if let Some(z) = ext {
                            for y in b.y.drain(top..) {
                                if let SigBit::Net(n) = y {
                                    subst.set(n, z);
                                }
                            }
                            changed = true;
                        }
                    }
                }
            }
            CellKind::Unary(u) if u.op == UnaryOp::Neg => {
                while u.y.len() > 1 && unread(u.y.last().expect("nonempty")) {
                    u.y.pop();
                    changed = true;
                }
                changed |= trim(&mut u.a);
            }
            CellKind::Binary(b)
                if matches!(
                    b.op,
                    BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
                ) =>
            {
                let signed = b.a.signed && b.b.signed;
                if !signed {
                    b.a.signed = false;
                    b.b.signed = false;
                }
                // Both keep a bit of their own, so the comparison stays
                // between two numbers of the same kind.
                let (a0, b0) = (b.a.sig.len(), b.b.sig.len());
                trim(&mut b.a);
                trim(&mut b.b);
                if b.a.sig.is_empty() {
                    b.a.sig.push(SigBit::ZERO);
                }
                if b.b.sig.is_empty() {
                    b.b.sig.push(SigBit::ZERO);
                }
                changed |= (a0, b0) != (b.a.sig.len(), b.b.sig.len());
            }
            _ => {}
        }
    }
    subst.apply(m);
    changed
}
