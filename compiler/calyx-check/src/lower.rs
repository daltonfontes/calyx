//! Lowering: fills the IR with what the runtime evaluates.
//!
//! Runs only on programs without errors, so it can assume every name
//! resolves and every call is well formed. Names become indices, arguments
//! follow the order of the parameters, prompt templates are split into text
//! and `{paths}`, and each prompt gets the JSON Schema of its answer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use calyx_ir::{self as ir, Effect, NodeId, Part, PromptPart};
use calyx_syntax::ast::{
    Arg, Decl, Expr, ExprKind, GraphDecl, Ident, OnLimit, Program, Stmt, StrLit, ToolDecl,
    TypeDecl, TypeExpr, TypeKind,
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
            Decl::Type(t) => {
                cx.types.insert(&t.name.name, t);
                if let TypeKind::Variants(vs) = &t.ty.kind {
                    let enum_like = vs.iter().all(|v| v.fields.is_empty());
                    for v in vs {
                        cx.variants.insert(&v.name.name, enum_like);
                    }
                }
            }
            Decl::Tool(_) | Decl::Prompt(_) | Decl::Graph(_) => {}
        }
    }
    // Tools after types: their argument schemas need them.
    for d in &program.decls {
        if let Decl::Tool(t) = d {
            cx.tools.insert(&t.name.name, (out.tools.len(), t));
            let mut ir_tool = tool(t);
            let props: Vec<String> = t
                .params
                .iter()
                .map(|p| {
                    let mut name = String::new();
                    ir_string(&mut name, &p.name.name);
                    format!("{name}:{}", cx.schema(&p.ty, 0))
                })
                .collect();
            let required: Vec<String> = t
                .params
                .iter()
                .map(|p| {
                    let mut name = String::new();
                    ir_string(&mut name, &p.name.name);
                    name
                })
                .collect();
            ir_tool.schema = format!(
                "{{\"type\":\"object\",\"properties\":{{{}}},\"required\":[{}]}}",
                props.join(","),
                required.join(",")
            );
            out.tools.push(ir_tool);
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
    /// Every variant, and whether its type has only variants without
    /// fields. Those are their name as values (`"Optimist"`); the others are
    /// records with `kind` (`{"kind": "Rejected", "feedback": ...}`).
    variants: HashMap<&'p str, bool>,
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
    /// Names bound by loops and cases, innermost last, with their slots.
    locals: RefCell<Vec<(String, usize)>>,
    /// Slots used so far by the current node.
    slots: Cell<usize>,
}

impl Scope<'_> {
    fn bind(&self, name: &str) -> usize {
        let slot = self.slots.get();
        self.slots.set(slot + 1);
        self.locals.borrow_mut().push((name.to_owned(), slot));
        slot
    }

    fn unbind(&self, n: usize) {
        let mut l = self.locals.borrow_mut();
        let keep = l.len() - n;
        l.truncate(keep);
    }
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
            locals: RefCell::new(Vec::new()),
            slots: Cell::new(0),
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
            scope.slots.set(0);
            if n.name == "return" {
                n.value = ret.map(|e| self.expr(e, &scope));
                n.nlocals = scope.slots.get();
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
            n.nlocals = scope.slots.get();
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
            ExprKind::Binary { op, left, right } => ir::Expr::Binary {
                op: op.clone(),
                left: Box::new(self.expr(left, scope)),
                right: Box::new(self.expr(right, scope)),
            },
            ExprKind::Unary { op, value } => ir::Expr::Unary {
                op: op.clone(),
                value: Box::new(self.expr(value, scope)),
            },
            ExprKind::If { cond, then, els } => ir::Expr::If {
                cond: Box::new(self.expr(cond, scope)),
                then: Box::new(self.expr(then, scope)),
                els: Box::new(self.expr(els, scope)),
            },
            ExprKind::Match { value, cases } => ir::Expr::Match {
                value: Box::new(self.expr(value, scope)),
                cases: cases
                    .iter()
                    .map(|c| {
                        // Names bind the variant's fields by position.
                        let fields = c
                            .variant
                            .as_ref()
                            .map(|v| self.variant_fields(&v.name))
                            .unwrap_or_default();
                        let binds: Vec<(String, usize)> = c
                            .binds
                            .iter()
                            .zip(fields)
                            .filter(|(b, _)| b.name != "_")
                            .map(|(b, f)| (f, scope.bind(&b.name)))
                            .collect();
                        let body = self.expr(&c.body, scope);
                        scope.unbind(binds.len());
                        ir::MatchCase {
                            variant: c.variant.as_ref().map(|v| v.name.clone()),
                            binds,
                            body,
                        }
                    })
                    .collect(),
            },
            ExprKind::Loop {
                var,
                init,
                max,
                body,
                on_limit,
            } => {
                let init = self.expr(init, scope);
                let slot = scope.bind(&var.name);
                let body = self.expr(body, scope);
                scope.unbind(1);
                ir::Expr::Loop {
                    slot,
                    init: Box::new(init),
                    max: *max,
                    body: Box::new(body),
                    on_limit: match on_limit {
                        OnLimit::Last => None,
                        OnLimit::Fail(lit) => Some(plain_text(lit)),
                        _ => Some(format!("the loop reached its limit of {max} turns")),
                    },
                }
            }
            ExprKind::Done(v) => ir::Expr::Done(Box::new(self.expr(v, scope))),
            ExprKind::Next(v) => ir::Expr::Next(Box::new(self.expr(v, scope))),
            ExprKind::Try(v) => ir::Expr::Try(Box::new(self.expr(v, scope))),
            ExprKind::Agent(a) => {
                let (prompt, args) = match a.task.as_ref().map(|t| &t.kind) {
                    Some(ExprKind::Call {
                        callee,
                        args: pargs,
                    }) => match &callee.kind {
                        ExprKind::Ident(p) => match self.prompts.get(p.as_str()) {
                            Some((i, params)) => (
                                *i,
                                self.ordered(params.iter().map(String::as_str), pargs, scope),
                            ),
                            None => (0, Vec::new()),
                        },
                        _ => (0, Vec::new()),
                    },
                    _ => (0, Vec::new()),
                };
                let action = |o: &OnLimit, what: &str| match o {
                    OnLimit::Fail(lit) => Some(plain_text(lit)),
                    OnLimit::FinalAnswer => None,
                    _ => Some(format!("the agent {what}")),
                };
                ir::Expr::Agent(Box::new(ir::Agent {
                    model: self.models.get(a.model.name.as_str()).copied().unwrap_or(0),
                    prompt,
                    args,
                    tools: a
                        .tools
                        .iter()
                        .filter_map(|t| self.tools.get(t.name.as_str()).map(|(i, _)| *i))
                        .collect(),
                    max_turns: a.max_turns.map_or(1, |m| m.0),
                    on_turn_limit: action(&a.on_turn_limit, "reached its turn limit"),
                    on_stuck: action(&a.on_stuck, "got stuck repeating the same action"),
                }))
            }
        }
    }

    /// The fields of a variant, in order. `Ok` and `Failed` are the result
    /// of `try`.
    fn variant_fields(&self, name: &str) -> Vec<String> {
        match name {
            "Ok" => return vec!["value".into()],
            "Failed" => return vec!["error".into()],
            _ => {}
        }
        for decl in self.types.values() {
            if let TypeKind::Variants(vs) = &decl.ty.kind
                && let Some(v) = vs.iter().find(|v| v.name.name == name)
            {
                return v.fields.iter().map(|f| f.name.name.clone()).collect();
            }
        }
        Vec::new()
    }

    /// `Name(field=value)`: a record, or a variant with `kind`.
    fn construct(
        &self,
        kind: Option<&str>,
        fields: &[String],
        args: &[Arg],
        scope: &Scope,
    ) -> ir::Expr {
        let values = self.ordered(fields.iter().map(String::as_str), args, scope);
        let mut out: Vec<(String, ir::Expr)> = Vec::new();
        if let Some(k) = kind {
            out.push(("kind".into(), ir::Expr::Text(k.to_owned())));
        }
        out.extend(fields.iter().cloned().zip(values));
        ir::Expr::Record(out)
    }

    fn name(&self, n: &str, scope: &Scope) -> ir::Expr {
        if let Some((_, slot)) = scope.locals.borrow().iter().rev().find(|(l, _)| l == n) {
            return ir::Expr::Local(*slot);
        }
        if scope.item == Some(n) {
            return ir::Expr::Item;
        }
        if let Some(i) = scope.params.iter().position(|p| *p == n) {
            return ir::Expr::Param(i);
        }
        if let Some(id) = scope.nodes.get(n) {
            return ir::Expr::Node(*id);
        }
        match self.variants.get(n) {
            Some(true) => return ir::Expr::Text(n.to_owned()),
            Some(false) => {
                return ir::Expr::Record(vec![("kind".into(), ir::Expr::Text(n.to_owned()))]);
            }
            None => {}
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
        if let Some(decl) = self.types.get(name.as_str())
            && let TypeKind::Record(fields) = &decl.ty.kind
        {
            let names: Vec<String> = fields.iter().map(|f| f.name.name.clone()).collect();
            return self.construct(None, &names, args, scope);
        }
        if self.variants.contains_key(name.as_str()) {
            for decl in self.types.values() {
                if let TypeKind::Variants(vs) = &decl.ty.kind
                    && let Some(v) = vs.iter().find(|v| v.name.name == *name)
                {
                    let names: Vec<String> = v.fields.iter().map(|f| f.name.name.clone()).collect();
                    return self.construct(Some(name), &names, args, scope);
                }
            }
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
            // One alternative per variant: `kind` names it, and its own
            // fields are required.
            TypeKind::Variants(vs) => {
                let alternatives: Vec<String> = vs
                    .iter()
                    .map(|v| {
                        let mut kind = String::new();
                        ir_string(&mut kind, &v.name.name);
                        let mut props = vec![format!(
                            "\"kind\":{{\"type\":\"string\",\"enum\":[{kind}]}}"
                        )];
                        let mut required = vec!["\"kind\"".to_owned()];
                        for f in &v.fields {
                            let mut name = String::new();
                            ir_string(&mut name, &f.name.name);
                            props.push(format!("{name}:{}", self.schema(&f.ty, depth + 1)));
                            required.push(name);
                        }
                        format!(
                            "{{\"type\":\"object\",\"properties\":{{{}}},\"required\":[{}]}}",
                            props.join(","),
                            required.join(",")
                        )
                    })
                    .collect();
                format!("{{\"anyOf\":[{}]}}", alternatives.join(","))
            }
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

/// The text of a literal without interpolation.
fn plain_text(lit: &StrLit) -> String {
    split_template(&lit.text)
        .into_iter()
        .map(|p| match p {
            PromptPart::Lit(s) => s,
            PromptPart::Path(path) => format!("{{{}}}", path.join(".")),
        })
        .collect()
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
    let description =
        t.props
            .iter()
            .find_map(|p| match (p.key.name.as_str(), p.value.as_slice()) {
                (
                    "description",
                    [
                        Expr {
                            kind: ExprKind::Str(lit),
                            ..
                        },
                    ],
                ) => Some(plain_text(lit)),
                _ => None,
            });
    let repeatable = t.props.iter().any(|p| p.key.name == "repeatable");
    ir::Tool {
        schema: String::new(),
        description,
        repeatable,
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
