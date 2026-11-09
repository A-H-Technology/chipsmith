//! Verilog tokens, and the lexer that reads one text into them.
//!
//! Compiler directives come out as [`Tok::Directive`] tokens; the
//! preprocessor ([`crate::pp`]) acts on them and asks the lexer for the raw
//! text a directive takes (a macro body, a macro's arguments).

use std::rc::Rc;

use crate::source::{bail, FileId, Result, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    Bin,
    Oct,
    Dec,
    Hex,
}

impl Base {
    pub fn bits_per_digit(self) -> Option<u32> {
        match self {
            Base::Bin => Some(1),
            Base::Oct => Some(3),
            Base::Hex => Some(4),
            Base::Dec => None,
        }
    }
}

/// Which language a source file is in. SystemVerilog's keywords are a
/// superset of Verilog's; a `.v` file may still use `logic` as a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dialect {
    /// IEEE 1364-2005.
    Verilog2005,
    /// IEEE 1800-2017: what synthesis needs of it (`logic`,
    /// `always_comb`/`always_ff`/`always_latch`, `unique`/`priority`),
    /// the rest of its keywords reserved.
    SystemVerilog2017,
}

impl Dialect {
    /// By extension, as Quartus and Vivado decide: `.sv` is
    /// SystemVerilog, anything else Verilog.
    pub fn of_path(path: &std::path::Path) -> Dialect {
        match path.extension().and_then(|e| e.to_str()) {
            Some("sv") => Dialect::SystemVerilog2017,
            _ => Dialect::Verilog2005,
        }
    }
}

macro_rules! keywords {
    (
        verilog { $($kw:ident = $s:literal),* $(,)? }
        systemverilog { $($skw:ident = $ss:literal),* $(,)? }
    ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Kw { $($kw,)* $($skw),* }
        impl Kw {
            /// `s` as a keyword of `dialect`, if it is one.
            pub fn parse(s: &str, dialect: Dialect) -> Option<Kw> {
                // A match on the string compares against every keyword in
                // turn; identifiers are most of the tokens.
                static TABLE: std::sync::OnceLock<rustc_hash::FxHashMap<&'static str, Kw>> =
                    std::sync::OnceLock::new();
                TABLE
                    .get_or_init(|| [$(($s, Kw::$kw),)* $(($ss, Kw::$skw)),*].into_iter().collect())
                    .get(s)
                    .copied()
                    .filter(|k| k.dialect() <= dialect)
            }
            pub fn name(self) -> &'static str {
                match self { $(Kw::$kw => $s,)* $(Kw::$skw => $ss),* }
            }
            /// The first dialect that reserves it.
            fn dialect(self) -> Dialect {
                match self {
                    $(Kw::$kw => Dialect::Verilog2005,)*
                    $(Kw::$skw => Dialect::SystemVerilog2017),*
                }
            }
        }
    };
}

// IEEE 1364-2005 Annex B and IEEE 1800-2017 Annex B, all of them: a
// design can't use one as a name even where chipsmith doesn't support the
// construct.
keywords! {
    verilog {
    Always = "always", And = "and", Assign = "assign", Automatic = "automatic",
    Begin = "begin", Buf = "buf", Bufif0 = "bufif0", Bufif1 = "bufif1",
    Case = "case", Casex = "casex", Casez = "casez", Cell = "cell", Cmos = "cmos",
    Config = "config", Deassign = "deassign", Default = "default",
    Defparam = "defparam", Design = "design", Disable = "disable", Edge = "edge",
    Else = "else", End = "end", Endcase = "endcase", Endconfig = "endconfig",
    Endfunction = "endfunction", Endgenerate = "endgenerate",
    Endmodule = "endmodule", Endprimitive = "endprimitive",
    Endspecify = "endspecify", Endtable = "endtable", Endtask = "endtask",
    Event = "event", For = "for", Force = "force", Forever = "forever",
    Fork = "fork", Function = "function", Generate = "generate",
    Genvar = "genvar", Highz0 = "highz0", Highz1 = "highz1", If = "if",
    Ifnone = "ifnone", Incdir = "incdir", Include = "include",
    Initial = "initial", Inout = "inout", Input = "input", Instance = "instance",
    Integer = "integer", Join = "join", Large = "large", Liblist = "liblist",
    Library = "library", Localparam = "localparam", Macromodule = "macromodule",
    Medium = "medium", Module = "module", Nand = "nand", Negedge = "negedge",
    Nmos = "nmos", Nor = "nor", Noshowcancelled = "noshowcancelled", Not = "not",
    Notif0 = "notif0", Notif1 = "notif1", Or = "or", Output = "output",
    Parameter = "parameter", Pmos = "pmos", Posedge = "posedge",
    Primitive = "primitive", Pull0 = "pull0", Pull1 = "pull1",
    Pulldown = "pulldown", Pullup = "pullup",
    PulsestyleOnevent = "pulsestyle_onevent",
    PulsestyleOndetect = "pulsestyle_ondetect", Rcmos = "rcmos", Real = "real",
    Realtime = "realtime", Reg = "reg", Release = "release", Repeat = "repeat",
    Rnmos = "rnmos", Rpmos = "rpmos", Rtran = "rtran", Rtranif0 = "rtranif0",
    Rtranif1 = "rtranif1", Scalared = "scalared",
    Showcancelled = "showcancelled", Signed = "signed", Small = "small",
    Specify = "specify", Specparam = "specparam", Strong0 = "strong0",
    Strong1 = "strong1", Supply0 = "supply0", Supply1 = "supply1",
    Table = "table", Task = "task", Time = "time", Tran = "tran",
    Tranif0 = "tranif0", Tranif1 = "tranif1", Tri = "tri", Tri0 = "tri0",
    Tri1 = "tri1", Triand = "triand", Trior = "trior", Trireg = "trireg",
    Unsigned = "unsigned", Use = "use", Uwire = "uwire", Vectored = "vectored",
    Wait = "wait", Wand = "wand", Weak0 = "weak0", Weak1 = "weak1",
    While = "while", Wire = "wire", Wor = "wor", Xnor = "xnor", Xor = "xor",
    }
    systemverilog {
    AcceptOn = "accept_on", Alias = "alias", AlwaysComb = "always_comb",
    AlwaysFf = "always_ff", AlwaysLatch = "always_latch", Assert = "assert",
    Assume = "assume", Before = "before", Bind = "bind", Bins = "bins",
    Binsof = "binsof", Bit = "bit", Break = "break", Byte = "byte",
    Chandle = "chandle", Checker = "checker", Class = "class",
    Clocking = "clocking", Const = "const", Constraint = "constraint",
    Context = "context", Continue = "continue", Cover = "cover",
    Covergroup = "covergroup", Coverpoint = "coverpoint", Cross = "cross",
    Dist = "dist", Do = "do", Endchecker = "endchecker", Endclass = "endclass",
    Endclocking = "endclocking", Endgroup = "endgroup",
    Endinterface = "endinterface", Endpackage = "endpackage",
    Endprogram = "endprogram", Endproperty = "endproperty",
    Endsequence = "endsequence", Enum = "enum", Eventually = "eventually",
    Expect = "expect", Export = "export", Extends = "extends", Extern = "extern",
    Final = "final", FirstMatch = "first_match", Foreach = "foreach",
    Forkjoin = "forkjoin", Global = "global", Iff = "iff",
    IgnoreBins = "ignore_bins", IllegalBins = "illegal_bins",
    Implements = "implements", Implies = "implies", Import = "import",
    Inside = "inside", Int = "int", Interconnect = "interconnect",
    Interface = "interface", Intersect = "intersect", JoinAny = "join_any",
    JoinNone = "join_none", Let = "let", Local = "local", Logic = "logic",
    Longint = "longint", Matches = "matches", Modport = "modport",
    Nettype = "nettype", New = "new", Nexttime = "nexttime", Null = "null",
    Package = "package", Packed = "packed", Priority = "priority",
    Program = "program", Property = "property", Protected = "protected",
    Pure = "pure", Rand = "rand", Randc = "randc", Randcase = "randcase",
    Randsequence = "randsequence", Ref = "ref", RejectOn = "reject_on",
    Restrict = "restrict", Return = "return", SAlways = "s_always",
    SEventually = "s_eventually", SNexttime = "s_nexttime", SUntil = "s_until",
    SUntilWith = "s_until_with", Sequence = "sequence", Shortint = "shortint",
    Shortreal = "shortreal", Soft = "soft", Solve = "solve", Static = "static",
    String = "string", Strong = "strong", Struct = "struct", Super = "super",
    SyncAcceptOn = "sync_accept_on", SyncRejectOn = "sync_reject_on",
    Tagged = "tagged", This = "this", Throughout = "throughout",
    Timeprecision = "timeprecision", Timeunit = "timeunit", Type = "type",
    Typedef = "typedef", Union = "union", Unique = "unique", Unique0 = "unique0",
    Until = "until", UntilWith = "until_with", Untyped = "untyped", Var = "var",
    Virtual = "virtual", Void = "void", WaitOrder = "wait_order", Weak = "weak",
    Wildcard = "wildcard", With = "with", Within = "within",
    }
}

macro_rules! puncts {
    ($($p:ident = $s:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum P { $($p),* }
        impl P {
            pub fn text(self) -> &'static str {
                match self { $(P::$p => $s),* }
            }
        }
        /// Longest first, so the lexer takes `<<<` before `<<` before `<`.
        const PUNCTS: &[(&str, P)] = &{
            let mut a = [$(($s, P::$p)),*];
            // Insertion sort by length, descending (const context).
            let mut i = 1;
            while i < a.len() {
                let mut j = i;
                while j > 0 && a[j - 1].0.len() < a[j].0.len() {
                    let t = a[j - 1];
                    a[j - 1] = a[j];
                    a[j] = t;
                    j -= 1;
                }
                i += 1;
            }
            a
        };
    };
}

puncts! {
    LParen = "(", RParen = ")", LBrack = "[", RBrack = "]", LBrace = "{",
    RBrace = "}", Comma = ",", Semi = ";", Colon = ":", Question = "?",
    Hash = "#", At = "@", Dot = ".", Eq = "=", Plus = "+", Minus = "-",
    Star = "*", Slash = "/", Percent = "%", Pow = "**", Bang = "!", Tilde = "~",
    Amp = "&", Pipe = "|", Caret = "^", TildeCaret = "~^", CaretTilde = "^~",
    TildeAmp = "~&", TildePipe = "~|", AmpAmp = "&&", PipePipe = "||",
    EqEq = "==", BangEq = "!=", EqEqEq = "===", BangEqEq = "!==", Lt = "<",
    Le = "<=", Gt = ">", Ge = ">=", Shl = "<<", Shr = ">>", AShl = "<<<",
    AShr = ">>>", Arrow = "->", PlusColon = "+:", MinusColon = "-:",
    AttrOpen = "(*", AttrClose = "*)",
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// A simple or escaped identifier (without the backslash).
    Ident(Rc<str>),
    /// `$name`
    System(Rc<str>),
    /// `` `name ``: a compiler directive or macro use.
    Directive(Rc<str>),
    Kw(Kw),
    /// An unsigned decimal number, underscores removed: a size, or an
    /// unsized decimal constant.
    Decimal(Rc<str>),
    /// `'sh1F`: base, signedness and digits (underscores removed,
    /// lowercase); a size before it is a separate [`Tok::Decimal`].
    Based {
        signed: bool,
        base: Base,
        digits: Rc<str>,
    },
    /// `'0`, `'1`, `'x`, `'z`: every bit of the context's width.
    Fill(char),
    Real(Rc<str>),
    Str(Rc<[u8]>),
    P(P),
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("identifier `{s}`"),
            Tok::System(s) => format!("`${s}`"),
            Tok::Directive(s) => format!("`` `{s} ``"),
            Tok::Kw(k) => format!("`{}`", k.name()),
            Tok::Decimal(d) => format!("number `{d}`"),
            Tok::Based { .. } | Tok::Fill(_) => "number".into(),
            Tok::Real(r) => format!("real `{r}`"),
            Tok::Str(_) => "string".into(),
            Tok::P(p) => format!("`{}`", p.text()),
            Tok::Eof => "end of input".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub struct Lexer {
    pub file: FileId,
    text: Rc<str>,
    dialect: Dialect,
    pos: usize,
    /// Inside `(* ... *)`: only there is `*)` one token.
    attr_depth: u32,
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

impl Lexer {
    pub fn new(file: FileId, text: Rc<str>, dialect: Dialect) -> Lexer {
        Lexer {
            file,
            text,
            dialect,
            pos: 0,
            attr_depth: 0,
        }
    }

    fn bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    fn peek(&self, k: usize) -> u8 {
        self.bytes().get(self.pos + k).copied().unwrap_or(0)
    }

    fn span(&self, lo: usize) -> Span {
        Span {
            file: self.file,
            lo: lo as u32,
            hi: self.pos as u32,
        }
    }

    /// Whitespace and comments.
    fn skip_trivia(&mut self) -> Result<()> {
        loop {
            match (self.peek(0), self.peek(1)) {
                (b' ' | b'\t' | b'\r' | b'\n' | 0x0c, _) => self.pos += 1,
                (b'/', b'/') => {
                    while !matches!(self.peek(0), b'\n' | 0) {
                        self.pos += 1;
                    }
                }
                (b'/', b'*') => {
                    let lo = self.pos;
                    self.pos += 2;
                    loop {
                        match (self.peek(0), self.peek(1)) {
                            (0, _) if self.pos >= self.bytes().len() => {
                                bail!(self.span(lo), "unterminated block comment")
                            }
                            (b'*', b'/') => {
                                self.pos += 2;
                                break;
                            }
                            _ => self.pos += 1,
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    pub fn next_token(&mut self) -> Result<Token> {
        self.skip_trivia()?;
        let lo = self.pos;
        let c = self.peek(0);
        if self.pos >= self.bytes().len() {
            return Ok(Token {
                tok: Tok::Eof,
                span: self.span(lo),
            });
        }
        let tok = if is_ident_start(c) {
            let dialect = self.dialect;
            let s = self.ident_text();
            match Kw::parse(s, dialect) {
                Some(k) => Tok::Kw(k),
                None => Tok::Ident(s.into()),
            }
        } else if c == b'\\' {
            self.pos += 1;
            let start = self.pos;
            while !matches!(self.peek(0), b' ' | b'\t' | b'\r' | b'\n' | 0) {
                self.pos += 1;
            }
            if self.pos == start {
                bail!(self.span(lo), "empty escaped identifier");
            }
            Tok::Ident(self.text[start..self.pos].into())
        } else if c == b'$' && is_ident_char(self.peek(1)) {
            self.pos += 1;
            Tok::System(self.ident_text().into())
        } else if c == b'`' {
            self.pos += 1;
            if !is_ident_start(self.peek(0)) {
                bail!(self.span(lo), "expected a directive or macro name after `");
            }
            Tok::Directive(self.ident_text().into())
        } else if c.is_ascii_digit() {
            self.number(lo)?
        } else if c == b'\'' {
            self.based(lo)?
        } else if c == b'"' {
            self.string(lo)?
        } else {
            self.punct(lo)?
        };
        Ok(Token {
            tok,
            span: self.span(lo),
        })
    }

    fn ident_text(&mut self) -> &str {
        let start = self.pos;
        while is_ident_char(self.peek(0)) {
            self.pos += 1;
        }
        &self.text[start..self.pos]
    }

    fn number(&mut self, lo: usize) -> Result<Tok> {
        while self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
            self.pos += 1;
        }
        let int_end = self.pos;
        let mut real = false;
        if self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
            real = true;
            self.pos += 1;
            while self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
                self.pos += 1;
            }
        }
        if matches!(self.peek(0), b'e' | b'E')
            && (self.peek(1).is_ascii_digit()
                || matches!(self.peek(1), b'+' | b'-') && self.peek(2).is_ascii_digit())
        {
            real = true;
            self.pos += 2;
            while self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
                self.pos += 1;
            }
        }
        if real {
            return Ok(Tok::Real(self.text[lo..self.pos].replace('_', "").into()));
        }
        Ok(Tok::Decimal(self.text[lo..int_end].replace('_', "").into()))
    }

    /// `'b`, `'sh`, ... and the digits after it (whitespace allowed
    /// between), or a `'0`-style fill.
    fn based(&mut self, lo: usize) -> Result<Tok> {
        self.pos += 1;
        if matches!(self.peek(0), b'0' | b'1' | b'x' | b'X' | b'z' | b'Z')
            && !is_ident_char(self.peek(1))
        {
            let c = self.peek(0).to_ascii_lowercase() as char;
            self.pos += 1;
            return Ok(Tok::Fill(c));
        }
        let signed = matches!(self.peek(0), b's' | b'S');
        if signed {
            self.pos += 1;
        }
        let base = match self.peek(0) {
            b'b' | b'B' => Base::Bin,
            b'o' | b'O' => Base::Oct,
            b'd' | b'D' => Base::Dec,
            b'h' | b'H' => Base::Hex,
            _ => bail!(self.span(lo), "expected a base (b, o, d or h) after '"),
        };
        self.pos += 1;
        while matches!(self.peek(0), b' ' | b'\t') {
            self.pos += 1;
        }
        let start = self.pos;
        while self.peek(0).is_ascii_hexdigit()
            || matches!(self.peek(0), b'_' | b'x' | b'X' | b'z' | b'Z' | b'?')
        {
            self.pos += 1;
        }
        if self.pos == start {
            bail!(self.span(lo), "expected digits after the base");
        }
        let digits: String = self.text[start..self.pos]
            .chars()
            .filter(|&c| c != '_')
            .map(|c| c.to_ascii_lowercase())
            .collect();
        Ok(Tok::Based {
            signed,
            base,
            digits: digits.into(),
        })
    }

    fn string(&mut self, lo: usize) -> Result<Tok> {
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            let c = self.peek(0);
            if self.pos >= self.bytes().len() || c == b'\n' {
                bail!(self.span(lo), "unterminated string");
            }
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = self.peek(0);
                    self.pos += 1;
                    out.push(match e {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'\\' => b'\\',
                        b'"' => b'"',
                        b'\n' => continue,
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                if matches!(self.peek(0), b'0'..=b'7') {
                                    v = v * 8 + (self.peek(0) - b'0') as u32;
                                    self.pos += 1;
                                }
                            }
                            v as u8
                        }
                        other => other,
                    });
                }
                _ => out.push(c),
            }
        }
        Ok(Tok::Str(out.into()))
    }

    fn punct(&mut self, lo: usize) -> Result<Tok> {
        let rest = &self.bytes()[self.pos..];
        let first = rest[0];
        for &(s, p) in PUNCTS {
            if s.as_bytes()[0] != first || !rest.starts_with(s.as_bytes()) {
                continue;
            }
            match p {
                // `@(*)` is a star in parentheses, not an attribute.
                P::AttrOpen if rest.get(2) == Some(&b')') => continue,
                P::AttrOpen => self.attr_depth += 1,
                P::AttrClose if self.attr_depth == 0 => continue,
                P::AttrClose => self.attr_depth -= 1,
                _ => {}
            }
            self.pos += s.len();
            return Ok(Tok::P(p));
        }
        let c = self.text[self.pos..].chars().next().unwrap_or('?');
        self.pos += c.len_utf8();
        bail!(self.span(lo), "unexpected character {c:?}")
    }

    /// The rest of the line, for a directive: a `` `define ``'s body (with
    /// `\` line continuations and without a trailing `//` comment).
    pub fn rest_of_line(&mut self) -> (String, Span) {
        let lo = self.pos;
        let mut out = String::new();
        let mut in_string = false;
        loop {
            let c = self.peek(0);
            if self.pos >= self.bytes().len() {
                break;
            }
            match c {
                b'\n' => break,
                b'\\' if !in_string && matches!(self.peek(1), b'\n' | b'\r') => {
                    self.pos += 1;
                    if self.peek(0) == b'\r' {
                        self.pos += 1;
                    }
                    if self.peek(0) == b'\n' {
                        self.pos += 1;
                    }
                    out.push('\n');
                    continue;
                }
                b'\\' if in_string => {
                    out.push('\\');
                    self.pos += 1;
                    if self.peek(0) != b'\n' {
                        let ch = self.text[self.pos..].chars().next().unwrap();
                        out.push(ch);
                        self.pos += ch.len_utf8();
                    }
                    continue;
                }
                b'"' => in_string = !in_string,
                b'/' if !in_string && self.peek(1) == b'/' => {
                    while !matches!(self.peek(0), b'\n' | 0) {
                        self.pos += 1;
                    }
                    break;
                }
                b'/' if !in_string && self.peek(1) == b'*' => {
                    self.pos += 2;
                    while self.pos < self.bytes().len()
                        && !(self.peek(0) == b'*' && self.peek(1) == b'/')
                    {
                        self.pos += 1;
                    }
                    self.pos = (self.pos + 2).min(self.bytes().len());
                    out.push(' ');
                    continue;
                }
                _ => {}
            }
            let ch = self.text[self.pos..].chars().next().unwrap();
            out.push(ch);
            self.pos += ch.len_utf8();
        }
        (out, self.span(lo))
    }

    /// Whether the next character is `(` with nothing before it: a
    /// function-like macro's parameter list must touch its name.
    pub fn at_paren(&self) -> bool {
        self.peek(0) == b'('
    }

    /// A macro use's arguments, `(a, b(c, d), e)`: split at the commas
    /// outside brackets and strings. `None` if no `(` follows (after
    /// whitespace).
    pub fn macro_args(&mut self) -> Result<Option<Vec<String>>> {
        let save = self.pos;
        self.skip_trivia()?;
        if self.peek(0) != b'(' {
            self.pos = save;
            return Ok(None);
        }
        let lo = self.pos;
        self.pos += 1;
        let mut args = Vec::new();
        let mut cur = String::new();
        let mut depth = 0u32;
        loop {
            if self.pos >= self.bytes().len() {
                bail!(self.span(lo), "unterminated macro arguments");
            }
            let c = self.peek(0);
            match c {
                b'(' | b'[' | b'{' => depth += 1,
                b')' if depth == 0 => {
                    self.pos += 1;
                    args.push(cur);
                    return Ok(Some(args));
                }
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b',' if depth == 0 => {
                    args.push(std::mem::take(&mut cur));
                    self.pos += 1;
                    continue;
                }
                b'"' => {
                    let start = self.pos;
                    self.string(start)?;
                    cur.push_str(&self.text[start..self.pos]);
                    continue;
                }
                b'/' if matches!(self.peek(1), b'/' | b'*') => {
                    self.skip_trivia()?;
                    cur.push(' ');
                    continue;
                }
                _ => {}
            }
            let ch = self.text[self.pos..].chars().next().unwrap();
            cur.push(ch);
            self.pos += ch.len_utf8();
        }
    }
}

/// Lexes a whole text, for tests.
#[cfg(test)]
pub fn lex_all(text: &str) -> Result<Vec<Tok>> {
    lex_dialect(text, Dialect::Verilog2005)
}

#[cfg(test)]
pub fn lex_dialect(text: &str, dialect: Dialect) -> Result<Vec<Tok>> {
    let mut sm = crate::source::SourceMap::default();
    let f = sm.add_file("t.v".into(), text.into());
    let mut l = Lexer::new(f, text.into(), dialect);
    let mut out = Vec::new();
    loop {
        let t = l.next_token()?;
        if t.tok == Tok::Eof {
            return Ok(out);
        }
        out.push(t.tok);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_punctuation() {
        let toks = lex_all("8 'h FF + 'sb1_0 <<< 3.5 @(*) (* keep *) `foo \\esc[0] ").unwrap();
        assert_eq!(toks[0], Tok::Decimal("8".into()));
        assert_eq!(
            toks[1],
            Tok::Based {
                signed: false,
                base: Base::Hex,
                digits: "ff".into()
            }
        );
        assert_eq!(toks[2], Tok::P(P::Plus));
        assert_eq!(
            toks[3],
            Tok::Based {
                signed: true,
                base: Base::Bin,
                digits: "10".into()
            }
        );
        assert_eq!(toks[4], Tok::P(P::AShl));
        assert_eq!(toks[5], Tok::Real("3.5".into()));
        assert_eq!(
            &toks[6..10],
            &[
                Tok::P(P::At),
                Tok::P(P::LParen),
                Tok::P(P::Star),
                Tok::P(P::RParen)
            ]
        );
        assert_eq!(toks[10], Tok::P(P::AttrOpen));
        assert_eq!(toks[11], Tok::Ident("keep".into()));
        assert_eq!(toks[12], Tok::P(P::AttrClose));
        assert_eq!(toks[13], Tok::Directive("foo".into()));
        assert_eq!(toks[14], Tok::Ident("esc[0]".into()));
    }

    #[test]
    fn systemverilog_keywords_are_names_in_verilog() {
        let v = lex_dialect("logic always_comb", Dialect::Verilog2005).unwrap();
        assert_eq!(
            v,
            [Tok::Ident("logic".into()), Tok::Ident("always_comb".into())]
        );
        let sv = lex_dialect("logic always_comb wire", Dialect::SystemVerilog2017).unwrap();
        assert_eq!(
            sv,
            [
                Tok::Kw(Kw::Logic),
                Tok::Kw(Kw::AlwaysComb),
                Tok::Kw(Kw::Wire)
            ]
        );
    }

    #[test]
    fn errors_have_positions() {
        let e = lex_all("a /* never closed").unwrap_err();
        assert!(e.message.contains("unterminated"));
    }
}
