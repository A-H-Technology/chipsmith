//! Source text, positions in it, and the errors that point at them.

use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// One text the lexer reads: a file, or a macro's expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(u32);

/// A byte range in one source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: FileId,
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    /// From the start of `self` to the end of `other`, if both are in one
    /// text (a node whose pieces came from different macro expansions keeps
    /// its first piece).
    pub fn to(self, other: Span) -> Span {
        if self.file == other.file && other.hi >= self.lo {
            Span {
                hi: other.hi,
                ..self
            }
        } else {
            self
        }
    }
}

#[derive(Debug)]
enum Origin {
    File,
    /// A macro's body with its arguments substituted, lexed as a text of
    /// its own; positions in it are reported at the macro's use.
    Expansion {
        site: Span,
    },
}

#[derive(Debug)]
pub(crate) struct SourceFile {
    /// The path as given (it goes into `src` attributes, as yosys has it).
    pub name: String,
    pub text: Rc<str>,
    line_starts: Vec<u32>,
    origin: Origin,
}

/// Every text read so far.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

/// A position as people read it: path, 1-based line and column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: String,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}:{}", self.path, self.line, self.col)
    }
}

impl SourceMap {
    pub(crate) fn add_file(&mut self, name: String, text: Rc<str>) -> FileId {
        self.add(name, text, Origin::File)
    }

    pub(crate) fn add_expansion(&mut self, macro_name: &str, text: Rc<str>, site: Span) -> FileId {
        self.add(format!("`{macro_name}"), text, Origin::Expansion { site })
    }

    fn add(&mut self, name: String, text: Rc<str>, origin: Origin) -> FileId {
        let line_starts = std::iter::once(0)
            .chain(
                text.bytes()
                    .enumerate()
                    .filter(|&(_, b)| b == b'\n')
                    .map(|(i, _)| i as u32 + 1),
            )
            .collect();
        self.files.push(SourceFile {
            name,
            text,
            line_starts,
            origin,
        });
        FileId(self.files.len() as u32 - 1)
    }

    pub(crate) fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    /// The path of the file a span is in, through macro expansions.
    pub(crate) fn path(&self, span: Span) -> &str {
        let span = self.unexpand(span);
        &self.file(span.file).name
    }

    /// The span in a real file: a macro expansion's text reports at the
    /// macro's use.
    fn unexpand(&self, mut span: Span) -> Span {
        while let Origin::Expansion { site } = self.file(span.file).origin {
            span = site;
        }
        span
    }

    fn line_col(&self, file: FileId, offset: u32) -> (u32, u32) {
        let f = self.file(file);
        let line = f.line_starts.partition_point(|&s| s <= offset) - 1;
        let start = f.line_starts[line] as usize;
        let col = f.text[start..offset as usize].chars().count() + 1;
        (line as u32 + 1, col as u32)
    }

    pub fn location(&self, span: Span) -> Location {
        let span = self.unexpand(span);
        let (line, col) = self.line_col(span.file, span.lo);
        Location {
            path: self.file(span.file).name.clone(),
            line,
            col,
        }
    }

    /// yosys' `src` attribute: `path:line.col-line.col`, end exclusive.
    pub fn src(&self, span: Span) -> String {
        let span = self.unexpand(span);
        let (l0, c0) = self.line_col(span.file, span.lo);
        let (l1, c1) = self.line_col(span.file, span.hi);
        format!("{}:{l0}.{c0}-{l1}.{c1}", self.file(span.file).name)
    }
}

/// An error at a place in the source, as the front end passes it around.
#[derive(Debug, Clone)]
pub(crate) struct Diag {
    pub span: Option<Span>,
    pub message: String,
}

pub(crate) type Result<T, E = Diag> = std::result::Result<T, E>;

impl Diag {
    pub fn new(span: Span, message: impl Into<String>) -> Diag {
        Diag {
            span: Some(span),
            message: message.into(),
        }
    }

    pub fn global(message: impl Into<String>) -> Diag {
        Diag {
            span: None,
            message: message.into(),
        }
    }
}

/// Builds an `Err(Diag)` at a span: `bail!(span, "x {}", y)`.
macro_rules! bail {
    ($span:expr, $($fmt:tt)*) => {
        return Err($crate::source::Diag::new($span, format!($($fmt)*)))
    };
}
pub(crate) use bail;

/// What went wrong reading or elaborating the design, and where.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct Error {
    pub location: Option<Location>,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.location {
            Some(l) => write!(f, "{l}: {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl Error {
    pub(crate) fn from_diag(sources: &SourceMap, d: Diag) -> Error {
        Error {
            location: d.span.map(|s| sources.location(s)),
            message: d.message,
        }
    }

    pub(crate) fn io(path: &Path, e: std::io::Error) -> Error {
        Error {
            location: None,
            message: format!("reading {}: {e}", path.display()),
        }
    }
}

/// A path relative to the directory a source file is in.
pub(crate) fn sibling(of: &str, name: &str) -> PathBuf {
    Path::new(of)
        .parent()
        .map_or_else(|| PathBuf::from(name), |d| d.join(name))
}
