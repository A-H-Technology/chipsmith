//! chipsmith's Verilog front end: Verilog source in, a
//! [`chipsmith_netlist::Design`] out, at the level yosys reaches after
//! `read_verilog; hierarchy -check -top <top>; proc` (ADR-0001, ADR-0004).
//!
//! ```no_run
//! # fn main() -> Result<(), chipsmith_verilog::Error> {
//! let mut fe = chipsmith_verilog::Frontend::new();
//! fe.read_library("cells.v".as_ref())?; // blackboxes: interfaces only
//! fe.read_file("top.v".as_ref())?;
//! let design = fe.elaborate("top")?;
//! # Ok(()) }
//! ```
//!
//! # Pipeline
//!
//! - `pp`: the preprocessor (macros textual, IEEE 1364-2005 §19), over
//!   `lex`'s tokens.
//! - `parse`: recursive descent to the `ast`.
//! - `elab`: parameters, generate, hierarchy, expressions (§5.4/5.5
//!   sizing and signedness), procedures run symbolically into multiplexer
//!   trees and registers (ADR-0006), all through `build`'s cell builder,
//!   which folds cells with constant inputs using `fold` (RTLIL's
//!   operator semantics, so constant expressions and logic agree).
//!
//! # What comes out
//!
//! One module per module and parameter set (`$paramod\name\P=...` when
//! parameters are overridden), the top marked `top`, blackbox modules
//! with their ports. Instances of the design's modules and of library
//! cells are generic cells with port directions; library cells keep the
//! instance's parameters. Registers are `$dff`/`$adff`/`$aldff`/`$dffsr`
//! (as `proc` makes them: enables and synchronous resets are left to
//! `opt_dff`), latches `$dlatch`. A memory is one `$mem_v2` named after
//! it, with an asynchronous read port per read (`memory_dff` merges the
//! registers after them, as for yosys' front end), a write port per write
//! in a clocked block, and its initial contents; one written from logic
//! or with `=` becomes a register per word (`mem[3]`), as yosys'
//! `mem2reg`. `c ? d : 'z` is a `$tribuf` (what yosys' `tribuf` pass
//! makes of the `$mux` its front end builds). Initial values are `init`
//! attributes on register wires. Cells are named `$type$file:line$n`,
//! wires keep their Verilog names (`gen[2].x` inside generate blocks).
//!
//! Anything outside the supported subset is an [`Error`] with its
//! position, never a guess.

mod ast;
mod build;
mod elab;
mod fold;
mod lex;
mod parse;
mod pp;
mod source;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::rc::Rc;

pub use source::{Error, Location};

use ast::Role;
use pp::{Defines, Preprocessor};
use source::SourceMap;

/// Source files read so far, and the macros they defined.
#[derive(Default)]
pub struct Frontend {
    sources: SourceMap,
    defines: Defines,
    include_dirs: Vec<PathBuf>,
    modules: Vec<ast::Module>,
}

impl Frontend {
    pub fn new() -> Frontend {
        Frontend::default()
    }

    /// `-D name=value`.
    pub fn define(&mut self, name: &str, value: &str) {
        self.defines.define(name, value);
    }

    /// Where `` `include `` looks after the including file's directory.
    pub fn include_dir(&mut self, dir: impl Into<PathBuf>) {
        self.include_dirs.push(dir.into());
    }

    /// A design source file. Macros it defines stay defined for the files
    /// read after it, as in yosys.
    pub fn read_file(&mut self, path: &Path) -> Result<(), Error> {
        self.read_path(path, Role::Design)
    }

    /// A cell library (`read_verilog -lib`): its modules are blackboxes,
    /// only their ports and parameters count.
    pub fn read_library(&mut self, path: &Path) -> Result<(), Error> {
        self.read_path(path, Role::Library)
    }

    /// Design source from memory; `name` is the path errors and `src`
    /// attributes show.
    pub fn read_source(&mut self, name: &str, text: &str) -> Result<(), Error> {
        self.read(name, text, Role::Design)
    }

    /// A cell library from memory, as [`Frontend::read_library`].
    pub fn read_library_source(&mut self, name: &str, text: &str) -> Result<(), Error> {
        self.read(name, text, Role::Library)
    }

    fn read_path(&mut self, path: &Path, role: Role) -> Result<(), Error> {
        let text = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        self.read(
            &path.display().to_string(),
            &String::from_utf8_lossy(&text),
            role,
        )
    }

    fn read(&mut self, name: &str, text: &str, role: Role) -> Result<(), Error> {
        let file = self.sources.add_file(name.to_owned(), Rc::from(text));
        let result = Preprocessor::run(
            &mut self.sources,
            &mut self.defines,
            &self.include_dirs,
            file,
        )
        .and_then(|toks| parse::parse(&toks, role));
        match result {
            Ok(mods) => {
                self.modules.extend(mods);
                Ok(())
            }
            Err(d) => Err(Error::from_diag(&self.sources, d)),
        }
    }

    /// The design below `top`, every module elaborated for each set of
    /// parameters it's instantiated with.
    pub fn elaborate(&self, top: &str) -> Result<chipsmith_netlist::Design, Error> {
        elab::elaborate(&self.sources, &self.modules, top)
            .map_err(|d| Error::from_diag(&self.sources, d))
    }
}
