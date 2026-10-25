//! chipsmith's Verilog front end.

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

use pp::{Defines, Preprocessor};
use source::SourceMap;

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

    pub fn define(&mut self, name: &str, value: &str) {
        self.defines.define(name, value);
    }

    pub fn include_dir(&mut self, dir: impl Into<PathBuf>) {
        self.include_dirs.push(dir.into());
    }

    pub fn read_file(&mut self, path: &Path) -> Result<(), Error> {
        self.read_path(path, false)
    }

    pub fn read_library(&mut self, path: &Path) -> Result<(), Error> {
        self.read_path(path, true)
    }

    fn read_path(&mut self, path: &Path, lib: bool) -> Result<(), Error> {
        let text = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        self.read_source(
            &path.display().to_string(),
            &String::from_utf8_lossy(&text),
            lib,
        )
    }

    pub fn read_source(&mut self, name: &str, text: &str, lib: bool) -> Result<(), Error> {
        let file = self.sources.add_file(name.to_owned(), Rc::from(text));
        let result = Preprocessor::run(
            &mut self.sources,
            &mut self.defines,
            &self.include_dirs,
            file,
        )
        .and_then(|toks| parse::parse(&toks, lib));
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
