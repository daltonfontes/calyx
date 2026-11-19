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
pub use parser::{INDEX, MAP, SLICE, parse};
pub use source::{LineCol, Source, Span};

/// Where the `{...}` opened at `open` in a text ends: the index of its `}`.
/// Inside, a `"..."` is a text of its own (its `}` and `{` do not count)
/// and braces nest, so `{counts["a"]}` and `{join(xs, ", ")}` are whole.
/// `None` when it does not close on its line.
pub fn interpolation_end(text: &str, open: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    if i < b.len() && b[i] == b'\n' {
                        return None;
                    }
                    i += 1;
                }
                if i >= b.len() {
                    return None;
                }
            }
            b'\n' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}
