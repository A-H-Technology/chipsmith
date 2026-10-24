//! Flip-flops and latches: RTLIL's 16 word-level storage cells (`$dff`,
//! `$adffe`, `$sdffce`, `$dlatch`, ...) and their 1-bit gate-level forms
//! (`$_DFF_P_`, `$_DFFE_PN0P_`, ...), as one type.
//!
//! RTLIL spells each combination of clock, enable and reset as a separate
//! cell type, and at gate level spells the polarities into the name too.
//! Here they're one [`Ff`] whose [`FfKind`] says which controls it has, the
//! way yosys' own `FfData` sees them: legalizing registers is then a match
//! on what the register has, not on 60 type names, and a combination RTLIL
//! has no cell for (a latch with a synchronous reset, a `$sdff` whose reset
//! ignores an enable it doesn't have) can't be built. [`Word`] and [`Bit`]
//! say which level it is: a gate-level register is one bit wide and resets
//! to 0 or 1, never x.

use std::fmt;

use crate::ports::{CellError, Decoder, Dir, Params, Ports};
use crate::sig::{Const, Ctrl, Polarity, SigBit, SigSpec};

/// Word-level (`$dff`) or gate-level (`$_DFF_P_`) registers.
pub trait Width: sealed::Sealed + Clone + fmt::Debug + PartialEq + Eq {
    type Sig: Clone + fmt::Debug + PartialEq + Eq;
    /// A reset value.
    type Value: Clone + fmt::Debug + PartialEq + Eq;
    #[doc(hidden)]
    const NAMES: PortNames;
    fn bits(sig: &Self::Sig) -> &[SigBit];
    fn bits_mut(sig: &mut Self::Sig) -> &mut [SigBit];
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Word {}
    impl Sealed for super::Bit {}
}

/// `$dff` and friends: any width, x allowed in reset values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word;

/// `$_DFF_P_` and friends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bit;

#[doc(hidden)]
pub struct PortNames {
    clk: &'static str,
    en: &'static str,
    arst: &'static str,
    srst: &'static str,
    aload: &'static str,
    set: &'static str,
    clr: &'static str,
}

impl Width for Word {
    type Sig = SigSpec;
    type Value = Const;
    const NAMES: PortNames = PortNames {
        clk: "CLK",
        en: "EN",
        arst: "ARST",
        srst: "SRST",
        aload: "ALOAD",
        set: "SET",
        clr: "CLR",
    };
    fn bits(sig: &SigSpec) -> &[SigBit] {
        sig
    }
    fn bits_mut(sig: &mut SigSpec) -> &mut [SigBit] {
        sig
    }
}

impl Width for Bit {
    type Sig = SigBit;
    type Value = bool;
    const NAMES: PortNames = PortNames {
        clk: "C",
        en: "E",
        arst: "R",
        srst: "R",
        aload: "L",
        set: "S",
        clr: "R",
    };
    fn bits(sig: &SigBit) -> &[SigBit] {
        std::slice::from_ref(sig)
    }
    fn bits_mut(sig: &mut SigBit) -> &mut [SigBit] {
        std::slice::from_mut(sig)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ff<W: Width> {
    pub q: W::Sig,
    pub kind: FfKind<W>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfKind<W: Width> {
    /// `$ff`: Q takes D on every tick of the formal global clock.
    Global { d: W::Sig },
    /// Edge-triggered.
    Dff {
        clk: Ctrl,
        d: W::Sig,
        control: DffControl<W>,
    },
    /// Transparent while `en` is active.
    Dlatch {
        en: Ctrl,
        d: W::Sig,
        reset: LatchReset<W>,
    },
    /// `$sr`: set/clear only, no data input.
    Sr(SetClr<W>),
}

/// What acts on an edge-triggered register besides its clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DffControl<W: Width> {
    /// `$dff`, `$dffe`.
    Plain { en: Option<Ctrl> },
    /// `$adff`, `$adffe`: Q is `value` while `arst` is active.
    Async {
        arst: Ctrl,
        value: W::Value,
        en: Option<Ctrl>,
    },
    /// `$sdff`, `$sdffe`, `$sdffce`: Q takes `value` on a clock edge with
    /// `srst` active.
    Sync {
        srst: Ctrl,
        value: W::Value,
        en: SyncEnable,
    },
    /// `$aldff`, `$aldffe`: Q follows AD while `aload` is active.
    Load {
        aload: Ctrl,
        ad: W::Sig,
        en: Option<Ctrl>,
    },
    /// `$dffsr`, `$dffsre`: per-bit asynchronous set and clear.
    SetClr { sr: SetClr<W>, en: Option<Ctrl> },
}

/// A synchronous reset's relation to the clock enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncEnable {
    /// `$sdff`.
    Always,
    /// `$sdffe`: the reset acts whether or not the enable is.
    ResetOverEnable(Ctrl),
    /// `$sdffce`: the reset acts only on enabled edges.
    EnableOverReset(Ctrl),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatchReset<W: Width> {
    /// `$dlatch`.
    None,
    /// `$adlatch`.
    Async { arst: Ctrl, value: W::Value },
    /// `$dlatchsr`.
    SetClr(SetClr<W>),
}

/// Asynchronous per-bit set and clear (clear wins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetClr<W: Width> {
    pub set: W::Sig,
    pub set_polarity: Polarity,
    pub clr: W::Sig,
    pub clr_polarity: Polarity,
}

impl<W: Width> Ff<W> {
    pub fn width(&self) -> usize {
        W::bits(&self.q).len()
    }

    /// The data input, if it has one (`$sr` doesn't).
    pub fn d(&self) -> Option<&W::Sig> {
        match &self.kind {
            FfKind::Global { d } | FfKind::Dff { d, .. } | FfKind::Dlatch { d, .. } => Some(d),
            FfKind::Sr(_) => None,
        }
    }

    /// The clock enable of an edge-triggered register.
    pub fn enable(&self) -> Option<Ctrl> {
        match &self.kind {
            FfKind::Dff { control, .. } => match control {
                DffControl::Plain { en }
                | DffControl::Async { en, .. }
                | DffControl::Load { en, .. }
                | DffControl::SetClr { en, .. } => *en,
                DffControl::Sync { en, .. } => match en {
                    SyncEnable::Always => None,
                    SyncEnable::ResetOverEnable(c) | SyncEnable::EnableOverReset(c) => Some(*c),
                },
            },
            _ => None,
        }
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        let w = self.width();
        let mut bad = None;
        self.for_each_port(&mut |name, _, bits| {
            let expect = match name {
                "D" | "Q" | "AD" | "SET" | "CLR" => w,
                _ => 1,
            };
            if bits.len() != expect && bad.is_none() {
                bad = Some(format!("{name} is {} bits, expected {expect}", bits.len()));
            }
        });
        bad.map_or(Ok(()), Err)
    }
}

impl<W: Width> Ports for Ff<W> {
    fn for_each_port<'a>(&'a self, f: &mut dyn FnMut(&'a str, Dir, &'a [SigBit])) {
        let n = &W::NAMES;
        let ctrl = |f: &mut dyn FnMut(&'a str, Dir, &'a [SigBit]), name, c: &'a Ctrl| {
            f(name, Dir::In, std::slice::from_ref(&c.sig))
        };
        let sr = |f: &mut dyn FnMut(&'a str, Dir, &'a [SigBit]), sr: &'a SetClr<W>| {
            f(n.set, Dir::In, W::bits(&sr.set));
            f(n.clr, Dir::In, W::bits(&sr.clr));
        };
        match &self.kind {
            FfKind::Global { d } => f("D", Dir::In, W::bits(d)),
            FfKind::Dff { clk, d, control } => {
                ctrl(f, n.clk, clk);
                f("D", Dir::In, W::bits(d));
                let en = match control {
                    DffControl::Plain { en } => en.as_ref(),
                    DffControl::Async { arst, en, .. } => {
                        ctrl(f, n.arst, arst);
                        en.as_ref()
                    }
                    DffControl::Sync { srst, en, .. } => {
                        ctrl(f, n.srst, srst);
                        match en {
                            SyncEnable::Always => None,
                            SyncEnable::ResetOverEnable(c) | SyncEnable::EnableOverReset(c) => {
                                Some(c)
                            }
                        }
                    }
                    DffControl::Load { aload, ad, en } => {
                        ctrl(f, n.aload, aload);
                        f("AD", Dir::In, W::bits(ad));
                        en.as_ref()
                    }
                    DffControl::SetClr { sr: s, en } => {
                        sr(f, s);
                        en.as_ref()
                    }
                };
                if let Some(en) = en {
                    ctrl(f, n.en, en);
                }
            }
            FfKind::Dlatch { en, d, reset } => {
                ctrl(f, n.en, en);
                f("D", Dir::In, W::bits(d));
                match reset {
                    LatchReset::None => {}
                    LatchReset::Async { arst, .. } => ctrl(f, n.arst, arst),
                    LatchReset::SetClr(s) => sr(f, s),
                }
            }
            FfKind::Sr(s) => sr(f, s),
        }
        f("Q", Dir::Out, W::bits(&self.q));
    }

    fn for_each_bit_mut(&mut self, f: &mut dyn FnMut(&mut SigBit)) {
        let sig =
            |f: &mut dyn FnMut(&mut SigBit), s: &mut W::Sig| W::bits_mut(s).iter_mut().for_each(f);
        let sr = |f: &mut dyn FnMut(&mut SigBit), s: &mut SetClr<W>| {
            sig(f, &mut s.set);
            sig(f, &mut s.clr);
        };
        match &mut self.kind {
            FfKind::Global { d } => sig(f, d),
            FfKind::Dff { clk, d, control } => {
                f(&mut clk.sig);
                sig(f, d);
                let en = match control {
                    DffControl::Plain { en } => en.as_mut(),
                    DffControl::Async { arst, en, .. } => {
                        f(&mut arst.sig);
                        en.as_mut()
                    }
                    DffControl::Sync { srst, en, .. } => {
                        f(&mut srst.sig);
                        match en {
                            SyncEnable::Always => None,
                            SyncEnable::ResetOverEnable(c) | SyncEnable::EnableOverReset(c) => {
                                Some(c)
                            }
                        }
                    }
                    DffControl::Load { aload, ad, en } => {
                        f(&mut aload.sig);
                        sig(f, ad);
                        en.as_mut()
                    }
                    DffControl::SetClr { sr: s, en } => {
                        sr(f, s);
                        en.as_mut()
                    }
                };
                if let Some(en) = en {
                    f(&mut en.sig);
                }
            }
            FfKind::Dlatch { en, d, reset } => {
                f(&mut en.sig);
                sig(f, d);
                match reset {
                    LatchReset::None => {}
                    LatchReset::Async { arst, .. } => f(&mut arst.sig),
                    LatchReset::SetClr(s) => sr(f, s),
                }
            }
            FfKind::Sr(s) => sr(f, s),
        }
        sig(f, &mut self.q);
    }
}

/// The word-level storage cell types.
pub const WORD_TYPES: &[&str] = &[
    "$ff",
    "$dff",
    "$dffe",
    "$adff",
    "$adffe",
    "$sdff",
    "$sdffe",
    "$sdffce",
    "$aldff",
    "$aldffe",
    "$dffsr",
    "$dffsre",
    "$dlatch",
    "$adlatch",
    "$dlatchsr",
    "$sr",
];

impl Ff<Word> {
    /// `None` if `d`'s type isn't a word-level storage cell.
    pub(crate) fn decode(d: &mut Decoder) -> Option<Result<Ff<Word>, CellError>> {
        let ty = d.ty().to_owned();
        WORD_TYPES
            .contains(&ty.as_str())
            .then(|| Self::decode_known(&ty, d))
    }

    fn decode_known(ty: &str, d: &mut Decoder) -> Result<Ff<Word>, CellError> {
        let q = d.port("Q", Dir::Out)?;
        let w = q.len();
        d.width("WIDTH", w)?;
        let ctrl = |d: &mut Decoder, port: &str, param: &str| -> Result<Ctrl, CellError> {
            Ok(Ctrl {
                sig: d.bit(port, Dir::In)?,
                polarity: d.polarity(param)?,
            })
        };
        let set_clr = |d: &mut Decoder| -> Result<SetClr<Word>, CellError> {
            Ok(SetClr {
                set: d.port_w("SET", Dir::In, w)?,
                set_polarity: d.polarity("SET_POLARITY")?,
                clr: d.port_w("CLR", Dir::In, w)?,
                clr_polarity: d.polarity("CLR_POLARITY")?,
            })
        };
        let data = |d: &mut Decoder| d.port_w("D", Dir::In, w);
        let enable = |d: &mut Decoder| ctrl(d, "EN", "EN_POLARITY");
        let kind = match ty {
            "$ff" => FfKind::Global { d: data(d)? },
            "$dff" | "$dffe" | "$adff" | "$adffe" | "$sdff" | "$sdffe" | "$sdffce" | "$aldff"
            | "$aldffe" | "$dffsr" | "$dffsre" => {
                let clk = ctrl(d, "CLK", "CLK_POLARITY")?;
                let data = data(d)?;
                let en = if ty.ends_with('e') {
                    Some(enable(d)?)
                } else {
                    None
                };
                let control = match ty {
                    "$dff" | "$dffe" => DffControl::Plain { en },
                    "$adff" | "$adffe" => DffControl::Async {
                        arst: ctrl(d, "ARST", "ARST_POLARITY")?,
                        value: d.konst_w("ARST_VALUE", w)?,
                        en,
                    },
                    "$aldff" | "$aldffe" => DffControl::Load {
                        aload: ctrl(d, "ALOAD", "ALOAD_POLARITY")?,
                        ad: d.port_w("AD", Dir::In, w)?,
                        en,
                    },
                    "$dffsr" | "$dffsre" => DffControl::SetClr {
                        sr: set_clr(d)?,
                        en,
                    },
                    _ => DffControl::Sync {
                        srst: ctrl(d, "SRST", "SRST_POLARITY")?,
                        value: d.konst_w("SRST_VALUE", w)?,
                        en: match (ty, en) {
                            ("$sdffe", Some(c)) => SyncEnable::ResetOverEnable(c),
                            ("$sdffce", Some(c)) => SyncEnable::EnableOverReset(c),
                            _ => SyncEnable::Always,
                        },
                    },
                };
                FfKind::Dff {
                    clk,
                    d: data,
                    control,
                }
            }
            "$dlatch" | "$adlatch" | "$dlatchsr" => FfKind::Dlatch {
                en: enable(d)?,
                d: data(d)?,
                reset: match ty {
                    "$dlatch" => LatchReset::None,
                    "$adlatch" => LatchReset::Async {
                        arst: ctrl(d, "ARST", "ARST_POLARITY")?,
                        value: d.konst_w("ARST_VALUE", w)?,
                    },
                    _ => LatchReset::SetClr(set_clr(d)?),
                },
            },
            _ => FfKind::Sr(set_clr(d)?),
        };
        Ok(Ff { q, kind })
    }

    pub(crate) fn type_name(&self) -> &'static str {
        match &self.kind {
            FfKind::Global { .. } => "$ff",
            FfKind::Dff { control, .. } => match control {
                DffControl::Plain { en: None } => "$dff",
                DffControl::Plain { en: Some(_) } => "$dffe",
                DffControl::Async { en: None, .. } => "$adff",
                DffControl::Async { en: Some(_), .. } => "$adffe",
                DffControl::Sync { en, .. } => match en {
                    SyncEnable::Always => "$sdff",
                    SyncEnable::ResetOverEnable(_) => "$sdffe",
                    SyncEnable::EnableOverReset(_) => "$sdffce",
                },
                DffControl::Load { en: None, .. } => "$aldff",
                DffControl::Load { en: Some(_), .. } => "$aldffe",
                DffControl::SetClr { en: None, .. } => "$dffsr",
                DffControl::SetClr { en: Some(_), .. } => "$dffsre",
            },
            FfKind::Dlatch { reset, .. } => match reset {
                LatchReset::None => "$dlatch",
                LatchReset::Async { .. } => "$adlatch",
                LatchReset::SetClr(_) => "$dlatchsr",
            },
            FfKind::Sr(_) => "$sr",
        }
    }

    pub(crate) fn params(&self, p: &mut Params) {
        p.int("WIDTH", self.width());
        let sr = |p: &mut Params, s: &SetClr<Word>| {
            p.polarity("SET_POLARITY", s.set_polarity)
                .polarity("CLR_POLARITY", s.clr_polarity);
        };
        let en = |p: &mut Params, en: Option<&Ctrl>| {
            if let Some(en) = en {
                p.polarity("EN_POLARITY", en.polarity);
            }
        };
        match &self.kind {
            FfKind::Global { .. } => {}
            FfKind::Dff { clk, control, .. } => {
                p.polarity("CLK_POLARITY", clk.polarity);
                match control {
                    DffControl::Plain { en: e } => en(p, e.as_ref()),
                    DffControl::Async { arst, value, en: e } => {
                        p.polarity("ARST_POLARITY", arst.polarity)
                            .konst("ARST_VALUE", value.clone());
                        en(p, e.as_ref());
                    }
                    DffControl::Sync { srst, value, .. } => {
                        p.polarity("SRST_POLARITY", srst.polarity)
                            .konst("SRST_VALUE", value.clone());
                        en(p, self.enable().as_ref());
                    }
                    DffControl::Load { aload, en: e, .. } => {
                        p.polarity("ALOAD_POLARITY", aload.polarity);
                        en(p, e.as_ref());
                    }
                    DffControl::SetClr { sr: s, en: e } => {
                        sr(p, s);
                        en(p, e.as_ref());
                    }
                }
            }
            FfKind::Dlatch { en: e, reset, .. } => {
                en(p, Some(e));
                match reset {
                    LatchReset::None => {}
                    LatchReset::Async { arst, value } => {
                        p.polarity("ARST_POLARITY", arst.polarity)
                            .konst("ARST_VALUE", value.clone());
                    }
                    LatchReset::SetClr(s) => sr(p, s),
                }
            }
            FfKind::Sr(s) => sr(p, s),
        }
    }

    pub(crate) fn check_values(&self) -> Result<(), String> {
        let value = match &self.kind {
            FfKind::Dff {
                control: DffControl::Async { value, .. } | DffControl::Sync { value, .. },
                ..
            }
            | FfKind::Dlatch {
                reset: LatchReset::Async { value, .. },
                ..
            } => value,
            _ => return Ok(()),
        };
        if value.len() != self.width() {
            return Err(format!(
                "reset value is {} bits, expected {}",
                value.len(),
                self.width()
            ));
        }
        Ok(())
    }
}

impl Ff<Bit> {
    /// `None` if `d`'s type isn't a gate-level storage cell.
    pub(crate) fn decode(d: &mut Decoder) -> Option<Result<Ff<Bit>, CellError>> {
        let inner = d.ty().strip_prefix("$_")?.strip_suffix('_')?;
        let (base, letters) = inner.split_once('_').unwrap_or((inner, ""));
        let letters: Vec<char> = letters.chars().collect();
        let shape = Shape::parse(base, &letters)?;
        Some(Self::decode_shape(shape, &letters, d))
    }

    fn decode_shape(shape: Shape, l: &[char], d: &mut Decoder) -> Result<Ff<Bit>, CellError> {
        // Letters are polarities (P/N) and reset values (0/1) in a fixed
        // order per shape; `Shape::parse` checked which is which.
        let pol = |i: usize| Polarity::from_letter(l[i]).expect("checked by Shape::parse");
        let val = |i: usize| l[i] == '1';
        let ctrl = |d: &mut Decoder, port: &str, i: usize| -> Result<Ctrl, CellError> {
            Ok(Ctrl {
                sig: d.bit(port, Dir::In)?,
                polarity: pol(i),
            })
        };
        let set_clr = |d: &mut Decoder, i: usize| -> Result<SetClr<Bit>, CellError> {
            Ok(SetClr {
                set: d.bit("S", Dir::In)?,
                set_polarity: pol(i),
                clr: d.bit("R", Dir::In)?,
                clr_polarity: pol(i + 1),
            })
        };
        let q = d.bit("Q", Dir::Out)?;
        let kind = match shape {
            Shape::Global => FfKind::Global {
                d: d.bit("D", Dir::In)?,
            },
            Shape::Dlatch(reset) => FfKind::Dlatch {
                en: ctrl(d, "E", 0)?,
                d: d.bit("D", Dir::In)?,
                reset: match reset {
                    LatchShape::None => LatchReset::None,
                    LatchShape::Async => LatchReset::Async {
                        arst: ctrl(d, "R", 1)?,
                        value: val(2),
                    },
                    LatchShape::SetClr => LatchReset::SetClr(set_clr(d, 1)?),
                },
            },
            Shape::Sr => FfKind::Sr(set_clr(d, 0)?),
            Shape::Dff(control, has_en) => {
                let clk = ctrl(d, "C", 0)?;
                let data = d.bit("D", Dir::In)?;
                // The enable's letter is the last one.
                let en = if has_en {
                    Some(ctrl(d, "E", l.len() - 1)?)
                } else {
                    None
                };
                let control = match control {
                    DffShape::Plain => DffControl::Plain { en },
                    DffShape::Async => DffControl::Async {
                        arst: ctrl(d, "R", 1)?,
                        value: val(2),
                        en,
                    },
                    DffShape::Sync(enable_first) => DffControl::Sync {
                        srst: ctrl(d, "R", 1)?,
                        value: val(2),
                        en: match en {
                            None => SyncEnable::Always,
                            Some(c) if enable_first => SyncEnable::EnableOverReset(c),
                            Some(c) => SyncEnable::ResetOverEnable(c),
                        },
                    },
                    DffShape::Load => DffControl::Load {
                        aload: ctrl(d, "L", 1)?,
                        ad: d.bit("AD", Dir::In)?,
                        en,
                    },
                    DffShape::SetClr => DffControl::SetClr {
                        sr: set_clr(d, 1)?,
                        en,
                    },
                };
                FfKind::Dff {
                    clk,
                    d: data,
                    control,
                }
            }
        };
        Ok(Ff { q, kind })
    }

    pub(crate) fn type_name(&self) -> String {
        let mut letters = String::new();
        let mut pol = |p: Polarity| letters.push(p.letter());
        let base = match &self.kind {
            FfKind::Global { .. } => "FF",
            FfKind::Dff { clk, control, .. } => {
                pol(clk.polarity);
                let base = match control {
                    DffControl::Plain { en } => {
                        if en.is_some() {
                            "DFFE"
                        } else {
                            "DFF"
                        }
                    }
                    DffControl::Async { arst, value, en } => {
                        pol(arst.polarity);
                        letters.push(if *value { '1' } else { '0' });
                        if en.is_some() {
                            "DFFE"
                        } else {
                            "DFF"
                        }
                    }
                    DffControl::Sync { srst, value, en } => {
                        pol(srst.polarity);
                        letters.push(if *value { '1' } else { '0' });
                        match en {
                            SyncEnable::Always => "SDFF",
                            SyncEnable::ResetOverEnable(_) => "SDFFE",
                            SyncEnable::EnableOverReset(_) => "SDFFCE",
                        }
                    }
                    DffControl::Load { aload, en, .. } => {
                        pol(aload.polarity);
                        if en.is_some() {
                            "ALDFFE"
                        } else {
                            "ALDFF"
                        }
                    }
                    DffControl::SetClr { sr, en } => {
                        pol(sr.set_polarity);
                        letters.push(sr.clr_polarity.letter());
                        if en.is_some() {
                            "DFFSRE"
                        } else {
                            "DFFSR"
                        }
                    }
                };
                if let Some(en) = self.enable() {
                    letters.push(en.polarity.letter());
                }
                base
            }
            FfKind::Dlatch { en, reset, .. } => {
                letters.push(en.polarity.letter());
                match reset {
                    LatchReset::None => "DLATCH",
                    LatchReset::Async { arst, value } => {
                        letters.push(arst.polarity.letter());
                        letters.push(if *value { '1' } else { '0' });
                        "DLATCH"
                    }
                    LatchReset::SetClr(sr) => {
                        letters.push(sr.set_polarity.letter());
                        letters.push(sr.clr_polarity.letter());
                        "DLATCHSR"
                    }
                }
            }
            FfKind::Sr(sr) => {
                letters.push(sr.set_polarity.letter());
                letters.push(sr.clr_polarity.letter());
                "SR"
            }
        };
        if letters.is_empty() {
            format!("$_{base}_")
        } else {
            format!("$_{base}_{letters}_")
        }
    }
}

/// A gate-level storage cell type, from its name.
#[derive(Clone, Copy)]
enum Shape {
    Global,
    /// With an enable or not.
    Dff(DffShape, bool),
    Dlatch(LatchShape),
    Sr,
}

#[derive(Clone, Copy)]
enum DffShape {
    Plain,
    Async,
    /// `true` for `$_SDFFCE_`: the enable gates the reset.
    Sync(bool),
    Load,
    SetClr,
}

#[derive(Clone, Copy)]
enum LatchShape {
    None,
    Async,
    SetClr,
}

impl Shape {
    /// `P`/`N` where a polarity goes and `0`/`1` where a reset value does,
    /// in yosys' order: clock or enable, reset, value, clock enable.
    fn parse(base: &str, l: &[char]) -> Option<Shape> {
        let pattern: &[u8] = match base {
            "FF" => b"",
            "DFF" if l.len() == 1 => b"p",
            "DFF" => b"pp0",
            "DFFE" if l.len() == 2 => b"pp",
            "DFFE" => b"pp0p",
            "SDFF" => b"pp0",
            "SDFFE" | "SDFFCE" => b"pp0p",
            "ALDFF" => b"pp",
            "ALDFFE" => b"ppp",
            "DFFSR" => b"ppp",
            "DFFSRE" => b"pppp",
            "DLATCH" if l.len() == 1 => b"p",
            "DLATCH" => b"pp0",
            "DLATCHSR" => b"ppp",
            "SR" => b"pp",
            _ => return None,
        };
        let fits = pattern.len() == l.len()
            && pattern.iter().zip(l).all(|(p, c)| match p {
                b'p' => matches!(c, 'P' | 'N'),
                _ => matches!(c, '0' | '1'),
            });
        if !fits {
            return None;
        }
        Some(match base {
            "FF" => Shape::Global,
            "DFF" if l.len() == 1 => Shape::Dff(DffShape::Plain, false),
            "DFF" => Shape::Dff(DffShape::Async, false),
            "DFFE" if l.len() == 2 => Shape::Dff(DffShape::Plain, true),
            "DFFE" => Shape::Dff(DffShape::Async, true),
            "SDFF" => Shape::Dff(DffShape::Sync(false), false),
            "SDFFE" => Shape::Dff(DffShape::Sync(false), true),
            "SDFFCE" => Shape::Dff(DffShape::Sync(true), true),
            "ALDFF" => Shape::Dff(DffShape::Load, false),
            "ALDFFE" => Shape::Dff(DffShape::Load, true),
            "DFFSR" => Shape::Dff(DffShape::SetClr, false),
            "DFFSRE" => Shape::Dff(DffShape::SetClr, true),
            "DLATCH" if l.len() == 1 => Shape::Dlatch(LatchShape::None),
            "DLATCH" => Shape::Dlatch(LatchShape::Async),
            "DLATCHSR" => Shape::Dlatch(LatchShape::SetClr),
            _ => Shape::Sr,
        })
    }
}
