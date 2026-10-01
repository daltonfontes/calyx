//! The Calyx verifier.
//!
//! There is exactly one verifier (decision D10): the CLI uses it as a Rust
//! library, and the C runtime links it as a static library through the C ABI
//! in [`ffi`], to check graphs generated at run time before running them.
//!
//! M0 runs the lexer only. Each later milestone adds analyses here, all of
//! them linear or compositional so `calyx check` stays under one second.

pub mod ffi;

use calyx_syntax::{Diagnostic, Source, lex};

/// Result of checking one source file.
#[derive(Debug)]
pub struct Report {
    pub source: Source,
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == calyx_syntax::Severity::Error)
    }

    /// Human-readable output, one block per diagnostic.
    pub fn render(&self) -> String {
        self.diagnostics
            .iter()
            .map(|d| d.render(&self.source))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Machine-readable output for agents and the runtime.
    pub fn to_json(&self) -> String {
        Diagnostic::list_to_json(&self.diagnostics, &self.source)
    }
}

/// Checks a Calyx program.
pub fn check(name: &str, text: &str) -> Report {
    let source = Source::new(name, text);
    let (_tokens, diagnostics) = lex(&source.text);
    Report {
        source,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_program_has_no_diagnostics() {
        let r = check("ok.clyx", "graph g() -> Text {\n  return \"oi\"\n}\n");
        assert!(!r.has_errors());
        assert_eq!(r.to_json(), "[]");
    }

    #[test]
    fn invalid_program_reports_errors() {
        let r = check("bad.clyx", "node x = f(a);\n");
        assert!(r.has_errors());
        assert!(r.render().contains("error[E0004]"));
    }
}
