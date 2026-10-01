//! Recursive-descent parser for Calyx.
//!
//! The parser recovers from errors (it skips to the next statement or
//! declaration) so one run reports every problem. Constructs from later
//! milestones are recognized and reported as "not supported yet", with the
//! milestone where they arrive, instead of a generic syntax error.

use crate::ast::*;
use crate::lexer::{Token, TokenKind};
use crate::{Diagnostic, Span, lex};

/// Lexes and parses `text`.
pub fn parse(text: &str) -> (Program, Vec<Diagnostic>) {
    let (tokens, mut diags) = lex(text);
    let mut p = Parser {
        text,
        tokens,
        pos: 0,
        diags: Vec::new(),
    };
    let program = p.program();
    diags.append(&mut p.diags);
    (program, diags)
}

/// Units accepted after a number literal.
const UNITS: &[&str] = &[
    "tokens", "USD", "BRL", "EUR", "ms", "s", "min", "h", "days", "KB", "MB", "GB",
];
/// Units accepted after `/` in a rate, as in `50/s`.
const RATE_UNITS: &[&str] = &["s", "min", "h"];

/// Declarations planned for later milestones.
const FUTURE_DECLS: &[(&str, &str)] = &[
    ("fn", "M5"),
    ("message", "M6"),
    ("entity", "M6"),
    ("router", "a later milestone"),
];

/// Statements and expressions planned for later milestones.
const FUTURE_STMTS: &[(&str, &str)] = &[
    ("agent", "M5"),
    ("loop", "M5"),
    ("match", "M5"),
    ("if", "M5"),
    ("try", "M5"),
    ("state", "M6"),
    ("receive", "M6"),
    ("ask", "M6"),
    ("send", "M6"),
    ("respond", "a later milestone"),
    ("rounds", "a later milestone"),
    ("race", "a later milestone"),
    ("run", "a later milestone"),
];

struct Parser<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    diags: Vec<Diagnostic>,
}

/// Marker for "a diagnostic was already reported; recover".
struct Reported;
type PResult<T> = Result<T, Reported>;

impl Parser<'_> {
    // ----- token helpers -------------------------------------------------

    fn tok(&self) -> Token {
        self.tokens[self.pos]
    }

    fn kind(&self) -> TokenKind {
        self.tok().kind
    }

    fn nth_kind(&self, n: usize) -> TokenKind {
        self.tokens
            .get(self.pos + n)
            .map_or(TokenKind::Eof, |t| t.kind)
    }

    fn text_of(&self, t: Token) -> &str {
        &self.text[t.span.start as usize..t.span.end as usize]
    }

    fn is_word(&self, word: &str) -> bool {
        self.kind() == TokenKind::Ident && self.text_of(self.tok()) == word
    }

    fn advance(&mut self) -> Token {
        let t = self.tok();
        if t.kind != TokenKind::Eof {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.kind() == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    fn skip_newlines(&mut self) {
        while self.kind() == TokenKind::Newline {
            self.advance();
        }
    }

    fn describe(&self, t: Token) -> String {
        match t.kind {
            TokenKind::Newline => "end of line".into(),
            TokenKind::Eof => "end of file".into(),
            TokenKind::Str | TokenKind::LongStr => "text".into(),
            _ => format!("`{}`", self.text_of(t)),
        }
    }

    fn error_here(&mut self, code: &'static str, msg: &str, expected: &str) -> Reported {
        let t = self.tok();
        let observed = self.describe(t);
        self.diags.push(
            Diagnostic::error(code, msg, t.span)
                .expected(expected)
                .observed(observed),
        );
        Reported
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> PResult<Token> {
        if self.kind() == kind {
            Ok(self.advance())
        } else {
            Err(self.error_here("E0100", "syntax error", what))
        }
    }

    fn expect_word(&mut self, word: &str) -> PResult<Token> {
        if self.is_word(word) {
            Ok(self.advance())
        } else {
            Err(self.error_here("E0100", "syntax error", &format!("`{word}`")))
        }
    }

    fn ident(&mut self, what: &str) -> PResult<Ident> {
        let t = self.expect(TokenKind::Ident, what)?;
        Ok(Ident {
            name: self.text_of(t).to_owned(),
            span: t.span,
        })
    }

    fn span_from(&self, start: Span) -> Span {
        let end = self.tokens[self.pos.saturating_sub(1)].span.end;
        Span {
            start: start.start,
            end: end.max(start.start),
        }
    }

    fn unsupported(&mut self, what: &str, milestone: &str) -> Reported {
        let t = self.tok();
        self.diags.push(
            Diagnostic::error(
                "E0101",
                format!("`{what}` is not supported yet (planned for {milestone})"),
                t.span,
            )
            .observed(format!("`{what}`")),
        );
        Reported
    }

    /// Skips to the start of the next top-level declaration.
    fn recover_to_decl(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.kind() {
                TokenKind::Eof => return,
                TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => depth += 1,
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => depth -= 1,
                TokenKind::Newline if depth <= 0 => {
                    self.skip_newlines();
                    if self.at_decl_start() {
                        return;
                    }
                    continue;
                }
                _ => {}
            }
            self.advance();
        }
    }

    /// Skips to the end of the current statement (newline at depth 0) or to
    /// the `}` that closes the enclosing block, which is not consumed.
    fn recover_to_stmt(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.kind() {
                TokenKind::Eof => return,
                // `depth` goes negative when the error happened inside
                // parentheses: their closing tokens are part of this statement.
                TokenKind::Newline if depth <= 0 => return,
                TokenKind::RBrace if depth <= 0 => return,
                TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => depth += 1,
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => depth -= 1,
                _ => {}
            }
            self.advance();
        }
    }

    fn at_decl_start(&self) -> bool {
        const DECLS: &[&str] = &["model", "tool", "type", "prompt", "graph"];
        self.kind() == TokenKind::Ident
            && (DECLS.contains(&self.text_of(self.tok()))
                || FUTURE_DECLS
                    .iter()
                    .any(|(k, _)| *k == self.text_of(self.tok())))
    }

    // ----- declarations --------------------------------------------------

    fn program(&mut self) -> Program {
        let mut decls = Vec::new();
        loop {
            self.skip_newlines();
            if self.kind() == TokenKind::Eof {
                break;
            }
            match self.decl() {
                Ok(d) => decls.push(d),
                Err(Reported) => self.recover_to_decl(),
            }
        }
        Program { decls }
    }

    fn decl(&mut self) -> PResult<Decl> {
        let word = if self.kind() == TokenKind::Ident {
            self.text_of(self.tok()).to_owned()
        } else {
            String::new()
        };
        match word.as_str() {
            "model" => self.model_decl().map(Decl::Model),
            "tool" => self.tool_decl().map(Decl::Tool),
            "type" => self.type_decl().map(Decl::Type),
            "prompt" => self.prompt_decl().map(Decl::Prompt),
            "graph" => self.graph_decl().map(Decl::Graph),
            w => {
                if let Some((_, m)) = FUTURE_DECLS.iter().find(|(k, _)| *k == w) {
                    Err(self.unsupported(w, m))
                } else {
                    Err(self.error_here(
                        "E0100",
                        "syntax error",
                        "a declaration (`model`, `tool`, `type`, `prompt` or `graph`)",
                    ))
                }
            }
        }
    }

    fn model_decl(&mut self) -> PResult<ModelDecl> {
        let start = self.advance().span;
        let name = self.ident("a model name")?;
        self.expect(TokenKind::Eq, "`=`")?;
        self.expect_word("llm")?;
        self.expect(TokenKind::LParen, "`(`")?;
        self.skip_newlines();
        let id_tok = self.expect(TokenKind::Str, "the model identifier as text")?;
        let model_id = self.text_of(id_tok).trim_matches('"').to_owned();
        let mut max_output = None;
        self.skip_newlines();
        while self.eat(TokenKind::Comma) {
            self.skip_newlines();
            if self.kind() == TokenKind::RParen {
                break;
            }
            let key = self.ident("a model option")?;
            self.expect(TokenKind::Colon, "`:`")?;
            let value = self.expr()?;
            match (key.name.as_str(), &value.kind) {
                ("max_output", ExprKind::Int { value, unit })
                    if unit.as_deref() == Some("tokens") =>
                {
                    max_output = Some(*value);
                }
                ("max_output", _) => {
                    self.diags.push(
                        Diagnostic::error("E0100", "syntax error", value.span)
                            .expected("a number of tokens, e.g. `2_000 tokens`"),
                    );
                }
                _ => {
                    self.diags.push(
                        Diagnostic::error("E0102", "unknown model option", key.span)
                            .expected("`max_output`")
                            .observed(format!("`{}`", key.name)),
                    );
                }
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RParen, "`)`")?;
        Ok(ModelDecl {
            name,
            model_id,
            max_output,
            span: self.span_from(start),
        })
    }

    fn tool_decl(&mut self) -> PResult<ToolDecl> {
        let start = self.advance().span;
        let name = self.ident("a tool name")?;
        let params = self.params()?;
        self.expect(TokenKind::Arrow, "`->` and the return type")?;
        let ret = self.type_expr()?;
        self.expect(TokenKind::LBrace, "`{` with the tool properties")?;
        let mut props = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat(TokenKind::RBrace) {
                break;
            }
            if self.kind() == TokenKind::Eof {
                return Err(self.error_here("E0100", "syntax error", "`}`"));
            }
            let key = match self.ident("a tool property (e.g. `effect read`)") {
                Ok(k) => k,
                Err(r) => {
                    self.recover_to_stmt();
                    let _ = r;
                    continue;
                }
            };
            let mut value = Vec::new();
            let mut ok = true;
            while !matches!(
                self.kind(),
                TokenKind::Newline | TokenKind::RBrace | TokenKind::Eof
            ) {
                match self.expr() {
                    Ok(e) => value.push(e),
                    Err(Reported) => {
                        self.recover_to_stmt();
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                let span = Span {
                    start: key.span.start,
                    end: value.last().map_or(key.span.end, |e| e.span.end),
                };
                props.push(ToolProp { key, value, span });
            }
        }
        Ok(ToolDecl {
            name,
            params,
            ret,
            props,
            span: self.span_from(start),
        })
    }

    fn type_decl(&mut self) -> PResult<TypeDecl> {
        let start = self.advance().span;
        let name = self.ident("a type name")?;
        self.expect(TokenKind::Eq, "`=`")?;
        if self.kind() == TokenKind::Newline {
            // Variants may start on the next line: `type T =\n  | A\n  | B`.
            let mut n = 0;
            while self.nth_kind(n) == TokenKind::Newline {
                n += 1;
            }
            if self.nth_kind(n) == TokenKind::Pipe {
                self.skip_newlines();
            }
        }
        let ty = self.type_expr()?;
        Ok(TypeDecl {
            name,
            ty,
            span: self.span_from(start),
        })
    }

    fn prompt_decl(&mut self) -> PResult<PromptDecl> {
        let start = self.advance().span;
        let name = self.ident("a prompt name")?;
        let params = self.params()?;
        self.expect(TokenKind::Arrow, "`->` and the output type")?;
        let ret = self.type_expr()?;
        self.expect(TokenKind::LBrace, "`{` with the prompt text")?;
        self.skip_newlines();
        let template = match self.kind() {
            TokenKind::Str | TokenKind::LongStr => {
                let t = self.advance();
                self.str_lit(t)
            }
            _ => {
                return Err(self.error_here(
                    "E0100",
                    "syntax error",
                    "the prompt text, as `\"\"\"...\"\"\"`",
                ));
            }
        };
        self.skip_newlines();
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(PromptDecl {
            name,
            params,
            ret,
            template,
            span: self.span_from(start),
        })
    }

    fn graph_decl(&mut self) -> PResult<GraphDecl> {
        let start = self.advance().span;
        let name = self.ident("a graph name")?;
        let params = self.params()?;
        self.expect(TokenKind::Arrow, "`->` and the return type")?;
        let ret = self.type_expr()?;
        let mut max_effect = None;
        let mut decreases = None;
        loop {
            if self.is_word("effect") {
                self.advance();
                let mut words = Vec::new();
                while self.kind() == TokenKind::Ident && !self.is_word("decreases") {
                    words.push(self.ident("an effect")?);
                }
                max_effect = Some(words);
            } else if self.is_word("decreases") {
                self.advance();
                decreases = Some(self.ident("the parameter that decreases")?);
            } else {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::LBrace, "`{` with the graph body")?;
        let body = self.block_body();
        Ok(GraphDecl {
            name,
            params,
            ret,
            max_effect,
            decreases,
            body,
            span: self.span_from(start),
        })
    }

    fn params(&mut self) -> PResult<Vec<Param>> {
        self.expect(TokenKind::LParen, "`(` with the parameters")?;
        let mut params = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat(TokenKind::RParen) {
                break;
            }
            let name = self.ident("a parameter name")?;
            self.expect(TokenKind::Colon, "`:` and the parameter type")?;
            if self.is_word("reads") || self.is_word("edits") {
                let w = self.text_of(self.tok()).to_owned();
                return Err(self.unsupported(&w, "M6"));
            }
            let ty = self.type_expr()?;
            params.push(Param { name, ty });
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                self.skip_newlines();
                self.expect(TokenKind::RParen, "`,` or `)`")?;
                break;
            }
        }
        Ok(params)
    }

    // ----- types ---------------------------------------------------------

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let start = self.tok().span;
        match self.kind() {
            TokenKind::LBrace => {
                let fields = self.fields()?;
                Ok(TypeExpr {
                    kind: TypeKind::Record(fields),
                    span: self.span_from(start),
                })
            }
            TokenKind::Pipe => {
                let mut variants = Vec::new();
                while self.eat(TokenKind::Pipe) {
                    let name = self.ident("a variant name")?;
                    let fields = if self.kind() == TokenKind::LBrace {
                        self.fields()?
                    } else {
                        Vec::new()
                    };
                    variants.push(Variant { name, fields });
                    // The next variant may be on the next line.
                    let mut n = 0;
                    while self.nth_kind(n) == TokenKind::Newline {
                        n += 1;
                    }
                    if n > 0 && self.nth_kind(n) == TokenKind::Pipe {
                        self.skip_newlines();
                    }
                }
                Ok(TypeExpr {
                    kind: TypeKind::Variants(variants),
                    span: self.span_from(start),
                })
            }
            _ => {
                let name = self.ident("a type")?;
                let mut args = Vec::new();
                if self.eat(TokenKind::Lt) {
                    loop {
                        args.push(self.type_expr()?);
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::Gt, "`>`")?;
                }
                let mut max = None;
                if self.is_word("max") {
                    self.advance();
                    let t = self.expect(TokenKind::Int, "a number after `max`")?;
                    max = Some(self.int_value(t));
                }
                Ok(TypeExpr {
                    kind: TypeKind::Named { name, args, max },
                    span: self.span_from(start),
                })
            }
        }
    }

    fn fields(&mut self) -> PResult<Vec<Field>> {
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat(TokenKind::RBrace) {
                break;
            }
            let name = self.ident("a field name")?;
            self.expect(TokenKind::Colon, "`:` and the field type")?;
            let ty = self.type_expr()?;
            fields.push(Field { name, ty });
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                self.skip_newlines();
                self.expect(TokenKind::RBrace, "`,` or `}`")?;
                break;
            }
        }
        Ok(fields)
    }

    // ----- statements ----------------------------------------------------

    /// Parses statements until the closing `}` (consumed).
    fn block_body(&mut self) -> Vec<Stmt> {
        let mut body = Vec::new();
        loop {
            self.skip_newlines();
            match self.kind() {
                TokenKind::RBrace => {
                    self.advance();
                    return body;
                }
                TokenKind::Eof => {
                    let _ = self.error_here("E0100", "syntax error", "`}` closing the graph");
                    return body;
                }
                _ => {}
            }
            match self.stmt() {
                Ok(s) => {
                    if !matches!(
                        self.kind(),
                        TokenKind::Newline | TokenKind::RBrace | TokenKind::Eof
                    ) {
                        let _ = self.error_here(
                            "E0103",
                            "unexpected input after the statement",
                            "a line break",
                        );
                        self.recover_to_stmt();
                    }
                    body.push(s);
                }
                Err(Reported) => self.recover_to_stmt(),
            }
        }
    }

    /// Is the current token a keyword from a later milestone, used as a
    /// keyword? `ask(x)` or `run.x` are ordinary names; `ask Mem(...)` is not.
    fn future_keyword(&self) -> Option<(&'static str, &'static str)> {
        if self.kind() != TokenKind::Ident {
            return None;
        }
        let word = self.text_of(self.tok());
        let entry = FUTURE_STMTS.iter().find(|(k, _)| *k == word)?;
        let used_as_name = matches!(
            self.nth_kind(1),
            TokenKind::LParen
                | TokenKind::Dot
                | TokenKind::Comma
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::Newline
                | TokenKind::Eof
        );
        (!used_as_name).then_some(*entry)
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let word = if self.kind() == TokenKind::Ident {
            self.text_of(self.tok()).to_owned()
        } else {
            String::new()
        };
        match word.as_str() {
            "limits" => {
                self.advance();
                self.expect(TokenKind::LBrace, "`{`")?;
                let mut entries = Vec::new();
                loop {
                    self.skip_newlines();
                    if self.eat(TokenKind::RBrace) {
                        break;
                    }
                    let key = self.ident("a limit (`threads`, `rate`, `budget`, `memory`)")?;
                    self.expect(TokenKind::Colon, "`:`")?;
                    let value = self.expr()?;
                    entries.push((key, value));
                    self.skip_newlines();
                    if !self.eat(TokenKind::Comma) {
                        self.skip_newlines();
                        self.expect(TokenKind::RBrace, "`,` or `}`")?;
                        break;
                    }
                }
                Ok(Stmt::Limits(entries))
            }
            "node" => {
                self.advance();
                let name = self.ident("a node name")?;
                let fan_out = if self.eat(TokenKind::LBracket) {
                    let var = self.ident("the loop variable")?;
                    self.expect_word("in")?;
                    let over = self.expr()?;
                    self.expect(TokenKind::RBracket, "`]`")?;
                    Some((var, over))
                } else {
                    None
                };
                self.expect(TokenKind::Eq, "`=`")?;
                self.skip_newlines();
                let value = self.expr()?;
                Ok(Stmt::Node {
                    name,
                    fan_out,
                    value,
                })
            }
            "let" => {
                self.advance();
                if self.kind() == TokenKind::LBracket {
                    return Err(self.unsupported("let [..] destructuring", "M6"));
                }
                let name = self.ident("a name")?;
                self.expect(TokenKind::Eq, "`=`")?;
                self.skip_newlines();
                let value = self.expr()?;
                Ok(Stmt::Let { name, value })
            }
            "return" => {
                self.advance();
                Ok(Stmt::Return(self.expr()?))
            }
            _ => {
                if let Some((k, m)) = self.future_keyword() {
                    return Err(self.unsupported(k, m));
                }
                if self.nth_kind(1) == TokenKind::Ident
                    && self.text_of(self.tokens[self.pos + 1]) == "after"
                {
                    return Err(self.unsupported("after", "M6"));
                }
                Err(self.error_here(
                    "E0104",
                    "expected a statement",
                    "`node`, `let`, `return` or `limits`",
                ))
            }
        }
    }

    // ----- expressions ---------------------------------------------------

    fn expr(&mut self) -> PResult<Expr> {
        if let Some((k, m)) = self.future_keyword() {
            return Err(self.unsupported(k, m));
        }
        let mut e = self.primary()?;
        loop {
            match self.kind() {
                TokenKind::Dot => {
                    self.advance();
                    let name = self.ident("a field name")?;
                    let span = Span {
                        start: e.span.start,
                        end: name.span.end,
                    };
                    e = Expr {
                        kind: ExprKind::Field {
                            base: Box::new(e),
                            name,
                        },
                        span,
                    };
                }
                TokenKind::LParen => {
                    self.advance();
                    let args = self.args()?;
                    let span = self.span_from(e.span);
                    e = Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(e),
                            args,
                        },
                        span,
                    };
                }
                k if is_operator(k) => {
                    let t = self.tok();
                    let op = self.text_of(t).to_owned();
                    return Err(self.unsupported(&format!("operator {op}"), "M5"));
                }
                _ => return Ok(e),
            }
        }
    }

    fn args(&mut self) -> PResult<Vec<Arg>> {
        let mut args = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat(TokenKind::RParen) {
                break;
            }
            let name = if self.kind() == TokenKind::Ident && self.nth_kind(1) == TokenKind::Colon {
                let n = self.ident("an argument name")?;
                self.advance(); // `:`
                Some(n)
            } else {
                None
            };
            let value = self.expr()?;
            args.push(Arg { name, value });
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                self.skip_newlines();
                self.expect(TokenKind::RParen, "`,` or `)`")?;
                break;
            }
        }
        Ok(args)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let t = self.tok();
        match t.kind {
            TokenKind::Ident => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Ident(self.text_of(t).to_owned()),
                    span: t.span,
                })
            }
            TokenKind::Str | TokenKind::LongStr => {
                self.advance();
                let lit = self.str_lit(t);
                Ok(Expr {
                    kind: ExprKind::Str(lit),
                    span: t.span,
                })
            }
            TokenKind::Int => {
                self.advance();
                let value = self.int_value(t);
                let unit = self.unit();
                Ok(Expr {
                    kind: ExprKind::Int { value, unit },
                    span: self.span_from(t.span),
                })
            }
            TokenKind::Float => {
                self.advance();
                let value = self.text_of(t).replace('_', "").parse().unwrap_or(0.0);
                let unit = self.unit();
                Ok(Expr {
                    kind: ExprKind::Float { value, unit },
                    span: self.span_from(t.span),
                })
            }
            TokenKind::LBracket => {
                self.advance();
                let mut items = Vec::new();
                loop {
                    self.skip_newlines();
                    if self.eat(TokenKind::RBracket) {
                        break;
                    }
                    items.push(self.expr()?);
                    self.skip_newlines();
                    if !self.eat(TokenKind::Comma) {
                        self.skip_newlines();
                        self.expect(TokenKind::RBracket, "`,` or `]`")?;
                        break;
                    }
                }
                Ok(Expr {
                    kind: ExprKind::List(items),
                    span: self.span_from(t.span),
                })
            }
            TokenKind::LBrace => Err(self.unsupported("record literal", "M5")),
            _ => Err(self.error_here("E0102", "expected an expression", "a value or a call")),
        }
    }

    fn unit(&mut self) -> Option<String> {
        if self.kind() == TokenKind::Ident && UNITS.contains(&self.text_of(self.tok())) {
            let t = self.advance();
            return Some(self.text_of(t).to_owned());
        }
        if self.kind() == TokenKind::Slash
            && self.nth_kind(1) == TokenKind::Ident
            && RATE_UNITS.contains(&self.text_of(self.tokens[self.pos + 1]))
        {
            self.advance();
            let t = self.advance();
            return Some(format!("/{}", self.text_of(t)));
        }
        None
    }

    fn int_value(&mut self, t: Token) -> u64 {
        match self.text_of(t).replace('_', "").parse() {
            Ok(v) => v,
            Err(_) => {
                self.diags.push(
                    Diagnostic::error("E0105", "number too large", t.span)
                        .expected(format!("at most {}", u64::MAX)),
                );
                0
            }
        }
    }

    fn str_lit(&self, t: Token) -> StrLit {
        let quote = if t.kind == TokenKind::LongStr { 3 } else { 1 };
        let raw = self.text_of(t);
        let inner = if raw.len() >= 2 * quote {
            &raw[quote..raw.len() - quote]
        } else {
            ""
        };
        StrLit {
            text: inner.to_owned(),
            content_offset: t.span.start + quote as u32,
            span: t.span,
        }
    }
}

fn is_operator(k: TokenKind) -> bool {
    use TokenKind::*;
    matches!(
        k,
        Plus | PlusPlus
            | Minus
            | Star
            | Slash
            | Percent
            | EqEq
            | Ne
            | Lt
            | Gt
            | Le
            | Ge
            | AndAnd
            | OrOr
            | DotDot
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> Program {
        let (p, diags) = parse(src);
        assert!(diags.is_empty(), "{diags:#?}");
        p
    }

    #[test]
    fn full_subset_program() {
        let p = parse_ok(
            r#"
model claude = llm("claude-sonnet-5-5", max_output: 2_000 tokens)

tool web_search(query: Text) -> Text {
  effect     read
  max_output 4_000 tokens
  retry_on   [Timeout, RateLimit]
}

type Plan = { questions: List<Text> max 5 }

type Review =
  | Approved
  | Rejected { feedback: Text }

prompt split(topic: Text) -> Plan {
  """
  Divida {topic}.
  """
}

graph research(topic: Text) -> List<Text> effect read {
  limits { threads: 8, budget: 2 USD, rate: 50/s }
  node plan = claude(split(topic))
  node found[q in plan.questions] = web_search(q)
  let label = "pesquisa: {topic}"
  return found
}
"#,
        );
        assert_eq!(p.decls.len(), 6);
        let Decl::Graph(g) = &p.decls[5] else {
            panic!()
        };
        assert_eq!(g.body.len(), 5);
        assert_eq!(g.max_effect.as_ref().unwrap()[0].name, "read");
        let Decl::Type(t) = &p.decls[3] else { panic!() };
        assert!(matches!(&t.ty.kind, TypeKind::Variants(v) if v.len() == 2));
    }

    #[test]
    fn multi_line_call_arguments() {
        parse_ok("graph g() -> Text {\n  node a = f(\n    x,\n    y: z\n  )\n  return a\n}\n");
    }

    #[test]
    fn future_constructs_name_their_milestone() {
        let (_, diags) = parse("graph g() -> Text {\n  node a = agent claude {}\n  return a\n}\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "E0101");
        assert!(diags[0].message.contains("M5"));
    }

    #[test]
    fn recovers_and_reports_every_error() {
        let (p, diags) = parse(
            "graph g() -> Text {\n  node = f()\n  bogus x\n  return a\n}\nfn h() -> Text { x }\ngraph k() -> Text {\n  return b\n}\n",
        );
        let codes: Vec<_> = diags.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec!["E0100", "E0104", "E0101"]);
        assert_eq!(p.decls.len(), 2); // g and k
    }
}
