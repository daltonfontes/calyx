//! Intermediate representation (IR) of a compiled Calyx program.
//!
//! The IR is the *template* of decision D8: a parameterized task graph whose
//! fan-outs and loops are symbolic. The runtime unfolds it into the realized
//! graph as values arrive. It is plain data, so several versions of a program
//! can coexist (decision D23) and graphs generated at run time can be checked
//! and interpreted (decision D4).
//!
//! M0 only fixes the vocabulary; M1 fills it from the parser.

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
}

/// Index of a node inside a [`Graph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// A call to a model with a prompt.
    Model { model: String, prompt: String },
    /// A call to a tool.
    Tool { tool: String },
    /// One node per element of `over`, producing an ordered list (decision D7).
    FanOut { over: NodeId, body: Box<NodeKind> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: NodeId,
    pub name: String,
    pub kind: NodeKind,
    pub effect: Effect,
    /// Data dependencies: nodes whose values this node reads.
    pub inputs: Vec<NodeId>,
}

/// A compiled graph: the template the runtime unfolds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Graph {
    pub name: String,
    pub nodes: Vec<Node>,
    pub output: Option<NodeId>,
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
}
