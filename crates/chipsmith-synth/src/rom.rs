//! Constant tables: a `case` on one signal choosing among constants (a
//! ROM in logic, as sha256's round constants are) is a `$pmux` whose
//! selects compare that signal with constants. Lowered as it is, each
//! output bit is an OR of decoded minterms, which a 4-input LUT mapper
//! can't take apart: sha256's 64 words of 32 bits took 372 LUTs. Instead
//! each output bit is a Shannon tree as Quartus builds it: 4-input
//! cofactors of the table, one LUT each and shared between bits that agree
//! on them, under a mux of four in two LUTs (which a mapper doesn't find
//! by itself) per two more key bits.

use chipsmith_netlist::{BinaryOp, CellKind, Const, Lut, Module, SigBit, SigSpec, State};
use rustc_hash::FxHashMap;

/// Narrower tables map to one LUT per bit as they are; wider ones would
/// make big trees.
const MIN_KEY: usize = 5;
const MAX_KEY: usize = 8;

/// Rewrites every constant-table `$pmux` into `$lut`s; returns how many.
pub fn tables(m: &mut Module) -> usize {
    let mut driver: FxHashMap<SigBit, usize> = FxHashMap::default();
    for (i, c) in m.cells.iter().enumerate() {
        if let CellKind::Binary(b) = &c.kind {
            if b.op == BinaryOp::Eq {
                if let Some(&y) = b.y.first() {
                    driver.insert(y, i);
                }
            }
        }
    }
    let mut remove = vec![false; m.cells.len()];
    let mut found = Vec::new();
    for (i, c) in m.cells.iter().enumerate() {
        let CellKind::Pmux(p) = &c.kind else { continue };
        let Some((key, table)) = table(m, &driver, &p.a, &p.b, &p.s) else {
            continue;
        };
        found.push((key, table, p.y.clone()));
        remove[i] = true;
    }
    let n = found.len();
    let mut it = remove.into_iter();
    m.cells.retain(|_| !it.next().expect("one flag per cell"));
    let mut b = Builder {
        namer: crate::rewrite::Namer::new(m, "rom"),
        m,
    };
    for (key, table, y) in found {
        b.build(&key, &table, &y);
    }
    n
}

struct Builder<'a> {
    m: &'a mut Module,
    namer: crate::rewrite::Namer,
}

impl Builder<'_> {
    /// A LUT over `inputs` (at most four) computing `f` of its input value.
    fn lut(&mut self, inputs: &[SigBit], f: impl Fn(usize) -> bool, y: Option<SigBit>) -> SigBit {
        let y = y.unwrap_or_else(|| SigBit::Net(self.m.new_net()));
        let table = (0..1usize << inputs.len())
            .map(|v| State::from(f(v)))
            .collect();
        self.m.add_cell(
            self.namer.fresh(),
            CellKind::Lut(Lut {
                a: inputs.to_vec(),
                y,
                table: Const(table),
            }),
        );
        y
    }

    /// `data[sel]`: muxes of four in two LUTs each (the second reads the
    /// first and the high select: `L1 = s1 ? s0 : (s0 ? d1 : d0)`, then
    /// `s1 ? (L1 ? d3 : d2) : L1`), a mux of two in one.
    fn mux(&mut self, sel: &[SigBit], data: &[SigBit], y: Option<SigBit>) -> SigBit {
        match sel {
            [] => data[0],
            [s] => self.lut(
                &[*s, data[0], data[1]],
                |v| if v & 1 == 1 { v & 4 != 0 } else { v & 2 != 0 },
                y,
            ),
            [s0, s1, rest @ ..] => {
                let fours: Vec<SigBit> = data
                    .chunks(4)
                    .map(|d| {
                        let l1 = self.lut(
                            &[*s0, *s1, d[0], d[1]],
                            |v| {
                                let (s0, s1) = (v & 1 == 1, v & 2 != 0);
                                if s1 {
                                    s0
                                } else if s0 {
                                    v & 8 != 0
                                } else {
                                    v & 4 != 0
                                }
                            },
                            None,
                        );
                        let last = rest.is_empty().then_some(y).flatten();
                        self.lut(
                            &[l1, *s1, d[2], d[3]],
                            |v| {
                                let (l1, s1) = (v & 1 == 1, v & 2 != 0);
                                if s1 {
                                    if l1 {
                                        v & 8 != 0
                                    } else {
                                        v & 4 != 0
                                    }
                                } else {
                                    l1
                                }
                            },
                            last,
                        )
                    })
                    .collect();
                self.mux(rest, &fours, y)
            }
        }
    }

    /// One output bit per column of `table` (indexed by `key`'s value):
    /// 4-input cofactors over the key bits [`cofactor_order`] picks, each
    /// built once, then a mux on the others.
    fn build(&mut self, key: &SigSpec, table: &[Vec<State>], y: &SigSpec) {
        let order = cofactor_order(table, key.len());
        let (lows, highs) = order.split_at(4);
        let low_sigs: Vec<SigBit> = lows.iter().map(|&k| key[k]).collect();
        let high_sigs: Vec<SigBit> = highs.iter().map(|&k| key[k]).collect();
        let mut cofactors: FxHashMap<u16, SigBit> = FxHashMap::default();
        for (j, &yj) in y.iter().enumerate() {
            let mut data = Vec::new();
            for h in 0..1usize << highs.len() {
                let f = cofactor(table, j, lows, highs, h);
                let sig = match trivial(f, &low_sigs) {
                    Some(s) => s,
                    None => match cofactors.get(&f) {
                        Some(&s) => s,
                        None => {
                            let s = self.lut(&low_sigs, |v| f >> v & 1 == 1, None);
                            cofactors.insert(f, s);
                            s
                        }
                    },
                };
                data.push(sig);
            }
            let out = self.mux(&high_sigs, &data, Some(yj));
            if out != yj {
                // No mux (a constant or a shared signal): a buffer LUT.
                self.lut(&[out], |v| v == 1, Some(yj));
            }
        }
    }
}

/// Column `j`'s 4-input function of the `lows` key bits where the `highs`
/// read `h`.
fn cofactor(table: &[Vec<State>], j: usize, lows: &[usize], highs: &[usize], h: usize) -> u16 {
    let mut f = 0u16;
    for l in 0..16usize {
        let v: usize = lows
            .iter()
            .enumerate()
            .map(|(p, &k)| (l >> p & 1) << k)
            .sum::<usize>()
            + highs
                .iter()
                .enumerate()
                .map(|(p, &k)| (h >> p & 1) << k)
                .sum::<usize>();
        f |= ((table[v][j] == State::One) as u16) << l;
    }
    f
}

/// A cofactor that needs no LUT: a constant, or one of its inputs.
fn trivial(f: u16, inputs: &[SigBit]) -> Option<SigBit> {
    match f {
        0 => Some(SigBit::ZERO),
        0xFFFF => Some(SigBit::ONE),
        _ => [0xAAAAu16, 0xCCCC, 0xF0F0, 0xFF00]
            .iter()
            .position(|&v| v == f)
            .map(|p| inputs[p]),
    }
}

/// The key bits in the order the `$lut`s take them: lowering splits on
/// the last ones first, leaving 4-input cofactors over the first four, so
/// those are the four that leave the fewest distinct cofactors over every
/// output bit (shared cofactors are one LUT).
fn cofactor_order(table: &[Vec<State>], n: usize) -> Vec<usize> {
    let width = table.first().map_or(0, Vec::len);
    let mut best: Option<(usize, Vec<usize>)> = None;
    for low in 0u32..1 << n {
        if low.count_ones() != 4 {
            continue;
        }
        let lows: Vec<usize> = (0..n).filter(|&k| low >> k & 1 == 1).collect();
        let highs: Vec<usize> = (0..n).filter(|&k| low >> k & 1 == 0).collect();
        let vars = [SigBit::ZERO; 4];
        let mut seen = std::collections::HashSet::new();
        for j in 0..width {
            for h in 0..1usize << highs.len() {
                let f = cofactor(table, j, &lows, &highs, h);
                if trivial(f, &vars).is_none() {
                    seen.insert(f);
                }
            }
        }
        if best.as_ref().is_none_or(|(c, _)| seen.len() < *c) {
            best = Some((seen.len(), lows.into_iter().chain(highs).collect()));
        }
    }
    best.map_or_else(|| (0..n).collect(), |(_, o)| o)
}

/// The signal the `$pmux` is a table of, and its word for each value of
/// it, if every select is `key == constant` on one key, every word a
/// constant, and no two selects the same value.
fn table(
    m: &Module,
    driver: &FxHashMap<SigBit, usize>,
    a: &SigSpec,
    b: &SigSpec,
    s: &SigSpec,
) -> Option<(SigSpec, Vec<Vec<State>>)> {
    let konst = |x: &[SigBit]| -> Option<Vec<State>> {
        x.iter()
            .map(|b| match b {
                SigBit::Const(v @ (State::Zero | State::One)) => Some(*v),
                _ => None,
            })
            .collect()
    };
    let w = a.len();
    let default = konst(a)?;
    let mut key: Option<SigSpec> = None;
    let mut cases: Vec<(usize, Vec<State>)> = Vec::new();
    for (i, sel) in s.iter().enumerate() {
        let CellKind::Binary(eq) = &m.cells[*driver.get(sel)?].kind else {
            return None;
        };
        // One side the key, the other a constant as wide. Constant bits
        // of the key (zero extension) must agree with the case's, or it
        // never matches; the table is over the others.
        let (k, c) = match (konst(&eq.a.sig), konst(&eq.b.sig)) {
            (None, Some(c)) => (&eq.a, c),
            (Some(c), None) => (&eq.b, c),
            _ => return None,
        };
        if eq.a.sig.len() != eq.b.sig.len() || eq.y.iter().skip(1).any(|&y| y != SigBit::ZERO) {
            return None;
        }
        let vars: SigSpec = k
            .sig
            .iter()
            .copied()
            .filter(|b| b.net().is_some())
            .collect();
        if key.as_ref().is_some_and(|x| *x != vars) {
            return None;
        }
        key = Some(vars);
        let matches = k.sig.iter().zip(&c).all(|(kb, &cb)| match kb {
            SigBit::Const(v) => *v == cb,
            SigBit::Net(_) => true,
        });
        if !matches {
            continue;
        }
        let value = k
            .sig
            .iter()
            .zip(&c)
            .filter(|(kb, _)| kb.net().is_some())
            .enumerate()
            .map(|(bit, (_, &v))| (v == State::One) as usize * (1 << bit))
            .sum();
        if cases.iter().any(|&(v, _)| v == value) {
            return None;
        }
        cases.push((value, konst(&b[i * w..(i + 1) * w])?));
    }
    let key = key?;
    if !(MIN_KEY..=MAX_KEY).contains(&key.len()) {
        return None;
    }
    let mut table = vec![default; 1 << key.len()];
    for (v, word) in cases {
        table[v] = word;
    }
    Some((key, table))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_netlist::{Binary, Operand, Pmux};

    #[test]
    fn a_case_of_constants_becomes_luts() {
        let mut m = Module::new("top");
        let key = m.new_sig(5);
        let y = m.new_sig(2);
        let mut s = Vec::new();
        let mut b = Vec::new();
        for v in 0..20usize {
            let sel = m.new_sig(1);
            let c: SigSpec = (0..5)
                .map(|bit| SigBit::Const(State::from(v >> bit & 1 == 1)))
                .collect();
            m.add_cell(
                format!("eq{v}"),
                CellKind::Binary(Binary {
                    op: BinaryOp::Eq,
                    a: Operand {
                        sig: key.clone(),
                        signed: false,
                    },
                    b: Operand {
                        sig: c,
                        signed: false,
                    },
                    y: sel.clone(),
                }),
            );
            s.push(sel[0]);
            b.extend([
                SigBit::Const(State::from(v % 3 == 0)),
                SigBit::Const(State::from(v % 2 == 1)),
            ]);
        }
        m.add_cell(
            "pmux",
            CellKind::Pmux(Pmux {
                a: vec![SigBit::ZERO, SigBit::ONE],
                b,
                s,
                y: y.clone(),
            }),
        );
        assert_eq!(tables(&mut m), 1);
        let luts: Vec<Lut> = m
            .cells
            .iter()
            .filter_map(|c| match &c.kind {
                CellKind::Lut(l) => Some(l.clone()),
                _ => None,
            })
            .collect();
        assert!(luts.iter().all(|l| l.a.len() <= 4), "every LUT fits an LE");
        for v in 0..32usize {
            let mut val: FxHashMap<SigBit, bool> = FxHashMap::default();
            val.insert(SigBit::ZERO, false);
            val.insert(SigBit::ONE, true);
            for (bit, &k) in key.iter().enumerate() {
                val.insert(k, v >> bit & 1 == 1);
            }
            // The LUTs in any order: repeat until every output is known.
            while luts.iter().any(|l| !val.contains_key(&l.y)) {
                for l in &luts {
                    if let Some(i) = l.a.iter().enumerate().try_fold(0usize, |acc, (p, b)| {
                        val.get(b).map(|&x| acc | (x as usize) << p)
                    }) {
                        val.insert(l.y, l.table.0[i] == State::One);
                    }
                }
            }
            assert_eq!(val[&y[0]], v < 20 && v % 3 == 0, "bit 0 at {v}");
            assert_eq!(
                val[&y[1]],
                if v < 20 { v % 2 == 1 } else { true },
                "bit 1 at {v}"
            );
        }
    }

    /// The gain itself: a 64-word, 32-bit table of arbitrary constants
    /// maps to about 6 LUTs a bit (Quartus: 189 for sha256's), where its
    /// sum of decoded minterms took 372.
    #[test]
    fn a_six_bit_table_maps_to_about_six_luts_a_bit() {
        use chipsmith_netlist::{Dir, Wire};
        let mut m = Module::new("top");
        let key = m.new_sig(6);
        let y = m.new_sig(32);
        let mut w = Wire::new("key", key.clone());
        w.port = Some(Dir::In);
        m.wires.push(w);
        let mut w = Wire::new("y", y.clone());
        w.port = Some(Dir::Out);
        m.wires.push(w);
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let (mut s, mut b) = (Vec::new(), Vec::new());
        for v in 0..64usize {
            let sel = m.new_sig(1);
            let c: SigSpec = (0..6)
                .map(|bit| SigBit::Const(State::from(v >> bit & 1 == 1)))
                .collect();
            m.add_cell(
                format!("eq{v}"),
                CellKind::Binary(Binary {
                    op: BinaryOp::Eq,
                    a: Operand {
                        sig: key.clone(),
                        signed: false,
                    },
                    b: Operand {
                        sig: c,
                        signed: false,
                    },
                    y: sel.clone(),
                }),
            );
            s.push(sel[0]);
            for _ in 0..32 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                b.push(SigBit::Const(State::from(rng & 1 == 1)));
            }
        }
        m.add_cell(
            "pmux",
            CellKind::Pmux(Pmux {
                a: vec![SigBit::ZERO; 32],
                b,
                s,
                y,
            }),
        );
        assert_eq!(tables(&mut m), 1);
        crate::techmap::lower(&mut m, crate::techmap::is_lowerable).unwrap();
        let stats = crate::lutmap::map_module(&mut m).unwrap();
        assert!(stats.luts <= 200, "{} LUTs", stats.luts);
    }
}
