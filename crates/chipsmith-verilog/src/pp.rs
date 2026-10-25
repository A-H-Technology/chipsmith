//! The preprocessor: macros, conditional compilation, includes.
//!
//! Macros are textual, as IEEE 1364 §19.3 has them: a use's arguments are
//! substituted into the body as text, and the result is lexed as a text of
//! its own (a [`SourceMap`] expansion, whose positions report at the use).
//! That keeps `` `` `` pasting and macros that expand to partial
//! constructs working, which a token-level preprocessor can't do.
//!
//! The output is the whole token stream of a source file, plus the
//! `` `default_nettype `` changes as [`Pragma`] markers the parser picks up.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use rustc_hash::FxHashMap;

use crate::lex::{Lexer, Tok, Token};
use crate::source::{bail, sibling, Diag, Result, SourceMap, Span};

#[derive(Debug, Clone)]
struct Macro {
    /// `None` for an object-like macro; otherwise the formals, each with
    /// its default text (`` `define M(a, b=1) ``).
    params: Option<Vec<(String, Option<String>)>>,
    body: String,
}

/// Defines that persist across files, as `read_verilog -D` and earlier
/// files' `` `define ``s do in yosys.
#[derive(Debug, Default, Clone)]
pub struct Defines {
    macros: FxHashMap<String, Macro>,
}

impl Defines {
    pub fn define(&mut self, name: &str, body: &str) {
        self.macros.insert(
            name.to_owned(),
            Macro {
                params: None,
                body: body.to_owned(),
            },
        );
    }
}

/// A net type implicit nets get (`` `default_nettype ``).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultNettype {
    Wire,
    /// `none`: an undeclared name is an error.
    None,
}

/// The preprocessed tokens, in order.
pub struct Tokens {
    pub tokens: Vec<Token>,
    /// `` `default_nettype `` changes: from token index on.
    pub nettype: Vec<(usize, DefaultNettype)>,
}

enum Cond {
    /// Emitting tokens; `done` once a branch of this `ifdef` was taken.
    Active,
    /// Skipping, and a later `elsif`/`else` may still be taken.
    Waiting,
    /// Skipping to the `endif`: a branch was taken, or the enclosing
    /// region is skipped.
    Done,
}

pub struct Preprocessor<'a> {
    sources: &'a mut SourceMap,
    defines: &'a mut Defines,
    include_dirs: &'a [PathBuf],
    lexers: Vec<Lexer>,
    conds: Vec<(Cond, Span)>,
    out: Tokens,
}

const MAX_DEPTH: usize = 200;

impl<'a> Preprocessor<'a> {
    pub fn run(
        sources: &'a mut SourceMap,
        defines: &'a mut Defines,
        include_dirs: &'a [PathBuf],
        file: crate::source::FileId,
    ) -> Result<Tokens> {
        let text = sources.file(file).text.clone();
        let len = text.len();
        let mut pp = Preprocessor {
            sources,
            defines,
            include_dirs,
            lexers: vec![Lexer::new(file, text)],
            conds: Vec::new(),
            out: Tokens {
                tokens: Vec::with_capacity(len / 6),
                nettype: Vec::new(),
            },
        };
        pp.process()?;
        Ok(pp.out)
    }

    fn active(&self) -> bool {
        self.conds.iter().all(|(c, _)| matches!(c, Cond::Active))
    }

    fn lexer(&mut self) -> &mut Lexer {
        self.lexers.last_mut().expect("a lexer while processing")
    }

    fn process(&mut self) -> Result<()> {
        loop {
            let active = self.active();
            let t = match self.lexer().next_token() {
                Ok(t) => t,
                // Text in a skipped region needn't lex.
                Err(_) if !active => {
                    self.skip_char();
                    continue;
                }
                Err(e) => return Err(e),
            };
            match t.tok {
                Tok::Eof => {
                    self.lexers.pop();
                    if self.lexers.is_empty() {
                        if let Some((_, span)) = self.conds.last() {
                            bail!(*span, "`ifdef without `endif");
                        }
                        let eof = t;
                        self.out.tokens.push(eof);
                        return Ok(());
                    }
                }
                Tok::Directive(name) => self.directive(&name, t.span, active)?,
                _ if active => self.out.tokens.push(t),
                _ => {}
            }
        }
    }

    /// Steps over a character a skipped region can't lex.
    fn skip_char(&mut self) {
        let l = self.lexer();
        let (rest, _) = l.rest_of_line();
        let _ = rest;
    }

    fn directive(&mut self, name: &str, span: Span, active: bool) -> Result<()> {
        match name {
            "ifdef" | "ifndef" | "elsif" => {
                let ident = self.directive_ident(span, name)?;
                let defined = self.defines.macros.contains_key(&ident);
                let take = if name == "ifndef" { !defined } else { defined };
                if name == "elsif" {
                    let Some((c, _)) = self.conds.last_mut() else {
                        bail!(span, "`elsif without `ifdef");
                    };
                    *c = match c {
                        Cond::Waiting if take => Cond::Active,
                        Cond::Waiting => Cond::Waiting,
                        Cond::Active | Cond::Done => Cond::Done,
                    };
                } else {
                    let c = if !active {
                        Cond::Done
                    } else if take {
                        Cond::Active
                    } else {
                        Cond::Waiting
                    };
                    self.conds.push((c, span));
                }
            }
            "else" => {
                let Some((c, _)) = self.conds.last_mut() else {
                    bail!(span, "`else without `ifdef");
                };
                *c = match c {
                    Cond::Waiting => Cond::Active,
                    Cond::Active | Cond::Done => Cond::Done,
                };
            }
            "endif" => {
                if self.conds.pop().is_none() {
                    bail!(span, "`endif without `ifdef");
                }
            }
            _ if !active => {
                // A skipped `define's body may not even lex.
                if name == "define" {
                    self.lexer().rest_of_line();
                }
            }
            "define" => self.define(span)?,
            "undef" => {
                let ident = self.directive_ident(span, name)?;
                self.defines.macros.remove(&ident);
            }
            "undefineall" => self.defines.macros.clear(),
            "include" => self.include(span)?,
            "timescale" | "celldefine" | "endcelldefine" | "pragma" | "line" => {
                // No meaning for synthesis.
                if !matches!(name, "celldefine" | "endcelldefine") {
                    self.lexer().rest_of_line();
                }
            }
            "default_nettype" => {
                let ident = self.directive_ident(span, name)?;
                let kind = match ident.as_str() {
                    "none" => DefaultNettype::None,
                    "wire" | "tri" => DefaultNettype::Wire,
                    other => bail!(
                        span,
                        "`default_nettype {other} is not supported (only wire and none)"
                    ),
                };
                let at = self.out.tokens.len();
                self.out.nettype.push((at, kind));
            }
            "resetall" => {
                let at = self.out.tokens.len();
                self.out.nettype.push((at, DefaultNettype::Wire));
            }
            "unconnected_drive" | "nounconnected_drive" | "begin_keywords" | "end_keywords" => {
                bail!(span, "`{name} is not supported")
            }
            _ => self.expand(name, span)?,
        }
        Ok(())
    }

    fn directive_ident(&mut self, span: Span, directive: &str) -> Result<String> {
        let t = self.lexer().next_token()?;
        match t.tok {
            Tok::Ident(s) => Ok(s.to_string()),
            Tok::Kw(k) => Ok(k.name().to_owned()),
            other => bail!(
                span.to(t.span),
                "expected a name after `{directive}, found {}",
                other.describe()
            ),
        }
    }

    fn define(&mut self, span: Span) -> Result<()> {
        let name = self.directive_ident(span, "define")?;
        let params = if self.lexer().at_paren() {
            let Some(args) = self.lexer().macro_args()? else {
                unreachable!("at_paren saw the parenthesis")
            };
            let mut params = Vec::new();
            for a in args {
                let (formal, default) = match a.split_once('=') {
                    Some((f, d)) => (f.trim().to_owned(), Some(d.trim().to_owned())),
                    None => (a.trim().to_owned(), None),
                };
                if formal.is_empty() && params.is_empty() && default.is_none() {
                    continue; // `define M() ...
                }
                if !formal
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    bail!(span, "bad macro parameter `{formal}` in `define {name}");
                }
                params.push((formal, default));
            }
            Some(params)
        } else {
            None
        };
        let (body, _) = self.lexer().rest_of_line();
        self.defines.macros.insert(
            name,
            Macro {
                params,
                body: body.trim().to_owned(),
            },
        );
        Ok(())
    }

    fn include(&mut self, span: Span) -> Result<()> {
        let t = self.lexer().next_token()?;
        let Tok::Str(name) = t.tok else {
            bail!(span, "expected a file name in quotes after `include");
        };
        let name = String::from_utf8_lossy(&name).into_owned();
        let current = self.sources.path(span).to_owned();
        let candidates = std::iter::once(sibling(&current, &name))
            .chain(self.include_dirs.iter().map(|d| d.join(&name)))
            .chain(std::iter::once(PathBuf::from(&name)));
        let Some(path) = candidates.into_iter().find(|p| p.is_file()) else {
            bail!(span.to(t.span), "`include \"{name}\": file not found (searched next to {current} and the include directories)");
        };
        if self.lexers.len() > MAX_DEPTH {
            bail!(span, "`include nested too deeply");
        }
        let text = read(&path).map_err(|m| Diag::new(span, m))?;
        let file = self
            .sources
            .add_file(path.display().to_string(), text.clone());
        self.lexers.push(Lexer::new(file, text));
        Ok(())
    }

    fn expand(&mut self, name: &str, span: Span) -> Result<()> {
        let Some(m) = self.defines.macros.get(name).cloned() else {
            bail!(span, "undefined macro `{name}");
        };
        if self.lexers.len() > MAX_DEPTH {
            bail!(span, "macro `{name} expands recursively");
        }
        let text = match &m.params {
            None => m.body.clone(),
            Some(params) => {
                let Some(mut args) = self.lexer().macro_args()? else {
                    bail!(span, "macro `{name} takes arguments");
                };
                if params.is_empty() && args.len() == 1 && args[0].trim().is_empty() {
                    args.clear();
                }
                if args.len() > params.len() {
                    bail!(
                        span,
                        "macro `{name} takes {} arguments, given {}",
                        params.len(),
                        args.len()
                    );
                }
                let mut values = Vec::new();
                for (i, (formal, default)) in params.iter().enumerate() {
                    let given = args.get(i).map(|a| a.trim()).filter(|a| !a.is_empty());
                    match (given, default) {
                        (Some(a), _) => values.push((formal.as_str(), a.to_owned())),
                        (None, Some(d)) => values.push((formal.as_str(), d.clone())),
                        (None, None) if args.len() == params.len() => {
                            values.push((formal.as_str(), String::new()))
                        }
                        (None, None) => bail!(span, "macro `{name}: no value for `{formal}`"),
                    }
                }
                substitute(&m.body, &values)
            }
        };
        let text: Rc<str> = text.into();
        let file = self.sources.add_expansion(name, text.clone(), span);
        self.lexers.push(Lexer::new(file, text));
        Ok(())
    }
}

pub(crate) fn read(path: &Path) -> std::result::Result<Rc<str>, String> {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into())
        .map_err(|e| format!("reading {}: {e}", path.display()))
}

/// A macro body with its formals replaced, and `` `` `` (paste), `` `" ``
/// and `` `\`" `` applied.
fn substitute(body: &str, values: &[(&str, String)]) -> String {
    let b = body.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let mut in_string = false;
    while i < b.len() {
        let c = b[i];
        if c == b'`' && b.get(i + 1) == Some(&b'`') {
            i += 2;
            continue;
        }
        if c == b'`' && b.get(i + 1) == Some(&b'"') {
            out.push('"');
            in_string = !in_string;
            i += 2;
            continue;
        }
        if c == b'`' && body[i..].starts_with("`\\`\"") {
            out.push_str("\\\"");
            i += 4;
            continue;
        }
        if c == b'"' {
            in_string = !in_string;
        }
        if (c.is_ascii_alphabetic() || c == b'_') && (i == 0 || !is_word(b[i - 1])) {
            let start = i;
            while i < b.len() && is_word(b[i]) {
                i += 1;
            }
            let word = &body[start..i];
            // A formal is replaced even inside a `"-quoted string, which
            // is what `" is for.
            match values.iter().find(|(f, _)| *f == word) {
                Some((_, v)) if !in_string || body[..start].contains("`\"") => out.push_str(v),
                _ => out.push_str(word),
            }
            continue;
        }
        let ch = body[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pp(text: &str) -> Result<Vec<Tok>> {
        let mut sm = SourceMap::default();
        let f = sm.add_file("t.v".into(), text.into());
        let mut defs = Defines::default();
        let toks = Preprocessor::run(&mut sm, &mut defs, &[], f)?;
        Ok(toks.tokens.into_iter().map(|t| t.tok).collect())
    }

    fn idents(toks: &[Tok]) -> Vec<String> {
        toks.iter()
            .filter_map(|t| match t {
                Tok::Ident(s) => Some(s.to_string()),
                Tok::Decimal(s) => Some(s.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn conditionals() {
        let t = pp("`define A\n`ifdef A a `elsif B b `else c `endif\n\
                    `ifndef A d `elsif A e `else f `endif\n\
                    `ifdef X `ifdef A g `else h `endif `else i `endif")
        .unwrap();
        assert_eq!(idents(&t), ["a", "e", "i"]);
    }

    #[test]
    fn function_like_macros() {
        let t = pp("`define ADD(x, y=1) (x + y)\n`define TWICE(z) `ADD(z, z)\n\
                    `TWICE(q) `ADD(p) `define debug(c)\n `debug($display(\"a, b\");) r")
        .unwrap();
        assert_eq!(idents(&t), ["q", "q", "p", "1", "r"]);
    }

    #[test]
    fn continuation_and_comments() {
        let t = pp("`define M a \\\n b // c\n`M d").unwrap();
        assert_eq!(idents(&t), ["a", "b", "d"]);
    }

    #[test]
    fn undefined_macro_is_an_error() {
        let e = pp("x `nope y").unwrap_err();
        assert!(e.message.contains("undefined macro `nope"), "{}", e.message);
    }
}
