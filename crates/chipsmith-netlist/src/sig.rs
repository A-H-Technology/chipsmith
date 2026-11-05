//! Signals: the bits cells and wires connect, and constants.

use std::fmt;

/// One bit of a constant, RTLIL's `State`. `x` is "any value" and `z`
/// "undriven"; RTLIL's `-` and `m` never reach a netlist file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum State {
    Zero,
    One,
    X,
    Z,
}

impl State {
    pub fn parse(c: char) -> Option<State> {
        Some(match c {
            '0' => State::Zero,
            '1' => State::One,
            'x' => State::X,
            'z' => State::Z,
            _ => return None,
        })
    }

    pub fn char(self) -> char {
        match self {
            State::Zero => '0',
            State::One => '1',
            State::X => 'x',
            State::Z => 'z',
        }
    }
}

impl From<bool> for State {
    fn from(b: bool) -> State {
        if b {
            State::One
        } else {
            State::Zero
        }
    }
}

/// A constant of any width, least significant bit first (RTLIL's
/// `Const`, without the string flag: string values are
/// [`ParamValue::Str`](crate::ParamValue::Str)).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Const(pub Vec<State>);

impl Const {
    /// `width` bits of `value`, two's complement.
    pub fn from_int(value: i64, width: usize) -> Const {
        Const(
            (0..width)
                .map(|i| State::from(i < 64 && (value >> i) & 1 == 1 || i >= 64 && value < 0))
                .collect(),
        )
    }

    pub fn filled(state: State, width: usize) -> Const {
        Const(vec![state; width])
    }

    /// yosys' text form, most significant bit first (`"0101"`). `None` if
    /// a character isn't `01xz`.
    pub fn parse(s: &str) -> Option<Const> {
        s.chars()
            .rev()
            .map(State::parse)
            .collect::<Option<_>>()
            .map(Const)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The value as an unsigned integer, if every bit is 0 or 1 and it
    /// fits.
    pub fn as_u64(&self) -> Option<u64> {
        let mut v = 0u64;
        for (i, s) in self.0.iter().enumerate() {
            match s {
                State::Zero => {}
                State::One if i < 64 => v |= 1 << i,
                _ => return None,
            }
        }
        Some(v)
    }

    /// The value as a two's complement integer of this width.
    pub fn as_i64(&self) -> Option<i64> {
        let v = self.as_u64()?;
        let w = self.len();
        Some(if (1..64).contains(&w) && self.0[w - 1] == State::One {
            v as i64 - (1i64 << w)
        } else {
            v as i64
        })
    }

    /// RTLIL's `as_bool`: any bit set. Undefined bits count as 0.
    pub fn as_bool(&self) -> bool {
        self.0.contains(&State::One)
    }
}

impl fmt::Display for Const {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0
            .iter()
            .rev()
            .try_for_each(|s| write!(f, "{}", s.char()))
    }
}

/// One bit of a module's connectivity. Bits are canonical, as in yosys'
/// JSON (and unlike RTLIL's wires plus `connect()`s): two cell ports on the
/// same `Net` are connected, so a driver/user index needs no alias
/// resolution. Allocated by [`Module::new_net`](crate::Module::new_net).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Net(pub(crate) u32);

impl Net {
    /// Dense, from 0: usable as an index into per-net tables.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// What a port or wire bit is connected to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SigBit {
    Net(Net),
    Const(State),
}

impl SigBit {
    pub const ZERO: SigBit = SigBit::Const(State::Zero);
    pub const ONE: SigBit = SigBit::Const(State::One);
    pub const X: SigBit = SigBit::Const(State::X);
    pub const Z: SigBit = SigBit::Const(State::Z);

    pub fn net(self) -> Option<Net> {
        match self {
            SigBit::Net(n) => Some(n),
            SigBit::Const(_) => None,
        }
    }
}

impl From<Net> for SigBit {
    fn from(n: Net) -> SigBit {
        SigBit::Net(n)
    }
}

impl From<State> for SigBit {
    fn from(s: State) -> SigBit {
        SigBit::Const(s)
    }
}

/// A multi-bit signal, least significant bit first. Concatenation and
/// slicing are plain `Vec` operations, which is why there's no `$concat` or
/// `$slice` cell to model.
pub type SigSpec = Vec<SigBit>;

/// The sense of a control input (clock edge, enable, reset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Polarity {
    /// Rising edge, active high.
    Pos,
    /// Falling edge, active low.
    Neg,
}

impl Polarity {
    /// RTLIL's `*_POLARITY` parameters: 1 is positive.
    pub fn from_bool(positive: bool) -> Polarity {
        if positive {
            Polarity::Pos
        } else {
            Polarity::Neg
        }
    }

    pub fn is_pos(self) -> bool {
        self == Polarity::Pos
    }

    /// The letter in a gate-level cell's type (`$_DFF_N_`).
    pub fn letter(self) -> char {
        match self {
            Polarity::Pos => 'P',
            Polarity::Neg => 'N',
        }
    }

    pub fn from_letter(c: char) -> Option<Polarity> {
        match c {
            'P' => Some(Polarity::Pos),
            'N' => Some(Polarity::Neg),
            _ => None,
        }
    }
}

/// A one-bit control input and the sense it acts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ctrl {
    pub sig: SigBit,
    pub polarity: Polarity,
}

impl Ctrl {
    pub fn pos(sig: impl Into<SigBit>) -> Ctrl {
        Ctrl {
            sig: sig.into(),
            polarity: Polarity::Pos,
        }
    }
}
