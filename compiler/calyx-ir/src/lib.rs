//! Intermediate representation (IR) of a compiled Calyx program.
//!
//! The IR is the *template* of decision D8: a parameterized task graph whose
//! fan-outs and loops are symbolic. The runtime unfolds it into the realized
//! graph as values arrive. It is plain data, so several versions of a program
//! can coexist (decision D23) and graphs generated at run time can be checked
//! and interpreted (decision D4).
//!
//! M1 records the structure (nodes, kinds, types, effects, dependencies).
//! M2 adds the expressions the interpreter evaluates, and the models, tools
//! and prompts they call. [`Program::to_json`] is the form the C runtime loads.

use std::fmt;

mod json;

/// Effects, ordered from least to most dangerous (decision D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effect {
    Pure,
    Llm,
    Read,
    Sandbox,
    Write,
    WriteOnce,
}

impl Effect {
    /// The effect of a node is the largest effect of anything it calls.
    pub fn join(self, other: Effect) -> Effect {
        self.max(other)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Pure => "pure",
            Effect::Llm => "llm",
            Effect::Read => "read",
            Effect::Sandbox => "sandbox",
            Effect::Write => "write",
            Effect::WriteOnce => "write once",
        }
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Index of a node inside a [`Graph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// A pure value (`let`, literals, field access): never journaled.
    Pure,
    /// A call to a model with a prompt.
    Model { model: String, prompt: String },
    /// A call to a tool.
    Tool { tool: String },
    /// A call to another graph.
    Call { graph: String },
    /// A control construct: `loop`, `match`, `if`, `try`, `agent claude`.
    Other(String),
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeKind::Pure => f.write_str("pure"),
            NodeKind::Model { model, prompt } => write!(f, "model {model}({prompt})"),
            NodeKind::Tool { tool } => write!(f, "tool {tool}"),
            NodeKind::Call { graph } => write!(f, "graph {graph}"),
            NodeKind::Other(what) => f.write_str(what),
        }
    }
}

/// An expression the runtime evaluates. Names are already resolved to
/// indices: parameters, nodes, models, tools, prompts and graphs.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Text(String),
    Int(u64),
    Float(f64),
    /// A parameter of the enclosing graph, by position.
    Param(usize),
    /// The value of an earlier node of the same graph.
    Node(NodeId),
    /// The current item of a fan-out (`for each item in ...`).
    Item,
    Field(Box<Expr>, String),
    List(Vec<Expr>),
    /// Text with `{...}` interpolations.
    Interp(Vec<Part>),
    /// `model(prompt(args))`. Arguments follow the prompt's parameters.
    Model {
        model: usize,
        prompt: usize,
        args: Vec<Expr>,
    },
    /// `router(prompt(args))`: the router's models in turn, until an answer
    /// passes its check (decision D30).
    Route {
        router: usize,
        prompt: usize,
        args: Vec<Expr>,
    },
    /// Arguments follow the tool's parameters. `requires` are the call's
    /// preconditions (decision D29), sent to the tool, which checks them
    /// against the current state when it acts.
    Tool {
        tool: usize,
        args: Vec<Expr>,
        requires: Vec<Expr>,
    },
    /// `state.field` inside a `requires`: a field of the state the tool
    /// checks, known only to the tool. In an entity's handler, a field of
    /// the entity's state.
    State(String),
    Bool(bool),
    /// `receive` (decision D21): waits for a message; `on_timeout` is the
    /// value when the deadline passes first.
    Receive {
        message: String,
        /// What the message is about, written down with the wait.
        about: Option<Box<Expr>>,
        timeout_s: u64,
        on_timeout: Box<Expr>,
    },
    /// `value` in local `slot` while `body` is computed (a `def`'s `x = ...`).
    Let {
        slot: usize,
        value: Box<Expr>,
        body: Box<Expr>,
    },
    /// `[body for x in over if cond]`, with `x` in local `slot`.
    Comprehension {
        slot: usize,
        over: Box<Expr>,
        body: Box<Expr>,
        cond: Option<Box<Expr>>,
    },
    /// A call to a `def`; arguments follow its parameters.
    Def {
        def: usize,
        args: Vec<Expr>,
    },
    /// `len`, `take`, `sum`, `join`, `lower`, `upper`, `trim`.
    Builtin {
        name: String,
        args: Vec<Expr>,
    },
    /// `ask Entity(key).Handler(args)` or `send ...` (decisions D15, D21).
    Message {
        send: bool,
        entity: usize,
        handler: usize,
        key: Box<Expr>,
        args: Vec<Expr>,
    },
    /// Arguments follow the graph's parameters.
    Graph {
        graph: usize,
        args: Vec<Expr>,
    },
    /// A name bound inside the node: a loop's value or a `case` field.
    Local(usize),
    /// A record, or a variant with fields (with `kind` naming it).
    Record(Vec<(String, Expr)>),
    Binary {
        op: String,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Unary {
        op: String,
        value: Box<Expr>,
    },
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Match {
        value: Box<Expr>,
        cases: Vec<MatchCase>,
    },
    /// `loop` (decision D5): `slot` holds the carried value.
    Loop {
        slot: usize,
        init: Box<Expr>,
        max: u64,
        body: Box<Expr>,
        /// `None`: `on limit: last`; `Some(reason)`: fail.
        on_limit: Option<String>,
        /// `rounds` (decision D18): runs all its turns.
        rounds: bool,
    },
    /// `for each x in over: body` inside an expression, with `x` in local
    /// `slot`: every item at once.
    Each {
        slot: usize,
        over: Box<Expr>,
        body: Box<Expr>,
    },
    /// `race` (decision D12): the branches at once; the first whose value
    /// (in local `slot`) passes `cond` wins. With no winner, `on_none`, or
    /// failing with `on_none_fail`.
    Race {
        branches: Vec<(String, Expr)>,
        slot: usize,
        cond: Option<Box<Expr>>,
        on_none: Option<Box<Expr>>,
        on_none_fail: Option<String>,
    },
    Done(Box<Expr>),
    Next(Box<Expr>),
    /// `Ok(value)` or `Failed(error)` (decision D11).
    Try(Box<Expr>),
    /// The ReAct cycle (decision D5): the model, with tools, until it answers.
    Agent(Box<Agent>),
}

/// One `case` of a `match`.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchCase {
    /// `None` for `case _`.
    pub variant: Option<String>,
    /// Fields of the variant and the local slots they are bound to.
    pub binds: Vec<(String, usize)>,
    pub body: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub model: usize,
    /// The task: a prompt and its arguments.
    pub prompt: usize,
    pub args: Vec<Expr>,
    pub tools: Vec<usize>,
    /// Per tool: the sandboxes lent to it, as (parameter index, value).
    pub bound: Vec<Vec<(usize, Expr)>>,
    pub max_turns: u64,
    /// `None`: ask for a final answer; `Some(reason)`: fail.
    pub on_turn_limit: Option<String>,
    pub on_stuck: Option<String>,
}

/// A piece of an interpolated text.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Lit(String),
    Expr(Expr),
}

/// A piece of a prompt template: `{a.b}` becomes `Path(["a", "b"])`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptPart {
    Lit(String),
    Path(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub name: String,
    /// The provider's model identifier, e.g. `gemini-3.5-flash-lite`.
    pub id: String,
    pub max_output: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub params: Vec<String>,
    pub effect: Effect,
    /// In tokens (decision D16).
    pub max_output: Option<u64>,
    /// Per attempt; the default depends on the effect (decision D22).
    pub timeout_ms: u64,
    /// Error kinds the runtime retries.
    pub retry_on: Vec<String>,
    /// The tool returns `Text`; otherwise its output is decoded as JSON.
    pub returns_text: bool,
    /// JSON Schema of the arguments, for models that call the tool.
    pub schema: String,
    /// What the tool does, in words, for models that call it.
    pub description: Option<String>,
    /// Repeating it with the same arguments is legitimate (not "stuck").
    pub repeatable: bool,
    /// The parameter whose value is the idempotency key (`write` tools).
    pub idempotency_key: Option<usize>,
    /// What to do when a `write once` call may or may not have happened.
    pub on_uncertain: Option<Uncertain>,
    /// The tool returns `Unit`: nothing to make up when a call is taken as
    /// done without its answer.
    pub returns_unit: bool,
    /// The type of state the tool validates `requires` against.
    pub checks: Option<String>,
    /// Per parameter: a sandbox it borrows, `Some(true)` to edit it,
    /// `Some(false)` to read it (decision D26).
    pub borrows: Vec<Option<bool>>,
}

/// The `on_uncertain` policy of a `write once` tool (decision D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Uncertain {
    /// Stop the run; a person decides when resuming.
    Pause,
    /// Take the call as done and go on.
    AcceptLoss,
    /// Ask a `read` tool (returning `Bool`) whether the call happened; its
    /// arguments are parameters of the `write once` tool, by position.
    Verify { tool: usize, args: Vec<usize> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub name: String,
    pub params: Vec<String>,
    pub parts: Vec<PromptPart>,
    /// JSON Schema of the answer, or `None` when the prompt returns `Text`.
    pub schema: Option<String>,
    /// The schema wraps a non-object answer as `{"value": ...}`.
    pub wrapped: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub name: String,
    /// The node's type, as written in Calyx (e.g. `List<Text> max 5`).
    pub ty: String,
    pub kind: NodeKind,
    /// `Some(var)` for a fan-out `node xs[var in ...]`: one instance per item.
    pub fan_out: Option<String>,
    pub effect: Effect,
    /// Data dependencies: nodes whose values this node reads.
    pub inputs: Vec<NodeId>,
    /// The list a fan-out iterates over.
    pub over: Option<Expr>,
    /// What the node computes (for a fan-out: for each item).
    pub value: Option<Expr>,
    /// Estimated seconds from the start of this node to the end of the
    /// graph along its longest path: the scheduler runs higher ranks first
    /// (decision D24). Set by [`Graph::rank_nodes`].
    pub rank: f64,
    /// Local slots its expressions use (loop values, `case` fields).
    pub nlocals: usize,
}

/// What `limits ...` sets on a graph (decision D3). The programmer limits
/// concurrency; the runtime creates it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Limits {
    /// Calls (models and tools) in flight at once.
    pub threads: Option<u64>,
    /// Calls started per second.
    pub rate_per_s: Option<u64>,
    /// Maximum cost of the run: amount and currency.
    pub budget: Option<(f64, String)>,
}

/// A compiled graph: the template the runtime unfolds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Graph {
    pub name: String,
    /// Parameters as `(name, type)`.
    pub params: Vec<(String, String)>,
    pub ret: String,
    /// The graph's effect: the join of its nodes' effects.
    pub effect: Option<Effect>,
    /// Nodes in an order where every node comes after its inputs.
    pub nodes: Vec<Node>,
    pub output: Option<NodeId>,
    pub limits: Limits,
}

impl Graph {
    /// Estimated duration of one node, in seconds: the calls in its
    /// expressions (3 s per model or graph call, 1 s per tool call). Without
    /// expressions, an estimate from its effect.
    fn weight(n: &Node) -> f64 {
        fn calls(e: &Expr) -> f64 {
            match e {
                Expr::Model { args, .. } | Expr::Route { args, .. } | Expr::Graph { args, .. } => {
                    3.0 + args.iter().map(calls).sum::<f64>()
                }
                Expr::Tool { args, .. } => 1.0 + args.iter().map(calls).sum::<f64>(),
                Expr::State(_) | Expr::Bool(_) => 0.0,
                Expr::Receive { .. } => 1.0,
                Expr::Let { value, body, .. } => calls(value) + calls(body),
                Expr::Comprehension { over, body, .. } => calls(over) + calls(body),
                Expr::Def { args, .. } | Expr::Builtin { args, .. } => args.iter().map(calls).sum(),
                Expr::Message { key, args, .. } => {
                    0.1 + calls(key) + args.iter().map(calls).sum::<f64>()
                }
                Expr::Field(base, _) => calls(base),
                Expr::List(items) => items.iter().map(calls).sum(),
                Expr::Record(fields) => fields.iter().map(|(_, e)| calls(e)).sum(),
                Expr::Binary { left, right, .. } => calls(left) + calls(right),
                Expr::Unary { value, .. }
                | Expr::Done(value)
                | Expr::Next(value)
                | Expr::Try(value) => calls(value),
                Expr::If { cond, then, els } => calls(cond) + calls(then).max(calls(els)),
                Expr::Match { value, cases } => {
                    calls(value) + cases.iter().map(|c| calls(&c.body)).fold(0.0, f64::max)
                }
                // A loop is expected to turn about twice.
                Expr::Loop {
                    init,
                    body,
                    max,
                    rounds: true,
                    ..
                } => calls(init) + *max as f64 * calls(body),
                Expr::Loop { init, body, .. } => calls(init) + 2.0 * calls(body),
                // Items at once: about as long as one.
                Expr::Each { over, body, .. } => calls(over) + calls(body),
                Expr::Race { branches, .. } => {
                    branches.iter().map(|(_, b)| calls(b)).fold(0.0, f64::max)
                }
                // An agent: a few turns of a model call and a tool call each.
                Expr::Agent(a) => {
                    let turns = a.max_turns.min(3) as f64;
                    turns * 4.0 + a.args.iter().map(calls).sum::<f64>()
                }
                Expr::Interp(parts) => parts
                    .iter()
                    .map(|p| match p {
                        Part::Expr(e) => calls(e),
                        Part::Lit(_) => 0.0,
                    })
                    .sum(),
                _ => 0.0,
            }
        }
        if n.value.is_some() || n.over.is_some() {
            return n.over.as_ref().map_or(0.0, calls) + n.value.as_ref().map_or(0.0, calls);
        }
        match (&n.kind, n.effect) {
            (NodeKind::Call { .. }, _) => 3.0,
            (_, Effect::Pure) => 0.0,
            (_, Effect::Llm) => 3.0,
            (_, Effect::Sandbox) => 5.0,
            (_, Effect::Read | Effect::Write | Effect::WriteOnce) => 1.0,
        }
    }

    /// Sets each node's rank: its weight plus the largest rank among the
    /// nodes that read it. Nodes come after their inputs, so one backward
    /// pass is enough (linear in the size of the graph).
    pub fn rank_nodes(&mut self) {
        let mut rank: Vec<f64> = self.nodes.iter().map(Self::weight).collect();
        for i in (0..self.nodes.len()).rev() {
            for input in self.nodes[i].inputs.clone() {
                let j = input.0 as usize;
                let via = Self::weight(&self.nodes[j]) + rank[i];
                if via > rank[j] {
                    rank[j] = via;
                }
            }
        }
        for (n, r) in self.nodes.iter_mut().zip(rank) {
            n.rank = r;
        }
    }
}

/// A whole program.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Program {
    pub models: Vec<Model>,
    pub tools: Vec<Tool>,
    pub prompts: Vec<Prompt>,
    pub graphs: Vec<Graph>,
    pub entities: Vec<Entity>,
    pub defs: Vec<Def>,
    /// Types a run can `receive`: name and JSON Schema (to check what is
    /// delivered).
    pub messages: Vec<(String, String)>,
    pub routers: Vec<Router>,
}

/// A router (decision D30): its models, cheapest first, and the `def` that
/// checks an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Router {
    pub name: String,
    pub models: Vec<usize>,
    pub check: usize,
}

/// A pure function (decision D27): parameters in local slots `0..n`, its
/// body one expression (statements become `Let`s and `If`s).
#[derive(Debug, Clone, PartialEq)]
pub struct Def {
    pub name: String,
    pub params: Vec<String>,
    pub nlocals: usize,
    pub body: Expr,
}

/// An entity (decision D15): state kept between runs, one per key, changed
/// one message at a time.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub name: String,
    /// The state's fields and their initial values.
    pub state: Vec<(String, Expr)>,
    pub handlers: Vec<Handler>,
}

/// A message an entity handles. Its body sees the key in local slot 0, the
/// message's parameters in the next slots, and the state as `State(field)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    pub name: String,
    pub params: Vec<String>,
    /// The answer, for a handler that answers (`ask`).
    pub answer: Option<Expr>,
    /// New values of state fields, for a handler that changes it (`send`),
    /// all computed from the state before the message.
    pub updates: Vec<(String, Expr)>,
    pub nlocals: usize,
}

impl Program {
    pub fn graph_index(&self, name: &str) -> Option<usize> {
        self.graphs.iter().position(|g| g.name == name)
    }
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, g) in self.graphs.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{g}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Graph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|(n, t)| format!("{n}: {t}"))
            .collect();
        write!(
            f,
            "graph {}({}) -> {}",
            self.name,
            params.join(", "),
            self.ret
        )?;
        if let Some(e) = self.effect {
            write!(f, " [{e}]")?;
        }
        writeln!(f)?;
        for n in &self.nodes {
            write!(f, "  {} {}", n.id, n.name)?;
            if let Some(v) = &n.fan_out {
                write!(f, "[{v}]")?;
            }
            write!(f, ": {} = {} [{}]", n.ty, n.kind, n.effect)?;
            if !n.inputs.is_empty() {
                let ins: Vec<String> = n.inputs.iter().map(ToString::to_string).collect();
                write!(f, " <- {}", ins.join(" "))?;
            }
            writeln!(f)?;
        }
        match self.output {
            Some(o) => writeln!(f, "  return {o}"),
            None => writeln!(f, "  return"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_join_to_the_most_dangerous() {
        assert_eq!(Effect::Llm.join(Effect::Read), Effect::Read);
        assert_eq!(Effect::Write.join(Effect::Pure), Effect::Write);
        assert_eq!(Effect::WriteOnce.join(Effect::Sandbox), Effect::WriteOnce);
    }

    fn node(id: u32, effect: Effect, inputs: &[u32]) -> Node {
        Node {
            id: NodeId(id),
            name: format!("n{id}"),
            ty: "Text".into(),
            kind: NodeKind::Pure,
            fan_out: None,
            effect,
            inputs: inputs.iter().map(|&i| NodeId(i)).collect(),
            over: None,
            value: None,
            rank: 0.0,
            nlocals: 0,
        }
    }

    #[test]
    fn ranks_follow_the_longest_path() {
        // n0 (llm) -> n1 (read) -> n3 (llm); n0 -> n2 (pure) -> n3
        let mut g = Graph {
            nodes: vec![
                node(0, Effect::Llm, &[]),
                node(1, Effect::Read, &[0]),
                node(2, Effect::Pure, &[0]),
                node(3, Effect::Llm, &[1, 2]),
            ],
            ..Graph::default()
        };
        g.rank_nodes();
        let ranks: Vec<f64> = g.nodes.iter().map(|n| n.rank).collect();
        assert_eq!(ranks, vec![7.0, 4.0, 3.0, 3.0]);
    }

    #[test]
    fn displays_a_graph() {
        let g = Graph {
            name: "g".into(),
            params: vec![("x".into(), "Text".into())],
            ret: "Text".into(),
            effect: Some(Effect::Llm),
            nodes: vec![Node {
                id: NodeId(0),
                name: "a".into(),
                ty: "Text".into(),
                kind: NodeKind::Model {
                    model: "m".into(),
                    prompt: "p".into(),
                },
                fan_out: None,
                effect: Effect::Llm,
                inputs: vec![],
                over: None,
                value: None,
                rank: 0.0,
                nlocals: 0,
            }],
            output: Some(NodeId(0)),
            limits: Limits::default(),
        };
        assert_eq!(
            g.to_string(),
            "graph g(x: Text) -> Text [llm]\n  %0 a: Text = model m(p) [llm]\n  return %0\n"
        );
    }
}
