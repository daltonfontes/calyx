use crate::{Source, Span};
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// A structured diagnostic: stable code, expected, observed, location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Stable identifier, e.g. `E0001`. Agents can key fixes on it.
    pub code: &'static str,
    pub message: String,
    pub expected: Option<String>,
    pub observed: Option<String>,
    pub span: Span,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
            expected: None,
            observed: None,
            span,
        }
    }

    pub fn expected(mut self, expected: impl Into<String>) -> Self {
        self.expected = Some(expected.into());
        self
    }

    pub fn observed(mut self, observed: impl Into<String>) -> Self {
        self.observed = Some(observed.into());
        self
    }

    /// Human-readable rendering, one diagnostic per block.
    pub fn render(&self, src: &Source) -> String {
        let at = src.line_col(self.span.start);
        let mut out = format!(
            "{}[{}]: {}\n",
            self.severity.as_str(),
            self.code,
            self.message
        );
        if let Some(e) = &self.expected {
            let _ = writeln!(out, "- expected : {e}");
        }
        if let Some(o) = &self.observed {
            let _ = writeln!(out, "- observed : {o}");
        }
        let _ = writeln!(out, "Location: {}:{}:{}", src.name, at.line, at.col);
        out
    }

    /// JSON rendering of a single diagnostic, without external dependencies.
    pub fn to_json(&self, src: &Source) -> String {
        let at = src.line_col(self.span.start);
        let mut out = String::from("{");
        let _ = write!(out, "\"severity\":\"{}\"", self.severity.as_str());
        let _ = write!(out, ",\"code\":\"{}\"", self.code);
        let _ = write!(out, ",\"message\":{}", json_string(&self.message));
        if let Some(e) = &self.expected {
            let _ = write!(out, ",\"expected\":{}", json_string(e));
        }
        if let Some(o) = &self.observed {
            let _ = write!(out, ",\"observed\":{}", json_string(o));
        }
        let _ = write!(
            out,
            ",\"file\":{},\"line\":{},\"column\":{},\"start\":{},\"end\":{}}}",
            json_string(&src.name),
            at.line,
            at.col,
            self.span.start,
            self.span.end
        );
        out
    }

    /// JSON array of diagnostics.
    pub fn list_to_json(diags: &[Diagnostic], src: &Source) -> String {
        let items: Vec<String> = diags.iter().map(|d| d.to_json(src)).collect();
        format!("[{}]", items.join(","))
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_human_and_json() {
        let src = Source::new("a.clyx", "x\n\"oi");
        let d = Diagnostic::error("E0002", "unterminated string", Span::new(2, 5))
            .expected("closing `\"`")
            .observed("end of file");
        assert_eq!(
            d.render(&src),
            "error[E0002]: unterminated string\n- expected : closing `\"`\n- observed : end of file\nLocation: a.clyx:2:1\n"
        );
        assert_eq!(
            d.to_json(&src),
            r#"{"severity":"error","code":"E0002","message":"unterminated string","expected":"closing `\"`","observed":"end of file","file":"a.clyx","line":2,"column":1,"start":2,"end":5}"#
        );
    }
}
