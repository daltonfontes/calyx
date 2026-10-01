/// A half-open byte range `[start, end)` into a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }
}

/// A 1-based line and column (columns count characters, not bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

/// A named source file with a precomputed line index.
#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub text: String,
    line_starts: Vec<u32>,
}

impl Source {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        let text = text.into();
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i as u32 + 1))
            .collect();
        Self {
            name: name.into(),
            text,
            line_starts,
        }
    }

    pub fn line_col(&self, offset: u32) -> LineCol {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line] as usize;
        let col = self.text[line_start..offset as usize].chars().count();
        LineCol {
            line: line as u32 + 1,
            col: col as u32 + 1,
        }
    }

    pub fn slice(&self, span: Span) -> &str {
        &self.text[span.start as usize..span.end as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_counts_characters() {
        let src = Source::new("t", "ab\nçãx\n");
        assert_eq!(src.line_col(0), LineCol { line: 1, col: 1 });
        assert_eq!(src.line_col(3), LineCol { line: 2, col: 1 });
        // "ç" and "ã" are two bytes each; "x" starts at byte 7.
        assert_eq!(src.line_col(7), LineCol { line: 2, col: 3 });
    }
}
