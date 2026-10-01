//! Abstract syntax tree of a Calyx program.
//!
//! Every node keeps its [`Span`] so diagnostics can point at the source.

use crate::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Program {
    pub decls: Vec<Decl>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decl {
    Model(ModelDecl),
    Tool(ToolDecl),
    Type(TypeDecl),
    Prompt(PromptDecl),
    Graph(GraphDecl),
}

impl Decl {
    pub fn name(&self) -> &Ident {
        match self {
            Decl::Model(d) => &d.name,
            Decl::Tool(d) => &d.name,
            Decl::Type(d) => &d.name,
            Decl::Prompt(d) => &d.name,
            Decl::Graph(d) => &d.name,
        }
    }
}

/// `model claude = "id"`, optionally followed by a block with `max_output`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelDecl {
    pub name: Ident,
    pub model_id: String,
    pub max_output: Option<u64>,
    pub span: Span,
}

/// `tool name(params) -> T:` followed by a block of properties.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    pub props: Vec<ToolProp>,
    pub span: Span,
}

/// One line of a properties block, e.g. `effect write once`, `max_output 4000 tokens`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolProp {
    pub key: Ident,
    pub value: Vec<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: Ident,
    pub ty: TypeExpr,
    /// `box: reads Sandbox` or `box: edits Sandbox`: how a tool borrows a
    /// resource (decision D26).
    pub borrow: Option<Ident>,
}

/// `type Name = ...`
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl {
    pub name: Ident,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    /// `Text`, `List[Text] max 5`, `Map[Text, Nat]`
    Named {
        name: Ident,
        args: Vec<TypeExpr>,
        max: Option<u64>,
    },
    /// A block of `field: T` lines.
    Record(Vec<Field>),
    /// `A | B(field: T)`
    Variants(Vec<Variant>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: Ident,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: Ident,
    pub fields: Vec<Field>,
}

/// `prompt name(params) -> T:` followed by an indented `"""..."""` text.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    pub template: StrLit,
    pub span: Span,
}

/// `graph name(params) -> T:` followed by an indented body. The body may
/// declare `effect ...` (the maximum effect) and `decreases p` (recursion).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    pub max_effect: Option<Vec<Ident>>,
    pub decreases: Option<Ident>,
    pub body: Vec<Stmt>,
    /// Some statement failed to parse, so the body is partial. Checks that
    /// look at the body as a whole (such as "has a `return`") are skipped.
    pub incomplete: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `limits threads 8, budget 2 USD`
    Limits(Vec<(Ident, Expr)>),
    /// `x = e` (a step of the graph), or `xs = for each i in list: e`.
    Node {
        name: Ident,
        fan_out: Option<(Ident, Expr)>,
        value: Expr,
    },
    /// `return e`
    Return(Expr),
    /// `notice after paid, saved`: `notice` starts only after those steps
    /// finished, though it reads nothing from them (decision D2).
    After { node: Ident, after: Vec<Ident> },
}

/// A string literal. `text` is the raw content between the quotes, with
/// escapes still in place; `content_offset` is where it starts in the file.
#[derive(Debug, Clone, PartialEq)]
pub struct StrLit {
    pub text: String,
    pub content_offset: u32,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A value that failed to parse (already reported). Typed as an error so
    /// later uses of its name do not report again.
    Error,
    Ident(String),
    Str(StrLit),
    /// Integer with an optional unit: `8`, `2 USD`, `2_000 tokens`, `50/s`.
    Int {
        value: u64,
        unit: Option<String>,
    },
    /// Decimal with an optional unit: `0.20 USD`.
    Float {
        value: f64,
        unit: Option<String>,
    },
    List(Vec<Expr>),
    Field {
        base: Box<Expr>,
        name: Ident,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    /// `a + b`, `a == b`, `a and b`. `op` is the operator's text.
    Binary {
        op: String,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `not a`, `-a`.
    Unary {
        op: String,
        value: Box<Expr>,
    },
    /// `if cond:` then-branch, `else:` else-branch.
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    /// `match value:` followed by `case` lines.
    Match {
        value: Box<Expr>,
        cases: Vec<Case>,
    },
    /// `loop var = init, max N:` body, optional `on limit: ...` (decision D5).
    Loop {
        var: Ident,
        init: Box<Expr>,
        max: u64,
        body: Box<Expr>,
        on_limit: OnLimit,
    },
    /// `done value`: ends a loop with this value.
    Done(Box<Expr>),
    /// `next value`: the next turn of a loop, carrying this value.
    Next(Box<Expr>),
    /// `try value`: `Ok(value)` or `Failed(error)` (decision D11).
    Try(Box<Expr>),
    /// `agent model:` with a block of properties (decision D5).
    Agent(Box<AgentExpr>),
    /// A tool call with preconditions, checked by the tool against the
    /// current state when it acts (decision D29):
    /// `refund(order, amount):` then `requires state.total >= amount` lines.
    Guarded {
        call: Box<Expr>,
        requires: Vec<Expr>,
    },
    /// `reads repo` or `edits repo`: lends a resource to a tool for one
    /// call (decision D26). `mode` is `reads` or `edits`.
    Borrow {
        mode: Ident,
        target: Ident,
    },
}

/// `case Variant(field, ...):` or `case _:`, and what it evaluates to.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    /// `None` for `_`.
    pub variant: Option<Ident>,
    /// Fields of the variant bound to names of the same name.
    pub binds: Vec<Ident>,
    pub body: Expr,
    pub span: Span,
}

/// What a loop or an agent does when it reaches its limit.
#[derive(Debug, Clone, PartialEq)]
pub enum OnLimit {
    /// `last`: the loop ends with the last value it carried.
    Last,
    /// `final_answer`: the agent is asked for its answer one last time.
    FinalAnswer,
    /// `fail "reason"`: the run fails (or `try` gets `Failed`).
    Fail(StrLit),
    /// Not written. Loops then fail; agents must say (checked later).
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentExpr {
    pub model: Ident,
    /// `tools [a, b(reads repo)]`: each tool, with the resources lent to it.
    pub tools: Vec<AgentTool>,
    /// `max_turns N`
    pub max_turns: Option<(u64, Span)>,
    /// `task prompt(...)`
    pub task: Option<Expr>,
    pub on_turn_limit: OnLimit,
    pub on_stuck: OnLimit,
    pub span: Span,
}

/// A tool an agent may call, with resources lent to it for every call:
/// `edit_file(edits repo)`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentTool {
    pub name: Ident,
    pub lends: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: Expr,
}
