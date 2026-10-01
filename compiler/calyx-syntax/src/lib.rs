//! Source handling, diagnostics, lexer and parser for the Calyx language.
//!
//! Diagnostics are designed to be read by people and by AI agents: each one
//! carries a stable code, what was expected, what was observed and where.

pub mod ast;
mod diagnostic;
mod lexer;
mod parser;
mod source;

pub use diagnostic::{Diagnostic, Severity};
pub use lexer::{Token, TokenKind, lex};
pub use parser::parse;
pub use source::{LineCol, Source, Span};
