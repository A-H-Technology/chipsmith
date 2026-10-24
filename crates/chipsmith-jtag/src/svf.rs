//! Serial Vector Format: parse, then play over a `Cable`.
//!
//! Covers what bitstream writers emit for configuration: SIR/SDR with
//! TDI/TDO/MASK/SMASK, RUNTEST in TCKs, STATE, ENDIR/ENDDR, TRST without a
//! TRST pin, zero-length header/trailer and FREQUENCY. Anything else is an
//! error rather than skipped, so an SVF never half-plays.

use anyhow::{anyhow, bail, ensure, Context, Result};

use super::{Cable, TapState};

/// A bit vector, LSB (first shifted) first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bits {
    pub len: usize,
    pub bytes: Vec<u8>,
}

impl Bits {
    fn ones(len: usize) -> Bits {
        let mut bytes = vec![0xff; len.div_ceil(8)];
        if !len.is_multiple_of(8) {
            *bytes.last_mut().unwrap() = (1 << (len % 8)) - 1;
        }
        Bits { len, bytes }
    }

    /// SVF hex: the whole register as one number, most significant (last
    /// shifted) digit first.
    fn parse_hex(len: usize, hex: &str) -> Result<Bits> {
        let mut bytes = vec![0u8; len.div_ceil(8)];
        for (i, c) in hex.bytes().rev().enumerate() {
            let digit = (c as char)
                .to_digit(16)
                .ok_or_else(|| anyhow!("{:?} is not a hex digit", c as char))?
                as u8;
            let bit = i * 4;
            if bit >= len {
                ensure!(digit == 0, "value has bits set beyond its {len}-bit length");
                continue;
            }
            bytes[bit / 8] |= digit << (bit % 8);
            // Digits past a length that isn't a multiple of 4 must be zero.
            let spill = (bit + 4).saturating_sub(len);
            ensure!(
                digit >> (4 - spill) == 0,
                "value has bits set beyond its {len}-bit length"
            );
        }
        Ok(Bits { len, bytes })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shift {
    pub len: usize,
    pub tdi: Option<Bits>,
    pub tdo: Option<Bits>,
    pub mask: Option<Bits>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    EndIr(TapState),
    EndDr(TapState),
    State(Vec<TapState>),
    Sir(Shift),
    Sdr(Shift),
    RunTest {
        run_state: Option<TapState>,
        clocks: u32,
        end_state: Option<TapState>,
    },
    /// TRST ABSENT/OFF, zero-length HDR/HIR/TDR/TIR, FREQUENCY: nothing to do
    /// for a cable without TRST and a fixed TCK.
    Nothing,
}

pub fn parse(text: &str) -> Result<Vec<Command>> {
    let mut commands = Vec::new();
    for (n, statement) in statements(text).iter().enumerate() {
        let tokens = tokenize(statement);
        let Some((keyword, args)) = tokens.split_first() else {
            continue;
        };
        let command = parse_command(keyword, args)
            .with_context(|| format!("SVF statement {}: {}", n + 1, abbreviate(statement)))?;
        commands.push(command);
    }
    Ok(commands)
}

/// Statements end at `;`. Comments (`!`, `//`) run to the end of the line.
fn statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = match (line.find('!'), line.find("//")) {
            (Some(a), Some(b)) => &line[..a.min(b)],
            (Some(a), None) | (None, Some(a)) => &line[..a],
            (None, None) => line,
        };
        for (i, part) in line.split(';').enumerate() {
            if i > 0 {
                out.push(std::mem::take(&mut current));
            }
            current.push_str(part);
            current.push(' ');
        }
    }
    // Text after the last `;` is not a statement; other players ignore it too.
    out
}

/// Words, and parenthesised hex with its whitespace removed as one token.
fn tokenize(statement: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut rest = statement.trim();
    while !rest.is_empty() {
        if let Some(inner) = rest.strip_prefix('(') {
            let end = inner.find(')').unwrap_or(inner.len());
            tokens.push(format!(
                "({})",
                inner[..end].split_whitespace().collect::<String>()
            ));
            rest = inner.get(end + 1..).unwrap_or("").trim_start();
        } else {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '(')
                .unwrap_or(rest.len());
            tokens.push(rest[..end].to_ascii_uppercase());
            rest = rest[end..].trim_start();
        }
    }
    tokens
}

fn abbreviate(s: &str) -> String {
    let s = s.trim();
    if s.len() > 80 {
        format!("{}...", &s[..80])
    } else {
        s.to_string()
    }
}

fn parse_state(word: &str) -> Result<TapState> {
    use TapState::*;
    Ok(match word {
        "RESET" => Reset,
        "IDLE" => Idle,
        "DRSELECT" => DrSelect,
        "DRCAPTURE" => DrCapture,
        "DRSHIFT" => DrShift,
        "DREXIT1" => DrExit1,
        "DRPAUSE" => DrPause,
        "DREXIT2" => DrExit2,
        "DRUPDATE" => DrUpdate,
        "IRSELECT" => IrSelect,
        "IRCAPTURE" => IrCapture,
        "IRSHIFT" => IrShift,
        "IREXIT1" => IrExit1,
        "IRPAUSE" => IrPause,
        "IREXIT2" => IrExit2,
        "IRUPDATE" => IrUpdate,
        _ => bail!("unknown TAP state {word}"),
    })
}

fn stable(state: TapState) -> Result<TapState> {
    use TapState::*;
    ensure!(
        matches!(state, Reset | Idle | DrPause | IrPause),
        "{state:?} is not a stable state"
    );
    Ok(state)
}

fn parse_command(keyword: &str, args: &[String]) -> Result<Command> {
    Ok(match keyword {
        "ENDIR" | "ENDDR" => {
            let [state] = args else {
                bail!("{keyword} takes one state");
            };
            let state = stable(parse_state(state)?)?;
            if keyword == "ENDIR" {
                Command::EndIr(state)
            } else {
                Command::EndDr(state)
            }
        }
        "STATE" => {
            let path = args
                .iter()
                .map(|a| parse_state(a))
                .collect::<Result<Vec<_>>>()?;
            stable(*path.last().ok_or_else(|| anyhow!("STATE needs a state"))?)?;
            Command::State(path)
        }
        "SIR" => Command::Sir(parse_shift(args)?),
        "SDR" => Command::Sdr(parse_shift(args)?),
        "HIR" | "HDR" | "TIR" | "TDR" => {
            ensure!(
                parse_shift(args)?.len == 0,
                "{keyword} with a non-zero length (devices around the target) is not supported"
            );
            Command::Nothing
        }
        "TRST" => match args {
            [mode] if mode == "ABSENT" || mode == "OFF" => Command::Nothing,
            _ => bail!(
                "TRST {} needs a TRST pin, which the cable doesn't have",
                args.join(" ")
            ),
        },
        "FREQUENCY" => Command::Nothing,
        "RUNTEST" => parse_runtest(args)?,
        _ => bail!("unsupported SVF command {keyword}"),
    })
}

fn parse_shift(args: &[String]) -> Result<Shift> {
    let (len, mut rest) = args
        .split_first()
        .ok_or_else(|| anyhow!("missing length"))?;
    let len: usize = len.parse().with_context(|| format!("bad length {len}"))?;
    let mut shift = Shift {
        len,
        tdi: None,
        tdo: None,
        mask: None,
    };
    while let [field, value, tail @ ..] = rest {
        let hex = value
            .strip_prefix('(')
            .and_then(|v| v.strip_suffix(')'))
            .ok_or_else(|| anyhow!("{field} needs a (hex) value"))?;
        let bits = Bits::parse_hex(len, hex).with_context(|| field.clone())?;
        match field.as_str() {
            "TDI" => shift.tdi = Some(bits),
            "TDO" => shift.tdo = Some(bits),
            "MASK" => shift.mask = Some(bits),
            // SMASK marks TDI bits as don't-care; shifting them as given is fine.
            "SMASK" => {}
            _ => bail!("unknown field {field}"),
        }
        rest = tail;
    }
    ensure!(rest.is_empty(), "dangling {}", rest.join(" "));
    Ok(shift)
}

fn parse_runtest(args: &[String]) -> Result<Command> {
    let mut args = args;
    let mut run_state = None;
    if let [first, rest @ ..] = args {
        if let Ok(state) = parse_state(first) {
            run_state = Some(stable(state)?);
            args = rest;
        }
    }
    let (clocks, rest) = match args {
        [n, unit, rest @ ..] if unit == "TCK" => (
            n.parse::<u32>()
                .with_context(|| format!("bad clock count {n}"))?,
            rest,
        ),
        _ => bail!("only RUNTEST <n> TCK is supported (the cable's TCK is fixed)"),
    };
    let end_state = match rest {
        [] => None,
        [kw, state] if kw == "ENDSTATE" => Some(stable(parse_state(state)?)?),
        _ => bail!("unsupported RUNTEST arguments {}", rest.join(" ")),
    };
    Ok(Command::RunTest {
        run_state,
        clocks,
        end_state,
    })
}

/// Plays commands, keeping the state SVF carries between them.
pub struct Player<'c, C: Cable> {
    cable: &'c mut C,
    /// `None` until the first move: the TAP could be anywhere.
    state: Option<TapState>,
    end_ir: TapState,
    end_dr: TapState,
    run_state: TapState,
    end_state: TapState,
    /// TDI and MASK carry over to the next SIR/SDR of the same length.
    last_ir: Option<Shift>,
    last_dr: Option<Shift>,
}

impl<'c, C: Cable> Player<'c, C> {
    pub fn new(cable: &'c mut C) -> Self {
        Player {
            cable,
            state: None,
            end_ir: TapState::Idle,
            end_dr: TapState::Idle,
            run_state: TapState::Idle,
            end_state: TapState::Idle,
            last_ir: None,
            last_dr: None,
        }
    }

    pub fn play(&mut self, commands: &[Command]) -> Result<()> {
        for (n, command) in commands.iter().enumerate() {
            self.step(command)
                .with_context(|| format!("SVF command {} ({})", n + 1, describe(command)))?;
        }
        self.cable.flush()
    }

    fn goto(&mut self, to: TapState) -> Result<()> {
        let path = match self.state {
            Some(from) if to != TapState::Reset => from.path_to(to),
            // From an unknown state, go through Reset.
            None if to != TapState::Reset => {
                let mut p = vec![true; 5];
                p.extend(TapState::Reset.path_to(to));
                p
            }
            _ => vec![true; 5],
        };
        self.cable.tms(&path)?;
        self.state = Some(to);
        Ok(())
    }

    fn step(&mut self, command: &Command) -> Result<()> {
        match command {
            Command::Nothing => {}
            Command::EndIr(s) => self.end_ir = *s,
            Command::EndDr(s) => self.end_dr = *s,
            Command::State(path) => {
                for &s in path {
                    self.goto(s)?;
                }
            }
            Command::Sir(shift) => self.shift(true, shift)?,
            Command::Sdr(shift) => self.shift(false, shift)?,
            Command::RunTest {
                run_state,
                clocks,
                end_state,
            } => {
                if let Some(s) = run_state {
                    self.run_state = *s;
                    // A new run state resets the end state to it, unless given.
                    self.end_state = *s;
                }
                if let Some(s) = end_state {
                    self.end_state = *s;
                }
                self.goto(self.run_state)?;
                if self.run_state == TapState::Reset {
                    self.cable.tms(&vec![true; *clocks as usize])?;
                } else {
                    self.cable.idle(*clocks)?;
                }
                self.goto(self.end_state)?;
            }
        }
        Ok(())
    }

    fn shift(&mut self, ir: bool, shift: &Shift) -> Result<()> {
        let last = if ir { &self.last_ir } else { &self.last_dr };
        let carried = last.as_ref().filter(|l| l.len == shift.len);
        let tdi = shift
            .tdi
            .clone()
            .or_else(|| carried.and_then(|l| l.tdi.clone()))
            .ok_or_else(|| anyhow!("no TDI, and none carried over from a shift of this length"))?;
        let mask = shift
            .mask
            .clone()
            .or_else(|| carried.and_then(|l| l.mask.clone()))
            .unwrap_or_else(|| Bits::ones(shift.len));
        let resolved = Shift {
            len: shift.len,
            tdi: Some(tdi.clone()),
            tdo: shift.tdo.clone(),
            mask: Some(mask.clone()),
        };
        if ir {
            self.last_ir = Some(resolved);
        } else {
            self.last_dr = Some(resolved);
        }

        let (shift_state, end) = if ir {
            (TapState::IrShift, self.end_ir)
        } else {
            (TapState::DrShift, self.end_dr)
        };
        if shift.len > 0 {
            self.goto(shift_state)?;
            let tdo = self
                .cable
                .shift(&tdi.bytes, shift.len, true, shift.tdo.is_some())?;
            self.state = Some(if ir {
                TapState::IrExit1
            } else {
                TapState::DrExit1
            });
            if let (Some(got), Some(want)) = (tdo, &shift.tdo) {
                let differs = got
                    .iter()
                    .zip(&want.bytes)
                    .zip(&mask.bytes)
                    .any(|((g, w), m)| (g ^ w) & m != 0);
                ensure!(!differs, "TDO mismatch: read {}", hex(&got, shift.len));
            }
        }
        self.goto(end)
    }
}

fn hex(bytes: &[u8], len: usize) -> String {
    let h: String = bytes.iter().rev().map(|b| format!("{b:02X}")).collect();
    h[h.len() - len.div_ceil(4)..].to_string()
}

fn describe(command: &Command) -> String {
    match command {
        Command::Sir(s) => format!("SIR {}", s.len),
        Command::Sdr(s) => format!("SDR {}", s.len),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::SimCable;

    fn bits_of(v: &[bool]) -> u64 {
        v.iter().rev().fold(0, |acc, &b| acc << 1 | b as u64)
    }

    /// cram's sequence, with a 16-bit stand-in for the 6.8 Mbit stream and a
    /// 12-bit status register whose bit 9 plays CONF_DONE.
    const CONFIG: &str = "\
! EP2C35 JTAG configuration, written by cram
TRST ABSENT;
ENDDR IDLE;
ENDIR IRPAUSE;
STATE IDLE;
SIR 10 TDI (002);
RUNTEST IDLE 24000 TCK ENDSTATE IDLE;
SDR 16 TDI (BEEF);
SIR 10 TDI (004);
RUNTEST 120 TCK;
SDR 12 TDI (000) TDO (200) MASK (200);
SIR 10 TDI (003);
RUNTEST 120 TCK;
SIR 10 TDI (3FF);
STATE IDLE;
";

    #[test]
    fn hex_is_msb_first_and_bits_past_the_length_must_be_zero() {
        assert_eq!(Bits::parse_hex(10, "002").unwrap().bytes, [0x02, 0x00]);
        assert_eq!(Bits::parse_hex(10, "3FF").unwrap().bytes, [0xff, 0x03]);
        assert_eq!(Bits::parse_hex(16, "BEEF").unwrap().bytes, [0xef, 0xbe]);
        assert!(Bits::parse_hex(10, "7FF").is_err());
        assert!(Bits::parse_hex(10, "1000").is_err());
    }

    #[test]
    fn plays_the_configuration_sequence() {
        let mut cable = SimCable::new();
        cable.dr_tdo = vec![0x00, 0x02];
        let commands = parse(CONFIG).unwrap();
        Player::new(&mut cable).play(&commands).unwrap();

        let shifts: Vec<(bool, u64, usize)> = cable
            .shifts
            .iter()
            .map(|(ir, bits)| (*ir, bits_of(bits), bits.len()))
            .collect();
        assert_eq!(
            shifts,
            [
                (true, 0x002, 10),
                (false, 0xBEEF, 16),
                (true, 0x004, 10),
                (false, 0x000, 12),
                (true, 0x003, 10),
                (true, 0x3FF, 10),
            ]
        );
        assert_eq!(cable.idle_clocks, 24000 + 120 + 120);
        assert_eq!(cable.state, Some(TapState::Idle));
    }

    #[test]
    fn a_tdo_mismatch_under_the_mask_fails_the_play() {
        let mut cable = SimCable::new();
        // CONF_DONE low, every other bit high: only the masked bit counts.
        cable.dr_tdo = vec![0xff, 0xfd];
        let err = Player::new(&mut cable)
            .play(&parse(CONFIG).unwrap())
            .unwrap_err();
        assert!(format!("{err:#}").contains("TDO mismatch"), "{err:#}");
    }

    #[test]
    fn unsupported_commands_are_errors_not_skipped() {
        assert!(parse("RUNTEST 1E-3 SEC;").is_err());
        assert!(parse("TRST ON;").is_err());
        assert!(parse("HDR 8 TDI (FF);").is_err());
        assert!(parse("PIO (HL);").is_err());
    }

    #[test]
    fn hex_may_span_lines() {
        let c = parse("SDR 16 TDI (BE\n  EF);").unwrap();
        assert_eq!(
            c,
            [Command::Sdr(Shift {
                len: 16,
                tdi: Some(Bits {
                    len: 16,
                    bytes: vec![0xef, 0xbe]
                }),
                tdo: None,
                mask: None,
            })]
        );
    }
}
