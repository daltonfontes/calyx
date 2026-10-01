//! Source handling, diagnostics and lexer for the Calyx language.
//!
//! Diagnostics are designed to be read by people and by AI agents: each one
//! carries a stable code, what was expected, what was observed and where.

mod diagnostic;
mod lexer;
mod source;

pub use diagnostic::{Diagnostic, Severity};
pub use lexer::{Token, TokenKind, lex};
pub use source::{LineCol, Source, Span};
