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
        routers: HashMap::new(),
        tools: HashMap::new(),
        prompts: HashMap::new(),
        graphs: HashMap::new(),
        entities: HashMap::new(),
        defs: HashMap::new(),
        messages: HashSet::new(),
        types: HashMap::new(),
        unit_variants: HashMap::new(),
        variant_owners: HashMap::new(),
        tools_with_max_output: HashSet::new(),
        graph_effects: HashMap::new(),
        raw_write: false,
        raw_write_graphs: HashSet::new(),
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
    ir::Program {
        graphs,
        ..Default::default()
    }
}

// ----- declarations ---------------------------------------------------------

struct ToolSig {
    params: Vec<(String, Ty)>,
    ret: Ty,
    effect: Effect,
    /// `on_uncertain ...` and where it is written.
    policy: Option<(Policy, Span)>,
    /// `checks StateType`: the state `requires` is checked against.
    checks: Option<Ident>,
    /// Declares `idempotency_key`.
    keyed: bool,
    /// `batch p`: the type of the items of the list parameter `p`.
    batch: Option<Ty>,
    /// `compensate f(a, b)`: the tool that undoes a call, called with
    /// these parameters of this tool (D12, saga).
    compensate: Option<(Ident, Vec<Ident>)>,
}

/// What a `write once` tool does when a call may or may not have happened.
enum Policy {
    Pause,
    AcceptLoss,
    /// `verify(f(a, b))`: `f` is a tool, `a` and `b` this tool's parameters.
    Verify {
        tool: Ident,
        args: Vec<Ident>,
    },
}

/// A handler: its name, parameters, and the answer's type (`None` for a
/// handler that changes the state and answers nothing).
type HandlerSig = (String, Vec<(String, Ty)>, Option<Ty>);

/// An entity (decision D15): its key and its handlers.
struct EntitySig {
    key: Ty,
    handlers: Vec<HandlerSig>,
    /// Handlers that apply a change rather than set a new value: each
    /// update that uses the message also uses the field's current value
    /// (`next n = n + x`, `next xs = xs + [x]`). A send to one of these
    /// loses no other run's update, whatever its arguments came from.
    changes: HashSet<String>,
}

/// A pure function (decision D27).
struct DefSig {
    params: Vec<(String, Ty)>,
    ret: Ty,
}

/// Functions every program has. They are pure, like `def`s.
const BUILTINS: &[(&str, &str)] = &[
    ("len", "`len(list)` or `len(text)`"),
    ("take", "`take(list, n)`: the first `n` items"),
    ("sum", "`sum(list)` of numbers or money"),
    ("join", "`join(list_of_texts, separator)`"),
    ("lower", "`lower(text)`"),
    ("upper", "`upper(text)`"),
    ("trim", "`trim(text)`"),
];

struct PromptSig {
    params: Vec<(String, Ty)>,
    ret: Ty,
}

struct GraphSig {
    params: Vec<(String, Ty)>,
    ret: Ty,
}

/// A variant and its fields, in order.
type VariantSig = (String, Vec<(String, Ty)>);

enum UserType {
    Record(Vec<(String, Ty)>),
    /// Each variant with its fields.
    Variants(Vec<VariantSig>),
    Alias(Ty),
}

struct Cx<'a, 'p> {
    diags: &'a mut Vec<Diagnostic>,
    models: HashMap<&'p str, &'p ModelDecl>,
    /// Routers (decision D30): called like models.
    routers: HashMap<&'p str, &'p RouterDecl>,
    tools: HashMap<&'p str, ToolSig>,
    prompts: HashMap<&'p str, PromptSig>,
    graphs: HashMap<&'p str, (&'p GraphDecl, GraphSig)>,
    entities: HashMap<&'p str, EntitySig>,
    defs: HashMap<&'p str, DefSig>,
    /// Types declared with `message`: what a run can `receive`.
    messages: HashSet<&'p str>,
    types: HashMap<&'p str, UserType>,
    /// Variants without fields, usable as values: `Optimist` is a `Role`.
    unit_variants: HashMap<&'p str, &'p str>,
    /// The types that declare each variant name (for constructors).
    variant_owners: HashMap<&'p str, Vec<&'p str>>,
    /// Tools that declare `max_output` (agents may only use those, D16).
    tools_with_max_output: HashSet<String>,
    graph_effects: HashMap<String, Effect>,
    /// Set while checking a graph when it reaches a write with no
    /// `compensate`: what W0604 warns about in a race branch.
    raw_write: bool,
    /// The graphs that make such writes.
    raw_write_graphs: HashSet<String>,
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
            // Built-in functions (`len`, ...) may be redeclared: the
            // program's own name wins.
            if Ty::builtin(&name.name).is_some()
                || name.name == "List"
                || name.name == "Map"
                || name.name == "true"
                || name.name == "false"
            {
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
                Decl::Router(r) => {
                    self.routers.insert(&r.name.name, r);
                }
                Decl::Type(t) => {
                    if t.message {
                        self.messages.insert(&t.name.name);
                    }
                    // Placeholder so types can refer to each other by name.
                    self.types
                        .insert(&t.name.name, UserType::Variants(Vec::new()));
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
                        let mut out = Vec::new();
                        for v in vs {
                            let fields = self.fields(&v.fields);
                            if v.fields.is_empty() {
                                self.unit_variants.insert(&v.name.name, &t.name.name);
                            }
                            self.variant_owners
                                .entry(&v.name.name)
                                .or_default()
                                .push(&t.name.name);
                            out.push((v.name.name.clone(), fields));
                        }
                        UserType::Variants(out)
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
                    self.no_borrows(&p.params);
                    for ((_, ty), decl) in params.iter().zip(&p.params) {
                        if *ty == Ty::Sandbox {
                            self.push(
                                err("E0647", "a sandbox is not a value a prompt can show", decl.name.span)
                                    .expected("the sandbox lent to tools; the text the tools return goes to the model")
                                    .observed("a `Sandbox` parameter"),
                            );
                        }
                    }
                    let ret = self.ty(&p.ret);
                    let names: HashMap<String, Ty> = params.iter().cloned().collect();
                    self.check_interpolations(&p.template, &|n| names.get(n).cloned());
                    self.prompts.insert(&p.name.name, PromptSig { params, ret });
                }
                Decl::Graph(g) => {
                    let params = self.params(&g.params);
                    self.no_borrows(&g.params);
                    let ret = self.ty(&g.ret);
                    if ret == Ty::Sandbox {
                        self.push(
                            err("E0647", "a graph cannot return a sandbox", g.ret.span)
                                .expected("a value computed from it, e.g. the result of a tool that reads it")
                                .observed("`-> Sandbox`"),
                        );
                    }
                    self.graphs
                        .insert(&g.name.name, (g, GraphSig { params, ret }));
                }
                Decl::Entity(e) => {
                    let sig = self.entity_sig(e);
                    self.entities.insert(&e.name.name, sig);
                }
                Decl::Def(d) => {
                    self.no_borrows(&d.params);
                    let params = self.params(&d.params);
                    let ret = self.ty(&d.ret);
                    self.defs.insert(&d.name.name, DefSig { params, ret });
                }
                Decl::Model(_) | Decl::Type(_) | Decl::Router(_) => {}
            }
        }
        self.check_write_contracts(program);
        for d in &program.decls {
            if let Decl::Router(r) = d {
                self.check_router(r);
            }
        }
        for d in &program.decls {
            if let Decl::Def(f) = d {
                self.check_def(f);
            }
        }
        self.no_recursive_defs(program);
        for d in &program.decls {
            if let Decl::Entity(e) = d {
                self.check_entity(e);
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
        let mut params = self.params(&t.params);
        // `box: reads Sandbox` / `box: edits Sandbox` (decision D26).
        for (p, decl) in params.iter_mut().zip(&t.params) {
            match (&decl.borrow, &p.1) {
                (Some(m), Ty::Sandbox) => p.1 = Ty::Lent(m.name == "edits"),
                (Some(_), Ty::Error) => {}
                (Some(m), other) => self.push(
                    err("E0646", "only a `Sandbox` can be borrowed", m.span)
                        .expected(format!("`{} Sandbox`", m.name))
                        .observed(format!("`{} {other}`", m.name)),
                ),
                (None, Ty::Sandbox) => self.push(
                    err(
                        "E0646",
                        "a tool borrows a sandbox for each call",
                        decl.name.span,
                    )
                    .expected(format!(
                        "`{}: reads Sandbox` or `{}: edits Sandbox`",
                        decl.name.name, decl.name.name
                    ))
                    .observed("`Sandbox` without `reads` or `edits`"),
                ),
                _ => {}
            }
        }
        let ret = self.ty(&t.ret);
        let mut effect = None;
        let mut on_uncertain = false;
        let mut policy = None;
        let mut checks = None;
        let mut keyed = false;
        let mut batch = None;
        let mut compensate = None;
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
                "max_output" => {
                    self.tools_with_max_output.insert(t.name.name.clone());
                    self.prop_int(p, &["tokens"], "a number of tokens, e.g. `4000 tokens`")
                }
                "description" => {
                    if !matches!(p.value.as_slice(), [Expr { kind: ExprKind::Str(_), .. }]) {
                        self.bad_prop(p, "a text that tells a model what the tool does");
                    }
                }
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
                        keyed = true;
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
                    policy = match p.value.as_slice() {
                        [Expr { kind: ExprKind::Ident(n), .. }] if n == "pause" => {
                            Some(Policy::Pause)
                        }
                        [Expr { kind: ExprKind::Ident(n), .. }] if n == "accept_loss" => {
                            Some(Policy::AcceptLoss)
                        }
                        [Expr { kind: ExprKind::Call { callee, args }, .. }]
                            if matches!(&callee.kind, ExprKind::Ident(n) if n == "verify") =>
                        {
                            verify_policy(args)
                        }
                        _ => None,
                    }
                    .map(|pol| (pol, p.span));
                    if policy.is_none() {
                        self.bad_prop(
                            p,
                            "`verify(tool(param, ...))`, `pause` or `accept_loss`",
                        );
                    }
                }
                "batch" => match p.value.as_slice() {
                    [Expr { kind: ExprKind::Ident(n), span }] => {
                        match params.iter().find(|(pn, _)| pn == n) {
                            Some((_, Ty::List(item, _))) => batch = Some((**item).clone()),
                            Some((_, Ty::Error)) => batch = Some(Ty::Error),
                            found => {
                                // Poison: one error, not another on `verify`.
                                batch = Some(Ty::Error);
                                self.push(
                                err("E0637", "`batch` must name a list parameter of the tool", *span)
                                    .expected("a parameter of type `List[T]`: the items the call applies")
                                    .observed(match found {
                                        Some((_, t)) => format!("`{n}: {t}`"),
                                        None => format!("`{n}` is not a parameter"),
                                    }),
                                )
                            }
                        }
                    }
                    _ => self.bad_prop(p, "a parameter name"),
                },
                "checks" => match p.value.as_slice() {
                    [Expr { kind: ExprKind::Ident(n), span }] => {
                        checks = Some(Ident {
                            name: n.clone(),
                            span: *span,
                        })
                    }
                    _ => self.bad_prop(p, "a type name"),
                },
                "compensate" => match p.value.as_slice() {
                    [Expr { kind: ExprKind::Call { callee, args }, .. }] => {
                        let ExprKind::Ident(f) = &callee.kind else {
                            self.bad_prop(p, "a call `tool(param, ...)`: the tool that undoes this one");
                            continue;
                        };
                        let idents = args
                            .iter()
                            .map(|a| match (&a.name, &a.value.kind) {
                                (None, ExprKind::Ident(n)) => Some(Ident {
                                    name: n.clone(),
                                    span: a.value.span,
                                }),
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>();
                        match idents {
                            Some(args) => {
                                compensate = Some((
                                    Ident {
                                        name: f.clone(),
                                        span: callee.span,
                                    },
                                    args,
                                ))
                            }
                            None => self.bad_prop(
                                p,
                                "a call whose arguments are parameters of this tool",
                            ),
                        }
                    }
                    _ => self.bad_prop(p, "a call `tool(param, ...)`: the tool that undoes this one"),
                },
                "repeatable" => {
                    if !p.value.is_empty() {
                        self.bad_prop(p, "no value");
                    }
                }
                _ => self.push(
                    err("E0301", "unknown tool property", p.key.span)
                        .expected("`effect`, `max_output`, `timeout`, `retry_on`, `idempotency_key`, `on_uncertain`, `batch`, `compensate`, `checks`, `repeatable` or `description`")
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
        if params.iter().any(|(_, t)| *t == Ty::Lent(true)) && effect < Effect::Sandbox {
            self.push(
                err("E0648", "a tool that edits a sandbox needs `effect sandbox`", t.name.span)
                    .expected("`effect sandbox`: its changes are undone if the call fails, and restored on recovery")
                    .observed(format!("`effect {effect}`")),
            );
        }
        if parsed == Some(Effect::WriteOnce) && !on_uncertain {
            self.push(
                err("E0304", "`write once` tool needs a policy for uncertain outcomes", t.name.span)
                    .expected("`on_uncertain verify(...)`, `on_uncertain pause` or `on_uncertain accept_loss`")
                    .observed("no `on_uncertain` property"),
            );
        }
        if batch.is_some() {
            let verify = matches!(policy, Some((Policy::Verify { .. }, _)));
            if parsed != Some(Effect::WriteOnce) || !verify {
                self.push(
                    err("E0637", "a `batch` tool is a `write once` that verifies", t.name.span)
                        .expected("`effect write once` and `on_uncertain verify(f(...))`, where `f` finds the items already applied")
                        .observed(format!("`effect {effect}`{}", if verify { "" } else { " without `verify`" })),
                );
            }
            if !matches!(ret, Ty::Unit | Ty::Error) {
                self.push(
                    err("E0637", "a `batch` tool returns `Unit`", t.ret.span)
                        .expected("`-> Unit`: a call made in parts has no single answer")
                        .observed(format!("`-> {ret}`")),
                );
            }
        }
        ToolSig {
            params,
            ret,
            effect,
            policy,
            checks,
            keyed,
            batch,
            compensate,
        }
    }

    fn entity_sig(&mut self, e: &EntityDecl) -> EntitySig {
        let key = self.ty(&e.key.ty);
        let mut handlers: Vec<HandlerSig> = Vec::new();
        for h in &e.handlers {
            self.no_borrows(&h.params);
            let params = self.params(&h.params);
            let ret = h.ret.as_ref().map(|t| self.ty(t));
            if handlers.iter().any(|(n, _, _)| *n == h.name.name) {
                self.push(
                    err("E0650", "message handled twice", h.name.span)
                        .expected("one `on` per message")
                        .observed(format!("`on {}`", h.name.name)),
                );
                continue;
            }
            handlers.push((h.name.name.clone(), params, ret));
        }
        let changes = e
            .handlers
            .iter()
            .filter(|h| {
                let params: HashSet<String> =
                    h.params.iter().map(|p| p.name.name.clone()).collect();
                h.updates.iter().all(|(field, value)| {
                    !mentions_any(value, &params)
                        || mentions_any(value, &HashSet::from([field.name.clone()]))
                })
            })
            .map(|h| h.name.name.clone())
            .collect();
        EntitySig {
            key,
            handlers,
            changes,
        }
    }

    /// An entity's state and handlers. Handlers are pure: they compute
    /// with the state and the message only, so a handler never waits on
    /// anything, and `ask` cannot form cycles (decision D33).
    fn check_entity(&mut self, e: &EntityDecl) {
        let mut gc = GraphCx::default();
        let key_ty = self.entities[e.name.name.as_str()].key.clone();
        let mut names: HashSet<&str> = HashSet::new();
        names.insert(e.key.name.name.as_str());
        let mut fields: Vec<(String, Ty)> = Vec::new();
        for f in &e.state {
            let ty = self.ty(&f.ty);
            if !names.insert(f.name.name.as_str()) {
                self.push(
                    err("E0650", "name used twice in the entity", f.name.span)
                        .observed(format!("`{}`", f.name.name)),
                );
            }
            if let Some(span) = self.impure(&f.init) {
                self.push(
                    err(
                        "E0651",
                        "a state's initial value must be a plain value",
                        span,
                    )
                    .expected("a value written in the program, e.g. `[]`, `0` or `\"\"`")
                    .observed("a call"),
                );
            } else {
                let t = self.expr(&f.init, &GraphCx::default());
                if !assignable(&t.ty, &ty) && !is_empty_list(&f.init) {
                    self.push(
                        err("E0651", "initial value of the wrong type", f.init.span)
                            .expected(format!("`{ty}` for `{}`", f.name.name))
                            .observed(format!("`{}`", t.ty)),
                    );
                }
            }
            fields.push((f.name.name.clone(), ty));
        }
        gc.scope.insert(e.key.name.name.clone(), key_ty);
        for (n, t) in &fields {
            gc.scope.insert(n.clone(), t.clone());
        }
        for h in &e.handlers {
            let mut hc = gc.clone();
            for p in &h.params {
                if names.contains(p.name.name.as_str()) {
                    self.push(
                        err("E0650", "parameter hides a name of the entity", p.name.span)
                            .expected("a name different from the key and the state fields")
                            .observed(format!("`{}`", p.name.name)),
                    );
                }
                let t = self.ty(&p.ty);
                hc.scope.insert(p.name.name.clone(), t);
            }
            let body: Vec<&Expr> = h
                .returns
                .iter()
                .chain(h.updates.iter().map(|(_, v)| v))
                .collect();
            for b in &body {
                if let Some(span) = self.impure(b) {
                    self.push(
                        err("E0654", "a handler cannot call models, tools, graphs or entities", span)
                            .expected("a value computed from the state and the message; do the calls in a graph and send the result")
                            .observed("a call with effects"),
                    );
                }
            }
            match (&h.ret, &h.returns, h.updates.is_empty()) {
                (Some(rt), Some(r), true) => {
                    let want = self.ty(rt);
                    let t = self.expr(r, &hc);
                    if !assignable(&t.ty, &want) {
                        self.push(
                            err("E0610", "returned value has the wrong type", r.span)
                                .expected(format!("`{want}`"))
                                .observed(format!("`{}`", t.ty)),
                        );
                    }
                }
                (None, None, false) => {
                    let mut seen = HashSet::new();
                    for (f, v) in &h.updates {
                        let t = self.expr(v, &hc);
                        match fields.iter().find(|(n, _)| *n == f.name) {
                            None => self.push(
                                err("E0653", "`next` names something that is not a state field", f.span)
                                    .expected("a `state` field of the entity")
                                    .observed(format!("`{}`", f.name)),
                            ),
                            Some(_) if !seen.insert(f.name.as_str()) => self.push(
                                err("E0653", "a state field changed twice in one handler", f.span)
                                    .observed(format!("`next {}` twice", f.name)),
                            ),
                            Some((_, ty)) if !assignable(&t.ty, ty) && !is_empty_list(v) => self.push(
                                err("E0653", "new value of the wrong type", v.span)
                                    .expected(format!("`{ty}` for `{}`", f.name))
                                    .observed(format!("`{}`", t.ty)),
                            ),
                            Some(_) => {}
                        }
                    }
                }
                _ => self.push(
                    err("E0652", "a handler either answers or changes the state", h.name.span)
                        .expected("`on M(...) -> T:` with one `return`, or `on M(...):` with `next field = ...` lines")
                        .observed(match (&h.ret, &h.returns) {
                            (Some(_), None) => "`-> T` without `return`".to_owned(),
                            (None, Some(_)) => "`return` without `-> T`".to_owned(),
                            _ if !h.updates.is_empty() && h.returns.is_some() => "both `return` and `next`".to_owned(),
                            _ => "neither `return` nor `next`".to_owned(),
                        }),
                ),
            }
        }
    }

    /// The first call with effects inside `e`, if any.
    fn impure(&self, e: &Expr) -> Option<Span> {
        let mut found = None;
        visit(e, &mut |x| {
            if found.is_some() {
                return;
            }
            match &x.kind {
                ExprKind::Call { callee, .. } => {
                    if let ExprKind::Ident(n) = &callee.kind
                        && (self.models.contains_key(n.as_str())
                            || self.routers.contains_key(n.as_str())
                            || self.tools.contains_key(n.as_str())
                            || self.graphs.contains_key(n.as_str()))
                    {
                        found = Some(x.span);
                    }
                }
                ExprKind::Agent(_)
                | ExprKind::Message(_)
                | ExprKind::Borrow { .. }
                | ExprKind::Receive { .. } => found = Some(x.span),
                _ => {}
            }
        });
        found
    }

    /// `receive M, timeout T:` + `on timeout: value` (decision D21). The run
    /// may stop and wait for days: the deadline goes to the journal.
    fn receive(
        &mut self,
        message: &Ident,
        about: Option<&Expr>,
        timeout: Option<&Expr>,
        on_timeout: Option<&Expr>,
        span: Span,
        gc: &GraphCx,
    ) -> Typed {
        let ty = if self.messages.contains(message.name.as_str()) {
            Ty::User(message.name.clone())
        } else {
            self.push(
                err("E0670", "`receive` needs a `message` type", message.span)
                    .expected("a type declared with `message Name = ...`: what the run may receive from outside")
                    .observed(format!("`{}`", message.name)),
            );
            Ty::Error
        };
        match timeout {
            None => self.push(err("E0671", "`receive` needs a `timeout`", span).expected(
                "`receive M, timeout 3 days:` and `on timeout: value`; a run never waits forever",
            )),
            Some(t) => {
                let ok = matches!(&t.kind, ExprKind::Int { unit: Some(u), .. } if ["s", "min", "h", "days"].contains(&u.as_str()));
                if !ok {
                    self.push(
                        err(
                            "E0671",
                            "the timeout is a duration written in the program",
                            t.span,
                        )
                        .expected("a number of `s`, `min`, `h` or `days`, e.g. `3 days`"),
                    );
                }
            }
        }
        match on_timeout {
            None => self.push(
                err(
                    "E0672",
                    "`receive` must say what happens `on timeout`",
                    span,
                )
                .expected("an indented `on timeout: value` of the message's type"),
            ),
            Some(v) => {
                if let Some(s) = self.impure(v) {
                    self.push(
                        err("E0672", "`on timeout` is a value, not a call", s).expected(
                            "a value of the message's type, e.g. `Denied(reason=\"expirou\")`",
                        ),
                    );
                }
                let t = self.expr(v, gc);
                if !assignable(&t.ty, &ty) {
                    self.push(
                        err(
                            "E0672",
                            "`on timeout` gives a value of the wrong type",
                            v.span,
                        )
                        .expected(format!("`{ty}`"))
                        .observed(format!("`{}`", t.ty)),
                    );
                }
            }
        }
        let effect = about.map_or(Effect::Read, |a| self.expr(a, gc).effect.join(Effect::Read));
        Typed {
            ty,
            kind: NodeKind::Other(format!("receive {}", message.name)),
            effect,
        }
    }

    /// `ask Entity(key).Handler(args)` / `send ...` (decisions D15, D21).
    fn message(&mut self, m: &MessageExpr, gc: &GraphCx) -> Typed {
        let key = self.expr(&m.key, gc);
        let verb = if m.send { "send" } else { "ask" };
        let Some(sig) = self.entities.get(m.entity.name.as_str()) else {
            self.push(
                err("E0655", format!("`{verb}` needs an entity"), m.entity.span)
                    .expected("an entity declared with `entity`")
                    .observed(format!("`{}`", m.entity.name)),
            );
            return Typed::pure(Ty::Error);
        };
        let key_ty = sig.key.clone();
        let Some((_, params, ret)) = sig
            .handlers
            .iter()
            .find(|(n, _, _)| *n == m.handler.name)
            .cloned()
        else {
            let known: Vec<String> = sig
                .handlers
                .iter()
                .map(|(n, _, _)| format!("`{n}`"))
                .collect();
            self.push(
                err(
                    "E0655",
                    "the entity does not handle this message",
                    m.handler.span,
                )
                .expected(format!("one of {}", known.join(", ")))
                .observed(format!("`{}`", m.handler.name)),
            );
            return Typed::pure(Ty::Error);
        };
        if !assignable(&key.ty, &key_ty) {
            self.push(
                err("E0608", "the entity's key has the wrong type", m.key.span)
                    .expected(format!("`{key_ty}`"))
                    .observed(format!("`{}`", key.ty)),
            );
        }
        let callee = format!("{}.{}", m.entity.name, m.handler.name);
        if m.send {
            self.raw_write = true; // a message to an entity is not compensated
        }
        let inner = self.args(&callee, &params, &m.args, m.handler.span, gc);
        let effect = key.effect.join(inner);
        match (m.send, ret) {
            (false, Some(t)) => Typed {
                ty: t,
                kind: NodeKind::Other(format!("ask {callee}")),
                effect: effect.join(Effect::Read),
            },
            (true, None) => Typed {
                ty: Ty::Unit,
                kind: NodeKind::Other(format!("send {callee}")),
                effect: effect.join(Effect::Write),
            },
            (false, None) => {
                self.push(
                    err(
                        "E0656",
                        "`ask` waits for an answer this message does not give",
                        m.handler.span,
                    )
                    .expected(format!(
                        "`send {}(...).{}(...)`: it changes the state and answers nothing",
                        m.entity.name, m.handler.name
                    ))
                    .observed(format!(
                        "`ask` of `on {}(...)` without `-> T`",
                        m.handler.name
                    )),
                );
                Typed::pure(Ty::Error)
            }
            (true, Some(_)) => {
                self.push(
                    err(
                        "E0656",
                        "`send` to a message that only answers",
                        m.handler.span,
                    )
                    .expected(format!(
                        "`ask {}(...).{}(...)`, to use the answer",
                        m.entity.name, m.handler.name
                    ))
                    .observed(format!("`send` to `on {}(...) -> T`", m.handler.name)),
                );
                Typed::pure(Ty::Error)
            }
        }
    }

    /// A `def`'s body: statements that name values, `if`s, and a `return`
    /// at the end. Names may be given new values (each a new value; nothing
    /// changes in place), of the same type.
    fn check_def(&mut self, d: &DefDecl) {
        let sig = &self.defs[d.name.name.as_str()];
        let ret = sig.ret.clone();
        let mut gc = GraphCx::default();
        for (n, t) in &sig.params {
            gc.scope.insert(n.clone(), t.clone());
        }
        let returned = self.def_stmts(&d.body, &mut gc, &ret, true);
        if !returned {
            self.push(
                err("E0665", "a `def` ends with `return`", d.name.span)
                    .expected("`return value` as the last line of the body"),
            );
        }
    }

    /// Checks `stmts`, updating `gc`; whether they ended with `return`.
    fn def_stmts(&mut self, stmts: &[DefStmt], gc: &mut GraphCx, ret: &Ty, top: bool) -> bool {
        for (i, s) in stmts.iter().enumerate() {
            let last = i + 1 == stmts.len();
            match s {
                DefStmt::Assign(name, e) => {
                    let t = self.pure_expr(e, gc);
                    // Lists grow: `problems = []`, then `problems = problems + [...]`.
                    // A name keeps its element type, not its length.
                    if let (Some(Ty::List(old, _)), Ty::List(new, _)) =
                        (gc.scope.get(&name.name), &t)
                    {
                        let elem = match (old.as_ref(), new.as_ref()) {
                            (Ty::Error, n) => Some(n.clone()),
                            (o, n) if assignable(n, o) => Some(o.clone()),
                            _ => None,
                        };
                        if let Some(elem) = elem {
                            gc.scope
                                .insert(name.name.clone(), Ty::List(Box::new(elem), None));
                            continue;
                        }
                    }
                    match gc.scope.get(&name.name) {
                        Some(old) if !assignable(&t, old) && !matches!(t, Ty::Error) => {
                            self.push(
                                err("E0664", "a name keeps its type", e.span)
                                    .expected(format!("a `{old}`, as `{}` was before", name.name))
                                    .observed(format!(
                                        "`{t}`; use another name for a value of another type",
                                    )),
                            );
                        }
                        Some(_) => {}
                        None => {
                            gc.scope.insert(name.name.clone(), t);
                        }
                    }
                }
                DefStmt::If {
                    cond, then, els, ..
                } => {
                    let c = self.pure_expr(cond, gc);
                    self.expect_bool(&c, cond.span);
                    let mut g1 = gc.clone();
                    let mut g2 = gc.clone();
                    self.def_stmts(then, &mut g1, ret, false);
                    self.def_stmts(els, &mut g2, ret, false);
                    // A new name is visible after the `if` only if every
                    // branch gives it a value; otherwise it stays inside.
                    for (n, t) in &g1.scope {
                        if !gc.scope.contains_key(n) && g2.scope.contains_key(n) {
                            gc.scope.insert(n.clone(), t.clone());
                        }
                    }
                }
                DefStmt::Return(e) => {
                    let t = self.pure_expr(e, gc);
                    if !assignable(&t, ret) {
                        self.push(
                            err("E0610", "returned value has the wrong type", e.span)
                                .expected(format!("`{ret}`"))
                                .observed(format!("`{t}`")),
                        );
                    }
                    if !(top && last) {
                        self.push(
                            err("E0665", "`return` only at the end of a `def`", e.span)
                                .expected("one `return`, the last line; inside `if`, give a name a value instead"),
                        );
                    }
                    return top && last;
                }
            }
        }
        false
    }

    /// An expression in a pure place: no calls with effects (E0660).
    fn pure_expr(&mut self, e: &Expr, gc: &GraphCx) -> Ty {
        if let Some(span) = self.impure(e) {
            self.push(
                err("E0660", "a `def` cannot call models, tools, graphs or entities", span)
                    .expected("a value computed from the parameters; make the calls in a graph and pass the results")
                    .observed("a call with effects"),
            );
            return Ty::Error;
        }
        self.expr(e, gc).ty
    }

    /// `def`s that call themselves, directly or through others (E0661):
    /// without recursion, every `def` finishes (decision D17).
    fn no_recursive_defs(&mut self, program: &Program) {
        let mut calls: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut spans: HashMap<&str, Span> = HashMap::new();
        for d in &program.decls {
            let Decl::Def(f) = d else { continue };
            spans.insert(&f.name.name, f.name.span);
            let mut out = Vec::new();
            for e in def_exprs(&f.body) {
                visit(e, &mut |x| {
                    if let ExprKind::Call { callee, .. } = &x.kind
                        && let ExprKind::Ident(n) = &callee.kind
                        && let Some((k, _)) = self.defs.get_key_value(n.as_str())
                    {
                        out.push(*k);
                    }
                });
            }
            calls.insert(&f.name.name, out);
        }
        let mut reported = HashSet::new();
        let mut names: Vec<&str> = calls.keys().copied().collect();
        names.sort_unstable();
        for start in names {
            // Can `start` reach itself?
            let mut seen = HashSet::new();
            let mut stack: Vec<&str> = calls[start].clone();
            while let Some(n) = stack.pop() {
                if n == start {
                    if reported.insert(start) {
                        self.push(
                            err("E0661", "a `def` cannot call itself", spans[start])
                                .expected("a `def` without recursion (it always finishes); for repetition, a `loop` in a graph, or a recursive graph with `decreases`")
                                .observed(format!("`{start}` calls itself, directly or through other `def`s")),
                        );
                    }
                    break;
                }
                if seen.insert(n) {
                    stack.extend(calls.get(n).cloned().unwrap_or_default());
                }
            }
        }
    }

    /// `len`, `take`, `sum`, `join`, `lower`, `upper`, `trim`.
    fn builtin(&mut self, name: &str, args: &[Arg], span: Span, gc: &GraphCx) -> Option<Typed> {
        let usage = BUILTINS.iter().find(|(b, _)| *b == name)?.1;
        let typed: Vec<Typed> = args.iter().map(|a| self.expr(&a.value, gc)).collect();
        let effect = typed
            .iter()
            .map(|t| t.effect)
            .fold(Effect::Pure, Effect::join);
        let tys: Vec<Ty> = typed.into_iter().map(|t| t.ty).collect();
        let named = args.iter().any(|a| a.name.is_some());
        let int = |t: &Ty| matches!(t, Ty::Nat | Ty::Int | Ty::IntLit | Ty::Error);
        let ty = match (name, tys.as_slice()) {
            _ if named => None,
            ("len", [Ty::List(..) | Ty::Text | Ty::Error]) => Some(Ty::Nat),
            ("take", [l @ (Ty::List(..) | Ty::Error), n]) if int(n) => Some(l.clone()),
            ("sum", [Ty::List(t, _)])
                if is_number(t) || matches!(**t, Ty::Money | Ty::Duration) =>
            {
                Some(if **t == Ty::IntLit {
                    Ty::Int
                } else {
                    (**t).clone()
                })
            }
            ("sum", [Ty::Error]) => Some(Ty::Error),
            ("join", [l, Ty::Text | Ty::Error])
                if matches!(l, Ty::Error)
                    || matches!(l, Ty::List(t, _) if matches!(**t, Ty::Text | Ty::Error)) =>
            {
                Some(Ty::Text)
            }
            ("lower" | "upper" | "trim", [Ty::Text | Ty::Error]) => Some(Ty::Text),
            _ => None,
        };
        let ty = ty.unwrap_or_else(|| {
            let observed: Vec<String> = tys.iter().map(|t| format!("`{t}`")).collect();
            self.push(
                err(
                    "E0662",
                    format!("`{name}` does not take these arguments"),
                    span,
                )
                .expected(usage)
                .observed(format!("({})", observed.join(", "))),
            );
            Ty::Error
        });
        Some(Typed {
            ty,
            kind: NodeKind::Pure,
            effect,
        })
    }

    /// `reads` / `edits` belong to tool parameters only.
    fn no_borrows(&mut self, params: &[Param]) {
        for p in params {
            if let Some(m) = &p.borrow {
                self.push(
                    err("E0646", "only tools borrow resources", m.span)
                        .expected(format!(
                            "`{}: Sandbox` here; tools say `reads` or `edits`",
                            p.name.name
                        ))
                        .observed(format!("`{}`", m.name)),
                );
            }
        }
    }

    /// Checks what a tool's properties name, once every tool is known:
    /// `on_uncertain`, `checks` and `idempotency_key` (decisions D2, D29).
    fn check_write_contracts(&mut self, program: &'p Program) {
        for d in &program.decls {
            let Decl::Tool(t) = d else { continue };
            let Some(sig) = self.tools.get(t.name.name.as_str()) else {
                continue;
            };
            let mut out = Vec::new();
            if sig.effect == Effect::Write && !sig.keyed {
                out.push(
                    warn("W0601", "`write` tool without `idempotency_key`", t.name.span)
                        .expected("`idempotency_key param`, so retries and resumed runs cannot apply it twice")
                        .observed("no key: the runtime repeats the call after failures, so the tool itself must be idempotent"),
                );
            }
            if let Some((policy, span)) = &sig.policy {
                if sig.effect != Effect::WriteOnce {
                    out.push(
                        err(
                            "E0641",
                            "`on_uncertain` is only for `write once` tools",
                            *span,
                        )
                        .expected("`effect write once`, or no `on_uncertain`")
                        .observed(format!("`effect {}`", sig.effect)),
                    );
                }
                // A `verify` that finds what the call made gives its answer.
                let finds = matches!(policy, Policy::Verify { tool, .. }
                    if self.tools.get(tool.name.as_str())
                        .is_some_and(|v| matches!(&v.ret, Ty::List(r, _) if assignable(r, &sig.ret))));
                let takes_as_done = !matches!(policy, Policy::Pause) && !finds;
                if takes_as_done && !matches!(sig.ret, Ty::Unit | Ty::Error) {
                    out.push(
                        err("E0634", "this policy needs a tool that returns `Unit`", *span)
                            .expected("`-> Unit`: the run goes on as if the call happened, with no answer to use")
                            .observed(format!(
                                "`-> {}`; use `on_uncertain pause`, or a `verify` tool that returns `List[{}]`",
                                sig.ret, sig.ret
                            )),
                    );
                }
                if let Policy::Verify { tool, args } = policy {
                    out.extend(self.check_verify(sig, tool, args));
                }
            }
            if let Some((tool, args)) = &sig.compensate {
                out.extend(self.check_compensate(sig, tool, args));
            }
            if let Some(c) = &sig.checks
                && !matches!(self.types.get(c.name.as_str()), Some(UserType::Record(_)))
            {
                out.push(
                    err("E0635", "`checks` must name a record type", c.span)
                        .expected("a `type` with fields: the state the tool validates")
                        .observed(format!("`{}`", c.name)),
                );
            }
            for d in out {
                self.push(d);
            }
        }
    }

    /// `compensate f(a, b)` (D12): `f` undoes a call of a write tool, and
    /// may itself be made again after a crash, so it is a keyed `write`;
    /// its arguments are parameters of the tool it undoes.
    fn check_compensate(&self, sig: &ToolSig, tool: &Ident, args: &[Ident]) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if sig.effect < Effect::Write {
            out.push(
                err("E0695", "only a write can be compensated", tool.span)
                    .expected("`effect write` or `effect write once`")
                    .observed(format!("`effect {}`", sig.effect)),
            );
        }
        let Some(c) = self.tools.get(tool.name.as_str()) else {
            out.push(
                err("E0695", "`compensate` must call a tool", tool.span)
                    .expected("a `write` tool with an `idempotency_key` that undoes the call")
                    .observed(format!("`{}`", tool.name)),
            );
            return out;
        };
        if c.effect != Effect::Write || !c.keyed {
            out.push(
                err("E0695", "a compensation is a `write` with an `idempotency_key`", tool.span)
                    .expected("`effect write` and `idempotency_key`: after a crash it is sent again, and must not undo twice")
                    .observed(format!(
                        "`{}` is `{}`{}",
                        tool.name,
                        c.effect,
                        if c.keyed { "" } else { " without a key" }
                    )),
            );
        }
        if args.len() != c.params.len() {
            out.push(
                err(
                    "E0696",
                    "wrong number of arguments for the compensation",
                    tool.span,
                )
                .expected(format!("{}", c.params.len()))
                .observed(format!("{}", args.len())),
            );
        }
        for (a, (pname, pty)) in args.iter().zip(&c.params) {
            match sig.params.iter().find(|(n, _)| *n == a.name) {
                None => out.push(
                    err(
                        "E0696",
                        "compensation arguments must be parameters of the tool",
                        a.span,
                    )
                    .expected("a parameter of the tool it undoes: the same call, undone")
                    .observed(format!("`{}`", a.name)),
                ),
                Some((_, ty)) if !assignable(ty, pty) => out.push(
                    err("E0696", "compensation argument has the wrong type", a.span)
                        .expected(format!("`{pty}` for `{pname}`"))
                        .observed(format!("`{ty}`")),
                ),
                Some(_) => {}
            }
        }
        out
    }

    fn check_verify(&self, sig: &ToolSig, tool: &Ident, args: &[Ident]) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let Some(v) = self.tools.get(tool.name.as_str()) else {
            out.push(
                err("E0631", "`verify` must call a tool", tool.span)
                    .expected("a `read` tool that tells whether the call happened")
                    .observed(format!("`{}`", tool.name)),
            );
            return out;
        };
        if v.effect != Effect::Read {
            out.push(
                err("E0631", "`verify` must call a `read` tool", tool.span)
                    .expected("`effect read`: checking must not change anything")
                    .observed(format!("`{}` is `{}`", tool.name, v.effect)),
            );
        }
        // `Bool`: whether the call happened. `List[R]`: what it made, found
        // again (empty if it did not happen), so its answer can be used.
        // A batch: `List[T]`, the items already applied.
        let finds = match &sig.batch {
            Some(item) => matches!(&v.ret, Ty::List(r, _) if assignable(item, r)),
            None => matches!(&v.ret, Ty::List(r, _) if assignable(r, &sig.ret)),
        };
        if let Some(item) = &sig.batch {
            if !finds && !matches!(v.ret, Ty::Error) {
                out.push(
                    err(
                        "E0633",
                        "the `verify` tool of a batch returns the items already applied",
                        tool.span,
                    )
                    .expected(format!("`-> List[{item}]`"))
                    .observed(format!("`-> {}`", v.ret)),
                );
            }
        } else if !finds && !matches!(v.ret, Ty::Bool | Ty::Error) {
            let expected = if matches!(sig.ret, Ty::Unit) {
                "`-> Bool`: whether the call happened".to_owned()
            } else {
                format!(
                    "`-> Bool` (whether the call happened) or `-> List[{}]` (what it made, found again)",
                    sig.ret
                )
            };
            out.push(
                err(
                    "E0633",
                    "`verify` tool must return `Bool` or a list of the tool's answer",
                    tool.span,
                )
                .expected(expected)
                .observed(format!("`-> {}`", v.ret)),
            );
        }
        if args.len() != v.params.len() {
            out.push(
                err(
                    "E0632",
                    "wrong number of arguments for the `verify` tool",
                    tool.span,
                )
                .expected(format!("{}", v.params.len()))
                .observed(format!("{}", args.len())),
            );
        }
        for (a, (pname, pty)) in args.iter().zip(&v.params) {
            match sig.params.iter().find(|(n, _)| *n == a.name) {
                None => out.push(
                    err(
                        "E0632",
                        "`verify` arguments must be parameters of the tool",
                        a.span,
                    )
                    .expected("a parameter of the `write once` tool: the same call, looked up")
                    .observed(format!("`{}`", a.name)),
                ),
                Some((_, ty)) if !assignable(ty, pty) => out.push(
                    err("E0632", "`verify` argument has the wrong type", a.span)
                        .expected(format!("`{pty}` for `{pname}`"))
                        .observed(format!("`{ty}`")),
                ),
                Some(_) => {}
            }
        }
        out
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
    /// planned for a later milestone (decision D17) and reported here.
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
                        "recursive graphs are not supported yet (planned for a later milestone)",
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
        self.raw_write = false;
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
        let mut afters: Vec<(&Ident, &Vec<Ident>)> = Vec::new();
        let mut unordered: Vec<&Vec<Ident>> = Vec::new();
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
                Stmt::After { node, after } => afters.push((node, after)),
                Stmt::Unordered(steps) => unordered.push(steps),
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
        let mut deps: Vec<Vec<usize>> = locals
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
        // What each step reads, before ordering edges are added.
        let data_deps = deps.clone();
        // `a after b`: an ordering edge without data (decision D2).
        for (node, after) in &afters {
            let step = |cx: &mut Self, id: &Ident| match index.get(id.name.as_str()) {
                Some(&i) => Some(i),
                None => {
                    cx.push(
                        err(
                            "E0639",
                            "`after` names something that is not a step",
                            id.span,
                        )
                        .expected("a step of this graph (`name = ...`)")
                        .observed(format!("`{}`", id.name)),
                    );
                    None
                }
            };
            let Some(i) = step(self, node) else { continue };
            for a in after.iter() {
                if a.name == node.name {
                    self.push(
                        err("E0639", "a step cannot come after itself", a.span)
                            .observed(format!("`{} after {}`", node.name, a.name)),
                    );
                } else if let Some(j) = step(self, a) {
                    deps[i].push(j);
                }
            }
            deps[i].sort_unstable();
            deps[i].dedup();
        }
        // Sandboxes have one owner at a time (D26): a step that edits one
        // comes after every earlier step that borrows it; a step that reads
        // one, after every earlier step that edits it. No `after` needed.
        let sandboxes: HashSet<&str> = sig_params
            .iter()
            .filter(|(_, t)| *t == Ty::Sandbox)
            .map(|(n, _)| n.as_str())
            .collect();
        // Messages to an entity follow the same rule, as `@Entity`: a `send`
        // after every earlier message to it, an `ask` after every earlier
        // `send` (the run sees its own changes).
        let mut lent: Vec<Vec<(String, bool)>> = Vec::with_capacity(locals.len());
        for l in &locals {
            let mut out = Vec::new();
            {
                if let Some((_, over)) = l.fan_out {
                    self.borrows(over, &sandboxes, &mut out);
                }
                self.borrows(l.value, &sandboxes, &mut out);
                if let Some((var, _)) = l.fan_out
                    && let Some((s, _)) = out.iter().find(|(s, e)| *e && !s.starts_with('@'))
                {
                    self.push(
                        err(
                            "E0644",
                            "items of a `for each` cannot edit the same sandbox",
                            var.span,
                        )
                        .expected(format!(
                            "`reads {s}` in the items, or the edits in a step of their own"
                        ))
                        .observed(format!("`edits {s}` in every item, all at the same time")),
                    );
                }
            }
            lent.push(out);
        }
        for i in 0..locals.len() {
            for j in 0..i {
                let conflict = lent[i]
                    .iter()
                    .any(|(s, ei)| lent[j].iter().any(|(t, ej)| s == t && (*ei || *ej)));
                if conflict {
                    deps[i].push(j);
                }
            }
            deps[i].sort_unstable();
            deps[i].dedup();
        }
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
            if matches!(ty, Ty::Sandbox | Ty::Lent(_)) {
                self.push(
                    err("E0647", "a sandbox is not a value a step can keep", l.name.span)
                        .expected("lend it to tools where it is used: `tool(reads repo)` or `tool(edits repo)`")
                        .observed(format!("`{}` would be a `{ty}`", l.name.name)),
                );
            }
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
                over: None,
                value: None,
                rank: 0.0,
                nlocals: 0,
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
                if matches!(t.ty, Ty::Lent(_)) {
                    self.push(
                        err(
                            "E0647",
                            "a lent sandbox only goes to a tool that borrows it",
                            r.span,
                        )
                        .observed(format!("`return` of `{}`", t.ty)),
                    );
                }
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
                    let mut ret_lent = Vec::new();
                    self.borrows(r, &sandboxes, &mut ret_lent);
                    for (i, l) in lent.iter().enumerate() {
                        if l.iter()
                            .any(|(s, e)| ret_lent.iter().any(|(t, f)| s == t && (*e || *f)))
                        {
                            refs.push(i);
                        }
                    }
                    let id = NodeId(nodes.len() as u32);
                    nodes.push(ir::Node {
                        id,
                        name: "return".into(),
                        ty: t.ty.to_string(),
                        kind: t.kind,
                        fan_out: None,
                        effect: t.effect,
                        inputs: refs.iter().filter_map(|d| ids.get(d).copied()).collect(),
                        over: None,
                        value: None,
                        rank: 0.0,
                        nlocals: 0,
                    });
                    output = Some(id);
                }
            }
        }

        let commute = self.commuting(&unordered, &index, &deps);
        self.unordered_writes(&locals, &deps, &ids, &nodes, &commute);
        self.lost_updates(&locals, &data_deps);

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
        if std::mem::take(&mut self.raw_write) {
            self.raw_write_graphs.insert(g.name.name.clone());
        }

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
            limits: ir::Limits::default(),
        }
    }

    /// The sandboxes `e` borrows, and whether it edits each: `reads x` /
    /// `edits x` (also lent to an agent's tools), and a sandbox passed to a
    /// subgraph, which owns it for the call. Reports two edits of one
    /// sandbox that could run at the same time (E0645).
    fn borrows(&mut self, e: &Expr, sandboxes: &HashSet<&str>, out: &mut Vec<(String, bool)>) {
        let found = self.borrows_in(e, sandboxes);
        for b in found {
            if !out.contains(&b) {
                out.push(b);
            }
        }
    }

    fn borrows_in(&mut self, e: &Expr, sandboxes: &HashSet<&str>) -> Vec<(String, bool)> {
        let one = |s: &str, edits: bool| vec![(s.to_owned(), edits)];
        let children: Vec<&Expr> = match &e.kind {
            ExprKind::Borrow { mode, target } if sandboxes.contains(target.name.as_str()) => {
                return one(&target.name, mode.name == "edits");
            }
            ExprKind::Message(m) => {
                let mut v = vec![(format!("@{}", m.entity.name), m.send)];
                v.extend(self.borrows_in(&m.key, sandboxes));
                for a in &m.args {
                    v.extend(self.borrows_in(&a.value, sandboxes));
                }
                return v;
            }
            ExprKind::Call { callee, args } => {
                let to_graph = matches!(&callee.kind, ExprKind::Ident(n) if self.graphs.contains_key(n.as_str()));
                let mut kids = Vec::new();
                let mut passed = Vec::new();
                for a in args {
                    match &a.value.kind {
                        ExprKind::Ident(n) if to_graph && sandboxes.contains(n.as_str()) => {
                            passed.push((n.clone(), true));
                        }
                        _ => kids.push(&a.value),
                    }
                }
                let mut groups: Vec<Vec<(String, bool)>> =
                    kids.iter().map(|k| self.borrows_in(k, sandboxes)).collect();
                groups.extend(passed.into_iter().map(|p| vec![p]));
                return self.parallel(groups, e.span);
            }
            ExprKind::List(items) => items.iter().collect(),
            ExprKind::Field { base, .. } => vec![base],
            ExprKind::Binary { left, right, .. } => vec![left, right],
            ExprKind::Unary { value, .. }
            | ExprKind::Done(value)
            | ExprKind::Next(value)
            | ExprKind::Try(value) => vec![value],
            ExprKind::Guarded { call, .. } => vec![call],
            // Branches exclude each other; loop turns follow each other.
            ExprKind::If { cond, then, els } => {
                let mut v = self.borrows_in(cond, sandboxes);
                for b in [then, els] {
                    for x in self.borrows_in(b, sandboxes) {
                        if !v.contains(&x) {
                            v.push(x);
                        }
                    }
                }
                return v;
            }
            ExprKind::Match { value, cases } => {
                let mut v = self.borrows_in(value, sandboxes);
                for c in cases {
                    for x in self.borrows_in(&c.body, sandboxes) {
                        if !v.contains(&x) {
                            v.push(x);
                        }
                    }
                }
                return v;
            }
            ExprKind::Loop { init, body, .. } => {
                let mut v = self.borrows_in(init, sandboxes);
                v.extend(self.borrows_in(body, sandboxes));
                return v;
            }
            // Steps run in order, as written.
            ExprKind::Block { steps, tail } => {
                let mut v = Vec::new();
                for x in steps
                    .iter()
                    .map(|(_, s)| s)
                    .chain(std::iter::once(tail.as_ref()))
                {
                    for b in self.borrows_in(x, sandboxes) {
                        if !v.contains(&b) {
                            v.push(b);
                        }
                    }
                }
                return v;
            }
            // Every item at once: an edit would clash with itself.
            ExprKind::Each { over, body, .. } => {
                let o = self.borrows_in(over, sandboxes);
                let b = self.borrows_in(body, sandboxes);
                return self.parallel(vec![o, b.clone(), b], e.span);
            }
            // Every branch at once.
            ExprKind::Race(r) => {
                let groups = r
                    .branches
                    .iter()
                    .map(|(_, v)| self.borrows_in(v, sandboxes))
                    .collect();
                return self.parallel(groups, e.span);
            }
            // The runtime runs an agent's calls on one sandbox one at a time.
            ExprKind::Agent(a) => {
                let mut v = Vec::new();
                for t in &a.tools {
                    for l in &t.lends {
                        v.extend(self.borrows_in(l, sandboxes));
                    }
                }
                if let Some(t) = &a.task {
                    v.extend(self.borrows_in(t, sandboxes));
                }
                return v;
            }
            _ => Vec::new(),
        };
        let groups = children
            .into_iter()
            .map(|c| self.borrows_in(c, sandboxes))
            .collect();
        self.parallel(groups, e.span)
    }

    /// Borrows of parts that run at the same time: two of them editing the
    /// same sandbox is an error, since their order would be left to chance.
    fn parallel(&mut self, groups: Vec<Vec<(String, bool)>>, span: Span) -> Vec<(String, bool)> {
        let mut all: Vec<(String, bool)> = Vec::new();
        let mut reported = HashSet::new();
        for (k, g) in groups.iter().enumerate() {
            for (s, edits) in g {
                // Messages to one entity in one expression: the runtime
                // applies them one at a time, in either order.
                let clash = !s.starts_with('@')
                    && groups[..k]
                        .iter()
                        .any(|h| h.iter().any(|(t, e2)| t == s && (*edits || *e2)));
                if clash && reported.insert(s.clone()) {
                    self.push(
                        err(
                            "E0645",
                            "two uses of a sandbox that could run at the same time, one editing it",
                            span,
                        )
                        .expected(format!(
                            "the edit of `{s}` in a step of its own, so the order is defined"
                        ))
                        .observed(format!(
                            "`{s}` borrowed twice in one expression, at least once with `edits`"
                        )),
                    );
                }
            }
            for b in g {
                if !all.contains(b) {
                    all.push(b.clone());
                }
            }
        }
        all
    }

    /// `unordered a, b`: pairs of steps whose writes commute. Each name must
    /// be a step (`E0639`), and steps that already wait for one another
    /// cannot be unordered (`E0507`).
    fn commuting(
        &mut self,
        unordered: &[&Vec<Ident>],
        index: &HashMap<&str, usize>,
        deps: &[Vec<usize>],
    ) -> HashSet<(usize, usize)> {
        let reaches = |from: usize, to: usize| {
            let mut seen = HashSet::new();
            let mut stack = deps[from].clone();
            while let Some(d) = stack.pop() {
                if d == to {
                    return true;
                }
                if seen.insert(d) {
                    stack.extend(deps[d].iter().copied());
                }
            }
            false
        };
        let mut pairs = HashSet::new();
        for steps in unordered {
            let mut found: Vec<(usize, &Ident)> = Vec::new();
            for s in steps.iter() {
                match index.get(s.name.as_str()) {
                    Some(&i) => found.push((i, s)),
                    None => self.push(
                        err(
                            "E0639",
                            "`unordered` names something that is not a step",
                            s.span,
                        )
                        .expected("a step of this graph (`name = ...`)")
                        .observed(format!("`{}`", s.name)),
                    ),
                }
            }
            if steps.len() < 2 {
                self.push(
                    err(
                        "E0507",
                        "`unordered` needs two steps or more",
                        steps[0].span,
                    )
                    .expected("`unordered a, b`: the steps whose writes commute"),
                );
            }
            for (k, &(b, bn)) in found.iter().enumerate() {
                for &(a, an) in &found[..k] {
                    if reaches(a, b) || reaches(b, a) {
                        self.push(
                            err("E0507", "steps declared `unordered` are ordered", bn.span)
                                .expected(format!(
                                    "`{}` and `{}` independent: neither reads the other, and no `after` between them",
                                    an.name, bn.name
                                ))
                                .observed("one of them waits for the other".to_owned()),
                        );
                    }
                    pairs.insert((a.min(b), a.max(b)));
                }
            }
        }
        pairs
    }

    /// Two steps that write outside the run with no order between them may
    /// run at the same time, in either order (decision D2), unless the
    /// program says their writes commute (`unordered`).
    fn unordered_writes(
        &mut self,
        locals: &[Local],
        deps: &[Vec<usize>],
        ids: &HashMap<usize, NodeId>,
        nodes: &[ir::Node],
        commute: &HashSet<(usize, usize)>,
    ) {
        let writes: Vec<usize> = (0..locals.len())
            .filter(|i| {
                ids.get(i)
                    .is_some_and(|id| nodes[id.0 as usize].effect >= Effect::Write)
            })
            .collect();
        if writes.len() < 2 {
            return;
        }
        // Everything each step waits for, directly or not.
        let before = |start: usize| {
            let mut seen = HashSet::new();
            let mut stack = deps[start].clone();
            while let Some(d) = stack.pop() {
                if seen.insert(d) {
                    stack.extend(deps[d].iter().copied());
                }
            }
            seen
        };
        let befores: HashMap<usize, HashSet<usize>> =
            writes.iter().map(|&w| (w, before(w))).collect();
        for (k, &b) in writes.iter().enumerate() {
            if let Some(&a) = writes[..k].iter().find(|&&a| {
                !befores[&b].contains(&a)
                    && !befores[&a].contains(&b)
                    && !commute.contains(&(a.min(b), a.max(b)))
            }) {
                let (an, bn) = (&locals[a].name.name, &locals[b].name.name);
                self.push(
                    warn("W0602", "external writes without a defined order", locals[b].name.span)
                        .expected(format!("`{bn} after {an}` (or `{an} after {bn}`), a value one passes to the other, or `unordered {an}, {bn}` if the writes commute"))
                        .observed(format!("`{an}` and `{bn}` both write outside the run and may run at the same time")),
                );
            }
        }
    }

    /// `send E(k).M(f(ask E(k).N()))`: the run reads the entity, computes
    /// and sends the result back. Another run can change the entity in
    /// between, and one of the two updates is lost. The computation belongs
    /// in a handler, which sees the current state (decision D15).
    fn lost_updates(&mut self, locals: &[Local], deps: &[Vec<usize>]) {
        let changes: HashSet<(String, String)> = self
            .entities
            .iter()
            .flat_map(|(e, s)| s.changes.iter().map(|h| (e.to_string(), h.clone())))
            .collect();
        let messages = |e: &Expr, send: bool| {
            let mut out: Vec<(String, Span)> = Vec::new();
            visit(e, &mut |x| {
                if let ExprKind::Message(m) = &x.kind
                    && m.send == send
                    // A handler that applies a change sees the current
                    // state: nothing is lost (see `EntitySig::changes`).
                    && !(send
                        && changes.contains(&(m.entity.name.clone(), m.handler.name.clone())))
                {
                    out.push((m.entity.name.clone(), x.span));
                }
            });
            out
        };
        let asks: Vec<Vec<(String, Span)>> =
            locals.iter().map(|l| messages(l.value, false)).collect();
        for (i, l) in locals.iter().enumerate() {
            let sends = messages(l.value, true);
            if sends.is_empty() {
                continue;
            }
            // Everything this step's value comes from, itself included.
            let mut seen = HashSet::from([i]);
            let mut stack = deps[i].clone();
            while let Some(d) = stack.pop() {
                if seen.insert(d) {
                    stack.extend(deps[d].iter().copied());
                }
            }
            for (entity, span) in &sends {
                let read = seen.iter().find_map(|&j| {
                    asks[j]
                        .iter()
                        .find(|(e, _)| e == entity)
                        .map(|_| locals[j].name.name.clone())
                });
                if let Some(from) = read {
                    self.push(
                        warn("W0603", "the run reads an entity and sends back a value computed from it", *span)
                            .expected(format!("a handler of `{entity}` that computes the new value from its current state, e.g. `next count = count + 1`"))
                            .observed(format!("`{from}` asks `{entity}`, and this `send` depends on it: another run can change `{entity}` in between, and one update is lost")),
                    );
                }
            }
        }
    }

    fn is_global(&self, n: &str) -> bool {
        self.defs.contains_key(n)
            || self.entities.contains_key(n)
            || self.models.contains_key(n)
            || self.routers.contains_key(n)
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
                .expected("a graph without cycles (use `loop` for repetition)")
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
            ExprKind::Guarded { call, requires } => self.guarded(call, requires, gc),
            ExprKind::Message(m) => self.message(m, gc),
            ExprKind::Bool(_) => Typed::pure(Ty::Bool),
            ExprKind::Block { steps, tail } => {
                let (inner, effect) = self.steps(steps, gc);
                let t = self.expr(tail, &inner);
                Typed {
                    ty: t.ty,
                    kind: NodeKind::Other("steps".into()),
                    effect: effect.join(t.effect),
                }
            }
            ExprKind::Each { var, over, body } => {
                let o = self.expr(over, gc);
                let (elem, max) = match o.ty {
                    Ty::List(t, m) => (*t, m),
                    Ty::Error => (Ty::Error, None),
                    other => {
                        self.push(
                            err(
                                "E0609",
                                "`for each` over a value that is not a list",
                                over.span,
                            )
                            .expected("a list")
                            .observed(format!("`{other}`")),
                        );
                        (Ty::Error, None)
                    }
                };
                let mut inner = gc.clone();
                self.bind(&mut inner, var, elem);
                let b = self.expr(body, &inner);
                Typed {
                    ty: Ty::List(Box::new(b.ty), max),
                    kind: NodeKind::Other("for each".into()),
                    effect: o.effect.join(b.effect),
                }
            }
            ExprKind::Race(r) => self.race(r, e.span, gc),
            ExprKind::Receive {
                message,
                about,
                timeout,
                on_timeout,
            } => self.receive(
                message,
                about.as_deref(),
                timeout.as_deref(),
                on_timeout.as_deref(),
                e.span,
                gc,
            ),
            ExprKind::Comprehension {
                body,
                var,
                over,
                cond,
            } => {
                let o = self.expr(over, gc);
                let (elem, max) = match o.ty {
                    Ty::List(t, m) => (*t, m),
                    Ty::Error => (Ty::Error, None),
                    other => {
                        self.push(
                            err("E0609", "`for` over a value that is not a list", over.span)
                                .expected("a list")
                                .observed(format!("`{other}`")),
                        );
                        (Ty::Error, None)
                    }
                };
                let mut inner = gc.clone();
                inner.scope.insert(var.name.clone(), elem);
                if let Some(c) = cond {
                    let t = self.expr(c, &inner);
                    self.expect_bool(&t.ty, c.span);
                }
                let b = self.expr(body, &inner);
                let calls = self
                    .impure(body)
                    .or_else(|| cond.as_ref().and_then(|c| self.impure(c)));
                if let Some(span) = calls {
                    self.push(
                        err("E0663", "a list built with `for` is pure", span)
                            .expected(
                                "`name = for each x in list: ...` for calls, one step per item",
                            )
                            .observed("a call with effects inside `[... for ...]`"),
                    );
                }
                Typed {
                    ty: Ty::List(Box::new(b.ty), max),
                    kind: NodeKind::Pure,
                    effect: o.effect,
                }
            }
            ExprKind::Borrow { mode, target } => match gc.scope.get(&target.name) {
                Some(Ty::Sandbox) => Typed::pure(Ty::Lent(mode.name == "edits")),
                Some(Ty::Error) => Typed::pure(Ty::Error),
                Some(other) => {
                    self.push(
                        err("E0646", "only a `Sandbox` can be lent", target.span)
                            .expected("a `Sandbox` parameter of the graph")
                            .observed(format!("`{}` is `{other}`", target.name)),
                    );
                    Typed::pure(Ty::Error)
                }
                None => {
                    self.push(
                        err("E0602", "unknown name", target.span)
                            .expected("a `Sandbox` parameter of the graph")
                            .observed(format!("`{}`", target.name)),
                    );
                    Typed::pure(Ty::Error)
                }
            },
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
            ExprKind::Binary { op, left, right } => self.binary(op, left, right, e.span, gc),
            ExprKind::Unary { op, value } => {
                let t = self.expr(value, gc);
                let ok = match op.as_str() {
                    "not" => matches!(t.ty, Ty::Bool | Ty::Error),
                    _ => is_number(&t.ty) || matches!(t.ty, Ty::Money | Ty::Duration),
                };
                if !ok {
                    self.push(
                        err("E0613", "operator does not apply to this type", e.span)
                            .expected(if op == "not" { "a `Bool`" } else { "a number" })
                            .observed(format!("`{}`", t.ty)),
                    );
                }
                let ty = match (op.as_str(), &t.ty) {
                    ("not", _) => Ty::Bool,
                    (_, Ty::Nat | Ty::IntLit) => Ty::Int,
                    (_, other) => other.clone(),
                };
                Typed {
                    ty,
                    kind: NodeKind::Pure,
                    effect: t.effect,
                }
            }
            ExprKind::If { cond, then, els } => {
                let c = self.expr(cond, gc);
                self.expect_bool(&c.ty, cond.span);
                let a = self.expr(then, gc);
                let b = self.expr(els, gc);
                let ty = self.same_type(&a.ty, &b.ty, els.span);
                Typed {
                    ty,
                    kind: NodeKind::Other("if".into()),
                    effect: c.effect.join(a.effect).join(b.effect),
                }
            }
            ExprKind::Match { value, cases } => {
                let (v, branches) = self.match_cases(value, cases, e.span, gc);
                let mut effect = v.effect;
                let mut ty: Option<Ty> = None;
                for (case, inner) in cases.iter().zip(branches) {
                    let t = self.expr(&case.body, &inner);
                    effect = effect.join(t.effect);
                    ty = Some(match ty {
                        None => t.ty,
                        Some(first) => self.same_type(&first, &t.ty, case.body.span),
                    });
                }
                Typed {
                    ty: ty.unwrap_or(Ty::Error),
                    kind: NodeKind::Other("match".into()),
                    effect,
                }
            }
            ExprKind::Loop {
                var,
                init,
                body,
                on_limit,
                rounds,
                ..
            } => {
                let i = self.expr(init, gc);
                // `loop i = 0`: the value is an `Int`, not just the literal.
                // A list carried from turn to turn may change size.
                let var_ty = match &i.ty {
                    Ty::IntLit => Ty::Int,
                    Ty::List(t, _) => Ty::List(t.clone(), None),
                    t => t.clone(),
                };
                let mut inner = gc.clone();
                self.bind(&mut inner, var, var_ty.clone());
                let mut lp = LoopCx {
                    var_ty: var_ty.clone(),
                    done_ty: None,
                };
                let effect = self.tail(body, &inner, &mut lp);
                let ty = lp.done_ty.clone().unwrap_or_else(|| var_ty.clone());
                match on_limit {
                    OnLimit::Last if *rounds => {
                        if !assignable(&lp.var_ty, &ty) {
                            self.push(
                                err(
                                    "E0685",
                                    "rounds end with the value they carry: `done` gives a value of its type",
                                    e.span,
                                )
                                .expected(format!("`{}`", lp.var_ty))
                                .observed(format!("`{ty}`")),
                            );
                        }
                    }
                    OnLimit::Last => {
                        if !assignable(&lp.var_ty, &ty) {
                            self.push(
                                err(
                                    "E0623",
                                    "`on limit: last` needs `done` and the loop value to have the same type",
                                    e.span,
                                )
                                .expected(format!("`{ty}`"))
                                .observed(format!("`{}`", lp.var_ty)),
                            );
                        }
                    }
                    OnLimit::FinalAnswer => self.push(
                        err("E0629", "`final_answer` is for agents", e.span)
                            .expected("`on limit: last` or `on limit: fail \"reason\"`"),
                    ),
                    OnLimit::Fail(_) | OnLimit::Missing => {}
                }
                self.same_write_every_turn(var, body, *rounds);
                Typed {
                    ty,
                    kind: NodeKind::Other(if *rounds { "rounds" } else { "loop" }.into()),
                    effect: i.effect.join(effect),
                }
            }
            ExprKind::Done(v) | ExprKind::Next(v) => {
                let word = if matches!(e.kind, ExprKind::Done(_)) {
                    "done"
                } else {
                    "next"
                };
                self.push(
                    err(
                        "E0624",
                        format!("`{word}` outside the end of a loop body"),
                        e.span,
                    )
                    .expected("`done` or `next` as what a loop's body (or one of its cases) gives"),
                );
                let t = self.expr(v, gc);
                Typed::pure(t.ty)
            }
            ExprKind::Try(v) => {
                let t = self.expr(v, gc);
                Typed {
                    ty: Ty::Result(Box::new(t.ty)),
                    kind: NodeKind::Other("try".into()),
                    effect: t.effect,
                }
            }
            ExprKind::Agent(a) => self.agent(a, gc),
        }
    }

    /// `W0605`: a `write once` call in a loop body whose arguments use
    /// nothing that changes from turn to turn. Each turn is a new place in
    /// the run (its own key in the journal), so each turn writes again: a
    /// payment inside a retry loop is made once per turn.
    fn same_write_every_turn(&mut self, var: &Ident, body: &Expr, rounds: bool) {
        let mut turn_names: HashSet<String> = HashSet::from([var.name.clone()]);
        visit(body, &mut |e| match &e.kind {
            ExprKind::Block { steps, .. } => {
                turn_names.extend(steps.iter().map(|(n, _)| n.name.clone()))
            }
            ExprKind::Each { var, .. } | ExprKind::Comprehension { var, .. } => {
                turn_names.insert(var.name.clone());
            }
            ExprKind::Match { cases, .. } => turn_names.extend(
                cases
                    .iter()
                    .flat_map(|c| c.binds.iter().map(|b| b.name.clone())),
            ),
            _ => {}
        });
        let mut found = Vec::new();
        visit(body, &mut |e| {
            let ExprKind::Call { callee, args } = &e.kind else {
                return;
            };
            let ExprKind::Ident(name) = &callee.kind else {
                return;
            };
            let Some(sig) = self.tools.get(name.as_str()) else {
                return;
            };
            if sig.effect == Effect::WriteOnce
                && !args.iter().any(|a| mentions_any(&a.value, &turn_names))
            {
                found.push((name.clone(), e.span));
            }
        });
        let what = if rounds { "round" } else { "turn" };
        for (name, span) in found {
            self.push(
                warn("W0605", format!("the same `write once` call on every {what}"), span)
                    .expected(format!("the write after the {}, with its result; or arguments that change from {what} to {what}", if rounds { "rounds" } else { "loop" }))
                    .observed(format!("`{name}` takes nothing that changes between {what}s, and each {what} is a new call: it writes once per {what}")),
            );
        }
    }

    // ----- M5: operators, choices, loops, agents ----------------------------

    fn expect_bool(&mut self, ty: &Ty, span: Span) {
        if !matches!(ty, Ty::Bool | Ty::Error) {
            self.push(
                err("E0614", "condition is not a `Bool`", span)
                    .expected("`Bool`")
                    .observed(format!("`{ty}`")),
            );
        }
    }

    /// Both branches must give the same type; the result is the first.
    fn same_type(&mut self, first: &Ty, other: &Ty, span: Span) -> Ty {
        if assignable(other, first) {
            return first.clone();
        }
        if assignable(first, other) {
            return other.clone();
        }
        self.push(
            err("E0615", "branches give different types", span)
                .expected(format!("`{first}`"))
                .observed(format!("`{other}`")),
        );
        first.clone()
    }

    /// Adds a name bound by a loop or a `case`; it must not hide another.
    fn bind(&mut self, gc: &mut GraphCx, name: &Ident, ty: Ty) {
        if gc.scope.contains_key(&name.name) || self.is_global(&name.name) {
            self.push(
                err("E0501", "name already used", name.span)
                    .expected("a name not used by a parameter, node or declaration")
                    .observed(format!("`{}`", name.name)),
            );
        }
        gc.scope.insert(name.name.clone(), ty);
    }

    fn binary(&mut self, op: &str, left: &Expr, right: &Expr, span: Span, gc: &GraphCx) -> Typed {
        let l = self.expr(left, gc);
        let r = self.expr(right, gc);
        let effect = l.effect.join(r.effect);
        let (a, b) = (&l.ty, &r.ty);
        let ty = match op {
            "and" | "or" => {
                self.expect_bool(a, left.span);
                self.expect_bool(b, right.span);
                Some(Ty::Bool)
            }
            "==" | "!=" => (assignable(a, b) || assignable(b, a)).then_some(Ty::Bool),
            "<" | "<=" | ">" | ">=" => {
                let ordered = |t: &Ty| {
                    is_number(t) || matches!(t, Ty::Money | Ty::Duration | Ty::Text | Ty::Date)
                };
                (ordered(a) && (assignable(a, b) || assignable(b, a))).then_some(Ty::Bool)
            }
            "+" | "-" | "*" | "/" => arithmetic(op, a, b),
            "in" => match b {
                Ty::List(t, _) => (assignable(a, t) || assignable(t, a)).then_some(Ty::Bool),
                Ty::Text => matches!(a, Ty::Text).then_some(Ty::Bool),
                _ => None,
            },
            _ => None,
        };
        let ty = match ty {
            Some(t) => t,
            None if matches!(a, Ty::Error) || matches!(b, Ty::Error) => Ty::Error,
            None => {
                self.push(
                    err(
                        "E0613",
                        format!("operator `{op}` does not apply to these types"),
                        span,
                    )
                    .observed(format!("`{a}` {op} `{b}`")),
                );
                Ty::Error
            }
        };
        Typed {
            ty,
            kind: NodeKind::Pure,
            effect,
        }
    }

    /// The variants of a type that `match` can take apart.
    fn variants_of(&self, ty: &Ty) -> Option<Vec<VariantSig>> {
        match ty {
            Ty::Result(t) => Some(vec![
                ("Ok".into(), vec![("value".into(), (**t).clone())]),
                ("Failed".into(), vec![("error".into(), Ty::Text)]),
            ]),
            Ty::User(n) => match self.types.get(n.as_str()) {
                Some(UserType::Variants(vs)) => Some(vs.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Checks the cases of a `match` and returns the scope of each body.
    fn match_cases(
        &mut self,
        value: &Expr,
        cases: &[Case],
        span: Span,
        gc: &GraphCx,
    ) -> (Typed, Vec<GraphCx>) {
        let v = self.expr(value, gc);
        let variants = self.variants_of(&v.ty);
        if variants.is_none() && !matches!(v.ty, Ty::Error) {
            self.push(
                err("E0616", "`match` needs a value with variants", value.span)
                    .expected("a type declared as `A | B(...)`, or the result of `try`")
                    .observed(format!("`{}`", v.ty)),
            );
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut wildcard = false;
        let mut scopes = Vec::new();
        for case in cases {
            let mut inner = gc.clone();
            match (&case.variant, &variants) {
                (None, _) => wildcard = true,
                (Some(name), Some(vs)) => {
                    match vs.iter().find(|(n, _)| *n == name.name) {
                        None => self.push(
                            err("E0617", "not a variant of this type", name.span)
                                .expected(format!(
                                    "one of {}",
                                    vs.iter()
                                        .map(|(n, _)| format!("`{n}`"))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ))
                                .observed(format!("`{}`", name.name)),
                        ),
                        Some((_, fields)) => {
                            if !seen.insert(name.name.clone()) {
                                self.push(
                                    err("E0620", "variant matched twice", name.span)
                                        .observed(format!("`{}`", name.name)),
                                );
                            }
                            // Fields are bound by position, like Python's
                            // `case Point(x, y)`; `_` skips one.
                            if case.binds.len() > fields.len() {
                                self.push(
                                    err(
                                        "E0618",
                                        "more names than the variant has fields",
                                        case.span,
                                    )
                                    .expected(format!(
                                        "at most {}: {}",
                                        fields.len(),
                                        fields
                                            .iter()
                                            .map(|(f, _)| format!("`{f}`"))
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ))
                                    .observed(format!("{} names", case.binds.len())),
                                );
                            }
                            for (b, field) in case.binds.iter().zip(fields.iter()) {
                                if b.name != "_" {
                                    self.bind(&mut inner, b, field.1.clone());
                                }
                            }
                            for b in case.binds.iter().skip(fields.len()) {
                                inner.scope.insert(b.name.clone(), Ty::Error);
                            }
                        }
                    }
                }
                (Some(_), None) => {
                    for b in &case.binds {
                        inner.scope.insert(b.name.clone(), Ty::Error);
                    }
                }
            }
            scopes.push(inner);
        }
        if let Some(vs) = &variants
            && !wildcard
        {
            let missing: Vec<String> = vs
                .iter()
                .filter(|(n, _)| !seen.contains(n))
                .map(|(n, _)| format!("`{n}`"))
                .collect();
            if !missing.is_empty() {
                self.push(
                    err("E0619", "`match` does not cover every variant", span)
                        .expected("a `case` for each variant, or `case _`")
                        .observed(format!("missing {}", missing.join(", "))),
                );
            }
        }
        (v, scopes)
    }

    /// A loop body: `done`/`next` at its end, possibly inside `match`/`if`.
    /// `name = value` steps of a loop's or a round's body: each sees the
    /// ones before it.
    fn steps(&mut self, steps: &[(Ident, Expr)], gc: &GraphCx) -> (GraphCx, Effect) {
        let mut inner = gc.clone();
        let mut effect = Effect::Pure;
        for (name, v) in steps {
            let t = self.expr(v, &inner);
            effect = effect.join(t.effect);
            let ty = if t.ty == Ty::IntLit { Ty::Int } else { t.ty };
            self.bind(&mut inner, name, ty);
        }
        (inner, effect)
    }

    /// `race first where cond:` (decision D12): branches of one type, run at
    /// once; the first whose value passes `cond` wins, the others are
    /// cancelled. `on none` says what it gives when none does.
    fn race(&mut self, r: &RaceExpr, span: Span, gc: &GraphCx) -> Typed {
        if r.branches.len() < 2 {
            self.push(
                err("E0680", "a race needs at least two branches", span)
                    .expected("two or more `name: value` lines"),
            );
        }
        let mut seen = HashSet::new();
        let mut ty: Option<Ty> = None;
        let mut effect = Effect::Pure;
        for (name, v) in &r.branches {
            if !seen.insert(name.name.as_str()) {
                self.push(
                    err(
                        "E0681",
                        "two branches of a race with the same name",
                        name.span,
                    )
                    .observed(format!("`{}`", name.name)),
                );
            }
            let outer_raw = std::mem::replace(&mut self.raw_write, false);
            let t = self.expr(v, gc);
            let raw = self.raw_write;
            self.raw_write = outer_raw || raw;
            effect = effect.join(t.effect);
            if t.effect >= Effect::Write && raw {
                self.push(
                    warn("W0604", "a branch of a race writes outside the run", v.span)
                        .expected("reads, models and sandboxes in the branches, or writes whose tools declare `compensate` (undone when the branch loses); else the write after the race, with the winner")
                        .observed(format!("`{}` can lose and still have written: a write in progress finishes when the race is decided", name.name)),
                );
            }
            ty = Some(match ty {
                None => t.ty,
                Some(first) => self.same_type(&first, &t.ty, v.span),
            });
        }
        let ty = match ty {
            Some(Ty::IntLit) => Ty::Int,
            Some(t) => t,
            None => Ty::Error,
        };
        // `race first N`: a quorum. N winners, as a list; `on none` (fewer
        // than N passed) gives a list too.
        let branch_ty = ty.clone();
        let ty = match r.count {
            Some((n, s)) => {
                if n == 0 || n as usize > r.branches.len() {
                    self.push(
                        err(
                            "E0686",
                            "`race first N` needs 1 to as many winners as branches",
                            s,
                        )
                        .expected(format!("a number from 1 to {}", r.branches.len()))
                        .observed(format!("{n}")),
                    );
                }
                Ty::List(Box::new(branch_ty.clone()), None)
            }
            None => ty,
        };
        if let Some(c) = &r.cond {
            let mut inner = gc.clone();
            inner.scope.insert("it".into(), branch_ty.clone());
            let t = self.expr(c, &inner);
            self.expect_bool(&t.ty, c.span);
            if let Some(s) = self.impure(c) {
                self.push(
                    err("E0682", "a race's condition is pure", s)
                        .expected(
                            "a condition on `it`, the branch's value, made of operators and `def`s",
                        )
                        .observed("a call with effects in `where`"),
                );
            }
        }
        match &r.on_none {
            None => self.push(err("E0683", "a race needs `on none`", span).expected(
                "`on none: fail \"reason\"` or `on none: value`, for when no branch wins",
            )),
            Some(OnNone::Fail(_)) => {}
            Some(OnNone::Value(v)) => {
                let t = self.expr(v, gc);
                effect = effect.join(t.effect);
                if !assignable(&t.ty, &ty) {
                    self.push(
                        err("E0684", "`on none` gives a value of another type", v.span)
                            .expected(if r.count.is_some() {
                                format!("`{ty}`, the list of winners the race gives")
                            } else {
                                format!("`{ty}`, like the branches")
                            })
                            .observed(format!("`{}`", t.ty)),
                    );
                }
            }
        }
        Typed {
            ty,
            kind: NodeKind::Other("race".into()),
            effect,
        }
    }

    fn tail(&mut self, e: &Expr, gc: &GraphCx, lp: &mut LoopCx) -> Effect {
        match &e.kind {
            ExprKind::Block { steps, tail } => {
                let (inner, effect) = self.steps(steps, gc);
                effect.join(self.tail(tail, &inner, lp))
            }
            ExprKind::Done(v) => {
                let t = self.expr(v, gc);
                lp.done_ty = Some(match lp.done_ty.take() {
                    None => t.ty,
                    Some(first) => self.same_type(&first, &t.ty, v.span),
                });
                t.effect
            }
            ExprKind::Next(v) => {
                let t = self.expr(v, gc);
                if !assignable(&t.ty, &lp.var_ty) {
                    self.push(
                        err("E0622", "`next` gives a value of another type", v.span)
                            .expected(format!("`{}`, like the loop's first value", lp.var_ty))
                            .observed(format!("`{}`", t.ty)),
                    );
                }
                t.effect
            }
            ExprKind::Match { value, cases } => {
                let (v, scopes) = self.match_cases(value, cases, e.span, gc);
                let mut effect = v.effect;
                for (case, inner) in cases.iter().zip(scopes) {
                    effect = effect.join(self.tail(&case.body, &inner, lp));
                }
                effect
            }
            ExprKind::If { cond, then, els } => {
                let c = self.expr(cond, gc);
                self.expect_bool(&c.ty, cond.span);
                c.effect
                    .join(self.tail(then, gc, lp))
                    .join(self.tail(els, gc, lp))
            }
            _ => {
                self.push(
                    err(
                        "E0621",
                        "a loop's body must end in `done` or `next`",
                        e.span,
                    )
                    .expected("`done value` to finish, or `next value` for another turn"),
                );
                self.expr(e, gc).effect
            }
        }
    }

    /// A tool call with `requires` lines (decision D29): conditions on the
    /// tool's state (`state.field`) and the graph's values, made only of
    /// operators, so the tool can check them when it acts.
    fn guarded(&mut self, call: &Expr, requires: &[Expr], gc: &GraphCx) -> Typed {
        let typed = self.expr(call, gc);
        let tool = match &call.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Ident(n) => self
                    .tools
                    .get(n.as_str())
                    .map(|s| (n, s.checks.clone(), s.effect)),
                _ => None,
            },
            _ => None,
        };
        let state = match tool {
            Some((_, Some(c), effect)) if effect >= Effect::Write => {
                match self.types.get(c.name.as_str()) {
                    Some(UserType::Record(_)) => Ty::User(c.name),
                    _ => Ty::Error, // reported at the tool
                }
            }
            Some((n, checks, effect)) => {
                let why = if effect < Effect::Write {
                    format!("`{n}` is `{effect}`: preconditions guard writes")
                } else if checks.is_none() {
                    format!("`{n}` has no `checks`")
                } else {
                    String::new()
                };
                self.push(
                    err("E0636", "`requires` needs a write tool that declares `checks`", call.span)
                        .expected("a `write` or `write once` tool with `checks StateType`: the tool validates the conditions")
                        .observed(why),
                );
                Ty::Error
            }
            None => {
                self.push(
                    err("E0636", "`requires` only applies to tool calls", call.span)
                        .expected("`name = tool(...):` followed by `requires ...` lines"),
                );
                Ty::Error
            }
        };
        let mut inner = gc.clone();
        inner.scope.insert("state".into(), state);
        for r in requires {
            if let Some(span) = not_an_operator(r) {
                self.push(
                    err("E0638", "`requires` may only use operators and values", span)
                        .expected("comparisons, `+ - * /`, `and`, `or`, `not`, `state.field` and values of the graph")
                        .observed("a call or another construct the tool cannot evaluate"),
                );
                continue;
            }
            let t = self.expr(r, &inner);
            self.expect_bool(&t.ty, r.span);
        }
        typed
    }

    fn agent(&mut self, a: &AgentExpr, gc: &GraphCx) -> Typed {
        let model = a.model.name.as_str();
        let mut effect = Effect::Llm;
        if !self.models.contains_key(model) {
            self.push(
                err("E0602", "unknown model", a.model.span)
                    .expected("a model declared with `model`")
                    .observed(format!("`{model}`")),
            );
        }
        for at in &a.tools {
            let t = &at.name;
            let lent: Vec<Ty> = at.lends.iter().map(|l| self.expr(l, gc).ty).collect();
            if let Some(sig) = self.tools.get(t.name.as_str()) {
                let borrowed: Vec<(String, Ty)> = sig
                    .params
                    .iter()
                    .filter(|(_, ty)| matches!(ty, Ty::Lent(_)))
                    .cloned()
                    .collect();
                if borrowed.len() != lent.len() {
                    let names: Vec<String> = borrowed
                        .iter()
                        .map(|(n, ty)| format!("`{n}: {ty}`"))
                        .collect();
                    self.push(
                        err(
                            "E0649",
                            "lend the agent's tool each sandbox it borrows",
                            t.span,
                        )
                        .expected(if names.is_empty() {
                            format!("`{}`, without parentheses: it borrows nothing", t.name)
                        } else {
                            format!("`{}(...)` lending {}", t.name, names.join(", "))
                        })
                        .observed(format!("{} lent", lent.len())),
                    );
                } else {
                    let spans: Vec<Span> = at.lends.iter().map(|l| l.span).collect();
                    for ((ty, (pname, pty)), span) in lent.iter().zip(&borrowed).zip(spans) {
                        if let Some(d) = lent_mismatch(ty, pty, pname, span) {
                            self.push(d);
                        }
                    }
                }
            }
            match self.tools.get(t.name.as_str()) {
                None => self.push(
                    err("E0626", "not a tool", t.span)
                        .expected("a tool declared with `tool`")
                        .observed(format!("`{}`", t.name)),
                ),
                Some(sig) => {
                    effect = effect.join(sig.effect);
                    if sig.effect == Effect::WriteOnce {
                        // A model decides when to call it, and may call it again.
                        self.push(
                            err("E0640", "an agent cannot use a `write once` tool", t.span)
                                .expected("`write once` calls as steps of the graph, e.g. after the agent, with its answer")
                                .observed(format!("`{}` is `write once`", t.name)),
                        );
                    }
                    if !self.tools_with_max_output.contains(t.name.as_str()) {
                        // An agent reads the whole output into the
                        // conversation: it must be bounded (decision D16).
                        self.push(
                            err(
                                "E0627",
                                "a tool used by an agent needs `max_output`",
                                t.span,
                            )
                            .expected("`max_output N tokens` in the tool's declaration")
                            .observed(format!("`{}` without it", t.name)),
                        );
                    }
                }
            }
        }
        match a.max_turns {
            None => self.push(
                err("E0628", "an agent needs `max_turns`", a.span)
                    .expected("`max_turns N`: the most model calls it may make"),
            ),
            Some((0, span)) => {
                self.push(err("E0628", "`max_turns` must be at least 1", span).observed("`0`"))
            }
            Some(_) => {}
        }
        for (event, action) in [("turn_limit", &a.on_turn_limit), ("stuck", &a.on_stuck)] {
            match action {
                OnLimit::FinalAnswer | OnLimit::Fail(_) => {}
                OnLimit::Missing => self.push(
                    err(
                        "E0629",
                        format!("an agent must say what happens `on {event}`"),
                        a.span,
                    )
                    .expected(format!(
                        "`on {event}: final_answer` or `on {event}: fail \"reason\"`"
                    )),
                ),
                OnLimit::Last => self.push(err("E0629", "`last` is for loops", a.span).expected(
                    format!("`on {event}: final_answer` or `on {event}: fail \"reason\"`"),
                )),
            }
        }
        let ty = match &a.task {
            None => {
                self.push(
                    err("E0628", "an agent needs a `task`", a.span)
                        .expected("`task some_prompt(...)`"),
                );
                Ty::Error
            }
            Some(task) => {
                let arg = [Arg {
                    name: None,
                    value: task.clone(),
                }];
                let t = self.model_call(model, task, &arg, gc);
                effect = effect.join(t.effect);
                t.ty
            }
        };
        if effect >= Effect::Write {
            self.raw_write = true; // an agent's writes have no compensation
        }
        Typed {
            ty,
            kind: NodeKind::Other(format!("agent {model}")),
            effect,
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
        if let Some(r) = self.routers.get(n).copied() {
            let typed = self.model_call(n, e, args, gc);
            // The check must take what the prompt answers.
            if let Some((_, check)) = &r.policy
                && let Some(sig) = self.defs.get(check.name.as_str())
                && let [(_, param)] = sig.params.as_slice()
                && typed.ty != Ty::Error
                && !assignable(&typed.ty, param)
            {
                let param = param.clone();
                self.push(
                    err("E0694", "the router's check takes another type", e.span)
                        .expected(format!(
                            "a prompt that answers `{param}`, what `{}` checks",
                            check.name
                        ))
                        .observed(format!("an answer of type `{}`", typed.ty)),
                );
            }
            return typed;
        }
        if self.prompts.contains_key(n) {
            self.push(
                err("E0606", "a prompt must be sent to a model", e.span)
                    .expected(format!("`model({n}(...))`, e.g. `claude({n}(...))`"))
                    .observed(format!("`{n}(...)`")),
            );
            return Typed::pure(Ty::Error);
        }
        if let Some(UserType::Record(fields)) = self.types.get(n) {
            let fields = fields.clone();
            let effect = self.construct(n, &fields, args, e.span, gc);
            return Typed {
                ty: Ty::User(n.to_owned()),
                kind: NodeKind::Pure,
                effect,
            };
        }
        if let Some(owners) = self.variant_owners.get(n).cloned() {
            if owners.len() > 1 {
                self.push(
                    err(
                        "E0630",
                        "variant name declared by several types",
                        callee.span,
                    )
                    .observed(format!("`{n}` in {}", owners.join(", "))),
                );
                return Typed::pure(Ty::Error);
            }
            let owner = owners[0];
            let fields = match self.types.get(owner) {
                Some(UserType::Variants(vs)) => vs
                    .iter()
                    .find(|(v, _)| v == n)
                    .map(|(_, f)| f.clone())
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            if fields.is_empty() {
                self.push(
                    err("E0625", "this variant has no fields", e.span)
                        .expected(format!("`{n}`, without parentheses")),
                );
                return Typed::pure(Ty::User(owner.to_owned()));
            }
            let effect = self.construct(n, &fields, args, e.span, gc);
            return Typed {
                ty: Ty::User(owner.to_owned()),
                kind: NodeKind::Pure,
                effect,
            };
        }
        if self.types.contains_key(n) {
            self.push(
                err(
                    "E0625",
                    "values of this type are built from its variants",
                    callee.span,
                )
                .observed(format!("`{n}(...)`")),
            );
            return Typed::pure(Ty::Error);
        }
        if let Some(sig) = self.defs.get(n) {
            let (params, ret) = (sig.params.clone(), sig.ret.clone());
            let inner = self.args(n, &params, args, e.span, gc);
            return Typed {
                ty: ret,
                kind: NodeKind::Pure,
                effect: inner,
            };
        }
        if let Some(sig) = self.tools.get(n) {
            let (params, ret, effect) = (sig.params.clone(), sig.ret.clone(), sig.effect);
            if effect >= Effect::Write && sig.compensate.is_none() {
                self.raw_write = true;
            }
            let inner = self.args(n, &params, args, e.span, gc);
            return Typed {
                ty: ret,
                kind: NodeKind::Tool { tool: n.to_owned() },
                effect: effect.join(inner),
            };
        }
        if let Some((_, sig)) = self.graphs.get(n) {
            let (params, ret) = (sig.params.clone(), sig.ret.clone());
            if self.raw_write_graphs.contains(n) {
                self.raw_write = true;
            }
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
        if let Some(t) = self.builtin(n, args, e.span, gc) {
            return t;
        }
        self.push(
            err("E0602", "unknown name", callee.span)
                .expected("a model, tool, graph or `def`")
                .observed(format!("`{n}`")),
        );
        Typed::pure(Ty::Error)
    }

    /// `Name(field=value, ...)`: every field, by name. A single field may
    /// also be given by position.
    fn construct(
        &mut self,
        name: &str,
        fields: &[(String, Ty)],
        args: &[Arg],
        span: Span,
        gc: &GraphCx,
    ) -> Effect {
        let mut effect = Effect::Pure;
        let mut given: HashSet<String> = HashSet::new();
        for (i, a) in args.iter().enumerate() {
            let t = self.expr(&a.value, gc);
            effect = effect.join(t.effect);
            let field = match &a.name {
                Some(f) => fields.iter().find(|(n, _)| *n == f.name),
                None if fields.len() == 1 && i == 0 => fields.first(),
                None => {
                    self.push(
                        err("E0625", "fields are given by name", a.value.span)
                            .expected(format!("`{name}(field=value, ...)`")),
                    );
                    continue;
                }
            };
            let Some((fname, fty)) = field else {
                let shown = a.name.as_ref().map_or(String::new(), |n| n.name.clone());
                self.push(
                    err("E0612", "unknown argument", a.value.span)
                        .expected(format!("a field of `{name}`"))
                        .observed(format!("`{shown}`")),
                );
                continue;
            };
            given.insert(fname.clone());
            if !assignable(&t.ty, fty) {
                self.push(
                    err("E0608", "argument has the wrong type", a.value.span)
                        .expected(format!("`{fty}` for `{fname}`"))
                        .observed(format!("`{}`", t.ty)),
                );
            }
        }
        let missing: Vec<String> = fields
            .iter()
            .filter(|(f, _)| !given.contains(f))
            .map(|(f, _)| format!("`{f}`"))
            .collect();
        if !missing.is_empty() {
            self.push(
                err("E0625", format!("`{name}` is missing fields"), span)
                    .observed(format!("missing {}", missing.join(", "))),
            );
        }
        effect
    }

    /// `router r = route [m1, m2]:` + `policy cheapest_that_passes(check)`
    /// (decision D30).
    fn check_router(&mut self, r: &RouterDecl) {
        for m in &r.models {
            if !self.models.contains_key(m.name.as_str()) {
                self.push(
                    err("E0690", "unknown model in a router", m.span)
                        .expected("a model declared with `model`")
                        .observed(format!("`{}`", m.name)),
                );
            }
        }
        if r.models.len() < 2 {
            self.push(
                err("E0691", "a router needs at least two models", r.name.span)
                    .expected("`route [cheaper, stronger]`, cheapest first"),
            );
        }
        let Some((policy, check)) = &r.policy else {
            self.push(
                err("E0692", "a router needs a policy", r.name.span)
                    .expected("`policy cheapest_that_passes(check)`, with `check` a `def` that takes the answer and gives `Bool`"),
            );
            return;
        };
        if policy.name != "cheapest_that_passes" {
            self.push(
                err("E0692", "unknown router policy", policy.span)
                    .expected("`cheapest_that_passes` (the only policy for now)")
                    .observed(format!("`{}`", policy.name)),
            );
        }
        match self.defs.get(check.name.as_str()) {
            Some(sig) if sig.params.len() == 1 && sig.ret == Ty::Bool => {}
            Some(sig) => {
                let observed = format!(
                    "`{}` takes {} value(s) and gives `{}`",
                    check.name,
                    sig.params.len(),
                    sig.ret
                );
                self.push(
                    err("E0693", "a router's check takes the answer and gives `Bool`", check.span)
                        .expected("`def check(answer: T) -> Bool`")
                        .observed(observed),
                );
            }
            None => self.push(
                err("E0693", "a router's check is a `def`", check.span)
                    .expected("`def check(answer: T) -> Bool`: pure, so the choice is the same on every run")
                    .observed(format!("`{}`", check.name)),
            ),
        }
    }

    fn model_call(&mut self, model: &str, e: &Expr, args: &[Arg], gc: &GraphCx) -> Typed {
        let mut prompt_call = None;
        for a in args {
            match &a.name {
                Some(n) if n.name == "continue" => self.push(
                    err(
                        "E0101",
                        "`continue=` is not supported yet (planned for a later milestone)",
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
            if let Some(d) = lent_mismatch(&t.ty, pty, pname, a.value.span) {
                self.push(d);
                continue;
            }
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

#[derive(Default, Clone)]
struct GraphCx {
    scope: HashMap<String, Ty>,
}

/// What a loop body's `done` and `next` are checked against.
struct LoopCx {
    var_ty: Ty,
    done_ty: Option<Ty>,
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
            ExprKind::Binary { left, right, .. } => {
                walk(left, f);
                walk(right, f);
            }
            ExprKind::Unary { value, .. }
            | ExprKind::Done(value)
            | ExprKind::Next(value)
            | ExprKind::Try(value) => walk(value, f),
            ExprKind::If { cond, then, els } => {
                walk(cond, f);
                walk(then, f);
                walk(els, f);
            }
            ExprKind::Match { value, cases } => {
                walk(value, f);
                cases.iter().for_each(|c| walk(&c.body, f));
            }
            ExprKind::Loop { init, body, .. } => {
                walk(init, f);
                walk(body, f);
            }
            ExprKind::Agent(a) => {
                if let Some(t) = &a.task {
                    walk(t, f);
                }
            }
            ExprKind::Guarded { call, requires } => {
                walk(call, f);
                requires.iter().for_each(|r| walk(r, f));
            }
            ExprKind::Message(m) => {
                walk(&m.key, f);
                m.args.iter().for_each(|a| walk(&a.value, f));
            }
            ExprKind::Comprehension {
                body, over, cond, ..
            } => {
                walk(over, f);
                walk(body, f);
                if let Some(c) = cond {
                    walk(c, f);
                }
            }
            ExprKind::Receive {
                about, on_timeout, ..
            } => {
                about
                    .iter()
                    .chain(on_timeout.iter())
                    .for_each(|v| walk(v, f));
            }
            ExprKind::Block { .. } | ExprKind::Each { .. } | ExprKind::Race(_) => {
                m9_kids(e).into_iter().for_each(|k| walk(k, f))
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
        Stmt::After { .. } | Stmt::Unordered(_) => {}
    }
}

/// Every expression written in a `def`'s body.
fn def_exprs(stmts: &[DefStmt]) -> Vec<&Expr> {
    let mut out = Vec::new();
    for s in stmts {
        match s {
            DefStmt::Assign(_, e) | DefStmt::Return(e) => out.push(e),
            DefStmt::If {
                cond, then, els, ..
            } => {
                out.push(cond);
                out.extend(def_exprs(then));
                out.extend(def_exprs(els));
            }
        }
    }
    out
}

/// `[]`: fits any list type.
fn is_empty_list(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::List(items) if items.is_empty())
}

/// Calls `f` on `e` and every expression inside it.
fn visit(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    let kids: Vec<&Expr> = match &e.kind {
        ExprKind::List(items) => items.iter().collect(),
        ExprKind::Field { base, .. } => vec![base],
        ExprKind::Call { callee, args } => std::iter::once(callee.as_ref())
            .chain(args.iter().map(|a| &a.value))
            .collect(),
        ExprKind::Binary { left, right, .. } => vec![left, right],
        ExprKind::Unary { value, .. }
        | ExprKind::Done(value)
        | ExprKind::Next(value)
        | ExprKind::Try(value) => vec![value],
        ExprKind::If { cond, then, els } => vec![cond, then, els],
        ExprKind::Match { value, cases } => std::iter::once(value.as_ref())
            .chain(cases.iter().map(|c| &c.body))
            .collect(),
        ExprKind::Loop { init, body, .. } => vec![init, body],
        ExprKind::Guarded { call, requires } => std::iter::once(call.as_ref())
            .chain(requires.iter())
            .collect(),
        ExprKind::Message(m) => std::iter::once(&m.key)
            .chain(m.args.iter().map(|a| &a.value))
            .collect(),
        ExprKind::Comprehension {
            body, over, cond, ..
        } => [Some(body.as_ref()), Some(over.as_ref()), cond.as_deref()]
            .into_iter()
            .flatten()
            .collect(),
        ExprKind::Receive {
            about, on_timeout, ..
        } => about
            .iter()
            .chain(on_timeout.iter())
            .map(|b| b.as_ref())
            .collect(),
        ExprKind::Block { .. } | ExprKind::Each { .. } | ExprKind::Race(_) => m9_kids(e),
        _ => Vec::new(),
    };
    for k in kids {
        visit(k, f);
    }
}

/// The expressions inside a block of steps, a `for each` or a `race`.
fn m9_kids(e: &Expr) -> Vec<&Expr> {
    match &e.kind {
        ExprKind::Block { steps, tail } => steps
            .iter()
            .map(|(_, v)| v)
            .chain(std::iter::once(tail.as_ref()))
            .collect(),
        ExprKind::Each { over, body, .. } => vec![over, body],
        ExprKind::Race(r) => r
            .cond
            .iter()
            .chain(r.branches.iter().map(|(_, v)| v))
            .chain(match &r.on_none {
                Some(OnNone::Value(v)) => Some(v),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A sandbox lent the wrong way (decision D26), or `None`.
fn lent_mismatch(arg: &Ty, param: &Ty, pname: &str, span: Span) -> Option<Diagnostic> {
    let mode = |e: bool| if e { "edits" } else { "reads" };
    match (arg, param) {
        (Ty::Error, _) | (_, Ty::Error) => None,
        (Ty::Lent(a), Ty::Lent(p)) if a == p => None,
        (Ty::Lent(a), Ty::Lent(p)) => Some(
            err(
                "E0643",
                format!("the tool needs `{}`, not `{}`", mode(*p), mode(*a)),
                span,
            )
            .expected(format!("`{} ...` for `{pname}`", mode(*p)))
            .observed(format!("`{} ...`", mode(*a))),
        ),
        (_, Ty::Lent(p)) => Some(
            err("E0642", "a sandbox must be lent to the tool", span)
                .expected(format!("`{} sandbox` for `{pname}`", mode(*p)))
                .observed(format!("a `{arg}`")),
        ),
        (Ty::Lent(_), _) => Some(
            err(
                "E0647",
                "a lent sandbox only goes to a tool that borrows it",
                span,
            )
            .expected(format!("a `{param}` for `{pname}`"))
            .observed(format!("`{arg}`")),
        ),
        _ => None,
    }
}

/// `verify(f(a, b))` as a policy, if it has that shape.
fn verify_policy(args: &[Arg]) -> Option<Policy> {
    let [Arg { name: None, value }] = args else {
        return None;
    };
    let ExprKind::Call { callee, args } = &value.kind else {
        return None;
    };
    let ExprKind::Ident(tool) = &callee.kind else {
        return None;
    };
    let args = args
        .iter()
        .map(|a| match (&a.name, &a.value.kind) {
            (None, ExprKind::Ident(n)) => Some(Ident {
                name: n.clone(),
                span: a.value.span,
            }),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Policy::Verify {
        tool: Ident {
            name: tool.clone(),
            span: callee.span,
        },
        args,
    })
}

/// The first part of a precondition the tool could not evaluate.
fn not_an_operator(e: &Expr) -> Option<Span> {
    match &e.kind {
        ExprKind::Ident(_)
        | ExprKind::Int { .. }
        | ExprKind::Float { .. }
        | ExprKind::Str(_)
        | ExprKind::Error => None,
        ExprKind::Field { base, .. } => not_an_operator(base),
        ExprKind::Binary { left, right, .. } => {
            not_an_operator(left).or_else(|| not_an_operator(right))
        }
        ExprKind::Unary { value, .. } => not_an_operator(value),
        _ => Some(e.span),
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
        ExprKind::Binary { left, right, .. } => {
            collect_refs(left, bound, index, out);
            collect_refs(right, bound, index, out);
        }
        ExprKind::Unary { value, .. }
        | ExprKind::Done(value)
        | ExprKind::Next(value)
        | ExprKind::Try(value) => collect_refs(value, bound, index, out),
        ExprKind::If { cond, then, els } => {
            for x in [cond, then, els] {
                collect_refs(x, bound, index, out);
            }
        }
        ExprKind::Match { value, cases } => {
            collect_refs(value, bound, index, out);
            for c in cases {
                collect_refs(&c.body, bound, index, out);
            }
        }
        ExprKind::Loop { init, body, .. } => {
            collect_refs(init, bound, index, out);
            collect_refs(body, bound, index, out);
        }
        ExprKind::Agent(a) => {
            if let Some(t) = &a.task {
                collect_refs(t, bound, index, out);
            }
        }
        ExprKind::Guarded { call, requires } => {
            collect_refs(call, bound, index, out);
            for r in requires {
                collect_refs(r, bound, index, out);
            }
        }
        ExprKind::Message(m) => {
            collect_refs(&m.key, bound, index, out);
            for a in &m.args {
                collect_refs(&a.value, bound, index, out);
            }
        }
        // The item's name hides a step of the same name inside.
        ExprKind::Comprehension {
            body,
            var,
            over,
            cond,
        } => {
            collect_refs(over, bound, index, out);
            let mut inner = Vec::new();
            collect_refs(body, bound, index, &mut inner);
            if let Some(c) = cond {
                collect_refs(c, bound, index, &mut inner);
            }
            let hidden = index.get(var.name.as_str()).copied();
            out.extend(inner.into_iter().filter(|i| Some(*i) != hidden));
        }
        ExprKind::Receive {
            about, on_timeout, ..
        } => {
            for v in about.iter().chain(on_timeout.iter()) {
                collect_refs(v, bound, index, out);
            }
        }
        // Names bound inside (steps, items, `it`) hide steps of the graph.
        ExprKind::Block { steps, tail } => {
            let mut inner = Vec::new();
            for (_, v) in steps {
                collect_refs(v, bound, index, &mut inner);
            }
            collect_refs(tail, bound, index, &mut inner);
            let hidden: Vec<usize> = steps
                .iter()
                .filter_map(|(n, _)| index.get(n.name.as_str()).copied())
                .collect();
            out.extend(inner.into_iter().filter(|i| !hidden.contains(i)));
        }
        ExprKind::Each { var, over, body } => {
            collect_refs(over, bound, index, out);
            let mut inner = Vec::new();
            collect_refs(body, bound, index, &mut inner);
            let hidden = index.get(var.name.as_str()).copied();
            out.extend(inner.into_iter().filter(|i| Some(*i) != hidden));
        }
        ExprKind::Race(r) => {
            for (_, v) in &r.branches {
                collect_refs(v, bound, index, out);
            }
            if let Some(OnNone::Value(v)) = &r.on_none {
                collect_refs(v, bound, index, out);
            }
            if let Some(c) = &r.cond {
                let mut inner = Vec::new();
                collect_refs(c, bound, index, &mut inner);
                let hidden = index.get("it").copied();
                out.extend(inner.into_iter().filter(|i| Some(*i) != hidden));
            }
        }
        ExprKind::Bool(_) => {}
        // Only graph parameters are lent: no step to depend on.
        ExprKind::Int { .. }
        | ExprKind::Float { .. }
        | ExprKind::Error
        | ExprKind::Borrow { .. } => {}
    }
}

fn is_number(t: &Ty) -> bool {
    matches!(t, Ty::Nat | Ty::Int | Ty::Float | Ty::IntLit)
}

/// Type of `a op b` for `+ - * /`, or `None` if the operator does not apply.
fn arithmetic(op: &str, a: &Ty, b: &Ty) -> Option<Ty> {
    let num = |a: &Ty, b: &Ty| -> Ty {
        match (a, b) {
            (Ty::Float, _) | (_, Ty::Float) => Ty::Float,
            _ if op == "/" => Ty::Float,
            (Ty::IntLit, Ty::IntLit) => Ty::IntLit,
            (Ty::Nat | Ty::IntLit, Ty::Nat | Ty::IntLit) if op != "-" => Ty::Nat,
            _ => Ty::Int,
        }
    };
    match (op, a, b) {
        (_, x, y) if is_number(x) && is_number(y) => Some(num(x, y)),
        ("+" | "-", Ty::Money, Ty::Money) | ("+" | "-", Ty::Duration, Ty::Duration) => {
            Some(a.clone())
        }
        ("*" | "/", Ty::Money | Ty::Duration, y) if is_number(y) => Some(a.clone()),
        ("*", x, Ty::Money | Ty::Duration) if is_number(x) => Some(b.clone()),
        ("+", Ty::Text, Ty::Text) => Some(Ty::Text),
        ("+", Ty::List(x, m), Ty::List(y, n)) if assignable(y, x) || assignable(x, y) => {
            let max = match (m, n) {
                (Some(m), Some(n)) => Some(m + n),
                _ => None,
            };
            let elem = if assignable(y, x) { x } else { y };
            Some(Ty::List(elem.clone(), max))
        }
        _ => None,
    }
}

/// Whether `e` uses one of `names`, directly or inside a text's `{...}`.
fn mentions_any(e: &Expr, names: &HashSet<String>) -> bool {
    let mut hit = false;
    visit(e, &mut |x| match &x.kind {
        ExprKind::Ident(n) => hit |= names.contains(n),
        ExprKind::Str(lit) => {
            for (path, _, _) in interpolations(lit) {
                let root = path.split(['.', '[', ' ']).next().unwrap_or("").trim();
                hit |= names.contains(root);
            }
        }
        _ => {}
    });
    hit
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
