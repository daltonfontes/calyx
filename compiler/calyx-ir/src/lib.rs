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
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeKind::Pure => f.write_str("pure"),
            NodeKind::Model { model, prompt } => write!(f, "model {model}({prompt})"),
            NodeKind::Tool { tool } => write!(f, "tool {tool}"),
            NodeKind::Call { graph } => write!(f, "graph {graph}"),
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
    /// Arguments follow the tool's parameters.
    Tool {
        tool: usize,
        args: Vec<Expr>,
    },
    /// Arguments follow the graph's parameters.
    Graph {
        graph: usize,
        args: Vec<Expr>,
    },
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
}

/// A whole program.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Program {
    pub models: Vec<Model>,
    pub tools: Vec<Tool>,
    pub prompts: Vec<Prompt>,
    pub graphs: Vec<Graph>,
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
            }],
            output: Some(NodeId(0)),
        };
        assert_eq!(
            g.to_string(),
            "graph g(x: Text) -> Text [llm]\n  %0 a: Text = model m(p) [llm]\n  return %0\n"
        );
    }
}
