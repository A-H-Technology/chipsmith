//! Two additions under a multiplexer that share an operand become one
//! addition of a multiplexed operand (Quartus' resource sharing):
//! `s ? x + b : x + a` is `x + (s ? b : a)`, one carry chain and a LUT a
//! bit for the operand instead of two chains and a LUT a bit for the
//! result. picorv32's next PC is `current_pc + 4` or `current_pc +
//! decoded_imm_j`.
//!
//! It runs before `wreduce`, while both sums are as wide as written:
//! narrowed, they no longer match (the increment by 4 loses bits 0 and 1,
//! the immediate bit 0, so the multiplexer reads a slice of each).

use std::collections::HashMap;

use chipsmith_netlist::{Binary, BinaryOp, CellKind, Dir, Module, Mux, Net, Operand, SigBit};

use crate::rewrite::Namer;

/// Where a sum's result bit is: the sum's cell and the bit.
type SumBit = (usize, usize);

fn sum_of(m: &Module, i: usize) -> &Binary {
    match &m.cells[i].kind {
        CellKind::Binary(b) => b,
        _ => unreachable!("a sum"),
    }
}

/// Shares every pair of sums a multiplexer chooses between whose results
/// nothing else reads. Returns how many pairs.
pub fn share_sums(m: &mut Module) -> usize {
    let mut uses: HashMap<Net, usize> = HashMap::new();
    let mut sum_bit: HashMap<Net, SumBit> = HashMap::new();
    for (i, c) in m.cells.iter().enumerate() {
        let sum = matches!(&c.kind, CellKind::Binary(b) if b.op == BinaryOp::Add);
        c.kind.for_each_port(|p| {
            for (k, n) in p
                .bits
                .iter()
                .enumerate()
                .filter_map(|(k, b)| Some((k, b.net()?)))
            {
                if p.dir == Some(Dir::Out) {
                    if sum {
                        sum_bit.insert(n, (i, k));
                    }
                } else {
                    *uses.entry(n).or_default() += 1;
                }
            }
        });
    }
    for (_, w, _) in m.ports() {
        for n in w.bits.iter().filter_map(|b| b.net()) {
            *uses.entry(n).or_default() += 1;
        }
    }
    let sum = |i: usize| sum_of(m, i);
    // The sum a multiplexer leg is bits lo.. of, read nowhere else.
    let leg = |bits: &[SigBit]| -> Option<(usize, usize)> {
        let &(i, lo) = sum_bit.get(&bits.first()?.net()?)?;
        let ok = bits
            .iter()
            .enumerate()
            .all(|(k, b)| b.net().and_then(|n| sum_bit.get(&n)) == Some(&(i, lo + k)));
        let y = &sum(i).y;
        let alone = y.iter().enumerate().all(|(k, b)| {
            let n = b.net().map_or(0, |n| uses.get(&n).copied().unwrap_or(0));
            if (lo..lo + bits.len()).contains(&k) {
                n == 1
            } else {
                n == 0
            }
        });
        (ok && alone).then_some((i, lo))
    };
    // (multiplexer, sum on A, sum on B, which operand of each is shared,
    // the A leg's first bit)
    let mut pairs = Vec::new();
    let mut claimed = vec![false; m.cells.len()];
    for (i, c) in m.cells.iter().enumerate() {
        let CellKind::Mux(mx) = &c.kind else { continue };
        let (Some((p, lo)), Some((q, lo_q))) = (leg(&mx.a), leg(&mx.b)) else {
            continue;
        };
        if p == q || lo != lo_q || claimed[p] || claimed[q] {
            continue;
        }
        let (sp, sq) = (sum(p), sum(q));
        let signed = |b: &Binary| b.a.signed && b.b.signed;
        if sp.y.len() != sq.y.len() || signed(sp) != signed(sq) {
            continue;
        }
        let ops = |b: &Binary| [b.a.sig.clone(), b.b.sig.clone()];
        let shared = (0..2)
            .flat_map(|x| (0..2).map(move |y| (x, y)))
            .find(|&(x, y)| ops(sp)[x] == ops(sq)[y]);
        let Some((x, y)) = shared else { continue };
        claimed[p] = true;
        claimed[q] = true;
        pairs.push((i, p, q, x, y, lo));
    }
    if pairs.is_empty() {
        return 0;
    }
    let mut namer = Namer::new(m, "share");
    let mut dead = vec![false; m.cells.len()];
    let mut new = Vec::new();
    for &(i, p, q, x, y, lo) in &pairs {
        let (sp, sq) = (sum_of(m, p), sum_of(m, q));
        let CellKind::Mux(mx) = &m.cells[i].kind else {
            unreachable!("a multiplexer")
        };
        let signed = sp.a.signed && sp.b.signed;
        let w = sp.y.len();
        let other = |b: &Binary, shared: usize| {
            let o = if shared == 0 { &b.b.sig } else { &b.a.sig };
            crate::techmap::extend(o, w, signed)
        };
        let shared = if x == 0 { sp.a.clone() } else { sp.b.clone() };
        let (a, b, s, out) = (other(sp, x), other(sq, y), mx.s, mx.y.clone());
        dead[i] = true;
        dead[p] = true;
        dead[q] = true;
        let operand = m.new_sig(w);
        let mut result = m.new_sig(w);
        result[lo..lo + out.len()].copy_from_slice(&out);
        new.push(CellKind::Mux(Mux {
            a,
            b,
            s,
            y: operand.clone(),
        }));
        new.push(CellKind::Binary(Binary {
            op: BinaryOp::Add,
            a: shared,
            b: Operand {
                sig: operand,
                signed,
            },
            y: result,
        }));
    }
    let mut i = 0;
    m.cells.retain(|_| {
        i += 1;
        !dead[i - 1]
    });
    for k in new {
        let name = namer.fresh();
        m.add_cell(name, k);
    }
    pairs.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_netlist::State;

    fn sums(m: &Module) -> usize {
        m.cells
            .iter()
            .filter(|c| matches!(&c.kind, CellKind::Binary(b) if b.op == BinaryOp::Add))
            .count()
    }

    fn add(m: &mut Module, a: &[SigBit], b: &[SigBit]) -> Vec<SigBit> {
        let y = m.new_sig(a.len());
        m.add_cell(
            format!("add{}", m.cells.len()),
            CellKind::Binary(Binary {
                op: BinaryOp::Add,
                a: Operand {
                    sig: a.to_vec(),
                    signed: false,
                },
                b: Operand {
                    sig: b.to_vec(),
                    signed: false,
                },
                y: y.clone(),
            }),
        );
        y
    }

    /// picorv32's next PC: `jal ? pc + imm : pc + 4`, read from bit 2 up.
    #[test]
    fn a_multiplexer_of_two_sums_shares_one() {
        let mut m = Module::new("top");
        let pc = m.new_sig(32);
        let imm = m.new_sig(32);
        let four: Vec<SigBit> = (0..32)
            .map(|i| SigBit::Const(State::from(i == 2)))
            .collect();
        let s = m.new_sig(1)[0];
        let inc = add(&mut m, &pc, &four);
        let jump = add(&mut m, &pc, &imm);
        let y = m.new_sig(30);
        m.add_cell(
            "mux",
            CellKind::Mux(Mux {
                a: inc[2..].to_vec(),
                b: jump[2..].to_vec(),
                s,
                y,
            }),
        );
        assert_eq!(share_sums(&mut m), 1);
        assert_eq!(sums(&m), 1);

        // A sum read elsewhere too stays.
        let mut m = Module::new("top");
        let (pc, imm, s) = (m.new_sig(8), m.new_sig(8), m.new_sig(1)[0]);
        let one: Vec<SigBit> = (0..8).map(|i| SigBit::Const(State::from(i == 0))).collect();
        let inc = add(&mut m, &pc, &one);
        let jump = add(&mut m, &pc, &imm);
        let y = m.new_sig(8);
        m.add_cell(
            "mux",
            CellKind::Mux(Mux {
                a: inc.clone(),
                b: jump,
                s,
                y,
            }),
        );
        let other = m.new_sig(8);
        m.add_cell(
            "also",
            CellKind::Mux(Mux {
                a: inc,
                b: pc,
                s,
                y: other,
            }),
        );
        assert_eq!(share_sums(&mut m), 0);
    }
}
