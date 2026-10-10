//! Recursive-descent parser for Calyx.
//!
//! The syntax looks like Python: blocks are introduced by `:` and indented,
//! calls use parentheses, keyword arguments use `=`, comments start with `#`.
//! The semantics are functional: every name is bound once and never changes.
//!
//! The parser recovers from errors (it skips to the next line or
//! declaration) so one run reports every problem. Constructs from later
//! milestones are recognized and reported as "not supported yet", with the
//! milestone where they arrive.

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
        depth: 0,
        broken: None,
        sends: 0,
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

const DECLS: &[&str] = &[
    "model", "tool", "type", "message", "prompt", "graph", "entity", "def", "router",
];

/// Declarations planned for later milestones.
const FUTURE_DECLS: &[(&str, &str)] = &[];

/// Statements and expressions planned for later milestones.
const FUTURE_STMTS: &[(&str, &str)] = &[
    ("state", "a later milestone"),
    ("respond", "a later milestone"),
    ("run", "a later milestone"),
];

struct Parser<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    /// Number of indented blocks open at `pos`.
    depth: usize,
    /// Name of a `name = ...` statement whose value failed to parse.
    broken: Option<Ident>,
    /// `send` statements so far, to name their steps.
    sends: usize,
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

    fn word(&self) -> &str {
        if self.kind() == TokenKind::Ident {
            self.text_of(self.tok())
        } else {
            ""
        }
    }

    fn is_word(&self, word: &str) -> bool {
        self.word() == word
    }

    fn advance(&mut self) -> Token {
        let t = self.tok();
        match t.kind {
            TokenKind::Eof => return t,
            TokenKind::Indent => self.depth += 1,
            TokenKind::Dedent => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        self.pos += 1;
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

    fn describe(&self, t: Token) -> String {
        match t.kind {
            TokenKind::Newline => "end of line".into(),
            TokenKind::Indent => "an indented line".into(),
            TokenKind::Dedent => "the end of the block".into(),
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

    fn error_here_at(
        &mut self,
        span: Span,
        code: &'static str,
        msg: &str,
        expected: &str,
        observed: &str,
    ) -> Reported {
        self.diags.push(
            Diagnostic::error(code, msg, span)
                .expected(expected)
                .observed(format!("`{observed}`")),
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

    /// End of a line: a `Newline`, or the end of the enclosing block.
    fn end_of_line(&mut self) -> PResult<()> {
        match self.kind() {
            TokenKind::Newline => {
                self.advance();
                Ok(())
            }
            TokenKind::Dedent | TokenKind::Eof => Ok(()),
            _ => {
                let r = self.error_here(
                    "E0103",
                    "unexpected input at the end of the line",
                    "a line break",
                );
                self.recover_line();
                Err(r)
            }
        }
    }

    /// `:` followed by a line break and an indented block. Leaves the parser
    /// on the first token of the block.
    fn block_start(&mut self, what: &str) -> PResult<()> {
        self.expect(TokenKind::Colon, &format!("`:` and {what}"))?;
        self.expect(
            TokenKind::Newline,
            &format!("a line break, then {what} indented"),
        )?;
        self.expect(TokenKind::Indent, &format!("{what}, indented"))?;
        Ok(())
    }

    /// Skips the rest of the current line, and any block that hangs from it.
    fn recover_line(&mut self) {
        self.recover_to(self.depth);
    }

    /// Skips to the start of the next line at block depth `base`. An error deep
    /// inside a nested block skips the rest of that block too, so the enclosing
    /// graph keeps parsing its next statement instead of ending early.
    fn recover_to(&mut self, base: usize) {
        loop {
            match self.kind() {
                TokenKind::Eof => return,
                TokenKind::Dedent if self.depth <= base => return,
                TokenKind::Dedent => {
                    self.advance();
                    if self.depth == base {
                        return;
                    }
                }
                TokenKind::Newline if self.depth == base => {
                    self.advance();
                    if self.kind() == TokenKind::Indent {
                        self.skip_block();
                    }
                    return;
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// Skips an indented block, starting at its `Indent`.
    fn skip_block(&mut self) {
        let mut level = 0usize;
        loop {
            match self.advance().kind {
                TokenKind::Indent => level += 1,
                TokenKind::Dedent => {
                    level = level.saturating_sub(1);
                    if level == 0 {
                        return;
                    }
                }
                TokenKind::Eof => return,
                _ => {}
            }
        }
    }

    /// Skips to the start of the next top-level declaration.
    fn recover_to_decl(&mut self) {
        let mut level = 0i32;
        loop {
            match self.kind() {
                TokenKind::Eof => return,
                TokenKind::Indent => level += 1,
                TokenKind::Dedent => {
                    level -= 1;
                    self.advance();
                    if level <= 0 && self.at_decl_start() {
                        return;
                    }
                    continue;
                }
                TokenKind::Newline if level <= 0 => {
                    self.advance();
                    while self.kind() == TokenKind::Dedent {
                        self.advance();
                    }
                    if self.at_decl_start() || self.kind() == TokenKind::Eof {
                        return;
                    }
                    continue;
                }
                _ => {}
            }
            self.advance();
        }
    }

    fn at_decl_start(&self) -> bool {
        let w = self.word();
        DECLS.contains(&w) || FUTURE_DECLS.iter().any(|(k, _)| *k == w)
    }

    // ----- declarations --------------------------------------------------

    fn program(&mut self) -> Program {
        let mut decls = Vec::new();
        loop {
            while matches!(self.kind(), TokenKind::Newline | TokenKind::Dedent) {
                self.advance();
            }
            match self.kind() {
                TokenKind::Eof => break,
                TokenKind::Indent => {
                    let _ = self.error_here(
                        "E0106",
                        "unexpected indentation",
                        "a declaration starting at the beginning of the line",
                    );
                    self.skip_block();
                    continue;
                }
                _ => {}
            }
            let at = self.pos;
            match self.decl() {
                Ok(d) => decls.push(d),
                // A declaration that already recovered to the next one stays there.
                Err(Reported) if self.pos > at && self.depth == 0 && self.at_decl_start() => {}
                Err(Reported) => self.recover_to_decl(),
            }
        }
        Program { decls }
    }

    fn decl(&mut self) -> PResult<Decl> {
        let word = self.word().to_owned();
        match word.as_str() {
            "model" => self.model_decl().map(Decl::Model),
            "tool" => self.tool_decl().map(Decl::Tool),
            "type" => self.type_decl().map(Decl::Type),
            "message" => self.type_decl().map(|mut t| {
                t.message = true;
                Decl::Type(t)
            }),
            "prompt" => self.prompt_decl().map(Decl::Prompt),
            "graph" => self.graph_decl().map(Decl::Graph),
            "entity" => self.entity_decl().map(Decl::Entity),
            "def" => self.def_decl().map(Decl::Def),
            "router" => self.router_decl().map(Decl::Router),
            w => {
                if let Some((_, m)) = FUTURE_DECLS.iter().find(|(k, _)| *k == w) {
                    Err(self.unsupported(w, m))
                } else {
                    Err(self.error_here(
                        "E0100",
                        "syntax error",
                        "a declaration (`model`, `router`, `tool`, `type`, `prompt`, `graph`, `entity` or `def`)",
                    ))
                }
            }
        }
    }

    /// `model claude = "id"` with an optional properties block.
    fn model_decl(&mut self) -> PResult<ModelDecl> {
        let start = self.advance().span;
        let name = self.ident("a model name")?;
        self.expect(TokenKind::Eq, "`=` and the model identifier")?;
        let id_tok = self.expect(
            TokenKind::Str,
            "the model identifier as text, e.g. \"claude-sonnet-5-5\"",
        )?;
        let model_id = self.text_of(id_tok).trim_matches('"').to_owned();
        let mut max_output = None;
        if self.kind() == TokenKind::Colon {
            for p in self.props("the model properties")? {
                match (p.key.name.as_str(), p.value.as_slice()) {
                    (
                        "max_output",
                        [
                            Expr {
                                kind: ExprKind::Int { value, unit },
                                ..
                            },
                        ],
                    ) if unit.as_deref() == Some("tokens") => {
                        max_output = Some(*value);
                    }
                    ("max_output", _) => self.diags.push(
                        Diagnostic::error("E0305", "invalid value for `max_output`", p.span)
                            .expected("a number of tokens, e.g. `2000 tokens`"),
                    ),
                    (k, _) => self.diags.push(
                        Diagnostic::error("E0102", "unknown model property", p.key.span)
                            .expected("`max_output`")
                            .observed(format!("`{k}`")),
                    ),
                }
            }
        } else {
            self.end_of_line()?;
        }
        Ok(ModelDecl {
            name,
            model_id,
            max_output,
            span: self.span_from(start),
        })
    }

    /// `router name = route [m1, m2, ...]:` then `policy policy_name(check)`.
    fn router_decl(&mut self) -> PResult<RouterDecl> {
        let start = self.advance().span; // router
        let name = self.ident("a router name")?;
        self.expect(TokenKind::Eq, "`= route [models]`")?;
        self.expect_word("route")?;
        self.expect(TokenKind::LBracket, "`[` and the models, cheapest first")?;
        let mut models = Vec::new();
        loop {
            models.push(self.ident("a model")?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBracket, "`]`")?;
        let mut policy = None;
        for p in self.props("the router's policy")? {
            match (p.key.name.as_str(), p.value.as_slice()) {
                (
                    "policy",
                    [
                        Expr {
                            kind: ExprKind::Call { callee, args },
                            ..
                        },
                    ],
                ) if matches!(&callee.kind, ExprKind::Ident(_))
                    && args.len() == 1
                    && args[0].name.is_none()
                    && matches!(&args[0].value.kind, ExprKind::Ident(_)) =>
                {
                    let (ExprKind::Ident(policy_name), ExprKind::Ident(check)) =
                        (&callee.kind, &args[0].value.kind)
                    else {
                        unreachable!("matched above")
                    };
                    policy = Some((
                        Ident {
                            name: policy_name.clone(),
                            span: callee.span,
                        },
                        Ident {
                            name: check.clone(),
                            span: args[0].value.span,
                        },
                    ));
                }
                ("policy", _) => self.diags.push(
                    Diagnostic::error("E0306", "invalid router policy", p.span)
                        .expected("`policy cheapest_that_passes(check)`, with `check` a `def`"),
                ),
                (k, _) => self.diags.push(
                    Diagnostic::error("E0102", "unknown router property", p.key.span)
                        .expected("`policy`")
                        .observed(format!("`{k}`")),
                ),
            }
        }
        Ok(RouterDecl {
            name,
            models,
            policy,
            span: self.span_from(start),
        })
    }

    /// A `:` block of `key value...` lines.
    fn props(&mut self, what: &str) -> PResult<Vec<ToolProp>> {
        self.block_start(what)?;
        let mut props = Vec::new();
        while !matches!(self.kind(), TokenKind::Dedent | TokenKind::Eof) {
            let key = match self.ident("a property name, e.g. `effect`") {
                Ok(k) => k,
                Err(Reported) => {
                    self.recover_line();
                    continue;
                }
            };
            let mut value = Vec::new();
            let mut ok = true;
            while !matches!(
                self.kind(),
                TokenKind::Newline | TokenKind::Dedent | TokenKind::Eof
            ) {
                match self.expr() {
                    Ok(e) => value.push(e),
                    Err(Reported) => {
                        self.recover_line();
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                self.eat(TokenKind::Newline);
                let span = Span {
                    start: key.span.start,
                    end: value.last().map_or(key.span.end, |e| e.span.end),
                };
                props.push(ToolProp { key, value, span });
            }
        }
        self.eat(TokenKind::Dedent);
        Ok(props)
    }

    fn tool_decl(&mut self) -> PResult<ToolDecl> {
        let start = self.advance().span;
        let name = self.ident("a tool name")?;
        let params = self.params()?;
        self.expect(TokenKind::Arrow, "`->` and the return type")?;
        let ret = self.type_expr()?;
        let props = self.props("the tool properties (e.g. `effect read`)")?;
        Ok(ToolDecl {
            name,
            params,
            ret,
            props,
            span: self.span_from(start),
        })
    }

    /// `type T = A | B(x: Text)`, `type Id = Text`, or `type T:` with fields.
    fn type_decl(&mut self) -> PResult<TypeDecl> {
        let start = self.advance().span;
        let name = self.ident("a type name")?;
        let ty_start = self.tok().span;
        let ty = if self.kind() == TokenKind::Colon {
            self.block_start("the fields (`name: Type`)")?;
            let mut fields = Vec::new();
            while !matches!(self.kind(), TokenKind::Dedent | TokenKind::Eof) {
                match self.field() {
                    Ok(f) => {
                        fields.push(f);
                        let _ = self.end_of_line();
                    }
                    Err(Reported) => self.recover_line(),
                }
            }
            self.eat(TokenKind::Dedent);
            TypeExpr {
                kind: TypeKind::Record(fields),
                span: self.span_from(ty_start),
            }
        } else {
            self.expect(TokenKind::Eq, "`=` or `:`")?;
            let ty = self.type_rhs()?;
            if self.kind() == TokenKind::Colon {
                // `type Plan = Graph[T]:` with limits for generated graphs (D4).
                let r = self.unsupported("a property block on a type", "a later milestone");
                self.recover_line();
                return Err(r);
            }
            self.end_of_line()?;
            ty
        };
        Ok(TypeDecl {
            message: false,
            name,
            ty,
            span: self.span_from(start),
        })
    }

    /// The right side of `type T = ...`: variants or another type.
    fn type_rhs(&mut self) -> PResult<TypeExpr> {
        let start = self.tok().span;
        let leading_pipe = self.eat(TokenKind::Pipe);
        let is_variants = leading_pipe
            || self.nth_kind(1) == TokenKind::Pipe
            || self.nth_kind(1) == TokenKind::LParen;
        if !is_variants {
            return self.type_expr();
        }
        let mut variants = Vec::new();
        loop {
            let name = self.ident("a variant name")?;
            let mut fields = Vec::new();
            if self.eat(TokenKind::LParen) {
                loop {
                    if self.eat(TokenKind::RParen) {
                        break;
                    }
                    fields.push(self.field()?);
                    if !self.eat(TokenKind::Comma) {
                        self.expect(TokenKind::RParen, "`,` or `)`")?;
                        break;
                    }
                }
            }
            variants.push(Variant { name, fields });
            if !self.eat(TokenKind::Pipe) {
                break;
            }
        }
        Ok(TypeExpr {
            kind: TypeKind::Variants(variants),
            span: self.span_from(start),
        })
    }

    fn field(&mut self) -> PResult<Field> {
        let name = self.ident("a field name")?;
        self.expect(TokenKind::Colon, "`:` and the field type")?;
        let ty = self.type_expr()?;
        Ok(Field { name, ty })
    }

    fn prompt_decl(&mut self) -> PResult<PromptDecl> {
        let start = self.advance().span;
        let name = self.ident("a prompt name")?;
        let params = self.params()?;
        self.expect(TokenKind::Arrow, "`->` and the output type")?;
        let ret = self.type_expr()?;
        self.block_start("the prompt text as `\"\"\"...\"\"\"`")?;
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
        self.end_of_line()?;
        if self.kind() != TokenKind::Dedent && self.kind() != TokenKind::Eof {
            return Err(self.error_here(
                "E0100",
                "syntax error",
                "the end of the prompt (a prompt has a single text)",
            ));
        }
        self.eat(TokenKind::Dedent);
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
        self.block_start("the graph body")?;
        let mut max_effect = None;
        let mut decreases = None;
        let mut body = Vec::new();
        let mut incomplete = false;
        while !matches!(self.kind(), TokenKind::Dedent | TokenKind::Eof) {
            if self.is_word("effect") {
                self.advance();
                let mut words = Vec::new();
                while self.kind() == TokenKind::Ident {
                    let t = self.advance();
                    words.push(Ident {
                        name: self.text_of(t).to_owned(),
                        span: t.span,
                    });
                }
                max_effect = Some(words);
                let _ = self.end_of_line();
                continue;
            }
            if self.is_word("decreases") {
                self.advance();
                match self.ident("the parameter that decreases") {
                    Ok(p) => decreases = Some(p),
                    Err(Reported) => self.recover_line(),
                }
                let _ = self.end_of_line();
                continue;
            }
            let base = self.depth;
            self.broken = None;
            match self.stmt() {
                Ok(s) => {
                    body.push(s);
                    // A statement that ended with an indented block is complete.
                    if self.tokens[self.pos.saturating_sub(1)].kind != TokenKind::Dedent {
                        let _ = self.end_of_line();
                    }
                }
                Err(Reported) => {
                    incomplete = true;
                    self.recover_to(base);
                    // Keep the name declared, so its uses do not cascade.
                    if let Some(name) = self.broken.take() {
                        let span = name.span;
                        body.push(Stmt::Node {
                            name,
                            fan_out: None,
                            value: Expr {
                                kind: ExprKind::Error,
                                span,
                            },
                        });
                    }
                }
            }
        }
        self.eat(TokenKind::Dedent);
        Ok(GraphDecl {
            name,
            params,
            ret,
            max_effect,
            decreases,
            body,
            incomplete,
            span: self.span_from(start),
        })
    }

    /// `def name(params) -> T:` then statements, ending with `return`.
    fn def_decl(&mut self) -> PResult<DefDecl> {
        let start = self.advance().span;
        let name = self.ident("a function name")?;
        let params = self.params()?;
        self.expect(TokenKind::Arrow, "`->` and the return type")?;
        let ret = self.type_expr()?;
        self.block_start("the function's body")?;
        let body = self.def_block()?;
        Ok(DefDecl {
            name,
            params,
            ret,
            body,
            span: self.span_from(start),
        })
    }

    /// Statements of a `def` (or of an `if` inside one), until the block ends.
    fn def_block(&mut self) -> PResult<Vec<DefStmt>> {
        let mut out = Vec::new();
        while !matches!(self.kind(), TokenKind::Dedent | TokenKind::Eof) {
            out.push(self.def_stmt()?);
        }
        self.eat(TokenKind::Dedent);
        Ok(out)
    }

    fn def_stmt(&mut self) -> PResult<DefStmt> {
        if self.is_word("return") {
            self.advance();
            let e = self.expr()?;
            self.end_of_line()?;
            return Ok(DefStmt::Return(e));
        }
        if self.is_word("if") {
            return self.def_if();
        }
        if self.kind() == TokenKind::Ident && self.nth_kind(1) == TokenKind::Eq {
            let name = self.ident("a name")?;
            self.advance(); // =
            let e = self.expr()?;
            // A value that ended with an indented block is complete.
            if self.tokens[self.pos.saturating_sub(1)].kind != TokenKind::Dedent {
                self.end_of_line()?;
            }
            return Ok(DefStmt::Assign(name, e));
        }
        Err(self.error_here(
            "E0104",
            "expected a statement",
            "`name = ...`, `if ...:` or `return ...`",
        ))
    }

    /// `if cond:` block, then `elif cond:` / `else:` blocks.
    fn def_if(&mut self) -> PResult<DefStmt> {
        let start = self.advance().span; // if / elif
        let cond = self.expr()?;
        self.block_start("the statements when the condition holds")?;
        let then = self.def_block()?;
        let els = if self.is_word("elif") {
            vec![self.def_if()?]
        } else if self.is_word("else") {
            self.advance();
            self.block_start("the statements when it does not")?;
            self.def_block()?
        } else {
            Vec::new()
        };
        Ok(DefStmt::If {
            cond,
            then,
            els,
            span: start,
        })
    }

    /// `entity Name(key k: T):` then `state` lines and `on` handlers.
    fn entity_decl(&mut self) -> PResult<EntityDecl> {
        let start = self.advance().span;
        let name = self.ident("an entity name")?;
        self.expect(TokenKind::LParen, "`(key name: Type)`")?;
        if !self.is_word("key") {
            return Err(self.error_here(
                "E0111",
                "an entity is identified by a key",
                "`key name: Type`",
            ));
        }
        self.advance();
        let kname = self.ident("the key's name")?;
        self.expect(TokenKind::Colon, "`:` and the key's type")?;
        let kty = self.type_expr()?;
        self.expect(TokenKind::RParen, "`)`")?;
        let key = Param {
            name: kname,
            ty: kty,
            borrow: None,
        };
        self.block_start("the entity's `state` and `on` handlers")?;
        let mut state = Vec::new();
        let mut handlers = Vec::new();
        while !matches!(self.kind(), TokenKind::Dedent | TokenKind::Eof) {
            if self.is_word("state") {
                self.advance();
                let fname = self.ident("a state field")?;
                self.expect(TokenKind::Colon, "`:` and the field's type")?;
                let ty = self.type_expr()?;
                self.expect(TokenKind::Eq, "`=` and the initial value")?;
                let init = self.expr()?;
                state.push(StateField {
                    name: fname,
                    ty,
                    init,
                });
                self.end_of_line()?;
            } else if self.is_word("on") {
                handlers.push(self.handler()?);
            } else {
                return Err(self.error_here(
                    "E0111",
                    "expected `state` or `on` in an entity",
                    "`state name: Type = value` or `on Message(...):`",
                ));
            }
        }
        self.eat(TokenKind::Dedent);
        Ok(EntityDecl {
            name,
            key,
            state,
            handlers,
            span: self.span_from(start),
        })
    }

    /// `on Name(params) -> T:` + `return e`, or `on Name(params):` + `next f = e` lines.
    fn handler(&mut self) -> PResult<Handler> {
        let start = self.advance().span; // on
        let name = self.ident("the message's name")?;
        let params = self.params()?;
        let ret = if self.eat(TokenKind::Arrow) {
            Some(self.type_expr()?)
        } else {
            None
        };
        self.block_start("the handler's body")?;
        let mut returns = None;
        let mut updates = Vec::new();
        while !matches!(self.kind(), TokenKind::Dedent | TokenKind::Eof) {
            if self.is_word("return") {
                self.advance();
                returns = Some(self.expr()?);
            } else if self.is_word("next") && self.nth_kind(1) == TokenKind::Ident {
                self.advance();
                let field = self.ident("a state field")?;
                self.expect(TokenKind::Eq, "`=` and the field's new value")?;
                updates.push((field, self.expr()?));
            } else {
                return Err(self.error_here(
                    "E0111",
                    "expected `return` or `next` in a handler",
                    "`return value` (a handler that answers) or `next field = value` (one that changes the state)",
                ));
            }
            self.end_of_line()?;
        }
        self.eat(TokenKind::Dedent);
        Ok(Handler {
            name,
            params,
            ret,
            returns,
            updates,
            span: self.span_from(start),
        })
    }

    fn params(&mut self) -> PResult<Vec<Param>> {
        self.expect(TokenKind::LParen, "`(` with the parameters")?;
        let mut params = Vec::new();
        loop {
            if self.eat(TokenKind::RParen) {
                break;
            }
            let name = self.ident("a parameter name")?;
            self.expect(TokenKind::Colon, "`:` and the parameter type")?;
            let borrow = if self.is_word("reads") || self.is_word("edits") {
                Some(self.ident("`reads` or `edits`")?)
            } else {
                None
            };
            let ty = self.type_expr()?;
            params.push(Param { name, ty, borrow });
            if !self.eat(TokenKind::Comma) {
                self.expect(TokenKind::RParen, "`,` or `)`")?;
                break;
            }
        }
        Ok(params)
    }

    // ----- types ---------------------------------------------------------

    /// `Text`, `List[Text]`, `List[Text] max 5`, `Map[Text, Nat]`
    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let start = self.tok().span;
        let name = self.ident("a type")?;
        if self.kind() == TokenKind::LParen {
            return Err(self.unsupported(&format!("{}(...) types", name.name), "a later milestone"));
        }
        let mut args = Vec::new();
        if self.eat(TokenKind::LBracket) {
            loop {
                args.push(self.type_expr()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBracket, "`]`")?;
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

    // ----- statements ----------------------------------------------------

    fn stmt(&mut self) -> PResult<Stmt> {
        if self.is_word("limits") {
            self.advance();
            let mut entries = Vec::new();
            loop {
                let key = self.ident("a limit (`threads`, `rate`, `budget` or `memory`)")?;
                let value = self.expr()?;
                entries.push((key, value));
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            return Ok(Stmt::Limits(entries));
        }
        if self.is_word("return") {
            self.advance();
            return Ok(Stmt::Return(self.expr()?));
        }
        if let Some((k, m)) = self.future_keyword() {
            return Err(self.unsupported(k, m));
        }
        // `send E(k).M(x)` on its own line: a step without a name.
        if self.is_word("send") && self.nth_kind(1) == TokenKind::Ident {
            let at = self.tok().span;
            let value = self.message_expr()?;
            let n = self.sends;
            self.sends += 1;
            return Ok(Stmt::Node {
                name: Ident {
                    name: format!("send_{n}"),
                    span: at,
                },
                fan_out: None,
                value,
            });
        }
        if self.kind() == TokenKind::Ident && self.nth_kind(1) == TokenKind::Eq {
            let name = self.ident("a name")?;
            self.advance(); // `=`
            self.broken = Some(name.clone());
            if self.is_word("for") && self.nth_kind(1) == TokenKind::Ident {
                let t = self.tokens[self.pos + 1];
                if self.text_of(t) == "each" {
                    return self.for_each(name);
                }
            }
            let value = self.expr()?;
            let value = self.with_requires(value)?;
            return Ok(Stmt::Node {
                name,
                fan_out: None,
                value,
            });
        }
        if self.kind() == TokenKind::Ident && self.nth_kind(1) == TokenKind::Comma {
            return Err(self.unsupported("a, b = ... (destructuring)", "a later milestone"));
        }
        if self.kind() == TokenKind::Ident
            && self.text_of(self.tokens[self.pos]) == "unordered"
            && self.nth_kind(1) == TokenKind::Ident
        {
            self.advance(); // unordered
            let mut steps = vec![self.ident("a step")?];
            while self.eat(TokenKind::Comma) {
                steps.push(self.ident("a step")?);
            }
            return Ok(Stmt::Unordered(steps));
        }
        if self.kind() == TokenKind::Ident
            && self.nth_kind(1) == TokenKind::Ident
            && self.text_of(self.tokens[self.pos + 1]) == "after"
        {
            let node = self.ident("a step")?;
            self.advance(); // after
            let mut after = vec![self.ident("the step it comes after")?];
            while self.eat(TokenKind::Comma) {
                after.push(self.ident("a step")?);
            }
            return Ok(Stmt::After { node, after });
        }
        Err(self.error_here(
            "E0104",
            "expected a statement",
            "`name = ...`, `return ...` or `limits ...`",
        ))
    }

    /// `name = for each x in list: expr` (inline or indented body).
    fn for_each(&mut self, name: Ident) -> PResult<Stmt> {
        let e = self.each_expr()?;
        let ExprKind::Each { var, over, body } = e.kind else {
            unreachable!("each_expr gives `Each`")
        };
        Ok(Stmt::Node {
            name,
            fan_out: Some((var, *over)),
            value: *body,
        })
    }

    /// `for each x in list: expr`, the body inline or indented on the next
    /// line. Starts at `for`.
    fn each_expr(&mut self) -> PResult<Expr> {
        let start = self.advance().span; // for
        self.advance(); // each
        let var = self.ident("the name of each item")?;
        self.expect_word("in")?;
        let over = self.expr()?;
        self.expect(TokenKind::Colon, "`:` and what to do with each item")?;
        let body = if self.eat(TokenKind::Newline) {
            self.expect(TokenKind::Indent, "what to do with each item, indented")?;
            let value = self.expr()?;
            self.eat(TokenKind::Newline);
            if self.kind() != TokenKind::Dedent {
                return Err(self.unsupported("multi-line `for each` bodies", "a later milestone"));
            }
            self.advance();
            value
        } else {
            self.expr()?
        };
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Each {
                var,
                over: Box::new(over),
                body: Box::new(body),
            },
        })
    }

    /// Is `for each` next?
    fn at_for_each(&self) -> bool {
        self.is_word("for")
            && self.nth_kind(1) == TokenKind::Ident
            && self.text_of(self.tokens[self.pos + 1]) == "each"
    }

    /// The body of a loop or of rounds, at the first token of its block:
    /// `name = value` steps (`for each` included), then what the turn gives.
    fn turn_body(&mut self) -> PResult<Expr> {
        let start = self.tok().span;
        let mut steps = Vec::new();
        while self.kind() == TokenKind::Ident && self.nth_kind(1) == TokenKind::Eq {
            let name = self.ident("a name")?;
            self.advance(); // `=`
            let value = if self.at_for_each() {
                self.each_expr()?
            } else {
                let value = self.expr()?;
                self.with_requires(value)?
            };
            if self.tokens[self.pos.saturating_sub(1)].kind != TokenKind::Dedent {
                self.end_of_line()?;
            }
            steps.push((name, value));
        }
        let tail = self.expr()?;
        if steps.is_empty() {
            return Ok(tail);
        }
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Block {
                steps,
                tail: Box::new(tail),
            },
        })
    }

    /// Is the current token a keyword from a later milestone, used as a
    /// keyword? `ask(x)` or `run.x` are ordinary names; `ask Mem(...)` is not.
    fn future_keyword(&self) -> Option<(&'static str, &'static str)> {
        let entry = FUTURE_STMTS.iter().find(|(k, _)| *k == self.word())?;
        let used_as_name = matches!(
            self.nth_kind(1),
            TokenKind::LParen
                | TokenKind::Dot
                | TokenKind::Comma
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::Eq
                | TokenKind::Newline
                | TokenKind::Eof
        );
        (!used_as_name).then_some(*entry)
    }

    // ----- expressions ---------------------------------------------------

    fn expr(&mut self) -> PResult<Expr> {
        if let Some((k, m)) = self.future_keyword() {
            return Err(self.unsupported(k, m));
        }
        if (self.is_word("ask") || self.is_word("send")) && self.nth_kind(1) == TokenKind::Ident {
            return self.message_expr();
        }
        if self.is_word("receive") && self.nth_kind(1) == TokenKind::Ident {
            return self.receive_expr();
        }
        if self.is_word("for") {
            return Err(self.error_here(
                "E0107",
                "`for each` must be assigned to a name",
                "`name = for each item in list: ...`",
            ));
        }
        if (self.is_word("reads") || self.is_word("edits")) && self.nth_kind(1) == TokenKind::Ident
        {
            let start = self.tok().span;
            let mode = self.ident("`reads` or `edits`")?;
            let target = self.ident("the resource to lend")?;
            return Ok(Expr {
                span: self.span_from(start),
                kind: ExprKind::Borrow { mode, target },
            });
        }
        self.or_expr()
    }

    /// Is the current word used as a keyword (followed by what it applies
    /// to), rather than as a name (`next.x`, `done(...)`, `f(match)`)?
    fn keyword(&self, word: &str) -> bool {
        self.is_word(word)
            && !matches!(
                self.nth_kind(1),
                TokenKind::LParen
                    | TokenKind::Dot
                    | TokenKind::Comma
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::Eq
                    | TokenKind::Newline
                    | TokenKind::Eof
            )
    }

    fn binary(&self, op: &str, left: Expr, right: Expr) -> Expr {
        let span = Span {
            start: left.span.start,
            end: right.span.end,
        };
        Expr {
            kind: ExprKind::Binary {
                op: op.to_owned(),
                left: Box::new(left),
                right: Box::new(right),
            },
            span,
        }
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut e = self.and_expr()?;
        while self.is_word("or") {
            self.advance();
            let r = self.and_expr()?;
            e = self.binary("or", e, r);
        }
        Ok(e)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut e = self.not_expr()?;
        while self.is_word("and") {
            self.advance();
            let r = self.not_expr()?;
            e = self.binary("and", e, r);
        }
        Ok(e)
    }

    fn not_expr(&mut self) -> PResult<Expr> {
        // `not` is never a name, so `not (a or b)` is the operator.
        if self.is_word("not") && (self.keyword("not") || self.nth_kind(1) == TokenKind::LParen) {
            let start = self.advance().span;
            let value = self.not_expr()?;
            return Ok(Expr {
                span: self.span_from(start),
                kind: ExprKind::Unary {
                    op: "not".into(),
                    value: Box::new(value),
                },
            });
        }
        self.cmp_expr()
    }

    fn cmp_expr(&mut self) -> PResult<Expr> {
        let e = self.add_expr()?;
        let op = match self.kind() {
            TokenKind::EqEq => "==",
            TokenKind::Ne => "!=",
            TokenKind::Lt => "<",
            TokenKind::Le => "<=",
            TokenKind::Gt => ">",
            TokenKind::Ge => ">=",
            // `x in list`, `part in text`.
            TokenKind::Ident if self.is_word("in") => "in",
            _ => return Ok(e),
        };
        self.advance();
        let r = self.add_expr()?;
        Ok(self.binary(op, e, r))
    }

    fn add_expr(&mut self) -> PResult<Expr> {
        let mut e = self.mul_expr()?;
        loop {
            let op = match self.kind() {
                TokenKind::Plus => "+",
                TokenKind::Minus => "-",
                _ => return Ok(e),
            };
            self.advance();
            let r = self.mul_expr()?;
            e = self.binary(op, e, r);
        }
    }

    fn mul_expr(&mut self) -> PResult<Expr> {
        let mut e = self.unary_expr()?;
        loop {
            let op = match self.kind() {
                TokenKind::Star => "*",
                TokenKind::Slash => "/",
                _ => return Ok(e),
            };
            self.advance();
            let r = self.unary_expr()?;
            e = self.binary(op, e, r);
        }
    }

    fn unary_expr(&mut self) -> PResult<Expr> {
        if self.kind() == TokenKind::Minus {
            let start = self.advance().span;
            let value = self.unary_expr()?;
            return Ok(Expr {
                span: self.span_from(start),
                kind: ExprKind::Unary {
                    op: "-".into(),
                    value: Box::new(value),
                },
            });
        }
        if self.keyword("if") {
            return self.if_expr();
        }
        if self.keyword("match") {
            return self.match_expr();
        }
        if self.keyword("loop") {
            return self.loop_expr();
        }
        if self.keyword("rounds") {
            return self.rounds_expr();
        }
        if self.keyword("race") {
            return self.race_expr();
        }
        if self.keyword("agent") {
            return self.agent_expr();
        }
        for (word, make) in [
            ("done", ExprKind::Done as fn(Box<Expr>) -> ExprKind),
            ("next", ExprKind::Next),
        ] {
            if self.keyword(word) {
                let start = self.advance().span;
                let value = self.expr()?;
                return Ok(Expr {
                    span: self.span_from(start),
                    kind: make(Box::new(value)),
                });
            }
        }
        if self.is_word("try") && (self.nth_kind(1) == TokenKind::Colon || self.keyword("try")) {
            let start = self.advance().span;
            let value = if self.kind() == TokenKind::Colon {
                self.body()?.0
            } else {
                self.expr()?
            };
            return Ok(Expr {
                span: self.span_from(start),
                kind: ExprKind::Try(Box::new(value)),
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
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
                TokenKind::AndAnd | TokenKind::OrOr | TokenKind::Bang => {
                    let word = match self.kind() {
                        TokenKind::AndAnd => "and",
                        TokenKind::OrOr => "or",
                        _ => "not",
                    };
                    return Err(self.error_here(
                        "E0100",
                        "syntax error",
                        &format!("the word `{word}`"),
                    ));
                }
                TokenKind::PlusPlus | TokenKind::Percent | TokenKind::DotDot => {
                    let t = self.tok();
                    let op = self.text_of(t).to_owned();
                    return Err(self.unsupported(&format!("operator {op}"), "a later milestone"));
                }
                _ => return Ok(e),
            }
        }
    }

    /// `: expr` on the same line, or `:` and an indented line. Returns the
    /// expression and whether it was an indented block (then its `Dedent`
    /// was consumed).
    fn body(&mut self) -> PResult<(Expr, bool)> {
        self.expect(TokenKind::Colon, "`:`")?;
        if !self.eat(TokenKind::Newline) {
            return Ok((self.expr()?, false));
        }
        self.expect(TokenKind::Indent, "an indented line")?;
        // `name = value` steps, in order, then what the branch gives.
        let e = self.turn_body()?;
        if self.tokens[self.pos.saturating_sub(1)].kind != TokenKind::Dedent {
            self.eat(TokenKind::Newline);
        }
        if self.kind() != TokenKind::Dedent {
            return Err(self.error_here(
                "E0108",
                "a block is its steps (`name = value`), then the value it gives",
                "the end of the block",
            ));
        }
        self.advance();
        Ok((e, true))
    }

    /// After an inline body, the line must end before what follows.
    fn after_body(&mut self, block: bool) -> PResult<()> {
        if !block {
            self.expect(TokenKind::Newline, "a line break")?;
        }
        Ok(())
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let start = self.advance().span;
        let cond = self.expr()?;
        let (then, block) = self.body()?;
        self.after_body(block)?;
        self.expect_word("else")?;
        let (els, _) = self.body()?;
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::If {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            },
        })
    }

    fn match_expr(&mut self) -> PResult<Expr> {
        let start = self.advance().span;
        let value = self.expr()?;
        self.block_start("the `case` lines")?;
        let mut cases = Vec::new();
        while !matches!(self.kind(), TokenKind::Dedent | TokenKind::Eof) {
            let case_start = self.expect_word("case")?.span;
            let name = self.ident("a variant name, or `_`")?;
            let mut binds = Vec::new();
            if self.eat(TokenKind::LParen) {
                loop {
                    if self.eat(TokenKind::RParen) {
                        break;
                    }
                    binds.push(self.ident("a field name")?);
                    if !self.eat(TokenKind::Comma) {
                        self.expect(TokenKind::RParen, "`,` or `)`")?;
                        break;
                    }
                }
            }
            let (body, block) = self.body()?;
            self.after_body(block)?;
            let variant = (name.name != "_").then_some(name);
            cases.push(Case {
                variant,
                binds,
                body,
                span: self.span_from(case_start),
            });
        }
        self.eat(TokenKind::Dedent);
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Match {
                value: Box::new(value),
                cases,
            },
        })
    }

    /// `last`, `final_answer` or `fail "reason"`.
    fn on_limit(&mut self) -> PResult<OnLimit> {
        if self.is_word("last") {
            self.advance();
            return Ok(OnLimit::Last);
        }
        if self.is_word("final_answer") {
            self.advance();
            return Ok(OnLimit::FinalAnswer);
        }
        if self.is_word("fail") {
            self.advance();
            let t = self.tok();
            if !matches!(t.kind, TokenKind::Str | TokenKind::LongStr) {
                return Err(self.error_here("E0100", "syntax error", "the reason, in quotes"));
            }
            self.advance();
            return Ok(OnLimit::Fail(self.str_lit(t)));
        }
        Err(self.error_here(
            "E0100",
            "syntax error",
            "`last`, `final_answer` or `fail \"reason\"`",
        ))
    }

    fn loop_expr(&mut self) -> PResult<Expr> {
        let start = self.advance().span;
        let var = self.ident("the name of the value the loop carries")?;
        self.expect(TokenKind::Eq, "`=` and the first value")?;
        let init = self.expr()?;
        self.expect(TokenKind::Comma, "`, max N` (a loop needs a limit)")?;
        self.expect_word("max")?;
        let t = self.expect(TokenKind::Int, "the maximum number of turns")?;
        let max = self.int_value(t);
        self.block_start("the body of the loop")?;
        let body = self.turn_body()?;
        if self.tokens[self.pos.saturating_sub(1)].kind != TokenKind::Dedent {
            self.end_of_line()?;
        }
        let mut on_limit = OnLimit::Missing;
        if self.is_word("on") {
            self.advance();
            self.expect_word("limit")?;
            self.expect(TokenKind::Colon, "`:`")?;
            on_limit = self.on_limit()?;
            self.end_of_line()?;
        }
        if self.kind() != TokenKind::Dedent {
            return Err(self.error_here(
                "E0108",
                "a loop's body is its steps and what each turn gives, then `on limit: ...`",
                "the end of the loop",
            ));
        }
        self.advance();
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Loop {
                var,
                init: Box::new(init),
                max,
                body: Box::new(body),
                on_limit,
                rounds: false,
            },
        })
    }

    /// `rounds N, carry var = init:` then a body like a loop's (decision
    /// D18). After N rounds it gives the value carried.
    fn rounds_expr(&mut self) -> PResult<Expr> {
        let start = self.advance().span; // rounds
        let t = self.expect(TokenKind::Int, "the number of rounds")?;
        let max = self.int_value(t);
        self.expect(TokenKind::Comma, "`, carry name = first value`")?;
        self.expect_word("carry")?;
        let var = self.ident("the name of the value the rounds carry")?;
        self.expect(TokenKind::Eq, "`=` and the first value")?;
        let init = self.expr()?;
        self.block_start("the body of a round")?;
        let body = self.turn_body()?;
        if self.tokens[self.pos.saturating_sub(1)].kind != TokenKind::Dedent {
            self.end_of_line()?;
        }
        if self.kind() != TokenKind::Dedent {
            return Err(self.error_here(
                "E0108",
                "a round's body is its steps, then `next value`",
                "the end of the rounds",
            ));
        }
        self.advance();
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Loop {
                var,
                init: Box::new(init),
                max,
                body: Box::new(body),
                on_limit: OnLimit::Last,
                rounds: true,
            },
        })
    }

    /// `race first [where cond]:` then `name: value` lines and `on none:`
    /// (decision D12).
    fn race_expr(&mut self) -> PResult<Expr> {
        let start = self.advance().span; // race
        self.expect_word("first")?;
        let cond = if self.is_word("where") {
            self.advance();
            Some(self.expr()?)
        } else {
            None
        };
        self.block_start("the branches of the race")?;
        let mut branches = Vec::new();
        let mut on_none = None;
        while !matches!(self.kind(), TokenKind::Dedent | TokenKind::Eof) {
            if self.is_word("on") && self.nth_kind(1) == TokenKind::Ident {
                self.advance();
                let what = self.ident("`none`")?;
                if what.name != "none" {
                    return Err(self.error_here_at(
                        what.span,
                        "E0113",
                        "a race handles only `on none`",
                        "`on none: fail \"reason\"` or `on none: value`",
                        &what.name,
                    ));
                }
                self.expect(TokenKind::Colon, "`:` and what the race gives")?;
                on_none = Some(
                    if self.is_word("fail") && self.nth_kind(1) != TokenKind::LParen {
                        self.advance();
                        let t = self.tok();
                        if !matches!(t.kind, TokenKind::Str | TokenKind::LongStr) {
                            return Err(self.error_here(
                                "E0100",
                                "syntax error",
                                "the reason, in quotes",
                            ));
                        }
                        self.advance();
                        OnNone::Fail(self.str_lit(t))
                    } else {
                        OnNone::Value(self.expr()?)
                    },
                );
                self.end_of_line()?;
                continue;
            }
            if !(self.kind() == TokenKind::Ident && self.nth_kind(1) == TokenKind::Colon) {
                return Err(self.error_here(
                    "E0113",
                    "expected a branch of the race",
                    "`name: value` or `on none: ...`",
                ));
            }
            let name = self.ident("the branch's name")?;
            self.advance(); // `:`
            let value = self.expr()?;
            self.end_of_line()?;
            branches.push((name, value));
        }
        self.eat(TokenKind::Dedent);
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Race(Box::new(RaceExpr {
                cond,
                branches,
                on_none,
            })),
        })
    }

    /// `receive Message [about value], timeout N unit:` then an indented
    /// `on timeout: value`.
    fn receive_expr(&mut self) -> PResult<Expr> {
        let start = self.advance().span; // receive
        let message = self.ident("the message type")?;
        let mut about = None;
        if self.is_word("about") {
            self.advance();
            about = Some(Box::new(self.add_expr()?));
        }
        let mut timeout = None;
        let mut on_timeout = None;
        if self.eat(TokenKind::Comma) {
            if !self.is_word("timeout") {
                return Err(self.error_here("E0112", "expected `timeout`", "`, timeout 3 days`"));
            }
            self.advance();
            timeout = Some(Box::new(self.add_expr()?));
        }
        if self.kind() == TokenKind::Colon {
            self.block_start("`on timeout: value`")?;
            if !(self.is_word("on") && self.nth_kind(1) == TokenKind::Ident) {
                return Err(self.error_here(
                    "E0112",
                    "expected `on timeout:`",
                    "`on timeout: value`",
                ));
            }
            self.advance();
            let what = self.ident("`timeout`")?;
            if what.name != "timeout" {
                return Err(self.error_here_at(
                    what.span,
                    "E0112",
                    "a `receive` handles only `on timeout`",
                    "`on timeout: value`",
                    &what.name,
                ));
            }
            self.expect(TokenKind::Colon, "`:` and the value")?;
            on_timeout = Some(Box::new(self.expr()?));
            self.end_of_line()?;
            if !self.eat(TokenKind::Dedent) {
                return Err(self.error_here(
                    "E0112",
                    "expected the end of the `receive`",
                    "nothing after `on timeout`",
                ));
            }
        }
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Receive {
                message,
                about,
                timeout,
                on_timeout,
            },
        })
    }

    /// `ask Entity(key).Handler(args)` or `send Entity(key).Handler(args)`.
    fn message_expr(&mut self) -> PResult<Expr> {
        let start = self.tok().span;
        let send = self.is_word("send");
        self.advance();
        let entity = self.ident("an entity")?;
        self.expect(TokenKind::LParen, "`(` and the entity's key")?;
        let key = self.expr()?;
        self.expect(TokenKind::RParen, "`)` after the key")?;
        self.expect(
            TokenKind::Dot,
            "`.` and the message, e.g. `Entity(key).Message(...)`",
        )?;
        let handler = self.ident("the message's name")?;
        self.expect(TokenKind::LParen, "`(` with the message's arguments")?;
        let args = self.args()?;
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Message(Box::new(MessageExpr {
                send,
                entity,
                key,
                handler,
                args,
            })),
        })
    }

    /// After a step's value: `x = tool(...):` or `x = try tool(...):`, then
    /// `requires` lines. Anything else is returned as it is.
    fn with_requires(&mut self, mut value: Expr) -> PResult<Expr> {
        if self.kind() != TokenKind::Colon {
            return Ok(value);
        }
        match value.kind {
            ExprKind::Call { .. } => value = self.requires_block(value)?,
            ExprKind::Try(inner) if matches!(inner.kind, ExprKind::Call { .. }) => {
                let guarded = self.requires_block(*inner)?;
                value = Expr {
                    span: self.span_from(value.span),
                    kind: ExprKind::Try(Box::new(guarded)),
                };
            }
            kind => value.kind = kind,
        }
        Ok(value)
    }

    /// `call:` followed by indented `requires condition` lines (D29).
    fn requires_block(&mut self, call: Expr) -> PResult<Expr> {
        let start = call.span;
        self.block_start("the call's `requires` lines")?;
        let mut requires = Vec::new();
        while !matches!(self.kind(), TokenKind::Dedent | TokenKind::Eof) {
            if !self.is_word("requires") {
                return Err(self.error_here(
                    "E0109",
                    "expected a precondition",
                    "`requires condition`",
                ));
            }
            self.advance();
            requires.push(self.expr()?);
            self.end_of_line()?;
        }
        self.eat(TokenKind::Dedent);
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Guarded {
                call: Box::new(call),
                requires,
            },
        })
    }

    fn agent_expr(&mut self) -> PResult<Expr> {
        let start = self.advance().span;
        let model = self.ident("the model the agent uses")?;
        self.block_start("the agent's properties")?;
        let mut agent = AgentExpr {
            model,
            tools: Vec::new(),
            max_turns: None,
            task: None,
            on_turn_limit: OnLimit::Missing,
            on_stuck: OnLimit::Missing,
            span: start,
        };
        while !matches!(self.kind(), TokenKind::Dedent | TokenKind::Eof) {
            let key = self.ident("an agent property")?;
            match key.name.as_str() {
                "tools" => {
                    self.expect(TokenKind::LBracket, "`[` and the tools")?;
                    loop {
                        if self.eat(TokenKind::RBracket) {
                            break;
                        }
                        let name = self.ident("a tool")?;
                        let mut lends = Vec::new();
                        if self.eat(TokenKind::LParen) {
                            loop {
                                if self.eat(TokenKind::RParen) {
                                    break;
                                }
                                lends.push(self.expr()?);
                                if !self.eat(TokenKind::Comma) {
                                    self.expect(TokenKind::RParen, "`,` or `)`")?;
                                    break;
                                }
                            }
                        }
                        agent.tools.push(AgentTool { name, lends });
                        if !self.eat(TokenKind::Comma) {
                            self.expect(TokenKind::RBracket, "`,` or `]`")?;
                            break;
                        }
                    }
                }
                "max_turns" => {
                    let t = self.expect(TokenKind::Int, "the maximum number of turns")?;
                    agent.max_turns = Some((self.int_value(t), t.span));
                }
                "task" => agent.task = Some(self.expr()?),
                "on" => {
                    let which = self.ident("`turn_limit` or `stuck`")?;
                    self.expect(TokenKind::Colon, "`:`")?;
                    let action = self.on_limit()?;
                    match which.name.as_str() {
                        "turn_limit" => agent.on_turn_limit = action,
                        "stuck" => agent.on_stuck = action,
                        _ => {
                            self.diags.push(
                                Diagnostic::error("E0108", "unknown agent event", which.span)
                                    .expected("`on turn_limit` or `on stuck`")
                                    .observed(format!("`on {}`", which.name)),
                            );
                        }
                    }
                }
                "compact" => return Err(self.unsupported("compact", "a later milestone")),
                other => {
                    let r = self.error_here_at(
                        key.span,
                        "E0108",
                        "unknown agent property",
                        "`tools`, `max_turns`, `task`, `on turn_limit` or `on stuck`",
                        other,
                    );
                    return Err(r);
                }
            }
            self.end_of_line()?;
        }
        self.eat(TokenKind::Dedent);
        agent.span = self.span_from(start);
        Ok(Expr {
            span: agent.span,
            kind: ExprKind::Agent(Box::new(agent)),
        })
    }

    fn args(&mut self) -> PResult<Vec<Arg>> {
        let mut args = Vec::new();
        loop {
            if self.eat(TokenKind::RParen) {
                break;
            }
            let name = if self.kind() == TokenKind::Ident && self.nth_kind(1) == TokenKind::Eq {
                let n = self.ident("an argument name")?;
                self.advance(); // `=`
                Some(n)
            } else {
                None
            };
            let value = self.expr()?;
            args.push(Arg { name, value });
            if !self.eat(TokenKind::Comma) {
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
                let text = self.text_of(t);
                let kind = match text {
                    "true" => ExprKind::Bool(true),
                    "false" => ExprKind::Bool(false),
                    _ => ExprKind::Ident(text.to_owned()),
                };
                Ok(Expr { kind, span: t.span })
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
                    if self.eat(TokenKind::RBracket) {
                        break;
                    }
                    items.push(self.expr()?);
                    // `[body for x in list if cond]`
                    if items.len() == 1 && self.is_word("for") {
                        self.advance();
                        let var = self.ident("the name of each item")?;
                        if !self.is_word("in") {
                            return Err(self.error_here(
                                "E0105",
                                "expected `in`",
                                "`in` and the list",
                            ));
                        }
                        self.advance();
                        let over = self.or_expr()?;
                        let cond = if self.is_word("if") {
                            self.advance();
                            Some(Box::new(self.or_expr()?))
                        } else {
                            None
                        };
                        self.expect(TokenKind::RBracket, "`]`")?;
                        let body = items.pop().expect("one item");
                        return Ok(Expr {
                            kind: ExprKind::Comprehension {
                                body: Box::new(body),
                                var,
                                over: Box::new(over),
                                cond,
                            },
                            span: self.span_from(t.span),
                        });
                    }
                    if !self.eat(TokenKind::Comma) {
                        self.expect(TokenKind::RBracket, "`,` or `]`")?;
                        break;
                    }
                }
                Ok(Expr {
                    kind: ExprKind::List(items),
                    span: self.span_from(t.span),
                })
            }
            TokenKind::LParen => {
                self.advance();
                let inner = self.expr()?;
                self.expect(TokenKind::RParen, "`)`")?;
                Ok(inner)
            }
            TokenKind::LBrace => Err(self.unsupported("`{...}` literals", "a later milestone")),
            _ => Err(self.error_here("E0102", "expected an expression", "a value or a call")),
        }
    }

    fn unit(&mut self) -> Option<String> {
        if UNITS.contains(&self.word()) {
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
# Um programa completo do subconjunto M1.
model claude = "claude-sonnet-5-5":
    max_output 2000 tokens

tool web_search(query: Text) -> Text:
    effect read
    max_output 4000 tokens
    retry_on [Timeout, RateLimit]

type Plan:
    questions: List[Text] max 5
    owner: Text

type Review = Approved | Rejected(feedback: Text)

type OrderId = Text

prompt split(topic: Text) -> Plan:
    """
    Divida {topic}.
    """

graph research(topic: Text) -> List[Text]:
    effect read
    limits threads 8, budget 2 USD, rate 50/s
    plan = claude(split(topic))
    found = for each q in plan.questions:
        web_search(q)
    label = "pesquisa: {topic}"
    return found
"#,
        );
        assert_eq!(p.decls.len(), 7);
        let Decl::Graph(g) = &p.decls[6] else {
            panic!()
        };
        assert_eq!(g.body.len(), 5);
        assert_eq!(g.max_effect.as_ref().unwrap()[0].name, "read");
        let Decl::Type(t) = &p.decls[3] else { panic!() };
        assert!(matches!(&t.ty.kind, TypeKind::Variants(v) if v.len() == 2));
        let Decl::Type(t) = &p.decls[2] else { panic!() };
        assert!(matches!(&t.ty.kind, TypeKind::Record(f) if f.len() == 2));
    }

    #[test]
    fn inline_for_each_and_keyword_arguments() {
        let p = parse_ok(
            "graph g(xs: List[Text]) -> List[Text]:\n    ys = for each x in xs: f(x, limit=2)\n    return ys\n",
        );
        let Decl::Graph(g) = &p.decls[0] else {
            panic!()
        };
        let Stmt::Node { fan_out, value, .. } = &g.body[0] else {
            panic!()
        };
        assert!(fan_out.is_some());
        let ExprKind::Call { args, .. } = &value.kind else {
            panic!()
        };
        assert_eq!(args[1].name.as_ref().unwrap().name, "limit");
    }

    #[test]
    fn calls_may_span_lines() {
        parse_ok("graph g() -> Text:\n    a = f(\n        x,\n    y=z,\n    )\n    return a\n");
    }

    #[test]
    fn future_constructs_name_their_milestone() {
        let (_, diags) = parse("graph g() -> Text:\n    respond a\n    return a\n");
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(diags[0].code, "E0101");
        assert!(diags[0].message.contains("later milestone"));
    }

    #[test]
    fn entities_and_messages() {
        let p = parse_ok(
            "entity E(key k: Text):\n    state n: Nat = 0\n    on Get() -> Nat:\n        return n\n    on Add(x: Nat):\n        next n = n + x\n\ngraph g(u: Text) -> Nat:\n    send E(u).Add(1)\n    return ask E(u).Get()\n",
        );
        let Decl::Entity(e) = &p.decls[0] else {
            panic!()
        };
        assert_eq!(e.state.len(), 1);
        assert_eq!(e.handlers[1].updates[0].0.name, "n");
        let Decl::Graph(g) = &p.decls[1] else {
            panic!()
        };
        assert!(matches!(&g.body[0], Stmt::Node { name, .. } if name.name == "send_0"));
    }

    fn graph_value(src: &str) -> Expr {
        let p = parse_ok(src);
        let Decl::Graph(g) = &p.decls[0] else {
            panic!()
        };
        match &g.body[0] {
            Stmt::Node { value, .. } => value.clone(),
            Stmt::Return(e) => e.clone(),
            Stmt::Limits(_) | Stmt::After { .. } | Stmt::Unordered(_) => panic!(),
        }
    }

    #[test]
    fn operators_follow_precedence() {
        let e = graph_value("graph g() -> Bool:\n    return a + b * c > d and not e\n");
        let ExprKind::Binary { op, left, .. } = &e.kind else {
            panic!("{e:?}")
        };
        assert_eq!(op, "and");
        let ExprKind::Binary { op, left, .. } = &left.kind else {
            panic!()
        };
        assert_eq!(op, ">");
        let ExprKind::Binary { op, right, .. } = &left.kind else {
            panic!()
        };
        assert_eq!(op, "+");
        assert!(matches!(&right.kind, ExprKind::Binary { op, .. } if op == "*"));
    }

    #[test]
    fn rounds_with_steps_and_for_each() {
        let e = graph_value(
            "graph g(q: Text) -> Text:\n    final = rounds 2, carry answers = start:\n        turn = for each r in roles:\n            m(say(r, answers))\n        count = len(turn)\n        next turn\n    return final\n",
        );
        let ExprKind::Loop {
            var,
            max,
            body,
            on_limit,
            rounds,
            ..
        } = &e.kind
        else {
            panic!("{e:?}")
        };
        assert!(*rounds);
        assert_eq!(var.name, "answers");
        assert_eq!(*max, 2);
        assert_eq!(*on_limit, OnLimit::Last);
        let ExprKind::Block { steps, tail } = &body.kind else {
            panic!("{body:?}")
        };
        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0].1.kind, ExprKind::Each { .. }));
        assert!(matches!(tail.kind, ExprKind::Next(_)));
    }

    #[test]
    fn race_with_where_branches_and_on_none() {
        let e = graph_value(
            "graph g(q: Text) -> Text:\n    best = race first where it.ok:\n        quick: m(p(q))\n        careful: deep(q)\n        on none: fail \"nada\"\n    return best\n",
        );
        let ExprKind::Race(r) = &e.kind else {
            panic!("{e:?}")
        };
        assert!(r.cond.is_some());
        let names: Vec<&str> = r.branches.iter().map(|(n, _)| n.name.as_str()).collect();
        assert_eq!(names, ["quick", "careful"]);
        assert!(matches!(r.on_none, Some(OnNone::Fail(_))));
    }

    #[test]
    fn loop_with_match_done_next_and_on_limit() {
        let e = graph_value(
            "graph g(d: Text) -> Text:\n    final = loop text = d, max 3:\n        match m(revise(text)):\n            case Approved:\n                done text\n            case Rejected(feedback): next m(rewrite(text, feedback))\n        on limit: last\n    return final\n",
        );
        let ExprKind::Loop {
            var,
            max,
            body,
            on_limit,
            ..
        } = &e.kind
        else {
            panic!("{e:?}")
        };
        assert_eq!(var.name, "text");
        assert_eq!(*max, 3);
        assert_eq!(*on_limit, OnLimit::Last);
        let ExprKind::Match { cases, .. } = &body.kind else {
            panic!()
        };
        assert_eq!(cases.len(), 2);
        assert!(matches!(cases[0].body.kind, ExprKind::Done(_)));
        assert_eq!(cases[1].binds[0].name, "feedback");
        assert!(matches!(cases[1].body.kind, ExprKind::Next(_)));
    }

    #[test]
    fn if_else_blocks_and_inline() {
        parse_ok(
            "graph g(a: Nat) -> Text:\n    x = if a > 1:\n        \"big\"\n    else:\n        \"small\"\n    return x\n",
        );
        parse_ok(
            "graph g(a: Nat) -> Text:\n    x = if a > 1: \"big\"\n    else: \"small\"\n    return x\n",
        );
    }

    #[test]
    fn agent_block_inside_for_each() {
        let p = parse_ok(
            "graph g(qs: List[Text]) -> List[Text]:\n    found = for each q in qs:\n        agent claude:\n            tools [web_search]\n            max_turns 10\n            task investigate(q)\n            on turn_limit: final_answer\n            on stuck: fail \"stuck\"\n    return found\n",
        );
        let Decl::Graph(g) = &p.decls[0] else {
            panic!()
        };
        let Stmt::Node { value, .. } = &g.body[0] else {
            panic!()
        };
        let ExprKind::Agent(a) = &value.kind else {
            panic!("{value:?}")
        };
        assert_eq!(a.tools[0].name.name, "web_search");
        assert_eq!(a.max_turns.map(|m| m.0), Some(10));
        assert_eq!(a.on_turn_limit, OnLimit::FinalAnswer);
        assert!(matches!(a.on_stuck, OnLimit::Fail(_)));
    }

    #[test]
    fn try_inline_and_keywords_as_names() {
        let e = graph_value("graph g(x: Text) -> Text:\n    r = try f(x)\n    return r\n");
        assert!(matches!(e.kind, ExprKind::Try(_)));
        // `next` followed by `(` or `.` is a name, not a keyword.
        let e = graph_value("graph g(x: Text) -> Text:\n    return next.x\n");
        assert!(matches!(e.kind, ExprKind::Field { .. }));
    }

    #[test]
    fn recovers_and_reports_every_error() {
        let (p, diags) = parse(
            "graph g() -> Text:\n    = f()\n    bogus x\n    return a\ndef h() -> Text:\n    x\ngraph k() -> Text:\n    return b\n",
        );
        let codes: Vec<_> = diags.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec!["E0104", "E0104", "E0104"]);
        assert_eq!(p.decls.len(), 2); // g and k
    }
}
