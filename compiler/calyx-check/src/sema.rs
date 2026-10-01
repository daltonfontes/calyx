//! Semantic analysis of the M1 subset: names, types, prompt variables,
//! effects, graph structure, and lowering to the IR.
//!
//! Every pass is linear in the size of the program (decision D10): each
//! declaration and each node is visited a constant number of times.

use std::collections::{HashMap, HashSet};

use calyx_ir::{self as ir, Effect, NodeId, NodeKind};
use calyx_syntax::ast::*;
use calyx_syntax::{Diagnostic, Severity, Span};

use crate::types::{Ty, assignable};

/// Checks a parsed program. Appends diagnostics; returns the IR.
pub fn check_program(program: &Program, diags: &mut Vec<Diagnostic>) -> ir::Program {
    let mut cx = Cx {
        diags,
        models: HashMap::new(),
        tools: HashMap::new(),
        prompts: HashMap::new(),
        graphs: HashMap::new(),
        types: HashMap::new(),
        unit_variants: HashMap::new(),
        graph_effects: HashMap::new(),
    };
    cx.collect(program);
    cx.resolve_signatures(program);
    let order = cx.graph_order(program);
    let mut graphs = Vec::new();
    for g in order {
        graphs.push(cx.check_graph(g));
    }
    // Keep the IR in source order.
    let position: HashMap<&str, usize> = program
        .decls
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name().name.as_str(), i))
        .collect();
    graphs.sort_by_key(|g: &ir::Graph| position.get(g.name.as_str()).copied().unwrap_or(0));
    ir::Program { graphs }
}

// ----- declarations ---------------------------------------------------------

struct ToolSig {
    params: Vec<(String, Ty)>,
    ret: Ty,
    effect: Effect,
}

struct PromptSig {
    params: Vec<(String, Ty)>,
    ret: Ty,
}

struct GraphSig {
    params: Vec<(String, Ty)>,
    ret: Ty,
}

enum UserType {
    Record(Vec<(String, Ty)>),
    Variants,
    Alias(Ty),
}

struct Cx<'a, 'p> {
    diags: &'a mut Vec<Diagnostic>,
    models: HashMap<&'p str, &'p ModelDecl>,
    tools: HashMap<&'p str, ToolSig>,
    prompts: HashMap<&'p str, PromptSig>,
    graphs: HashMap<&'p str, (&'p GraphDecl, GraphSig)>,
    types: HashMap<&'p str, UserType>,
    /// Variants without fields, usable as values: `Optimist` is a `Role`.
    unit_variants: HashMap<&'p str, &'p str>,
    graph_effects: HashMap<String, Effect>,
}

fn err(code: &'static str, msg: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::error(code, msg, span)
}

fn warn(code: &'static str, msg: impl Into<String>, span: Span) -> Diagnostic {
    let mut d = Diagnostic::error(code, msg, span);
    d.severity = Severity::Warning;
    d
}

impl<'p> Cx<'_, 'p> {
    fn push(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    /// Pass 1: one namespace for all declarations.
    fn collect(&mut self, program: &'p Program) {
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for d in &program.decls {
            let name = d.name();
            if Ty::builtin(&name.name).is_some() || name.name == "List" || name.name == "Map" {
                self.push(
                    err("E0201", "name is reserved for a built-in type", name.span)
                        .observed(format!("`{}`", name.name)),
                );
                continue;
            }
            if seen.insert(&name.name, name.span).is_some() {
                self.push(
                    err("E0201", "name already declared", name.span)
                        .expected("a unique name")
                        .observed(format!("`{}`", name.name)),
                );
                continue;
            }
            match d {
                Decl::Model(m) => {
                    self.models.insert(&m.name.name, m);
                }
                Decl::Type(t) => {
                    // Placeholder so types can refer to each other by name.
                    self.types.insert(&t.name.name, UserType::Variants);
                }
                _ => {}
            }
        }
    }

    /// Pass 2: resolve every signature and check tools and prompts.
    fn resolve_signatures(&mut self, program: &'p Program) {
        for d in &program.decls {
            if let Decl::Type(t) = d {
                if !self.types.contains_key(t.name.name.as_str()) {
                    continue;
                }
                let ut = match &t.ty.kind {
                    TypeKind::Record(fields) => UserType::Record(self.fields(fields)),
                    TypeKind::Variants(vs) => {
                        for v in vs {
                            self.fields(&v.fields);
                            if v.fields.is_empty() {
                                self.unit_variants.insert(&v.name.name, &t.name.name);
                            }
                        }
                        UserType::Variants
                    }
                    TypeKind::Named { .. } => UserType::Alias(self.ty(&t.ty)),
                };
                self.types.insert(&t.name.name, ut);
            }
        }
        for d in &program.decls {
            match d {
                Decl::Tool(t) => {
                    let sig = self.tool(t);
                    self.tools.insert(&t.name.name, sig);
                }
                Decl::Prompt(p) => {
                    let params = self.params(&p.params);
                    let ret = self.ty(&p.ret);
                    let names: HashMap<String, Ty> = params.iter().cloned().collect();
                    self.check_interpolations(&p.template, &|n| names.get(n).cloned());
                    self.prompts.insert(&p.name.name, PromptSig { params, ret });
                }
                Decl::Graph(g) => {
                    let params = self.params(&g.params);
                    let ret = self.ty(&g.ret);
                    self.graphs
                        .insert(&g.name.name, (g, GraphSig { params, ret }));
                }
                Decl::Model(_) | Decl::Type(_) => {}
            }
        }
    }

    fn fields(&mut self, fields: &[Field]) -> Vec<(String, Ty)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for f in fields {
            if !seen.insert(f.name.name.as_str()) {
                self.push(
                    err("E0201", "field declared twice", f.name.span)
                        .observed(format!("`{}`", f.name.name)),
                );
            }
            out.push((f.name.name.clone(), self.ty(&f.ty)));
        }
        out
    }

    fn params(&mut self, params: &[Param]) -> Vec<(String, Ty)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for p in params {
            if !seen.insert(p.name.name.as_str()) {
                self.push(
                    err("E0501", "parameter declared twice", p.name.span)
                        .observed(format!("`{}`", p.name.name)),
                );
            }
            out.push((p.name.name.clone(), self.ty(&p.ty)));
        }
        out
    }

    fn ty(&mut self, t: &TypeExpr) -> Ty {
        match &t.kind {
            TypeKind::Named { name, args, max } => {
                let n = name.name.as_str();
                let ty = if let Some(b) = Ty::builtin(n) {
                    self.arity(t, n, args, 0);
                    b
                } else if n == "List" {
                    if self.arity(t, n, args, 1) {
                        Ty::List(Box::new(self.ty(&args[0])), *max)
                    } else {
                        Ty::Error
                    }
                } else if n == "Map" {
                    if self.arity(t, n, args, 2) {
                        Ty::Map(Box::new(self.ty(&args[0])), Box::new(self.ty(&args[1])))
                    } else {
                        Ty::Error
                    }
                } else if self.types.contains_key(n) {
                    self.arity(t, n, args, 0);
                    match self.types.get(n) {
                        Some(UserType::Alias(a)) => a.clone(),
                        _ => Ty::User(n.to_owned()),
                    }
                } else {
                    self.push(
                        err("E0202", "unknown type", name.span)
                            .expected("a built-in type or one declared with `type`")
                            .observed(format!("`{n}`")),
                    );
                    return Ty::Error;
                };
                if max.is_some() && !matches!(ty, Ty::List(..)) {
                    self.push(
                        err("E0204", "`max` only applies to lists", t.span)
                            .observed(format!("`{n}`")),
                    );
                }
                ty
            }
            TypeKind::Record(_) | TypeKind::Variants(_) => {
                self.push(
                    err(
                        "E0203",
                        "inline record and variant types must be declared with `type`",
                        t.span,
                    )
                    .expected("a named type"),
                );
                Ty::Error
            }
        }
    }

    fn arity(&mut self, t: &TypeExpr, name: &str, args: &[TypeExpr], want: usize) -> bool {
        if args.len() == want {
            return true;
        }
        self.push(
            err("E0203", "wrong number of type arguments", t.span)
                .expected(format!("`{name}` with {want} type argument(s)"))
                .observed(format!("{}", args.len())),
        );
        false
    }

    // ----- tools ----------------------------------------------------------

    fn tool(&mut self, t: &ToolDecl) -> ToolSig {
        let params = self.params(&t.params);
        let ret = self.ty(&t.ret);
        let mut effect = None;
        let mut on_uncertain = false;
        let mut seen = HashSet::new();
        for p in &t.props {
            let key = p.key.name.as_str();
            if !seen.insert(key) {
                self.push(
                    err("E0301", "tool property declared twice", p.key.span)
                        .observed(format!("`{key}`")),
                );
            }
            match key {
                "effect" => effect = self.effect_words(&p.value, p.span),
                "max_output" => self.prop_int(p, &["tokens"], "a number of tokens, e.g. `4000 tokens`"),
                "timeout" => self.prop_int(
                    p,
                    &["ms", "s", "min", "h", "days"],
                    "a duration, e.g. `30 s` or `10 min`",
                ),
                "retry_on" => {
                    if !matches!(p.value.as_slice(), [Expr { kind: ExprKind::List(_), .. }]) {
                        self.bad_prop(p, "a list of error names, e.g. `[Timeout, RateLimit]`");
                    }
                }
                "idempotency_key" => match p.value.as_slice() {
                    [Expr { kind: ExprKind::Ident(n), span }] => {
                        if !params.iter().any(|(pn, _)| pn == n) {
                            self.push(
                                err("E0306", "idempotency key is not a parameter of the tool", *span)
                                    .expected("one of the tool's parameters")
                                    .observed(format!("`{n}`")),
                            );
                        }
                    }
                    _ => self.bad_prop(p, "a parameter name"),
                },
                "on_uncertain" => {
                    on_uncertain = true;
                    let ok = match p.value.as_slice() {
                        [Expr { kind: ExprKind::Ident(n), .. }] => {
                            n == "pause" || n == "accept_loss"
                        }
                        [Expr { kind: ExprKind::Call { callee, .. }, .. }] => {
                            matches!(&callee.kind, ExprKind::Ident(n) if n == "verify")
                        }
                        _ => false,
                    };
                    if !ok {
                        self.bad_prop(p, "`verify(...)`, `pause` or `accept_loss`");
                    }
                }
                "checks" => {
                    if !matches!(p.value.as_slice(), [Expr { kind: ExprKind::Ident(_), .. }]) {
                        self.bad_prop(p, "a type name");
                    }
                }
                "repeatable" => {
                    if !p.value.is_empty() {
                        self.bad_prop(p, "no value");
                    }
                }
                _ => self.push(
                    err("E0301", "unknown tool property", p.key.span)
                        .expected("`effect`, `max_output`, `timeout`, `retry_on`, `idempotency_key`, `on_uncertain`, `checks` or `repeatable`")
                        .observed(format!("`{key}`")),
                ),
            }
        }
        let parsed = effect;
        let effect = match parsed {
            Some(e) => e,
            None => {
                if !seen.contains("effect") {
                    self.push(
                        err("E0302", "tool has no effect", t.name.span)
                            .expected("`effect read`, `effect write`, `effect write once` or `effect sandbox`")
                            .observed("no `effect` property"),
                    );
                }
                Effect::WriteOnce // most conservative
            }
        };
        if parsed == Some(Effect::WriteOnce) && !on_uncertain {
            self.push(
                err("E0304", "`write once` tool needs a policy for uncertain outcomes", t.name.span)
                    .expected("`on_uncertain verify(...)`, `on_uncertain pause` or `on_uncertain accept_loss`")
                    .observed("no `on_uncertain` property"),
            );
        }
        ToolSig {
            params,
            ret,
            effect,
        }
    }

    fn effect_words(&mut self, value: &[Expr], span: Span) -> Option<Effect> {
        let words: Vec<&str> = value
            .iter()
            .filter_map(|e| match &e.kind {
                ExprKind::Ident(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        let effect = match (words.as_slice(), words.len() == value.len()) {
            (["read"], true) => Some(Effect::Read),
            (["write"], true) => Some(Effect::Write),
            (["write", "once"], true) => Some(Effect::WriteOnce),
            (["sandbox"], true) => Some(Effect::Sandbox),
            _ => None,
        };
        if effect.is_none() {
            self.push(
                err("E0303", "invalid effect", span)
                    .expected("`read`, `write`, `write once` or `sandbox`")
                    .observed(format!("`{}`", words.join(" "))),
            );
        }
        effect
    }

    fn prop_int(&mut self, p: &ToolProp, units: &[&str], expected: &str) {
        let ok = matches!(
            p.value.as_slice(),
            [Expr { kind: ExprKind::Int { unit: Some(u), .. }, .. }] if units.contains(&u.as_str())
        );
        if !ok {
            self.bad_prop(p, expected);
        }
    }

    fn bad_prop(&mut self, p: &ToolProp, expected: &str) {
        self.push(
            err(
                "E0305",
                format!("invalid value for `{}`", p.key.name),
                p.span,
            )
            .expected(expected),
        );
    }

    // ----- interpolation ----------------------------------------------------

    /// Checks every `{name.field}` in a text literal against `lookup`.
    fn check_interpolations(
        &mut self,
        lit: &StrLit,
        lookup: &dyn Fn(&str) -> Option<Ty>,
    ) -> Vec<String> {
        let mut roots = Vec::new();
        for (path, start, end) in interpolations(lit) {
            let span = Span::new(start, end);
            let mut segments = path.split('.').map(str::trim);
            let root = segments.next().unwrap_or_default();
            if root.is_empty() || !root.chars().all(|c| c.is_alphanumeric() || c == '_') {
                self.push(
                    err("E0402", "invalid interpolation", span)
                        .expected("`{name}` or `{name.field}`")
                        .observed(format!("`{{{path}}}`")),
                );
                continue;
            }
            let Some(mut ty) = lookup(root) else {
                self.push(
                    err("E0401", "unknown name in text", span)
                        .expected("a parameter or a value in scope")
                        .observed(format!("`{root}`")),
                );
                continue;
            };
            roots.push(root.to_owned());
            for seg in segments {
                ty = self.field_of(&ty, seg, span);
            }
        }
        roots
    }

    fn field_of(&mut self, ty: &Ty, field: &str, span: Span) -> Ty {
        match ty {
            Ty::Error => Ty::Error,
            Ty::List(..) if field == "length" => Ty::Nat,
            Ty::User(n) => {
                if let Some(UserType::Record(fields)) = self.types.get(n.as_str())
                    && let Some((_, t)) = fields.iter().find(|(f, _)| f == field)
                {
                    return t.clone();
                }
                self.no_field(ty, field, span)
            }
            _ => self.no_field(ty, field, span),
        }
    }

    fn no_field(&mut self, ty: &Ty, field: &str, span: Span) -> Ty {
        self.push(
            err("E0603", "unknown field", span)
                .expected(format!("a field of `{ty}`"))
                .observed(format!("`{field}`")),
        );
        Ty::Error
    }

    // ----- graphs -----------------------------------------------------------

    /// Orders graphs so callees are checked before callers. Recursion is
    /// planned for M5 (decision D17) and reported here.
    fn graph_order(&mut self, program: &'p Program) -> Vec<&'p GraphDecl> {
        let graphs: Vec<&GraphDecl> = program
            .decls
            .iter()
            .filter_map(|d| match d {
                Decl::Graph(g) if self.graphs.contains_key(g.name.name.as_str()) => Some(g),
                _ => None,
            })
            .collect();
        let index: HashMap<&str, usize> = graphs
            .iter()
            .enumerate()
            .map(|(i, g)| (g.name.name.as_str(), i))
            .collect();
        let mut calls: Vec<Vec<(usize, Span)>> = vec![Vec::new(); graphs.len()];
        for (i, g) in graphs.iter().enumerate() {
            for s in &g.body {
                for_each_expr(s, &mut |e| {
                    if let ExprKind::Call { callee, .. } = &e.kind
                        && let ExprKind::Ident(n) = &callee.kind
                        && let Some(&j) = index.get(n.as_str())
                    {
                        calls[i].push((j, callee.span));
                    }
                });
            }
        }
        // Depth-first post-order; a back edge is recursion.
        let mut state = vec![0u8; graphs.len()]; // 0 new, 1 visiting, 2 done
        let mut order = Vec::new();
        for start in 0..graphs.len() {
            self.visit_graph(start, &calls, &graphs, &mut state, &mut order);
        }
        order.into_iter().map(|i| graphs[i]).collect()
    }

    fn visit_graph(
        &mut self,
        i: usize,
        calls: &[Vec<(usize, Span)>],
        graphs: &[&GraphDecl],
        state: &mut [u8],
        order: &mut Vec<usize>,
    ) {
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for &(j, span) in &calls[i] {
            if state[j] == 1 {
                self.push(
                    err(
                        "E0101",
                        "recursive graphs are not supported yet (planned for M5)",
                        span,
                    )
                    .observed(format!(
                        "`{}` calls `{}`",
                        graphs[i].name.name, graphs[j].name.name
                    )),
                );
            } else {
                self.visit_graph(j, calls, graphs, state, order);
            }
        }
        state[i] = 2;
        order.push(i);
    }

    fn check_graph(&mut self, g: &'p GraphDecl) -> ir::Graph {
        let sig_params = self.graphs[g.name.name.as_str()].1.params.clone();
        let ret = self.graphs[g.name.name.as_str()].1.ret.clone();

        let mut gc = GraphCx::default();
        for (n, t) in &sig_params {
            gc.scope.insert(n.clone(), t.clone());
        }

        // Pass 1: collect local names.
        let mut locals: Vec<Local> = Vec::new();
        let mut returns = Vec::new();
        let mut limits = Vec::new();
        for s in &g.body {
            match s {
                Stmt::Node {
                    name,
                    fan_out,
                    value,
                } => locals.push(Local {
                    name,
                    fan_out: fan_out.as_ref(),
                    value,
                }),
                Stmt::Return(e) => returns.push(e),
                Stmt::Limits(entries) => limits.push(entries),
            }
        }
        let mut index: HashMap<&str, usize> = HashMap::new();
        for (i, l) in locals.iter().enumerate() {
            let n = l.name.name.as_str();
            let clash = gc.scope.contains_key(n) || index.contains_key(n);
            let global = self.is_global(n);
            if clash || global {
                self.push(
                    err("E0501", "name already used", l.name.span)
                        .expected("a name not used by a parameter, another node or a declaration")
                        .observed(format!("`{n}`")),
                );
            } else {
                index.insert(n, i);
            }
        }

        for entries in &limits {
            self.check_limits(entries);
        }

        // Pass 2: dependencies between locals, then a topological order.
        let deps: Vec<Vec<usize>> = locals
            .iter()
            .map(|l| {
                let bound = l.fan_out.map(|(v, _)| v.name.as_str());
                let mut out = Vec::new();
                let mut push = |e: &Expr| collect_refs(e, bound, &index, &mut out);
                if let Some((_, over)) = l.fan_out {
                    push(over);
                }
                push(l.value);
                out.sort_unstable();
                out.dedup();
                out
            })
            .collect();
        let (order, cyclic) = self.topo(&locals, &deps);
        // Nodes in a cycle were already reported: type them as errors up
        // front so they do not also show up as unknown names.
        for &i in &order[order.len() - cyclic..] {
            gc.scope.insert(locals[i].name.name.clone(), Ty::Error);
        }

        // Pass 3: type each local in dependency order.
        let mut ids: HashMap<usize, NodeId> = HashMap::new();
        let mut nodes = Vec::new();
        for &i in &order {
            let l = &locals[i];
            let (ty, kind, effect) = match l.fan_out {
                Some((var, over)) => {
                    let over_ty = self.expr(over, &gc).ty;
                    let (elem, max) = match over_ty {
                        Ty::List(t, m) => (*t, m),
                        Ty::Error => (Ty::Error, None),
                        other => {
                            self.push(
                                err(
                                    "E0609",
                                    "fan-out over a value that is not a list",
                                    over.span,
                                )
                                .expected("a list")
                                .observed(format!("`{other}`")),
                            );
                            (Ty::Error, None)
                        }
                    };
                    gc.scope.insert(var.name.clone(), elem);
                    let t = self.expr(l.value, &gc);
                    gc.scope.remove(&var.name);
                    (Ty::List(Box::new(t.ty), max), t.kind, t.effect)
                }
                None => {
                    let t = self.expr(l.value, &gc);
                    (t.ty, t.kind, t.effect)
                }
            };
            gc.scope.insert(l.name.name.clone(), ty.clone());
            let id = NodeId(nodes.len() as u32);
            ids.insert(i, id);
            nodes.push(ir::Node {
                id,
                name: l.name.name.clone(),
                ty: ty.to_string(),
                kind,
                fan_out: l.fan_out.map(|(v, _)| v.name.clone()),
                effect,
                inputs: deps[i].iter().filter_map(|d| ids.get(d).copied()).collect(),
            });
        }

        // Return.
        let mut output = None;
        let mut used: HashSet<usize> = deps.iter().flatten().copied().collect();
        match returns.as_slice() {
            [] if g.incomplete => {}
            [] => self.push(
                err("E0504", "graph has no `return`", g.name.span)
                    .expected("a `return` with the graph's result"),
            ),
            [r, rest @ ..] => {
                for extra in rest {
                    self.push(
                        err("E0505", "graph has more than one `return`", extra.span)
                            .expected("a single `return`"),
                    );
                }
                let t = self.expr(r, &gc);
                if !assignable(&t.ty, &ret) {
                    self.push(
                        err("E0610", "returned value has the wrong type", r.span)
                            .expected(format!("`{ret}`"))
                            .observed(format!("`{}`", t.ty)),
                    );
                }
                let mut refs = Vec::new();
                collect_refs(r, None, &index, &mut refs);
                used.extend(refs.iter().copied());
                output = match &r.kind {
                    ExprKind::Ident(n) => index.get(n.as_str()).and_then(|i| ids.get(i)).copied(),
                    _ => None,
                };
                if output.is_none() {
                    let id = NodeId(nodes.len() as u32);
                    nodes.push(ir::Node {
                        id,
                        name: "return".into(),
                        ty: t.ty.to_string(),
                        kind: t.kind,
                        fan_out: None,
                        effect: t.effect,
                        inputs: refs.iter().filter_map(|d| ids.get(d).copied()).collect(),
                    });
                    output = Some(id);
                }
            }
        }

        // Results nobody uses: wasted money for `llm` and `read` nodes.
        for (i, l) in locals.iter().enumerate() {
            if used.contains(&i) || !index.contains_key(l.name.name.as_str()) {
                continue;
            }
            let effect = ids
                .get(&i)
                .map_or(Effect::Pure, |id| nodes[id.0 as usize].effect);
            if effect <= Effect::Read {
                self.push(
                    warn("W0801", "value is never used", l.name.span)
                        .expected("a value used by another node or by `return`")
                        .observed(format!("`{}`", l.name.name)),
                );
            }
        }

        // Effect of the graph, and its declared maximum.
        let effect = nodes
            .iter()
            .map(|n| n.effect)
            .fold(Effect::Pure, Effect::join);
        if let Some(words) = &g.max_effect {
            let exprs: Vec<Expr> = words
                .iter()
                .map(|w| Expr {
                    kind: ExprKind::Ident(w.name.clone()),
                    span: w.span,
                })
                .collect();
            let span = words.first().map_or(g.name.span, |w| w.span);
            let max = match words.as_slice() {
                [w] if w.name == "pure" => Some(Effect::Pure),
                [w] if w.name == "llm" => Some(Effect::Llm),
                _ => self.effect_words(&exprs, span),
            };
            if let Some(max) = max
                && effect > max
            {
                let culprit = nodes
                    .iter()
                    .find(|n| n.effect > max)
                    .map_or_else(String::new, |n| {
                        format!(" (node `{}` is `{}`)", n.name, n.effect)
                    });
                self.push(
                    err("E0701", "graph exceeds its declared effect", g.name.span)
                        .expected(format!("at most `{max}`"))
                        .observed(format!("`{effect}`{culprit}")),
                );
            }
        }
        self.graph_effects.insert(g.name.name.clone(), effect);

        ir::Graph {
            name: g.name.name.clone(),
            params: sig_params
                .iter()
                .map(|(n, t)| (n.clone(), t.to_string()))
                .collect(),
            ret: ret.to_string(),
            effect: Some(effect),
            nodes,
            output,
        }
    }

    fn is_global(&self, n: &str) -> bool {
        self.models.contains_key(n)
            || self.tools.contains_key(n)
            || self.prompts.contains_key(n)
            || self.graphs.contains_key(n)
            || self.types.contains_key(n)
    }

    /// Kahn's algorithm. Locals in a cycle are reported and placed last;
    /// returns the order and how many locals at its end are in a cycle.
    fn topo(&mut self, locals: &[Local], deps: &[Vec<usize>]) -> (Vec<usize>, usize) {
        let n = locals.len();
        let mut indegree = vec![0usize; n];
        let mut users: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, ds) in deps.iter().enumerate() {
            for &d in ds {
                indegree[i] += 1;
                users[d].push(i);
            }
        }
        // Ready nodes in source order, for stable output.
        let mut ready: std::collections::BTreeSet<usize> =
            (0..n).filter(|&i| indegree[i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(i) = ready.pop_first() {
            order.push(i);
            for &u in &users[i] {
                indegree[u] -= 1;
                if indegree[u] == 0 {
                    ready.insert(u);
                }
            }
        }
        let sorted = order.len();
        if sorted < n {
            let placed: HashSet<usize> = order.iter().copied().collect();
            let stuck: Vec<usize> = (0..n).filter(|i| !placed.contains(i)).collect();
            let names: Vec<String> = stuck
                .iter()
                .map(|&i| format!("`{}`", locals[i].name.name))
                .collect();
            self.push(
                err(
                    "E0506",
                    "nodes depend on each other in a cycle",
                    locals[stuck[0]].name.span,
                )
                .expected("a graph without cycles (use `loop` for repetition, planned for M5)")
                .observed(names.join(", ")),
            );
            order.extend(stuck);
        }
        (order, n - sorted)
    }

    fn check_limits(&mut self, entries: &[(Ident, Expr)]) {
        for (key, value) in entries {
            let (ok, expected) = match key.name.as_str() {
                "threads" => (
                    matches!(&value.kind, ExprKind::Int { value, unit: None } if *value > 0),
                    "a positive number, e.g. `8`",
                ),
                "rate" => (
                    matches!(&value.kind, ExprKind::Int { unit: Some(u), .. } if u.starts_with('/')),
                    "a rate, e.g. `50/s`",
                ),
                "budget" => (
                    matches!(&value.kind,
                        ExprKind::Int { unit: Some(u), .. } | ExprKind::Float { unit: Some(u), .. }
                        if matches!(u.as_str(), "USD" | "BRL" | "EUR")),
                    "an amount of money, e.g. `2 USD`",
                ),
                "memory" => (
                    matches!(&value.kind, ExprKind::Int { unit: Some(u), .. } if matches!(u.as_str(), "KB" | "MB" | "GB")),
                    "a size, e.g. `4 GB`",
                ),
                other => {
                    self.push(
                        err("E0502", "unknown limit", key.span)
                            .expected("`threads`, `rate`, `budget` or `memory`")
                            .observed(format!("`{other}`")),
                    );
                    continue;
                }
            };
            if !ok {
                self.push(
                    err(
                        "E0503",
                        format!("invalid value for `{}`", key.name),
                        value.span,
                    )
                    .expected(expected),
                );
            }
        }
    }

    // ----- expressions --------------------------------------------------------

    fn expr(&mut self, e: &Expr, gc: &GraphCx) -> Typed {
        match &e.kind {
            ExprKind::Ident(n) => {
                if let Some(t) = gc.scope.get(n) {
                    return Typed::pure(t.clone());
                }
                if let Some(ty) = self.unit_variants.get(n.as_str()) {
                    return Typed::pure(Ty::User((*ty).to_owned()));
                }
                if self.is_global(n) {
                    self.push(
                        err("E0601", "declaration used as a value", e.span)
                            .expected(format!("a call, e.g. `{n}(...)`"))
                            .observed(format!("`{n}`")),
                    );
                } else {
                    self.push(
                        err("E0602", "unknown name", e.span)
                            .expected("a parameter, node or value in scope")
                            .observed(format!("`{n}`")),
                    );
                }
                Typed::pure(Ty::Error)
            }
            ExprKind::Str(lit) => {
                self.check_interpolations(lit, &|n| gc.scope.get(n).cloned());
                Typed::pure(Ty::Text)
            }
            ExprKind::Int { unit, .. } => Typed::pure(Ty::of_unit(unit.as_deref(), true)),
            ExprKind::Float { unit, .. } => Typed::pure(Ty::of_unit(unit.as_deref(), false)),
            ExprKind::List(items) => {
                let mut effect = Effect::Pure;
                let mut elem: Option<Ty> = None;
                for it in items {
                    let t = self.expr(it, gc);
                    effect = effect.join(t.effect);
                    match &elem {
                        None => elem = Some(t.ty),
                        Some(first) if !assignable(&t.ty, first) => self.push(
                            err("E0611", "list elements have different types", it.span)
                                .expected(format!("`{first}`"))
                                .observed(format!("`{}`", t.ty)),
                        ),
                        _ => {}
                    }
                }
                Typed {
                    ty: Ty::List(
                        Box::new(elem.unwrap_or(Ty::Error)),
                        Some(items.len() as u64),
                    ),
                    kind: NodeKind::Pure,
                    effect,
                }
            }
            ExprKind::Field { base, name } => {
                let t = self.expr(base, gc);
                let ty = self.field_of(&t.ty, &name.name, name.span);
                Typed { ty, ..t }
            }
            ExprKind::Call { callee, args } => self.call(e, callee, args, gc),
            ExprKind::Error => Typed::pure(Ty::Error),
        }
    }

    fn call(&mut self, e: &Expr, callee: &Expr, args: &[Arg], gc: &GraphCx) -> Typed {
        let ExprKind::Ident(name) = &callee.kind else {
            self.push(err("E0604", "this value cannot be called", callee.span));
            return Typed::pure(Ty::Error);
        };
        if gc.scope.contains_key(name) {
            self.push(
                err("E0604", "this value cannot be called", callee.span)
                    .expected("a model, tool or graph")
                    .observed(format!("`{name}`")),
            );
            return Typed::pure(Ty::Error);
        }
        let n = name.as_str();
        if self.models.contains_key(n) {
            return self.model_call(n, e, args, gc);
        }
        if self.prompts.contains_key(n) {
            self.push(
                err("E0606", "a prompt must be sent to a model", e.span)
                    .expected(format!("`model({n}(...))`, e.g. `claude({n}(...))`"))
                    .observed(format!("`{n}(...)`")),
            );
            return Typed::pure(Ty::Error);
        }
        if self.types.contains_key(n) {
            self.push(
                err(
                    "E0101",
                    "building values of a declared type is not supported yet (planned for M5)",
                    callee.span,
                )
                .observed(format!("`{n}(...)`")),
            );
            return Typed::pure(Ty::Error);
        }
        if let Some(sig) = self.tools.get(n) {
            let (params, ret, effect) = (sig.params.clone(), sig.ret.clone(), sig.effect);
            let inner = self.args(n, &params, args, e.span, gc);
            return Typed {
                ty: ret,
                kind: NodeKind::Tool { tool: n.to_owned() },
                effect: effect.join(inner),
            };
        }
        if let Some((_, sig)) = self.graphs.get(n) {
            let (params, ret) = (sig.params.clone(), sig.ret.clone());
            let inner = self.args(n, &params, args, e.span, gc);
            let effect = self.graph_effects.get(n).copied().unwrap_or(Effect::Pure);
            return Typed {
                ty: ret,
                kind: NodeKind::Call {
                    graph: n.to_owned(),
                },
                effect: effect.join(inner),
            };
        }
        self.push(
            err("E0602", "unknown name", callee.span)
                .expected("a model, tool or graph")
                .observed(format!("`{n}`")),
        );
        Typed::pure(Ty::Error)
    }

    fn model_call(&mut self, model: &str, e: &Expr, args: &[Arg], gc: &GraphCx) -> Typed {
        let mut prompt_call = None;
        for a in args {
            match &a.name {
                Some(n) if n.name == "continue" => self.push(
                    err(
                        "E0101",
                        "`continue=` is not supported yet (planned for M5)",
                        n.span,
                    )
                    .observed("`continue=`"),
                ),
                Some(n) => self.push(
                    err("E0612", "unknown argument", n.span)
                        .expected("a prompt call")
                        .observed(format!("`{}=`", n.name)),
                ),
                None if prompt_call.is_none() => prompt_call = Some(&a.value),
                None => self.push(
                    err("E0607", "a model takes a single prompt", a.value.span)
                        .expected("one prompt call")
                        .observed("another argument"),
                ),
            }
        }
        let fail = |cx: &mut Self, span: Span| {
            cx.push(
                err("E0605", "a model is called with a prompt", span)
                    .expected(format!("`{model}(some_prompt(...))`")),
            );
            Typed::pure(Ty::Error)
        };
        let Some(pc) = prompt_call else {
            return fail(self, e.span);
        };
        let ExprKind::Call {
            callee,
            args: pargs,
        } = &pc.kind
        else {
            return fail(self, pc.span);
        };
        let ExprKind::Ident(pname) = &callee.kind else {
            return fail(self, pc.span);
        };
        let Some(sig) = self.prompts.get(pname.as_str()) else {
            return fail(self, pc.span);
        };
        let (params, ret) = (sig.params.clone(), sig.ret.clone());
        let inner = self.args(pname, &params, pargs, pc.span, gc);
        Typed {
            ty: ret,
            kind: NodeKind::Model {
                model: model.to_owned(),
                prompt: pname.clone(),
            },
            effect: Effect::Llm.join(inner),
        }
    }

    /// Checks call arguments (positional first, then named). Returns the
    /// effect of evaluating them.
    fn args(
        &mut self,
        callee: &str,
        params: &[(String, Ty)],
        args: &[Arg],
        span: Span,
        gc: &GraphCx,
    ) -> Effect {
        let mut effect = Effect::Pure;
        let mut filled = vec![false; params.len()];
        let mut next_positional = 0;
        for a in args {
            let t = self.expr(&a.value, gc);
            effect = effect.join(t.effect);
            let slot = match &a.name {
                None => {
                    let s = next_positional;
                    next_positional += 1;
                    if s >= params.len() {
                        self.push(
                            err("E0607", "too many arguments", a.value.span)
                                .expected(format!("{} argument(s) for `{callee}`", params.len()))
                                .observed(format!("{}", args.len())),
                        );
                        continue;
                    }
                    s
                }
                Some(n) => match params.iter().position(|(p, _)| *p == n.name) {
                    Some(s) => s,
                    None => {
                        self.push(
                            err("E0612", "unknown argument", n.span)
                                .expected(format!("a parameter of `{callee}`"))
                                .observed(format!("`{}`", n.name)),
                        );
                        continue;
                    }
                },
            };
            if filled[slot] {
                self.push(
                    err("E0607", "argument given twice", a.value.span)
                        .observed(format!("`{}`", params[slot].0)),
                );
            }
            filled[slot] = true;
            let (pname, pty) = &params[slot];
            if !assignable(&t.ty, pty) {
                self.push(
                    err("E0608", "argument has the wrong type", a.value.span)
                        .expected(format!("`{pty}` for `{pname}`"))
                        .observed(format!("`{}`", t.ty)),
                );
            }
        }
        let missing: Vec<&str> = params
            .iter()
            .zip(&filled)
            .filter(|(_, f)| !**f)
            .map(|((n, _), _)| n.as_str())
            .collect();
        if !missing.is_empty() {
            self.push(
                err("E0607", "missing arguments", span)
                    .expected(format!("a value for `{}`", missing.join("`, `")))
                    .observed(format!("{} argument(s)", args.len())),
            );
        }
        effect
    }
}

struct Local<'p> {
    name: &'p Ident,
    fan_out: Option<&'p (Ident, Expr)>,
    value: &'p Expr,
}

#[derive(Default)]
struct GraphCx {
    scope: HashMap<String, Ty>,
}

struct Typed {
    ty: Ty,
    kind: NodeKind,
    effect: Effect,
}

impl Typed {
    fn pure(ty: Ty) -> Self {
        Self {
            ty,
            kind: NodeKind::Pure,
            effect: Effect::Pure,
        }
    }
}

/// Visits every expression in a statement (including nested ones).
fn for_each_expr(s: &Stmt, f: &mut dyn FnMut(&Expr)) {
    fn walk(e: &Expr, f: &mut dyn FnMut(&Expr)) {
        f(e);
        match &e.kind {
            ExprKind::List(items) => items.iter().for_each(|i| walk(i, f)),
            ExprKind::Field { base, .. } => walk(base, f),
            ExprKind::Call { callee, args } => {
                walk(callee, f);
                args.iter().for_each(|a| walk(&a.value, f));
            }
            _ => {}
        }
    }
    match s {
        Stmt::Limits(entries) => entries.iter().for_each(|(_, e)| walk(e, f)),
        Stmt::Node { fan_out, value, .. } => {
            if let Some((_, over)) = fan_out {
                walk(over, f);
            }
            walk(value, f);
        }
        Stmt::Return(e) => walk(e, f),
    }
}

/// Collects the locals referenced by `e` (by identifier or inside text
/// interpolation), ignoring `bound` (a fan-out variable).
fn collect_refs(e: &Expr, bound: Option<&str>, index: &HashMap<&str, usize>, out: &mut Vec<usize>) {
    let mut add = |n: &str| {
        if Some(n) != bound
            && let Some(&i) = index.get(n)
        {
            out.push(i);
        }
    };
    match &e.kind {
        ExprKind::Ident(n) => add(n),
        ExprKind::Str(lit) => {
            for (path, _, _) in interpolations(lit) {
                if let Some(root) = path.split('.').next() {
                    add(root.trim());
                }
            }
        }
        ExprKind::List(items) => items
            .iter()
            .for_each(|i| collect_refs(i, bound, index, out)),
        ExprKind::Field { base, .. } => collect_refs(base, bound, index, out),
        ExprKind::Call { args, .. } => args
            .iter()
            .for_each(|a| collect_refs(&a.value, bound, index, out)),
        ExprKind::Int { .. } | ExprKind::Float { .. } | ExprKind::Error => {}
    }
}

/// Finds `{...}` in a text literal: `(content, start, end)` with byte offsets
/// in the file. `\{` is an escaped brace, not an interpolation.
fn interpolations(lit: &StrLit) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let bytes = lit.text.as_bytes();
    let base = lit.content_offset as usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'{' => {
                let start = i;
                match lit.text[i + 1..].find('}') {
                    Some(len) => {
                        let content = lit.text[i + 1..i + 1 + len].trim().to_owned();
                        let end = i + 1 + len + 1;
                        out.push((content, base + start, base + end));
                        i = end;
                    }
                    None => break,
                }
            }
            _ => i += 1,
        }
    }
    out
}
