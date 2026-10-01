//! Lexer for Calyx source text.
//!
//! Calyx uses indentation for blocks, like Python. The lexer turns leading
//! whitespace into `Indent` / `Dedent` tokens and line breaks into `Newline`.
//! Inside `(...)`, `[...]` and `{...}` line breaks are ignored, so long calls
//! can span lines. The content of `"""..."""` text is never indentation.
//!
//! The lexer never stops at the first error: it reports a diagnostic, skips
//! the offending input and keeps going. Keywords are not distinguished here;
//! the parser recognizes them from identifiers.

use crate::{Diagnostic, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Ident,
    Int,
    Float,
    /// `"..."`, single line, may contain `{interpolation}`.
    Str,
    /// `"""..."""`, may span lines, may contain `{interpolation}`.
    LongStr,
    /// End of a logical line.
    Newline,
    /// The next line is indented more: a block starts.
    Indent,
    /// The indentation went back: a block ends.
    Dedent,

    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    EqEq,
    Ne,
    Bang,
    FatArrow,
    Arrow,
    Colon,
    Comma,
    Dot,
    DotDot,
    Pipe,
    OrOr,
    AndAnd,
    Plus,
    PlusPlus,
    Minus,
    Star,
    Slash,
    Percent,

    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Lexes `text` into tokens (always ending with `Eof`) and diagnostics.
pub fn lex(text: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lx = Lexer {
        text,
        pos: 0,
        tokens: Vec::new(),
        diags: Vec::new(),
        indents: vec![0],
        depth: 0,
        at_line_start: true,
    };
    lx.run();
    (lx.tokens, lx.diags)
}

struct Lexer<'a> {
    text: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    diags: Vec<Diagnostic>,
    /// Stack of indentation widths of the open blocks.
    indents: Vec<usize>,
    /// Bracket nesting: line breaks inside brackets are ignored.
    depth: i32,
    at_line_start: bool,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.text[self.pos..].chars().nth(n)
    }

    fn starts_with(&self, s: &str) -> bool {
        self.text[self.pos..].starts_with(s)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.pos),
        });
    }

    fn push_at(&mut self, kind: TokenKind, at: usize) {
        self.tokens.push(Token {
            kind,
            span: Span::new(at, at),
        });
    }

    fn last_kind(&self) -> Option<TokenKind> {
        self.tokens.last().map(|t| t.kind)
    }

    fn run(&mut self) {
        loop {
            if self.at_line_start && self.depth <= 0 {
                self.at_line_start = false;
                if self.indentation() {
                    continue;
                }
            }
            let Some(c) = self.peek() else { break };
            let start = self.pos;
            match c {
                ' ' | '\t' | '\r' => {
                    self.bump();
                }
                '\n' => {
                    self.bump();
                    if self.depth <= 0 {
                        self.newline(start);
                        self.at_line_start = true;
                    }
                }
                '#' => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.bump();
                    }
                }
                '"' if self.starts_with("\"\"\"") => self.long_string(start),
                '"' => self.string(start),
                c if c.is_ascii_digit() => self.number(start),
                c if c.is_alphabetic() || c == '_' => {
                    while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        self.bump();
                    }
                    self.push(TokenKind::Ident, start);
                }
                ';' => {
                    self.bump();
                    self.diags.push(
                        Diagnostic::error(
                            "E0004",
                            "Calyx does not use `;`",
                            Span::new(start, self.pos),
                        )
                        .expected("a line break between statements")
                        .observed("`;`"),
                    );
                }
                _ => self.punct(start, c),
            }
        }
        // Close the last line and every open block.
        let end = self.pos;
        self.newline(end);
        while self.indents.len() > 1 {
            self.indents.pop();
            self.push_at(TokenKind::Dedent, end);
        }
        self.push_at(TokenKind::Eof, end);
    }

    /// Emits one `Newline` for the end of a non-empty logical line.
    fn newline(&mut self, at: usize) {
        if !matches!(
            self.last_kind(),
            None | Some(TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent)
        ) {
            self.tokens.push(Token {
                kind: TokenKind::Newline,
                span: Span::new(at, at + usize::from(at < self.text.len())),
            });
        }
    }

    /// Measures the indentation of a new line and emits `Indent` / `Dedent`.
    /// Returns true when the line is blank or a comment (nothing emitted).
    fn indentation(&mut self) -> bool {
        let start = self.pos;
        let mut width = 0;
        let mut tab = None;
        while let Some(c) = self.peek() {
            match c {
                ' ' => width += 1,
                '\t' => {
                    tab.get_or_insert(self.pos);
                    width += 4;
                }
                '\r' => {}
                _ => break,
            }
            self.bump();
        }
        match self.peek() {
            // Blank and comment-only lines do not affect indentation.
            None | Some('\n' | '#') => return false,
            _ => {}
        }
        if let Some(at) = tab {
            self.diags.push(
                Diagnostic::error("E0006", "tab used for indentation", Span::new(at, at + 1))
                    .expected("spaces")
                    .observed("a tab"),
            );
        }
        let current = *self.indents.last().unwrap_or(&0);
        if width > current {
            self.indents.push(width);
            self.push(TokenKind::Indent, start);
        } else if width < current {
            // Close blocks only while the line still fits an outer one. A line
            // that falls between two levels stays in the inner block, so one
            // misplaced line does not end its graph and cascade into errors.
            while self.indents.len() > 1 && width <= self.indents[self.indents.len() - 2] {
                self.indents.pop();
                self.push_at(TokenKind::Dedent, self.pos);
            }
            if width != *self.indents.last().unwrap_or(&0) {
                self.diags.push(
                    Diagnostic::error(
                        "E0007",
                        "indentation does not match any outer block",
                        Span::new(start, self.pos),
                    )
                    .expected(format!(
                        "{} spaces, like an enclosing line",
                        self.indents.last().unwrap_or(&0)
                    ))
                    .observed(format!("{width} spaces")),
                );
            }
        }
        false
    }

    fn punct(&mut self, start: usize, c: char) {
        use TokenKind::*;
        let two = |a: char, b: char| c == a && self.peek_at(1) == Some(b);
        let (kind, len) = if two('=', '>') {
            (FatArrow, 2)
        } else if two('-', '>') {
            (Arrow, 2)
        } else if two('=', '=') {
            (EqEq, 2)
        } else if two('!', '=') {
            (Ne, 2)
        } else if two('<', '=') {
            (Le, 2)
        } else if two('>', '=') {
            (Ge, 2)
        } else if two('.', '.') {
            (DotDot, 2)
        } else if two('|', '|') {
            (OrOr, 2)
        } else if two('&', '&') {
            (AndAnd, 2)
        } else if two('+', '+') {
            (PlusPlus, 2)
        } else {
            let k = match c {
                '{' => LBrace,
                '}' => RBrace,
                '(' => LParen,
                ')' => RParen,
                '[' => LBracket,
                ']' => RBracket,
                '<' => Lt,
                '>' => Gt,
                '=' => Eq,
                '!' => Bang,
                ':' => Colon,
                ',' => Comma,
                '.' => Dot,
                '|' => Pipe,
                '+' => Plus,
                '-' => Minus,
                '*' => Star,
                '/' => Slash,
                '%' => Percent,
                _ => {
                    self.bump();
                    let mut d = Diagnostic::error(
                        "E0001",
                        "unexpected character",
                        Span::new(start, self.pos),
                    )
                    .observed(format!("`{c}`"));
                    if c == '&' {
                        // Borrows are written with words, not `&` (decision D26).
                        d = d.expected("`reads` or `edits` to lend a resource");
                    }
                    self.diags.push(d);
                    return;
                }
            };
            (k, 1)
        };
        match kind {
            LBrace | LParen | LBracket => self.depth += 1,
            RBrace | RParen | RBracket => self.depth -= 1,
            _ => {}
        }
        for _ in 0..len {
            self.bump();
        }
        self.push(kind, start);
    }

    fn number(&mut self, start: usize) {
        self.digits();
        // `1..3` is a range, not a float: only a digit after the dot makes a float.
        let is_float =
            self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit());
        if is_float {
            self.bump();
            self.digits();
            self.push(TokenKind::Float, start);
        } else {
            self.push(TokenKind::Int, start);
        }
    }

    fn digits(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
            self.bump();
        }
    }

    fn string(&mut self, start: usize) {
        self.bump(); // opening quote
        loop {
            match self.peek() {
                Some('"') => {
                    self.bump();
                    self.push(TokenKind::Str, start);
                    return;
                }
                Some('\\') => {
                    self.bump();
                    self.escape();
                }
                Some('\n') | None => {
                    self.diags.push(
                        Diagnostic::error(
                            "E0002",
                            "unterminated string",
                            Span::new(start, self.pos),
                        )
                        .expected(
                            "closing `\"` on the same line (use `\"\"\"` for multi-line text)",
                        )
                        .observed(if self.peek().is_some() {
                            "end of line"
                        } else {
                            "end of file"
                        }),
                    );
                    // Still emit the text, so the parser does not cascade.
                    self.push(TokenKind::Str, start);
                    return;
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    fn long_string(&mut self, start: usize) {
        self.pos += 3;
        loop {
            if self.starts_with("\"\"\"") {
                self.pos += 3;
                self.push(TokenKind::LongStr, start);
                return;
            }
            match self.peek() {
                Some('\\') => {
                    self.bump();
                    self.escape();
                }
                Some(_) => {
                    self.bump();
                }
                None => {
                    self.diags.push(
                        Diagnostic::error(
                            "E0003",
                            "unterminated multi-line text",
                            Span::new(start, start + 3),
                        )
                        .expected("closing `\"\"\"`")
                        .observed("end of file"),
                    );
                    self.push(TokenKind::LongStr, start);
                    return;
                }
            }
        }
    }

    fn escape(&mut self) {
        let start = self.pos - 1;
        match self.peek() {
            Some('n' | 't' | 'r' | '"' | '\\' | '{' | '}') => {
                self.bump();
            }
            other => {
                if other.is_some() {
                    self.bump();
                }
                self.diags.push(
                    Diagnostic::error(
                        "E0005",
                        "invalid escape sequence",
                        Span::new(start, self.pos),
                    )
                    .expected("one of `\\n`, `\\t`, `\\r`, `\\\"`, `\\\\`, `\\{`, `\\}`")
                    .observed(match other {
                        Some(c) => format!("`\\{c}`"),
                        None => "end of file".into(),
                    }),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let (toks, diags) = lex(src);
        assert!(diags.is_empty(), "{diags:?}");
        toks.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn blocks_become_indent_and_dedent() {
        assert_eq!(
            kinds("graph g():\n    a = 1\n    b = 2\nmodel m\n"),
            vec![
                Ident, Ident, LParen, RParen, Colon, Newline, Indent, Ident, Eq, Int, Newline,
                Ident, Eq, Int, Newline, Dedent, Ident, Ident, Newline, Eof
            ]
        );
    }

    #[test]
    fn nested_blocks_close_at_end_of_file() {
        assert_eq!(
            kinds("a:\n  b:\n    c"),
            vec![
                Ident, Colon, Newline, Indent, Ident, Colon, Newline, Indent, Ident, Newline,
                Dedent, Dedent, Eof
            ]
        );
    }

    #[test]
    fn blank_and_comment_lines_are_ignored() {
        assert_eq!(
            kinds("a:\n\n    # comentário\n    b # fim\n\n# fora\nc\n"),
            vec![
                Ident, Colon, Newline, Indent, Ident, Newline, Dedent, Ident, Newline, Eof
            ]
        );
    }

    #[test]
    fn line_breaks_inside_brackets_are_ignored() {
        assert_eq!(
            kinds("f(\n  a,\n      b\n)\n"),
            vec![Ident, LParen, Ident, Comma, Ident, RParen, Newline, Eof]
        );
    }

    #[test]
    fn long_text_content_is_not_indentation() {
        assert_eq!(
            kinds("p:\n    \"\"\"\n  linha\n        outra {x}\n    \"\"\"\nq\n"),
            vec![
                Ident, Colon, Newline, Indent, LongStr, Newline, Dedent, Ident, Newline, Eof
            ]
        );
    }

    #[test]
    fn ranges_and_numbers() {
        assert_eq!(kinds("1..3"), vec![Int, DotDot, Int, Newline, Eof]);
        assert_eq!(kinds("0.20 USD"), vec![Float, Ident, Newline, Eof]);
        assert_eq!(kinds("2_000 tokens"), vec![Int, Ident, Newline, Eof]);
    }

    #[test]
    fn operators() {
        assert_eq!(
            kinds("=> -> == != <= >= ++ || &&"),
            vec![
                FatArrow, Arrow, EqEq, Ne, Le, Ge, PlusPlus, OrOr, AndAnd, Newline, Eof
            ]
        );
    }

    #[test]
    fn strings_and_unicode_identifiers() {
        assert_eq!(kinds(r#""relatorios/{topic}.md""#), vec![Str, Newline, Eof]);
        assert_eq!(kinds(r#""a \"b\" \{lit\}""#), vec![Str, Newline, Eof]);
        assert_eq!(kinds("orçamento"), vec![Ident, Newline, Eof]);
    }

    #[test]
    fn indentation_errors() {
        let (_, diags) = lex("a:\n\tb\n");
        assert_eq!(diags[0].code, "E0006");
        let (_, diags) = lex("a:\n    b\n  c\n");
        assert_eq!(diags[0].code, "E0007");
    }

    #[test]
    fn errors_are_reported_and_lexing_continues() {
        let (toks, diags) = lex("a ; b & c\n\"oops\nd");
        let codes: Vec<_> = diags.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec!["E0004", "E0001", "E0002"]);
        let idents = toks.iter().filter(|t| t.kind == Ident).count();
        assert_eq!(idents, 4); // a, b, c, d
    }

    #[test]
    fn unterminated_long_string() {
        let (_, diags) = lex("\"\"\" never closed");
        assert_eq!(diags[0].code, "E0003");
    }
}
