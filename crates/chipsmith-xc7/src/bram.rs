//! A block RAM's configuration (UG473): a RAMB18E1, or a RAMB36E1 (a BRAM
//! tile's two halves as one), as one type that synthesis writes as the
//! primitive's parameters and the packer, FASM writer and decoder read back
//! ([`Config::from_params`], [`Config::params`]).
//!
//! The block holds a data array (16 Kib or 32 Kib) and a parity array (2
//! Kib or 4 Kib). A port of width w reads and writes word `addr >> shift`:
//! data bits `word * data + k` and parity bits `word * parity + k`, so
//! every width sees the same bits (an x18 word is two x9 words, each byte
//! with its parity bit). A byte enable covers a byte's eight data bits and
//! its parity bit.
//!
//! Each port is TDP (true dual port) up to x18 (x36); in SDP (simple dual
//! port) mode the block is read on port A and written on port B, either up
//! to x36 (x72) wide, the data on both ports' pins (`{B, A}`).

use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context, Result};
use chipsmith_netlist::{Const, ParamValue, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Size {
    /// RAMB18E1.
    Half,
    /// RAMB36E1.
    Full,
}

impl Size {
    pub fn primitive(self) -> &'static str {
        match self {
            Size::Half => "RAMB18E1",
            Size::Full => "RAMB36E1",
        }
    }

    pub fn from_primitive(ty: &str) -> Option<Size> {
        match ty {
            "RAMB18E1" => Some(Size::Half),
            "RAMB36E1" => Some(Size::Full),
            _ => None,
        }
    }

    pub fn data_bits(self) -> usize {
        match self {
            Size::Half => 16384,
            Size::Full => 32768,
        }
    }

    pub fn parity_bits(self) -> usize {
        self.data_bits() / 8
    }

    /// The address pins' width: 14, 16 (a RAMB36E1's ADDR[15] cascades,
    /// so 15 address its words).
    pub fn addr_pins(self) -> usize {
        match self {
            Size::Half => 14,
            Size::Full => 16,
        }
    }

    /// Data pins per port (DIADI, DOADO): 16, 32.
    pub fn data_pins(self) -> usize {
        match self {
            Size::Half => 16,
            Size::Full => 32,
        }
    }

    /// The widest port of true dual port mode.
    pub fn tdp_max(self) -> Width {
        match self {
            Size::Half => Width::X18,
            Size::Full => Width::X36,
        }
    }

    /// The width of simple dual port mode's wide side.
    pub fn sdp(self) -> Width {
        match self {
            Size::Half => Width::X36,
            Size::Full => Width::X72,
        }
    }

    /// How many RAMB18 halves it takes.
    pub fn halves(self) -> usize {
        match self {
            Size::Half => 1,
            Size::Full => 2,
        }
    }
}

/// A port's width: data bits, and from x9 one parity bit per byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Width {
    X1,
    X2,
    X4,
    X9,
    X18,
    X36,
    X72,
}

impl Width {
    pub const ALL: [Width; 7] = [
        Width::X1,
        Width::X2,
        Width::X4,
        Width::X9,
        Width::X18,
        Width::X36,
        Width::X72,
    ];

    pub fn bits(self) -> usize {
        match self {
            Width::X1 => 1,
            Width::X2 => 2,
            Width::X4 => 4,
            Width::X9 => 9,
            Width::X18 => 18,
            Width::X36 => 36,
            Width::X72 => 72,
        }
    }

    pub fn from_bits(n: usize) -> Option<Width> {
        Width::ALL.into_iter().find(|w| w.bits() == n)
    }

    pub fn data(self) -> usize {
        match self.bits() {
            n if n >= 9 => n / 9 * 8,
            n => n,
        }
    }

    pub fn parity(self) -> usize {
        self.bits() / 9
    }

    /// Byte enables: one per byte, one for a port narrower than x9.
    pub fn lanes(self) -> usize {
        self.parity().max(1)
    }

    /// The address bits below a word.
    pub fn shift(self) -> usize {
        self.data().trailing_zeros() as usize
    }

    /// Words in a block of `size`.
    pub fn depth(self, size: Size) -> usize {
        size.data_bits() / self.data()
    }
}

/// What a port's read data shows on an edge it writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WriteMode {
    /// The new data (bytes not written: what is stored).
    #[default]
    WriteFirst,
    /// What was stored before; the other port reading the word on the
    /// same edge gets that too.
    ReadFirst,
    /// The read data from before (no read).
    NoChange,
}

impl WriteMode {
    fn name(self) -> &'static str {
        match self {
            WriteMode::WriteFirst => "WRITE_FIRST",
            WriteMode::ReadFirst => "READ_FIRST",
            WriteMode::NoChange => "NO_CHANGE",
        }
    }

    fn parse(s: &str) -> Result<WriteMode> {
        Ok(match s {
            "WRITE_FIRST" => WriteMode::WriteFirst,
            "READ_FIRST" => WriteMode::ReadFirst,
            "NO_CHANGE" => WriteMode::NoChange,
            _ => bail!("WRITE_MODE {s}"),
        })
    }
}

/// A RAMB18's INIT or SRVAL as its configuration bits hold them: the value
/// moved from a word of width `w` ({parity, data} from bit 0) into the
/// x18 layout (data from bit 0, parity from bit 16), its 18 bits in
/// reverse order (found with Vivado: INIT_A = 1 << k of an x18 port sets
/// bit 17 - k, of an x9 port's parity bit 1).
pub fn half_bits(v: u64, w: Width) -> u64 {
    let (dw, pw) = (w.data().min(16), w.parity().min(2));
    let x18 = (v & ((1 << dw) - 1)) | (v >> dw & ((1 << pw) - 1)) << 16;
    (0..18).fold(0, |a, i| a | (x18 >> i & 1) << (17 - i))
}

/// [`half_bits`] backwards.
pub fn from_half_bits(bits: u64, w: Width) -> u64 {
    let x18 = (0..18).fold(0u64, |a, i| a | (bits >> i & 1) << (17 - i));
    let (dw, pw) = (w.data().min(16), w.parity().min(2));
    (x18 & ((1 << dw) - 1)) | (x18 >> 16 & ((1 << pw) - 1)) << dw
}

/// One port's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub read: Option<Width>,
    pub write: Option<Width>,
    pub mode: WriteMode,
    /// The output register (DO<p>_REG), on the port's clock.
    pub out_reg: bool,
    /// RSTREG resets the output register whether REGCE is high or not
    /// (RSTREG_PRIORITY "RSTREG"); else only with REGCE.
    pub rstreg_first: bool,
    /// The read data's power-up value and the value the resets set: per
    /// port, its pins' {parity, data} (18 or 36 bits).
    pub init: u64,
    pub srval: u64,
}

impl Port {
    pub fn unused() -> Port {
        Port {
            read: None,
            write: None,
            mode: WriteMode::WriteFirst,
            out_reg: false,
            rstreg_first: true,
            init: 0,
            srval: 0,
        }
    }
}

/// Inputs the block inverts (IS_<pin>_INVERTED), per port.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Inversions {
    pub clk: [bool; 2],
    pub en: [bool; 2],
    pub rstram: [bool; 2],
    pub rstreg: [bool; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub size: Size,
    /// Port A reads all of an SDP block's width, port B writes it.
    pub sdp: bool,
    pub ports: [Port; 2],
    pub inv: Inversions,
    /// The data array, then the parity array.
    pub contents: Vec<bool>,
}

const INVERTIBLE: [(&str, &str); 4] = [
    ("CLKARDCLK", "CLKBWRCLK"),
    ("ENARDEN", "ENBWREN"),
    ("RSTRAMARSTRAM", "RSTRAMB"),
    ("RSTREGARSTREG", "RSTREGB"),
];

fn int(params: &BTreeMap<String, ParamValue>, name: &str, default: i64) -> Result<i64> {
    Ok(match params.get(name) {
        None => default,
        Some(ParamValue::Bits(c)) => {
            c.0.iter()
                .enumerate()
                .try_fold(0i64, |v, (i, s)| match s {
                    State::One => Some(v | 1 << i),
                    State::Zero | State::X | State::Z => Some(v),
                })
                .context(name.to_string())?
        }
        Some(ParamValue::Str(s)) => s
            .trim()
            .parse()
            .with_context(|| format!("{name} = {s:?}"))?,
    })
}

fn string<'a>(params: &'a BTreeMap<String, ParamValue>, name: &str, default: &'a str) -> &'a str {
    match params.get(name) {
        Some(ParamValue::Str(s)) => s.as_str(),
        _ => default,
    }
}

/// A bit-vector parameter's bits, low first, `width` of them (x as 0).
fn bit_vec(params: &BTreeMap<String, ParamValue>, name: &str, width: usize) -> Vec<bool> {
    let mut out = vec![false; width];
    if let Some(ParamValue::Bits(c)) = params.get(name) {
        for (o, s) in out.iter_mut().zip(&c.0) {
            *o = *s == State::One;
        }
    }
    out
}

fn bits_param(v: impl Iterator<Item = bool>) -> ParamValue {
    ParamValue::Bits(Const(v.map(State::from).collect()))
}

impl Config {
    /// Whether the configuration is one the block has.
    pub fn check(&self) -> Result<()> {
        let s = self.size;
        let [a, b] = &self.ports;
        if self.sdp {
            ensure!(
                a.write.is_none() && b.read.is_none(),
                "an SDP block reads on port A and writes on port B"
            );
            ensure!(
                a.read == Some(s.sdp()) || b.write == Some(s.sdp()),
                "an SDP block has a {}-bit side",
                s.sdp().bits()
            );
            ensure!(
                a.mode == b.mode && a.mode != WriteMode::NoChange,
                "an SDP block's write modes are both READ_FIRST or both WRITE_FIRST"
            );
            ensure!(!b.out_reg, "an SDP block's output register is port A's");
        } else {
            for p in &self.ports {
                for w in [p.read, p.write].into_iter().flatten() {
                    ensure!(w <= s.tdp_max(), "x{} on a TDP {}", w.bits(), s.primitive());
                }
            }
        }
        ensure!(
            self.contents.len() == s.data_bits() + s.parity_bits(),
            "contents of {} bits",
            self.contents.len()
        );
        Ok(())
    }

    /// From a RAMB18E1's or RAMB36E1's parameters.
    pub fn from_params(ty: &str, params: &BTreeMap<String, ParamValue>) -> Result<Config> {
        let size = Size::from_primitive(ty).with_context(|| format!("{ty} isn't a block RAM"))?;
        let width = |name: &str| -> Result<Option<Width>> {
            match int(params, name, 0)? {
                0 => Ok(None),
                n => Width::from_bits(n as usize)
                    .map(Some)
                    .with_context(|| format!("{name} = {n}")),
            }
        };
        let sdp = match string(params, "RAM_MODE", "TDP") {
            "TDP" => false,
            "SDP" => true,
            m => bail!("RAM_MODE {m}"),
        };
        let half = size.data_pins() + size.data_pins() / 8;
        let port = |p: char| -> Result<Port> {
            let w = |v: Vec<bool>| {
                v.iter()
                    .enumerate()
                    .fold(0u64, |a, (i, &b)| a | u64::from(b) << i)
            };
            Ok(Port {
                read: width(&format!("READ_WIDTH_{p}"))?,
                write: width(&format!("WRITE_WIDTH_{p}"))?,
                mode: WriteMode::parse(string(params, &format!("WRITE_MODE_{p}"), "WRITE_FIRST"))?,
                out_reg: int(params, &format!("DO{p}_REG"), 0)? != 0,
                rstreg_first: match string(params, &format!("RSTREG_PRIORITY_{p}"), "RSTREG") {
                    "RSTREG" => true,
                    "REGCE" => false,
                    r => bail!("RSTREG_PRIORITY_{p} {r}"),
                },
                init: w(bit_vec(params, &format!("INIT_{p}"), half)),
                srval: w(bit_vec(params, &format!("SRVAL_{p}"), half)),
            })
        };
        let mut ports = [port('A')?, port('B')?];
        if sdp {
            // The primitive leaves the unused sides' widths 0; Vivado
            // writes them anyway. The read's output register is both
            // ports'.
            ensure!(
                ports[0].out_reg == ports[1].out_reg,
                "an SDP {ty} with DOA_REG and DOB_REG apart"
            );
            ports[0].write = None;
            ports[1].read = None;
            ports[1].out_reg = false;
        }
        let inv =
            |pin: &str| -> Result<bool> { Ok(int(params, &format!("IS_{pin}_INVERTED"), 0)? != 0) };
        let mut inversions = Inversions::default();
        for (i, (a, b)) in INVERTIBLE.into_iter().enumerate() {
            let pair = [inv(a)?, inv(b)?];
            match i {
                0 => inversions.clk = pair,
                1 => inversions.en = pair,
                2 => inversions.rstram = pair,
                _ => inversions.rstreg = pair,
            }
        }
        let mut contents = Vec::with_capacity(size.data_bits() + size.parity_bits());
        for i in 0..size.data_bits() / 256 {
            contents.extend(bit_vec(params, &format!("INIT_{i:02X}"), 256));
        }
        for i in 0..size.parity_bits() / 256 {
            contents.extend(bit_vec(params, &format!("INITP_{i:02X}"), 256));
        }
        let c = Config {
            size,
            sdp,
            ports,
            inv: inversions,
            contents,
        };
        c.check()?;
        Ok(c)
    }

    /// The primitive's parameters.
    pub fn params(&self) -> BTreeMap<String, ParamValue> {
        let mut p = BTreeMap::new();
        let int = |v: usize| ParamValue::int(v as i64);
        let half = self.size.data_pins() + self.size.data_pins() / 8;
        p.insert(
            "RAM_MODE".into(),
            ParamValue::Str(if self.sdp { "SDP" } else { "TDP" }.into()),
        );
        for (port, l) in self.ports.iter().zip(['A', 'B']) {
            p.insert(
                format!("READ_WIDTH_{l}"),
                int(port.read.map_or(0, Width::bits)),
            );
            p.insert(
                format!("WRITE_WIDTH_{l}"),
                int(port.write.map_or(0, Width::bits)),
            );
            p.insert(
                format!("WRITE_MODE_{l}"),
                ParamValue::Str(port.mode.name().into()),
            );
            // SDP's read spans both ports' output registers.
            let out_reg = port.out_reg || (self.sdp && self.ports[0].out_reg);
            p.insert(format!("DO{l}_REG"), int(out_reg.into()));
            p.insert(
                format!("RSTREG_PRIORITY_{l}"),
                ParamValue::Str(if port.rstreg_first { "RSTREG" } else { "REGCE" }.into()),
            );
            p.insert(
                format!("INIT_{l}"),
                bits_param((0..half).map(|i| port.init >> i & 1 == 1)),
            );
            p.insert(
                format!("SRVAL_{l}"),
                bits_param((0..half).map(|i| port.srval >> i & 1 == 1)),
            );
        }
        let inv = [self.inv.clk, self.inv.en, self.inv.rstram, self.inv.rstreg];
        for ((a, b), v) in INVERTIBLE.into_iter().zip(inv) {
            p.insert(
                format!("IS_{a}_INVERTED"),
                bits_param(std::iter::once(v[0])),
            );
            p.insert(
                format!("IS_{b}_INVERTED"),
                bits_param(std::iter::once(v[1])),
            );
        }
        let data = self.size.data_bits();
        for (i, chunk) in self.contents[..data].chunks(256).enumerate() {
            p.insert(format!("INIT_{i:02X}"), bits_param(chunk.iter().copied()));
        }
        for (i, chunk) in self.contents[data..].chunks(256).enumerate() {
            p.insert(format!("INITP_{i:02X}"), bits_param(chunk.iter().copied()));
        }
        p
    }

    /// Whether the block's read data on port `p` is ever used.
    pub fn reads(&self, p: usize) -> bool {
        self.ports[p].read.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_and_depths() {
        assert_eq!(Width::X9.data(), 8);
        assert_eq!(Width::X36.parity(), 4);
        assert_eq!(Width::X18.depth(Size::Half), 1024);
        assert_eq!(Width::X1.depth(Size::Full), 32768);
        assert_eq!(Width::X72.shift(), 6);
        assert_eq!(Width::X4.lanes(), 1);
    }

    #[test]
    fn init_bits_are_reversed_in_the_x18_layout() {
        // Vivado's bitstreams: x18 INIT 0x2a5a5 holds 0x29695; x9's parity
        // bit is bit 1; x36 SDP halves are x18 words.
        assert_eq!(half_bits(0x2a5a5, Width::X18), 0x29695);
        assert_eq!(half_bits(1 << 8, Width::X9), 1 << 1);
        assert_eq!(half_bits(1, Width::X4), 1 << 17);
        for w in [Width::X1, Width::X2, Width::X4, Width::X9, Width::X18] {
            let v = 0x2a5a5 & ((1 << w.bits()) - 1);
            assert_eq!(from_half_bits(half_bits(v, w), w), v, "{w:?}");
        }
    }

    #[test]
    fn parameters_read_back() {
        let size = Size::Half;
        let mut contents = vec![false; size.data_bits() + size.parity_bits()];
        contents[5] = true;
        contents[16384 + 3] = true;
        let c = Config {
            size,
            sdp: true,
            ports: [
                Port {
                    read: Some(Width::X36),
                    mode: WriteMode::ReadFirst,
                    out_reg: true,
                    init: 0x2_a5a5,
                    ..Port::unused()
                },
                Port {
                    write: Some(Width::X9),
                    mode: WriteMode::ReadFirst,
                    srval: 7,
                    ..Port::unused()
                },
            ],
            inv: Inversions {
                clk: [false, true],
                ..Default::default()
            },
            contents,
        };
        c.check().unwrap();
        assert_eq!(Config::from_params("RAMB18E1", &c.params()).unwrap(), c);
    }

    #[test]
    fn invalid_configurations_are_refused() {
        let size = Size::Half;
        let c = Config {
            size,
            sdp: false,
            ports: [
                Port {
                    read: Some(Width::X36),
                    ..Port::unused()
                },
                Port::unused(),
            ],
            inv: Inversions::default(),
            contents: vec![false; size.data_bits() + size.parity_bits()],
        };
        assert!(c.check().is_err(), "x36 is SDP only on a RAMB18E1");
    }
}
