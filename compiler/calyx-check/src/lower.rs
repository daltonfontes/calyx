//! Lowering: fills the IR with what the runtime evaluates.
//!
//! Runs only on programs without errors, so it can assume every name
//! resolves and every call is well formed. Names become indices, arguments
//! follow the order of the parameters, prompt templates are split into text
//! and `{paths}`, and each prompt gets the JSON Schema of its answer.

use std::collections::HashMap;

use calyx_ir::{self as ir, Effect, NodeId, Part, PromptPart};
use calyx_syntax::ast::{
    Arg, Decl, Expr, ExprKind, GraphDecl, Ident, Program, Stmt, StrLit, ToolDecl, TypeDecl,
    TypeExpr, TypeKind,
};

pub fn lower(program: &Program, out: &mut ir::Program) {
    let mut cx = Lower {
        models: HashMap::new(),
        tools: HashMap::new(),
        prompts: HashMap::new(),
        types: HashMap::new(),
        variants: HashMap::new(),
        graphs: out
            .graphs
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let params = g.params.iter().map(|(n, _)| n.clone()).collect();
                (g.name.clone(), (i, params))
            })
            .collect(),
    };
    for d in &program.decls {
        match d {
            Decl::Model(m) => {
                cx.models.insert(&m.name.name, out.models.len());
                out.models.push(ir::Model {
                    name: m.name.name.clone(),
                    id: m.model_id.clone(),
                    max_output: m.max_output,
                });
            }
            Decl::Tool(t) => {
                cx.tools.insert(&t.name.name, (out.tools.len(), t));
                out.tools.push(tool(t));
            }
            Decl::Type(t) => {
                cx.types.insert(&t.name.name, t);
                if let TypeKind::Variants(vs) = &t.ty.kind {
                    for v in vs.iter().filter(|v| v.fields.is_empty()) {
                        cx.variants.insert(&v.name.name, ());
                    }
                }
            }
            Decl::Prompt(_) | Decl::Graph(_) => {}
        }
    }
    for d in &program.decls {
        if let Decl::Prompt(p) = d {
            let params: Vec<String> = p.params.iter().map(|x| x.name.name.clone()).collect();
            cx.prompts
                .insert(&p.name.name, (out.prompts.len(), params.clone()));
            let (schema, wrapped) = if is_text(&p.ret) {
                (None, false)
            } else {
                let s = cx.schema(&p.ret, 0);
                if s.starts_with("{\"type\":\"object\"") {
                    (Some(s), false)
                } else {
                    (
                        Some(format!(
                            "{{\"type\":\"object\",\"properties\":{{\"value\":{s}}},\"required\":[\"value\"]}}"
                        )),
                        true,
                    )
                }
            };
            out.prompts.push(ir::Prompt {
                name: p.name.name.clone(),
                params,
                parts: prompt_parts(&p.template),
                schema,
                wrapped,
            });
        }
    }
    let decls: HashMap<&str, &GraphDecl> = program
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Graph(g) => Some((g.name.name.as_str(), g)),
            _ => None,
        })
        .collect();
    for g in &mut out.graphs {
        if let Some(decl) = decls.get(g.name.as_str()) {
            cx.graph(decl, g);
        }
    }
}

struct Lower<'p> {
    models: HashMap<&'p str, usize>,
    tools: HashMap<&'p str, (usize, &'p ToolDecl)>,
    prompts: HashMap<&'p str, (usize, Vec<String>)>,
    types: HashMap<&'p str, &'p TypeDecl>,
    /// Variants without fields; as values they are their name.
    variants: HashMap<&'p str, ()>,
    /// Index in the IR and parameter names.
    graphs: HashMap<String, (usize, Vec<String>)>,
}

/// `for each var in list` of a statement, if it has one.
type FanOut = Option<(Ident, Expr)>;

/// Names visible inside a graph.
struct Scope<'a> {
    params: Vec<&'a str>,
    nodes: HashMap<String, NodeId>,
    item: Option<&'a str>,
}

impl Lower<'_> {
    fn graph(&self, decl: &GraphDecl, g: &mut ir::Graph) {
        let mut scope = Scope {
            params: decl.params.iter().map(|p| p.name.name.as_str()).collect(),
            nodes: g
                .nodes
                .iter()
                .filter(|n| n.name != "return")
                .map(|n| (n.name.clone(), n.id))
                .collect(),
            item: None,
        };
        let mut stmts: HashMap<&str, (&FanOut, &Expr)> = HashMap::new();
        let mut ret = None;
        for s in &decl.body {
            match s {
                Stmt::Node {
                    name,
                    fan_out,
                    value,
                } => {
                    stmts.insert(&name.name, (fan_out, value));
                }
                Stmt::Return(e) => ret = Some(e),
                Stmt::Limits(entries) => g.limits = limits(entries),
            }
        }
        for n in &mut g.nodes {
            if n.name == "return" {
                n.value = ret.map(|e| self.expr(e, &scope));
                continue;
            }
            let Some((fan_out, value)) = stmts.get(n.name.as_str()) else {
                continue;
            };
            match fan_out {
                Some((var, over)) => {
                    n.over = Some(self.expr(over, &scope));
                    scope.item = Some(var.name.as_str());
                    n.value = Some(self.expr(value, &scope));
                    scope.item = None;
                }
                None => n.value = Some(self.expr(value, &scope)),
            }
        }
        g.rank_nodes();
    }

    fn expr(&self, e: &Expr, scope: &Scope) -> ir::Expr {
        match &e.kind {
            ExprKind::Ident(n) => self.name(n, scope),
            ExprKind::Str(lit) => self.text(lit, scope),
            ExprKind::Int { value, .. } => ir::Expr::Int(*value),
            ExprKind::Float { value, .. } => ir::Expr::Float(*value),
            ExprKind::List(items) => {
                ir::Expr::List(items.iter().map(|i| self.expr(i, scope)).collect())
            }
            ExprKind::Field { base, name } => {
                ir::Expr::Field(Box::new(self.expr(base, scope)), name.name.clone())
            }
            ExprKind::Call { callee, args } => self.call(callee, args, scope),
            // Only present in programs with errors, which are never lowered.
            ExprKind::Error => ir::Expr::Text(String::new()),
        }
    }

    fn name(&self, n: &str, scope: &Scope) -> ir::Expr {
        if scope.item == Some(n) {
            return ir::Expr::Item;
        }
        if let Some(i) = scope.params.iter().position(|p| *p == n) {
            return ir::Expr::Param(i);
        }
        if let Some(id) = scope.nodes.get(n) {
            return ir::Expr::Node(*id);
        }
        if self.variants.contains_key(n) {
            return ir::Expr::Text(n.to_owned());
        }
        ir::Expr::Text(String::new())
    }

    fn call(&self, callee: &Expr, args: &[Arg], scope: &Scope) -> ir::Expr {
        let ExprKind::Ident(name) = &callee.kind else {
            return ir::Expr::Text(String::new());
        };
        if let Some(&model) = self.models.get(name.as_str()) {
            // The checker guarantees one positional argument: a prompt call.
            let pc = args.iter().find(|a| a.name.is_none()).map(|a| &a.value);
            if let Some(Expr {
                kind:
                    ExprKind::Call {
                        callee: pcallee,
                        args: pargs,
                    },
                ..
            }) = pc
                && let ExprKind::Ident(pname) = &pcallee.kind
                && let Some((prompt, params)) = self.prompts.get(pname.as_str())
            {
                return ir::Expr::Model {
                    model,
                    prompt: *prompt,
                    args: self.ordered(params.iter().map(String::as_str), pargs, scope),
                };
            }
            return ir::Expr::Text(String::new());
        }
        if let Some((tool, decl)) = self.tools.get(name.as_str()) {
            let params = decl.params.iter().map(|p| p.name.name.as_str());
            return ir::Expr::Tool {
                tool: *tool,
                args: self.ordered(params, args, scope),
            };
        }
        if let Some((graph, params)) = self.graphs.get(name.as_str()) {
            return ir::Expr::Graph {
                graph: *graph,
                args: self.ordered(params.iter().map(String::as_str), args, scope),
            };
        }
        ir::Expr::Text(String::new())
    }

    /// Arguments in parameter order: positional first, then by name.
    fn ordered<'a>(
        &self,
        params: impl Iterator<Item = &'a str>,
        args: &[Arg],
        scope: &Scope,
    ) -> Vec<ir::Expr> {
        let params: Vec<&str> = params.collect();
        let mut slots: Vec<Option<ir::Expr>> = vec![None; params.len()];
        let mut next = 0;
        for a in args {
            let slot = match &a.name {
                None => {
                    next += 1;
                    next - 1
                }
                Some(n) => match params.iter().position(|p| *p == n.name) {
                    Some(s) => s,
                    None => continue,
                },
            };
            if slot < slots.len() {
                slots[slot] = Some(self.expr(&a.value, scope));
            }
        }
        slots
            .into_iter()
            .map(|s| s.unwrap_or_else(|| ir::Expr::Text(String::new())))
            .collect()
    }

    /// A text literal: plain, or with `{...}` resolved in the graph's scope.
    fn text(&self, lit: &StrLit, scope: &Scope) -> ir::Expr {
        let parts = split_template(&lit.text);
        if parts.iter().all(|p| matches!(p, PromptPart::Lit(_))) {
            let text = parts
                .into_iter()
                .map(|p| match p {
                    PromptPart::Lit(s) => s,
                    PromptPart::Path(_) => String::new(),
                })
                .collect();
            return ir::Expr::Text(text);
        }
        ir::Expr::Interp(
            parts
                .into_iter()
                .map(|p| match p {
                    PromptPart::Lit(s) => Part::Lit(s),
                    PromptPart::Path(path) => {
                        let mut it = path.into_iter();
                        let root = it.next().unwrap_or_default();
                        let mut e = self.name(&root, scope);
                        for field in it {
                            e = ir::Expr::Field(Box::new(e), field);
                        }
                        Part::Expr(e)
                    }
                })
                .collect(),
        )
    }

    /// JSON Schema of a type, as accepted by OpenAI-compatible APIs.
    fn schema(&self, t: &TypeExpr, depth: usize) -> String {
        let TypeKind::Named { name, args, max } = &t.kind else {
            return "{\"type\":\"object\"}".into();
        };
        match (name.name.as_str(), args.as_slice()) {
            ("Text" | "Date" | "Duration", _) => "{\"type\":\"string\"}".into(),
            ("Nat", _) => "{\"type\":\"integer\",\"minimum\":0}".into(),
            ("Int", _) => "{\"type\":\"integer\"}".into(),
            ("Float" | "Money", _) => "{\"type\":\"number\"}".into(),
            ("Bool", _) => "{\"type\":\"boolean\"}".into(),
            ("List", [item]) => {
                let mut s = format!(
                    "{{\"type\":\"array\",\"items\":{}",
                    self.schema(item, depth + 1)
                );
                if let Some(m) = max {
                    s.push_str(&format!(",\"maxItems\":{m}"));
                }
                s.push('}');
                s
            }
            ("Map", [_, v]) => format!(
                "{{\"type\":\"object\",\"additionalProperties\":{}}}",
                self.schema(v, depth + 1)
            ),
            (user, _) => match self.types.get(user) {
                // Recursive types are cut off rather than expanded forever.
                Some(decl) if depth < 16 => self.user_schema(decl, depth),
                _ => "{\"type\":\"object\"}".into(),
            },
        }
    }

    fn user_schema(&self, decl: &TypeDecl, depth: usize) -> String {
        match &decl.ty.kind {
            TypeKind::Named { .. } => self.schema(&decl.ty, depth + 1),
            TypeKind::Record(fields) => {
                let props: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let mut name = String::new();
                        ir_string(&mut name, &f.name.name);
                        format!("{name}:{}", self.schema(&f.ty, depth + 1))
                    })
                    .collect();
                let required: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let mut name = String::new();
                        ir_string(&mut name, &f.name.name);
                        name
                    })
                    .collect();
                format!(
                    "{{\"type\":\"object\",\"properties\":{{{}}},\"required\":[{}]}}",
                    props.join(","),
                    required.join(",")
                )
            }
            TypeKind::Variants(vs) if vs.iter().all(|v| v.fields.is_empty()) => {
                let names: Vec<String> = vs
                    .iter()
                    .map(|v| {
                        let mut name = String::new();
                        ir_string(&mut name, &v.name.name);
                        name
                    })
                    .collect();
                format!("{{\"type\":\"string\",\"enum\":[{}]}}", names.join(","))
            }
            // Variants with fields arrive with `match` (M5).
            TypeKind::Variants(_) => "{\"type\":\"object\"}".into(),
        }
    }
}

fn ir_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `limits threads 8, rate 50/s, budget 2 USD` (already checked).
fn limits(entries: &[(Ident, Expr)]) -> ir::Limits {
    let mut l = ir::Limits::default();
    for (key, value) in entries {
        match (key.name.as_str(), &value.kind) {
            ("threads", ExprKind::Int { value, .. }) => l.threads = Some(*value),
            ("rate", ExprKind::Int { value, unit }) => {
                // `50/s`, `600/min`: stored per second, rounded up.
                let per = match unit.as_deref() {
                    Some("/min") => 60,
                    Some("/h") => 3600,
                    _ => 1,
                };
                l.rate_per_s = Some(value.div_ceil(per).max(1));
            }
            ("budget", ExprKind::Int { value, unit }) => {
                l.budget = Some((*value as f64, unit.clone().unwrap_or_default()));
            }
            ("budget", ExprKind::Float { value, unit }) => {
                l.budget = Some((*value, unit.clone().unwrap_or_default()));
            }
            _ => {}
        }
    }
    l
}

fn is_text(t: &TypeExpr) -> bool {
    matches!(&t.kind, TypeKind::Named { name, args, .. } if name.name == "Text" && args.is_empty())
}

fn tool(t: &ToolDecl) -> ir::Tool {
    let mut effect = Effect::Read;
    let mut max_output = None;
    let mut timeout_ms = None;
    let mut retry_on = Vec::new();
    for p in &t.props {
        match p.key.name.as_str() {
            "effect" => {
                let words: Vec<&str> = p
                    .value
                    .iter()
                    .filter_map(|e| match &e.kind {
                        ExprKind::Ident(n) => Some(n.as_str()),
                        _ => None,
                    })
                    .collect();
                effect = match words.as_slice() {
                    ["write", "once"] => Effect::WriteOnce,
                    ["write"] => Effect::Write,
                    ["sandbox"] => Effect::Sandbox,
                    _ => Effect::Read,
                };
            }
            "max_output" => {
                if let [
                    Expr {
                        kind: ExprKind::Int { value, .. },
                        ..
                    },
                ] = p.value.as_slice()
                {
                    max_output = Some(*value);
                }
            }
            "timeout" => {
                if let [
                    Expr {
                        kind: ExprKind::Int { value, unit },
                        ..
                    },
                ] = p.value.as_slice()
                {
                    let ms = match unit.as_deref() {
                        Some("ms") => 1,
                        Some("min") => 60_000,
                        Some("h") => 3_600_000,
                        _ => 1_000,
                    };
                    timeout_ms = Some(value.saturating_mul(ms));
                }
            }
            "retry_on" => {
                if let [
                    Expr {
                        kind: ExprKind::List(items),
                        ..
                    },
                ] = p.value.as_slice()
                {
                    retry_on = items
                        .iter()
                        .filter_map(|e| match &e.kind {
                            ExprKind::Ident(n) => Some(n.clone()),
                            _ => None,
                        })
                        .collect();
                }
            }
            _ => {}
        }
    }
    // Default timeouts per attempt (decision D22).
    let default = match effect {
        Effect::Read => 30_000,
        Effect::Sandbox => 600_000,
        Effect::Pure | Effect::Llm => 300_000,
        Effect::Write | Effect::WriteOnce => 60_000,
    };
    ir::Tool {
        name: t.name.name.clone(),
        params: t.params.iter().map(|p| p.name.name.clone()).collect(),
        effect,
        max_output,
        timeout_ms: timeout_ms.unwrap_or(default),
        retry_on,
        returns_text: is_text(&t.ret),
    }
}

/// A prompt's text without the indentation of the source, split into
/// literal text and `{paths}`.
fn prompt_parts(lit: &StrLit) -> Vec<PromptPart> {
    split_template(&dedent(&lit.text))
}

/// Removes the common indentation and the blank first and last lines that
/// `"""` blocks have in the source.
pub(crate) fn dedent(text: &str) -> String {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    if lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                ""
            } else {
                &l[indent.min(l.len())..]
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Splits raw literal text (escapes still in it) into text and `{paths}`.
pub(crate) fn split_template(raw: &str) -> Vec<PromptPart> {
    let mut parts = Vec::new();
    let mut lit = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => lit.push('\n'),
                Some('t') => lit.push('\t'),
                Some('r') => lit.push('\r'),
                Some(other) => lit.push(other),
                None => {}
            },
            '{' => {
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                    inner.push(c);
                }
                if !lit.is_empty() {
                    parts.push(PromptPart::Lit(std::mem::take(&mut lit)));
                }
                parts.push(PromptPart::Path(
                    inner.split('.').map(|s| s.trim().to_owned()).collect(),
                ));
            }
            c => lit.push(c),
        }
    }
    if !lit.is_empty() {
        parts.push(PromptPart::Lit(lit));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedents_prompt_blocks() {
        let text = "\n    Linha um.\n\n      recuo extra\n    Tema: {topic}\n    ";
        assert_eq!(dedent(text), "Linha um.\n\n  recuo extra\nTema: {topic}");
    }

    #[test]
    fn splits_templates() {
        let parts = split_template(r"a {x.y} b \{c\} \n");
        assert_eq!(
            parts,
            vec![
                PromptPart::Lit("a ".into()),
                PromptPart::Path(vec!["x".into(), "y".into()]),
                PromptPart::Lit(" b {c} \n".into()),
            ]
        );
    }
}
