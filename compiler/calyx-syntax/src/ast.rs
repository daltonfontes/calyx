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

/// `model claude = llm("id", max_output: 2_000 tokens)`
#[derive(Debug, Clone, PartialEq)]
pub struct ModelDecl {
    pub name: Ident,
    pub model_id: String,
    pub max_output: Option<u64>,
    pub span: Span,
}

/// `tool name(params) -> T { props }`
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    pub props: Vec<ToolProp>,
    pub span: Span,
}

/// One line inside a tool block, e.g. `effect write once`, `max_output 4_000 tokens`.
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
    /// `Text`, `List<Text> max 5`, `Map<Text, Nat>`
    Named {
        name: Ident,
        args: Vec<TypeExpr>,
        max: Option<u64>,
    },
    /// `{ field: T, ... }`
    Record(Vec<Field>),
    /// `| A | B { field: T }`
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

/// `prompt name(params) -> T { """...""" }`
#[derive(Debug, Clone, PartialEq)]
pub struct PromptDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    pub template: StrLit,
    pub span: Span,
}

/// `graph name(params) -> T [effect E] [decreases p] { body }`
#[derive(Debug, Clone, PartialEq)]
pub struct GraphDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    pub max_effect: Option<Vec<Ident>>,
    pub decreases: Option<Ident>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `limits { threads: 8, budget: 2 USD }`
    Limits(Vec<(Ident, Expr)>),
    /// `node x = e` or `node xs[i in list] = e`
    Node {
        name: Ident,
        fan_out: Option<(Ident, Expr)>,
        value: Expr,
    },
    /// `let x = e`
    Let { name: Ident, value: Expr },
    /// `return e`
    Return(Expr),
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: Expr,
}
