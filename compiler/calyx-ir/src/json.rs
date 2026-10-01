//! JSON form of the IR, loaded by the C runtime.
//!
//! Written by hand to keep the compiler free of dependencies. Expressions
//! are objects with a kind `k`; names are already indices into the
//! program's `models`, `tools`, `prompts` and `graphs`.

use crate::{Effect, Expr, Graph, Model, Node, Part, Program, Prompt, PromptPart, Tool};

/// Format version, checked by the runtime.
pub const IR_VERSION: u32 = 1;

pub(crate) fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn list<T>(out: &mut String, items: &[T], mut f: impl FnMut(&mut String, &T)) {
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        f(out, item);
    }
    out.push(']');
}

fn strings(out: &mut String, items: &[String]) {
    list(out, items, |o, s| string(o, s));
}

fn opt_u64(out: &mut String, v: Option<u64>) {
    match v {
        Some(n) => out.push_str(&n.to_string()),
        None => out.push_str("null"),
    }
}

fn effect(out: &mut String, e: Effect) {
    string(out, e.as_str());
}

impl Program {
    /// The program as one JSON object.
    pub fn to_json(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!("{{\"version\":{IR_VERSION},\"models\":"));
        list(&mut o, &self.models, model);
        o.push_str(",\"tools\":");
        list(&mut o, &self.tools, tool);
        o.push_str(",\"prompts\":");
        list(&mut o, &self.prompts, prompt);
        o.push_str(",\"graphs\":");
        list(&mut o, &self.graphs, graph);
        o.push('}');
        o
    }
}

fn model(o: &mut String, m: &Model) {
    o.push_str("{\"name\":");
    string(o, &m.name);
    o.push_str(",\"id\":");
    string(o, &m.id);
    o.push_str(",\"max_output\":");
    opt_u64(o, m.max_output);
    o.push('}');
}

fn tool(o: &mut String, t: &Tool) {
    o.push_str("{\"name\":");
    string(o, &t.name);
    o.push_str(",\"params\":");
    strings(o, &t.params);
    o.push_str(",\"effect\":");
    effect(o, t.effect);
    o.push_str(",\"max_output\":");
    opt_u64(o, t.max_output);
    o.push_str(&format!(",\"timeout_ms\":{}", t.timeout_ms));
    o.push_str(",\"retry_on\":");
    strings(o, &t.retry_on);
    o.push_str(&format!(",\"returns_text\":{}}}", t.returns_text));
}

fn prompt(o: &mut String, p: &Prompt) {
    o.push_str("{\"name\":");
    string(o, &p.name);
    o.push_str(",\"params\":");
    strings(o, &p.params);
    o.push_str(",\"parts\":");
    list(o, &p.parts, |o, part| match part {
        PromptPart::Lit(s) => {
            o.push_str("{\"lit\":");
            string(o, s);
            o.push('}');
        }
        PromptPart::Path(path) => {
            o.push_str("{\"path\":");
            strings(o, path);
            o.push('}');
        }
    });
    o.push_str(",\"schema\":");
    match &p.schema {
        // Already JSON.
        Some(s) => o.push_str(s),
        None => o.push_str("null"),
    }
    o.push_str(&format!(",\"wrapped\":{}}}", p.wrapped));
}

fn graph(o: &mut String, g: &Graph) {
    o.push_str("{\"name\":");
    string(o, &g.name);
    o.push_str(",\"params\":");
    list(o, &g.params, |o, (n, _)| string(o, n));
    o.push_str(",\"param_types\":");
    list(o, &g.params, |o, (_, t)| string(o, t));
    o.push_str(",\"ret\":");
    string(o, &g.ret);
    o.push_str(",\"nodes\":");
    list(o, &g.nodes, node);
    o.push_str(",\"output\":");
    opt_u64(o, g.output.map(|n| u64::from(n.0)));
    o.push('}');
}

fn node(o: &mut String, n: &Node) {
    o.push_str("{\"name\":");
    string(o, &n.name);
    o.push_str(",\"type\":");
    string(o, &n.ty);
    o.push_str(",\"effect\":");
    effect(o, n.effect);
    o.push_str(",\"over\":");
    opt_expr(o, n.over.as_ref());
    o.push_str(",\"value\":");
    opt_expr(o, n.value.as_ref());
    o.push('}');
}

fn opt_expr(o: &mut String, e: Option<&Expr>) {
    match e {
        Some(e) => expr(o, e),
        None => o.push_str("null"),
    }
}

fn call(o: &mut String, kind: &str, fields: &[(&str, usize)], args: &[Expr]) {
    o.push_str(&format!("{{\"k\":\"{kind}\""));
    for (name, v) in fields {
        o.push_str(&format!(",\"{name}\":{v}"));
    }
    o.push_str(",\"args\":");
    list(o, args, expr);
    o.push('}');
}

fn expr(o: &mut String, e: &Expr) {
    match e {
        Expr::Text(s) => {
            o.push_str("{\"k\":\"text\",\"v\":");
            string(o, s);
            o.push('}');
        }
        Expr::Int(n) => o.push_str(&format!("{{\"k\":\"int\",\"v\":{n}}}")),
        Expr::Float(f) => o.push_str(&format!("{{\"k\":\"float\",\"v\":{f:?}}}")),
        Expr::Param(i) => o.push_str(&format!("{{\"k\":\"param\",\"i\":{i}}}")),
        Expr::Node(n) => o.push_str(&format!("{{\"k\":\"node\",\"i\":{}}}", n.0)),
        Expr::Item => o.push_str("{\"k\":\"item\"}"),
        Expr::Field(base, name) => {
            o.push_str("{\"k\":\"field\",\"base\":");
            expr(o, base);
            o.push_str(",\"name\":");
            string(o, name);
            o.push('}');
        }
        Expr::List(items) => {
            o.push_str("{\"k\":\"list\",\"items\":");
            list(o, items, expr);
            o.push('}');
        }
        Expr::Interp(parts) => {
            o.push_str("{\"k\":\"interp\",\"parts\":");
            list(o, parts, |o, p| match p {
                Part::Lit(s) => {
                    o.push_str("{\"lit\":");
                    string(o, s);
                    o.push('}');
                }
                Part::Expr(e) => {
                    o.push_str("{\"expr\":");
                    expr(o, e);
                    o.push('}');
                }
            });
            o.push('}');
        }
        Expr::Model {
            model,
            prompt,
            args,
        } => call(o, "model", &[("model", *model), ("prompt", *prompt)], args),
        Expr::Tool { tool, args } => call(o, "tool", &[("tool", *tool)], args),
        Expr::Graph { graph, args } => call(o, "graph", &[("graph", *graph)], args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_text() {
        let mut o = String::new();
        string(&mut o, "a\"b\\c\nd\u{1}");
        assert_eq!(o, r#""a\"b\\c\nd\u0001""#);
    }

    #[test]
    fn writes_expressions() {
        let mut o = String::new();
        expr(
            &mut o,
            &Expr::Model {
                model: 0,
                prompt: 1,
                args: vec![Expr::Field(
                    Box::new(Expr::Node(crate::NodeId(2))),
                    "q".into(),
                )],
            },
        );
        assert_eq!(
            o,
            r#"{"k":"model","model":0,"prompt":1,"args":[{"k":"field","base":{"k":"node","i":2},"name":"q"}]}"#
        );
    }
}
