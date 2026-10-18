//! RayScript's front end: lexer, parser and syntax tree.
//!
//! RayScript is hscript's logic, Haxe's expressions, with HXX-style markup as expressions. The parser
//! always returns a tree, with `Error` nodes where it could not read, and the diagnostics it found, so
//! an editor can work with a script that is half typed.

pub mod ast;
pub mod dump;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;

pub use ast::Module;
pub use span::{Diagnostic, LineIndex, Severity, Span};

/// What parsing a script gave.
pub struct Parsed {
    pub module: Module,
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == Severity::Error)
    }
}

/// Parses a script.
pub fn parse(source: &str) -> Parsed {
    let mut parser = parser::Parser::new(source);
    let module = parser.parse_module();
    Parsed { module, diagnostics: parser.finish() }
}
