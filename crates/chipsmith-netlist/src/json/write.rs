//! The IR as yosys' JSON netlist, for `read_json`.

use std::io::{self, Write};

use crate::module::{Design, Module};
use crate::param::{Attributes, ParamValue};
use crate::sig::SigBit;

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid design: {0}")]
    Invalid(String),
}

/// Writes the design in `write_json`'s format (and layout, so the two
/// diff cleanly). Fails without writing anything if [`Design::check`]
/// does: yosys would reject or misread the result.
pub fn write_json(design: &Design, out: impl Write) -> Result<(), WriteError> {
    design.check().map_err(WriteError::Invalid)?;
    let mut w = io::BufWriter::new(out);
    writeln!(w, "{{")?;
    write!(w, "  \"creator\": ")?;
    string(
        &mut w,
        concat!("chipsmith-netlist ", env!("CARGO_PKG_VERSION")),
    )?;
    writeln!(w, ",")?;
    writeln!(w, "  \"modules\": {{")?;
    for (i, m) in design.modules.iter().enumerate() {
        module(&mut w, m)?;
        writeln!(w, "{}", comma(i, design.modules.len()))?;
    }
    writeln!(w, "  }}")?;
    writeln!(w, "}}")?;
    w.flush()?;
    Ok(())
}

fn comma(i: usize, n: usize) -> &'static str {
    if i + 1 < n {
        ","
    } else {
        ""
    }
}

fn string(w: &mut impl Write, s: &str) -> io::Result<()> {
    serde_json::to_writer(&mut *w, s).map_err(io::Error::from)
}

/// A JSON net id: yosys numbers from 2, so ids never read as the
/// constants `0` and `1`.
fn bits(w: &mut impl Write, bits: &[SigBit]) -> io::Result<()> {
    write!(w, "[")?;
    for (i, b) in bits.iter().enumerate() {
        let sep = if i == 0 { " " } else { ", " };
        match b {
            SigBit::Net(n) => write!(w, "{sep}{}", n.index() + 2)?,
            SigBit::Const(s) => write!(w, "{sep}\"{}\"", s.char())?,
        }
    }
    write!(w, " ]")
}

/// The inverse of the reader's `parse_param`: a string that would read
/// back as bits gets a trailing space, as yosys' writer adds.
fn param(w: &mut impl Write, v: &ParamValue) -> io::Result<()> {
    match v {
        ParamValue::Bits(c) => write!(w, "\"{c}\""),
        ParamValue::Str(s) => {
            let looks_like_bits = match s.find(|c| !matches!(c, '0' | '1' | 'x' | 'z')) {
                None => true,
                Some(i) => s[i..].bytes().all(|b| b == b' '),
            };
            if looks_like_bits {
                string(w, &format!("{s} "))
            } else {
                string(w, s)
            }
        }
    }
}

fn params<'a>(
    w: &mut impl Write,
    indent: &str,
    key: &str,
    entries: impl ExactSizeIterator<Item = (&'a str, &'a ParamValue)>,
) -> io::Result<()> {
    let n = entries.len();
    write!(w, "{indent}\"{key}\": {{")?;
    for (i, (k, v)) in entries.enumerate() {
        write!(w, "\n{indent}  ")?;
        string(w, k)?;
        write!(w, ": ")?;
        param(w, v)?;
        write!(w, "{}", comma(i, n))?;
    }
    if n > 0 {
        write!(w, "\n{indent}")?;
    }
    write!(w, "}}")
}

fn attrs(w: &mut impl Write, indent: &str, a: &Attributes) -> io::Result<()> {
    params(
        w,
        indent,
        "attributes",
        a.iter().map(|(k, v)| (k.as_str(), v)),
    )
}

fn hide_name(name: &str) -> u8 {
    name.starts_with('$') as u8
}

fn module(w: &mut impl Write, m: &Module) -> io::Result<()> {
    write!(w, "    ")?;
    string(w, &m.name)?;
    writeln!(w, ": {{")?;
    attrs(w, "      ", &m.attributes)?;
    writeln!(w, ",")?;
    if !m.parameter_default_values.is_empty() {
        params(
            w,
            "      ",
            "parameter_default_values",
            m.parameter_default_values
                .iter()
                .map(|(k, v)| (k.as_str(), v)),
        )?;
        writeln!(w, ",")?;
    }

    write!(w, "      \"ports\": {{")?;
    let ports: Vec<_> = m.ports().collect();
    for (i, (_, wire, dir)) in ports.iter().enumerate() {
        write!(w, "\n        ")?;
        string(w, &wire.name)?;
        write!(
            w,
            ": {{\n          \"direction\": \"{}\",\n          \"bits\": ",
            dir.name()
        )?;
        bits(w, &wire.bits)?;
        wire_shape(w, "          ", wire.offset, wire.upto, wire.signed)?;
        write!(w, "\n        }}{}", comma(i, ports.len()))?;
    }
    writeln!(w, "\n      }},")?;

    write!(w, "      \"cells\": {{")?;
    for (i, c) in m.cells.iter().enumerate() {
        let g = c.kind.to_generic();
        write!(w, "\n        ")?;
        string(w, &c.name)?;
        writeln!(w, ": {{")?;
        writeln!(w, "          \"hide_name\": {},", hide_name(&c.name))?;
        write!(w, "          \"type\": ")?;
        string(w, &g.ty)?;
        writeln!(w, ",")?;
        params(
            w,
            "          ",
            "parameters",
            g.params.iter().map(|(k, v)| (k.as_str(), v)),
        )?;
        writeln!(w, ",")?;
        attrs(w, "          ", &c.attributes)?;
        writeln!(w, ",")?;
        let directed: Vec<_> = g.ports.iter().filter(|p| p.dir.is_some()).collect();
        if !directed.is_empty() {
            write!(w, "          \"port_directions\": {{")?;
            for (j, p) in directed.iter().enumerate() {
                write!(w, "\n            ")?;
                string(w, &p.name)?;
                let dir = p.dir.expect("filtered").name();
                write!(w, ": \"{dir}\"{}", comma(j, directed.len()))?;
            }
            writeln!(w, "\n          }},")?;
        }
        write!(w, "          \"connections\": {{")?;
        for (j, p) in g.ports.iter().enumerate() {
            write!(w, "\n            ")?;
            string(w, &p.name)?;
            write!(w, ": ")?;
            bits(w, &p.sig)?;
            write!(w, "{}", comma(j, g.ports.len()))?;
        }
        if !g.ports.is_empty() {
            write!(w, "\n          ")?;
        }
        write!(w, "}}\n        }}{}", comma(i, m.cells.len()))?;
    }
    writeln!(w, "\n      }},")?;

    if !m.memories.is_empty() {
        write!(w, "      \"memories\": {{")?;
        for (i, mem) in m.memories.iter().enumerate() {
            write!(w, "\n        ")?;
            string(w, &mem.name)?;
            writeln!(w, ": {{")?;
            writeln!(w, "          \"hide_name\": {},", hide_name(&mem.name))?;
            attrs(w, "          ", &mem.attributes)?;
            writeln!(w, ",")?;
            writeln!(w, "          \"width\": {},", mem.width)?;
            writeln!(w, "          \"start_offset\": {},", mem.start_offset)?;
            write!(w, "          \"size\": {}\n        }}", mem.size)?;
            write!(w, "{}", comma(i, m.memories.len()))?;
        }
        writeln!(w, "\n      }},")?;
    }

    write!(w, "      \"netnames\": {{")?;
    for (i, wire) in m.wires.iter().enumerate() {
        write!(w, "\n        ")?;
        string(w, &wire.name)?;
        writeln!(w, ": {{")?;
        writeln!(w, "          \"hide_name\": {},", hide_name(&wire.name))?;
        write!(w, "          \"bits\": ")?;
        bits(w, &wire.bits)?;
        writeln!(w, ",")?;
        attrs(w, "          ", &wire.attributes)?;
        wire_shape(w, "          ", wire.offset, wire.upto, wire.signed)?;
        write!(w, "\n        }}{}", comma(i, m.wires.len()))?;
    }
    writeln!(w, "\n      }}")?;
    write!(w, "    }}")
}

/// `offset`, `upto` and `signed`, which yosys writes only when set.
fn wire_shape(
    w: &mut impl Write,
    indent: &str,
    offset: i64,
    upto: bool,
    signed: bool,
) -> io::Result<()> {
    if offset != 0 {
        write!(w, ",\n{indent}\"offset\": {offset}")?;
    }
    if upto {
        write!(w, ",\n{indent}\"upto\": 1")?;
    }
    if signed {
        write!(w, ",\n{indent}\"signed\": 1")?;
    }
    Ok(())
}
