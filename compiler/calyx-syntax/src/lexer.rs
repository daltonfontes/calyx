//! Lexer for Calyx source text.
//!
//! The lexer never stops at the first error: it reports a diagnostic, skips
//! the offending input and keeps going, so one `calyx check` run shows every
//! lexical problem in the file. Keywords are not distinguished here; the
//! parser recognizes them from identifiers.

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
    /// One or more line breaks. Statements are separated by newlines.
    Newline,

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
    };
    lx.run();
    (lx.tokens, lx.diags)
}

struct Lexer<'a> {
    text: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    diags: Vec<Diagnostic>,
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

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            let start = self.pos;
            match c {
                ' ' | '\t' | '\r' => {
                    self.bump();
                }
                '\n' => {
                    self.bump();
                    // Collapse consecutive line breaks into one token.
                    if self.tokens.last().map(|t| t.kind) != Some(TokenKind::Newline) {
                        self.push(TokenKind::Newline, start);
                    }
                }
                '/' if self.starts_with("//") => {
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
        let end = self.pos;
        self.push(TokenKind::Eof, end);
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
    fn node_declaration() {
        assert_eq!(
            kinds("node xs[q in plan.questions] = web_search(q)\n"),
            vec![
                Ident, Ident, LBracket, Ident, Ident, Ident, Dot, Ident, RBracket, Eq, Ident,
                LParen, Ident, RParen, Newline, Eof
            ]
        );
    }

    #[test]
    fn ranges_and_numbers() {
        assert_eq!(kinds("1..3"), vec![Int, DotDot, Int, Eof]);
        assert_eq!(kinds("0.20 USD"), vec![Float, Ident, Eof]);
        assert_eq!(kinds("2_000 tokens"), vec![Int, Ident, Eof]);
        assert_eq!(kinds("50/s"), vec![Int, Slash, Ident, Eof]);
    }

    #[test]
    fn operators() {
        assert_eq!(
            kinds("=> -> == != <= >= ++ || &&"),
            vec![
                FatArrow, Arrow, EqEq, Ne, Le, Ge, PlusPlus, OrOr, AndAnd, Eof
            ]
        );
    }

    #[test]
    fn comments_and_newlines_collapse() {
        assert_eq!(
            kinds("a // comentário\n\n\nb"),
            vec![Ident, Newline, Ident, Eof]
        );
    }

    #[test]
    fn strings() {
        assert_eq!(kinds(r#""relatorios/{topic}.md""#), vec![Str, Eof]);
        assert_eq!(
            kinds("\"\"\"\n  linha 1\n  {x}\n\"\"\""),
            vec![LongStr, Eof]
        );
        assert_eq!(kinds(r#""a \"b\" \{lit\}""#), vec![Str, Eof]);
    }

    #[test]
    fn unicode_identifiers() {
        assert_eq!(kinds("orçamento"), vec![Ident, Eof]);
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
