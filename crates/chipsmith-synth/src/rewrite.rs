//! What every pass needs to change a module: merging nets, naming new
//! cells, registers' power-up values, and building gates.
//!
//! The IR has no `connect()` (ADR-0005): a pass that makes one net the same
//! as another signal records it in a [`Subst`] and rewrites the module once
//! at the end, rather than keeping an alias map in sync while it works.

use std::collections::HashSet;

use chipsmith_netlist::{
    attr, CellKind, Const, Gate, GateOp, Module, Net, ParamValue, SigBit, SigSpec, State, Wire,
};
use rustc_hash::FxHashMap;

/// Nets to replace by other signals, applied to the whole module at once.
pub struct Subst {
    to: Vec<Option<SigBit>>,
}

impl Subst {
    pub fn new(m: &Module) -> Subst {
        Subst {
            to: vec![None; m.net_count()],
        }
    }

    /// From now on `from` is `to`. `from` must be a net nothing else will
    /// drive: its driver is being removed or rewired.
    pub fn set(&mut self, from: Net, to: SigBit) {
        if self.resolve(to) == SigBit::Net(from) {
            return;
        }
        if from.index() >= self.to.len() {
            self.to.resize(from.index() + 1, None);
        }
        self.to[from.index()] = Some(to);
    }

    pub fn is_empty(&self) -> bool {
        self.to.iter().all(Option::is_none)
    }

    /// What `b` ends up as, following chains of replacements.
    pub fn resolve(&self, mut b: SigBit) -> SigBit {
        while let SigBit::Net(n) = b {
            match self.to.get(n.index()).copied().flatten() {
                Some(next) => b = next,
                None => break,
            }
        }
        b
    }

    pub fn resolve_sig(&self, s: &[SigBit]) -> SigSpec {
        s.iter().map(|&b| self.resolve(b)).collect()
    }

    pub fn apply(self, m: &mut Module) {
        if self.is_empty() {
            return;
        }
        m.map_bits(|b| self.resolve(b));
        // A power-up value on a bit that is now a constant means nothing
        // (and yosys rejects one that disagrees with the constant).
        for w in &mut m.wires {
            let Some(ParamValue::Bits(c)) = w.attributes.get_mut(attr::INIT) else {
                continue;
            };
            for (b, s) in w.bits.iter().zip(c.0.iter_mut()) {
                if matches!(b, SigBit::Const(_)) {
                    *s = State::X;
                }
            }
            if c.0.iter().all(|s| *s == State::X) {
                w.attributes.remove(attr::INIT);
            }
        }
    }
}

/// Fresh cell names: `$chipsmith$<pass>$<n>`, checked once against the
/// names in use instead of per cell (`Module::fresh_name` scans the module).
pub struct Namer {
    prefix: String,
    next: usize,
    used: HashSet<String>,
}

impl Namer {
    pub fn new(m: &Module, pass: &str) -> Namer {
        Namer {
            prefix: format!("$chipsmith${pass}$"),
            next: 0,
            used: m
                .cells
                .iter()
                .map(|c| c.name.clone())
                .filter(|n| n.starts_with("$chipsmith$"))
                .collect(),
        }
    }

    pub fn fresh(&mut self) -> String {
        loop {
            let name = format!("{}{}", self.prefix, self.next);
            self.next += 1;
            if !self.used.contains(&name) {
                return name;
            }
        }
    }
}

/// Registers' power-up values, which RTLIL keeps as `init` attributes on
/// the wires naming their outputs.
pub fn inits(m: &Module) -> FxHashMap<Net, State> {
    let mut out = FxHashMap::default();
    for w in &m.wires {
        let Some(ParamValue::Bits(c)) = w.attributes.get(attr::INIT) else {
            continue;
        };
        for (b, s) in w.bits.iter().zip(&c.0) {
            if let (SigBit::Net(n), State::Zero | State::One) = (b, s) {
                out.insert(*n, *s);
            }
        }
    }
    out
}

/// Replaces every `init` attribute: `values` is the whole new set. Wires
/// keep the bits they name; a net no wire names gets a `$init` wire of its
/// own.
pub fn set_inits(m: &mut Module, values: &FxHashMap<Net, State>) {
    let mut named = HashSet::new();
    for w in &mut m.wires {
        w.attributes.remove(attr::INIT);
        let init: Vec<State> = w
            .bits
            .iter()
            .map(|b| match b {
                SigBit::Net(n) => values.get(n).copied().unwrap_or(State::X),
                SigBit::Const(_) => State::X,
            })
            .collect();
        if init.iter().any(|s| *s != State::X) {
            for b in &w.bits {
                if let SigBit::Net(n) = b {
                    named.insert(*n);
                }
            }
            w.attributes
                .insert(attr::INIT.into(), ParamValue::Bits(Const(init)));
        }
    }
    let mut rest: Vec<(&Net, &State)> = values.iter().filter(|(n, _)| !named.contains(n)).collect();
    rest.sort();
    let mut used: HashSet<String> = m.wires.iter().map(|w| w.name.clone()).collect();
    let mut i = 0;
    for (n, s) in rest {
        let name = loop {
            let name = format!("$chipsmith$init${i}");
            i += 1;
            if used.insert(name.clone()) {
                break name;
            }
        };
        let mut w = Wire::new(name, vec![SigBit::Net(*n)]);
        w.attributes
            .insert(attr::INIT.into(), ParamValue::Bits(Const(vec![*s])));
        m.wires.push(w);
    }
}

/// Adds gates to a module, folding constants and trivial cases as it goes
/// (what `opt_expr` would do right after `simplemap`), so lowering a word
/// with constant bits doesn't leave gates behind for a later pass to clean.
/// `x` inputs are kept as `x`: whether to read one as 0 is the caller's
/// choice.
pub struct Gates<'m> {
    pub m: &'m mut Module,
    namer: Namer,
}

impl<'m> Gates<'m> {
    pub fn new(m: &'m mut Module, pass: &str) -> Gates<'m> {
        let namer = Namer::new(m, pass);
        Gates { m, namer }
    }

    pub fn fresh_name(&mut self) -> String {
        self.namer.fresh()
    }

    pub fn add(&mut self, kind: CellKind) {
        let name = self.namer.fresh();
        self.m.add_cell(name, kind);
    }

    /// A gate driving a fresh net.
    fn gate(&mut self, op: GateOp, inputs: &[SigBit]) -> SigBit {
        let y = SigBit::Net(self.m.new_net());
        let g = Gate::new(op, inputs, y).expect("arity per op");
        self.add(CellKind::Gate(g));
        y
    }

    pub fn not(&mut self, a: SigBit) -> SigBit {
        match a {
            SigBit::Const(State::Zero) => SigBit::ONE,
            SigBit::Const(State::One) => SigBit::ZERO,
            SigBit::Const(_) => SigBit::X,
            _ => self.gate(GateOp::Not, &[a]),
        }
    }

    pub fn and(&mut self, a: SigBit, b: SigBit) -> SigBit {
        match (a, b) {
            (SigBit::ZERO, _) | (_, SigBit::ZERO) => SigBit::ZERO,
            (SigBit::ONE, x) | (x, SigBit::ONE) => x,
            _ if a == b => a,
            _ => self.gate(GateOp::And, &[a, b]),
        }
    }

    pub fn or(&mut self, a: SigBit, b: SigBit) -> SigBit {
        match (a, b) {
            (SigBit::ONE, _) | (_, SigBit::ONE) => SigBit::ONE,
            (SigBit::ZERO, x) | (x, SigBit::ZERO) => x,
            _ if a == b => a,
            _ => self.gate(GateOp::Or, &[a, b]),
        }
    }

    pub fn xor(&mut self, a: SigBit, b: SigBit) -> SigBit {
        match (a, b) {
            (SigBit::ZERO, x) | (x, SigBit::ZERO) => x,
            (SigBit::ONE, x) | (x, SigBit::ONE) => self.not(x),
            _ if a == b => SigBit::ZERO,
            _ => self.gate(GateOp::Xor, &[a, b]),
        }
    }

    pub fn xnor(&mut self, a: SigBit, b: SigBit) -> SigBit {
        match (a, b) {
            (SigBit::Const(_), _) | (_, SigBit::Const(_)) => {
                let x = self.xor(a, b);
                self.not(x)
            }
            _ if a == b => SigBit::ONE,
            _ => self.gate(GateOp::Xnor, &[a, b]),
        }
    }

    /// `s ? b : a`.
    pub fn mux(&mut self, a: SigBit, b: SigBit, s: SigBit) -> SigBit {
        match s {
            SigBit::ZERO => a,
            SigBit::ONE => b,
            _ if a == b => a,
            // `opt_expr -mux_undef`: an x input may as well be the other.
            _ if a == SigBit::X => b,
            _ if b == SigBit::X => a,
            _ => match (a, b) {
                (SigBit::ZERO, SigBit::ONE) => s,
                (SigBit::ZERO, _) => self.and(s, b),
                (_, SigBit::ONE) => self.or(s, a),
                _ => self.gate(GateOp::Mux, &[a, b, s]),
            },
        }
    }

    /// A balanced tree of `op` over `bits`, pairing neighbours as
    /// `simplemap` does; `empty` for no bits.
    pub fn reduce(&mut self, op: GateOp, bits: &[SigBit], empty: SigBit) -> SigBit {
        if bits.is_empty() {
            return empty;
        }
        let mut level: SigSpec = bits.to_vec();
        while level.len() > 1 {
            level = level
                .chunks(2)
                .map(|p| match *p {
                    [a, b] => match op {
                        GateOp::And => self.and(a, b),
                        GateOp::Or => self.or(a, b),
                        GateOp::Xor => self.xor(a, b),
                        _ => unreachable!("reductions are AND, OR, XOR"),
                    },
                    [a] => a,
                    _ => unreachable!(),
                })
                .collect();
        }
        level[0]
    }

    pub fn reduce_or(&mut self, bits: &[SigBit]) -> SigBit {
        self.reduce(GateOp::Or, bits, SigBit::ZERO)
    }

    pub fn reduce_and(&mut self, bits: &[SigBit]) -> SigBit {
        self.reduce(GateOp::And, bits, SigBit::ONE)
    }
}

/// Gives every register bit without a power-up value one: what its
/// asynchronous reset sets, else 0. An undefined power-up value may be
/// anything, so this refines the design; deciding it once, before any pass
/// reasons about power-up, keeps every later pass (and the LE register,
/// which powers up 0, inverted where it must hold a 1) agreeing on it.
pub fn define_inits(m: &mut Module) {
    let mut init = inits(m);
    let before = init.len();
    for c in &m.cells {
        let mut one = |q: SigBit, value: State| {
            if let SigBit::Net(n) = q {
                init.entry(n).or_insert(match value {
                    State::One => State::One,
                    _ => State::Zero,
                });
            }
        };
        match &c.kind {
            CellKind::Ff(ff) => {
                let value = |i: usize| match &ff.kind {
                    chipsmith_netlist::FfKind::Dff {
                        control: chipsmith_netlist::DffControl::Async { value, .. },
                        ..
                    } => value.0[i],
                    _ => State::Zero,
                };
                for (i, q) in ff.q.iter().enumerate() {
                    one(*q, value(i));
                }
            }
            CellKind::FfGate(ff) => {
                let value = match &ff.kind {
                    chipsmith_netlist::FfKind::Dff {
                        control: chipsmith_netlist::DffControl::Async { value, .. },
                        ..
                    } => State::from(*value),
                    _ => State::Zero,
                };
                one(ff.q, value);
            }
            _ => {}
        }
    }
    if init.len() != before {
        set_inits(m, &init);
    }
}
