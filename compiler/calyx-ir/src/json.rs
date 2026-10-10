//! JSON form of the IR, loaded by the C runtime.
//!
//! Written by hand to keep the compiler free of dependencies. Expressions
//! are objects with a kind `k`; names are already indices into the
//! program's `models`, `tools`, `prompts` and `graphs`.

use crate::{
    Effect, Expr, Graph, Limits, Model, Node, Part, Program, Prompt, PromptPart, Tool, Uncertain,
};

fn opt_string(o: &mut String, v: Option<&str>) {
    match v {
        Some(s) => string(o, s),
        None => o.push_str("null"),
    }
}

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
        o.push_str(",\"entities\":");
        list(&mut o, &self.entities, entity);
        o.push_str(",\"messages\":");
        list(&mut o, &self.messages, |o, (n, s)| {
            o.push_str("{\"name\":");
            string(o, n);
            o.push_str(",\"schema\":");
            o.push_str(s);
            o.push('}');
        });
        o.push_str(",\"routers\":");
        list(&mut o, &self.routers, |o, r| {
            o.push_str("{\"name\":");
            string(o, &r.name);
            o.push_str(",\"models\":");
            list(o, &r.models, |o, m| o.push_str(&m.to_string()));
            o.push_str(&format!(",\"check\":{}}}", r.check));
        });
        o.push_str(",\"defs\":");
        list(&mut o, &self.defs, |o, d| {
            o.push_str("{\"name\":");
            string(o, &d.name);
            o.push_str(",\"params\":");
            strings(o, &d.params);
            o.push_str(&format!(",\"nlocals\":{},\"body\":", d.nlocals));
            expr(o, &d.body, &mut 0);
            o.push('}');
        });
        o.push('}');
        o
    }
}

fn entity(o: &mut String, e: &crate::Entity) {
    o.push_str("{\"name\":");
    string(o, &e.name);
    o.push_str(",\"state\":");
    list(o, &e.state, |o, (n, v)| {
        o.push_str("{\"name\":");
        string(o, n);
        o.push_str(",\"init\":");
        expr(o, v, &mut 0);
        o.push('}');
    });
    o.push_str(",\"handlers\":");
    list(o, &e.handlers, |o, h| {
        o.push_str("{\"name\":");
        string(o, &h.name);
        o.push_str(",\"params\":");
        strings(o, &h.params);
        o.push_str(&format!(",\"nlocals\":{},\"answer\":", h.nlocals));
        opt_expr(o, h.answer.as_ref());
        o.push_str(",\"updates\":");
        list(o, &h.updates, |o, (f, v)| {
            o.push_str("{\"field\":");
            string(o, f);
            o.push_str(",\"v\":");
            expr(o, v, &mut 0);
            o.push('}');
        });
        o.push('}');
    });
    o.push('}');
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
    o.push_str(&format!(",\"returns_text\":{}", t.returns_text));
    o.push_str(",\"returns\":");
    o.push_str(if t.returns.is_empty() {
        "null"
    } else {
        &t.returns
    });
    o.push_str(",\"schema\":");
    o.push_str(&t.schema);
    o.push_str(",\"description\":");
    opt_string(o, t.description.as_deref());
    o.push_str(&format!(",\"repeatable\":{}", t.repeatable));
    o.push_str(",\"idempotency_key\":");
    opt_u64(o, t.idempotency_key.map(|i| i as u64));
    o.push_str(",\"batch\":");
    opt_u64(o, t.batch.map(|i| i as u64));
    o.push_str(",\"on_uncertain\":");
    match &t.on_uncertain {
        None => o.push_str("null"),
        Some(Uncertain::Pause) => o.push_str("{\"policy\":\"pause\"}"),
        Some(Uncertain::AcceptLoss) => o.push_str("{\"policy\":\"accept_loss\"}"),
        Some(Uncertain::Verify { tool, args }) => {
            let args: Vec<String> = args.iter().map(usize::to_string).collect();
            o.push_str(&format!(
                "{{\"policy\":\"verify\",\"tool\":{tool},\"args\":[{}]}}",
                args.join(",")
            ));
        }
    }
    o.push_str(",\"compensate\":");
    match &t.compensate {
        None => o.push_str("null"),
        Some((tool, args)) => {
            let args: Vec<String> = args.iter().map(usize::to_string).collect();
            o.push_str(&format!(
                "{{\"tool\":{tool},\"args\":[{}]}}",
                args.join(",")
            ));
        }
    }
    o.push_str(&format!(",\"returns_unit\":{}", t.returns_unit));
    o.push_str(",\"checks\":");
    opt_string(o, t.checks.as_deref());
    o.push_str(",\"borrows\":");
    list(o, &t.borrows, |o, b| {
        o.push_str(match b {
            None => "null",
            Some(true) => "\"edits\"",
            Some(false) => "\"reads\"",
        })
    });
    o.push('}');
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
    o.push_str(",\"limits\":");
    limits(o, &g.limits);
    o.push('}');
}

fn limits(o: &mut String, l: &Limits) {
    o.push_str("{\"threads\":");
    opt_u64(o, l.threads);
    o.push_str(",\"rate_per_s\":");
    opt_u64(o, l.rate_per_s);
    o.push_str(",\"budget\":");
    match &l.budget {
        Some((amount, unit)) => {
            o.push_str(&format!("{{\"amount\":{amount:?},\"unit\":"));
            string(o, unit);
            o.push('}');
        }
        None => o.push_str("null"),
    }
    o.push('}');
}

fn node(o: &mut String, n: &Node) {
    o.push_str("{\"name\":");
    string(o, &n.name);
    o.push_str(",\"type\":");
    string(o, &n.ty);
    o.push_str(",\"effect\":");
    effect(o, n.effect);
    o.push_str(",\"inputs\":");
    list(o, &n.inputs, |o, i| o.push_str(&i.0.to_string()));
    o.push_str(&format!(",\"rank\":{:?},\"nlocals\":{}", n.rank, n.nlocals));
    // Call ids start at 0 in each expression: the list and the items are
    // separate places in the realized graph (`node#k` and `node[j]#k`).
    o.push_str(",\"over\":");
    opt_expr(o, n.over.as_ref());
    o.push_str(",\"value\":");
    opt_expr(o, n.value.as_ref());
    o.push('}');
}

fn opt_expr(o: &mut String, e: Option<&Expr>) {
    match e {
        Some(e) => expr(o, e, &mut 0),
        None => o.push_str("null"),
    }
}

/// A call gets an id after its arguments (post-order), so ids do not
/// depend on which calls happen to finish first. The runtime keys calls in
/// the journal by node instance and id.
fn call(o: &mut String, kind: &str, fields: &[(&str, usize)], args: &[Expr], ids: &mut usize) {
    let mut a = String::new();
    list(&mut a, args, |o, e| expr(o, e, ids));
    let id = *ids;
    *ids += 1;
    o.push_str(&format!("{{\"k\":\"{kind}\",\"id\":{id}"));
    for (name, v) in fields {
        o.push_str(&format!(",\"{name}\":{v}"));
    }
    o.push_str(",\"args\":");
    o.push_str(&a);
    o.push('}');
}

fn expr(o: &mut String, e: &Expr, ids: &mut usize) {
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
            expr(o, base, ids);
            o.push_str(",\"name\":");
            string(o, name);
            o.push('}');
        }
        Expr::List(items) => {
            o.push_str("{\"k\":\"list\",\"items\":");
            list(o, items, |o, e| expr(o, e, ids));
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
                    expr(o, e, ids);
                    o.push('}');
                }
            });
            o.push('}');
        }
        Expr::Model {
            model,
            prompt,
            args,
        } => call(
            o,
            "model",
            &[("model", *model), ("prompt", *prompt)],
            args,
            ids,
        ),
        // Model `i` of the router is keyed `scope#id.i`; the choice, `scope#id`.
        Expr::Route {
            router,
            prompt,
            args,
        } => call(
            o,
            "route",
            &[("router", *router), ("prompt", *prompt)],
            args,
            ids,
        ),
        Expr::Tool {
            tool,
            args,
            requires,
        } => {
            call(o, "tool", &[("tool", *tool)], args, ids);
            if !requires.is_empty() {
                // Inside the call's object: drop its `}` and add the list.
                o.pop();
                o.push_str(",\"requires\":");
                list(o, requires, |o, e| expr(o, e, ids));
                o.push('}');
            }
        }
        Expr::Message {
            send,
            entity,
            handler,
            key,
            args,
        } => {
            // The key is the first argument: a call's arguments come first.
            let mut all = vec![(**key).clone()];
            all.extend(args.iter().cloned());
            let kind = if *send { "send" } else { "ask" };
            call(
                o,
                kind,
                &[("entity", *entity), ("handler", *handler)],
                &all,
                ids,
            );
        }
        Expr::Bool(b) => o.push_str(&format!("{{\"k\":\"bool\",\"v\":{b}}}")),
        // Journaled like a call, so it gets an id after what it contains.
        Expr::Receive {
            message,
            about,
            timeout_s,
            on_timeout,
        } => {
            let mut a = String::new();
            if let Some(about) = about {
                expr(&mut a, about, ids);
            }
            let mut v = String::new();
            expr(&mut v, on_timeout, ids);
            let id = *ids;
            *ids += 1;
            o.push_str(&format!("{{\"k\":\"receive\",\"id\":{id},\"message\":"));
            string(o, message);
            if !a.is_empty() {
                o.push_str(&format!(",\"about\":{a}"));
            }
            o.push_str(&format!(",\"timeout_s\":{timeout_s},\"on_timeout\":{v}}}"));
        }
        Expr::Let { slot, value, body } => {
            o.push_str(&format!("{{\"k\":\"let\",\"slot\":{slot},\"v\":"));
            expr(o, value, ids);
            o.push_str(",\"body\":");
            expr(o, body, ids);
            o.push('}');
        }
        Expr::Comprehension {
            slot,
            over,
            body,
            cond,
        } => {
            o.push_str(&format!("{{\"k\":\"comp\",\"slot\":{slot},\"over\":"));
            expr(o, over, ids);
            o.push_str(",\"body\":");
            expr(o, body, ids);
            o.push_str(",\"cond\":");
            match cond {
                Some(c) => expr(o, c, ids),
                None => o.push_str("null"),
            }
            o.push('}');
        }
        // Pure: no call id (nothing goes to the journal).
        Expr::Def { def, args } => {
            o.push_str(&format!("{{\"k\":\"def\",\"def\":{def},\"args\":"));
            list(o, args, |o, e| expr(o, e, ids));
            o.push('}');
        }
        Expr::Builtin { name, args } => {
            o.push_str("{\"k\":\"builtin\",\"name\":");
            string(o, name);
            o.push_str(",\"args\":");
            list(o, args, |o, e| expr(o, e, ids));
            o.push('}');
        }
        Expr::State(field) => {
            o.push_str("{\"k\":\"state\",\"field\":");
            string(o, field);
            o.push('}');
        }
        Expr::Graph { graph, args } => call(o, "graph", &[("graph", *graph)], args, ids),
        Expr::Local(i) => o.push_str(&format!("{{\"k\":\"local\",\"i\":{i}}}")),
        Expr::Record(fields) => {
            o.push_str("{\"k\":\"record\",\"names\":");
            list(o, fields, |o, (n, _)| string(o, n));
            o.push_str(",\"values\":");
            list(o, fields, |o, (_, e)| expr(o, e, ids));
            o.push('}');
        }
        Expr::Binary { op, left, right } => {
            o.push_str("{\"k\":\"bin\",\"op\":");
            string(o, op);
            o.push_str(",\"l\":");
            expr(o, left, ids);
            o.push_str(",\"r\":");
            expr(o, right, ids);
            o.push('}');
        }
        Expr::Unary { op, value } => {
            o.push_str("{\"k\":\"un\",\"op\":");
            string(o, op);
            o.push_str(",\"v\":");
            expr(o, value, ids);
            o.push('}');
        }
        Expr::If { cond, then, els } => {
            o.push_str("{\"k\":\"if\",\"c\":");
            expr(o, cond, ids);
            o.push_str(",\"t\":");
            expr(o, then, ids);
            o.push_str(",\"e\":");
            expr(o, els, ids);
            o.push('}');
        }
        Expr::Match { value, cases } => {
            o.push_str("{\"k\":\"match\",\"v\":");
            expr(o, value, ids);
            o.push_str(",\"cases\":");
            list(o, cases, |o, c| {
                o.push_str("{\"variant\":");
                opt_string(o, c.variant.as_deref());
                o.push_str(",\"binds\":");
                list(o, &c.binds, |o, (f, slot)| {
                    o.push('[');
                    string(o, f);
                    o.push_str(&format!(",{slot}]"));
                });
                o.push_str(",\"body\":");
                expr(o, &c.body, ids);
                o.push('}');
            });
            o.push('}');
        }
        Expr::Loop {
            slot,
            init,
            max,
            body,
            on_limit,
            rounds,
        } => {
            let mut inner = String::new();
            inner.push_str(",\"init\":");
            expr(&mut inner, init, ids);
            inner.push_str(",\"body\":");
            expr(&mut inner, body, ids);
            let id = *ids;
            *ids += 1;
            o.push_str(&format!(
                "{{\"k\":\"loop\",\"id\":{id},\"slot\":{slot},\"max\":{max}"
            ));
            o.push_str(&inner);
            o.push_str(",\"on_limit\":");
            opt_string(o, on_limit.as_deref());
            if *rounds {
                o.push_str(",\"rounds\":true");
            }
            o.push('}');
        }
        // Each item's calls are keyed by the item: `scope#id[j]#call`.
        Expr::Each { slot, over, body } => {
            let mut inner = String::new();
            inner.push_str(",\"over\":");
            expr(&mut inner, over, ids);
            inner.push_str(",\"body\":");
            expr(&mut inner, body, ids);
            let id = *ids;
            *ids += 1;
            o.push_str(&format!("{{\"k\":\"each\",\"id\":{id},\"slot\":{slot}"));
            o.push_str(&inner);
            o.push('}');
        }
        // A branch's calls are keyed by it: `scope#id.name#call`; the
        // winner goes to the journal as `scope#id`.
        Expr::Race {
            count,
            branches,
            slot,
            cond,
            on_none,
            on_none_fail,
        } => {
            let mut inner = String::new();
            inner.push_str(",\"count\":");
            opt_u64(&mut inner, *count);
            inner.push_str(",\"names\":");
            list(&mut inner, branches, |o, (n, _)| string(o, n));
            inner.push_str(",\"branches\":");
            list(&mut inner, branches, |o, (_, b)| expr(o, b, ids));
            inner.push_str(",\"cond\":");
            match cond {
                Some(c) => expr(&mut inner, c, ids),
                None => inner.push_str("null"),
            }
            inner.push_str(",\"on_none\":");
            match on_none {
                Some(v) => expr(&mut inner, v, ids),
                None => inner.push_str("null"),
            }
            inner.push_str(",\"on_none_fail\":");
            opt_string(&mut inner, on_none_fail.as_deref());
            let id = *ids;
            *ids += 1;
            o.push_str(&format!("{{\"k\":\"race\",\"id\":{id},\"slot\":{slot}"));
            o.push_str(&inner);
            o.push('}');
        }
        Expr::Done(v) | Expr::Next(v) | Expr::Try(v) => {
            let k = match e {
                Expr::Done(_) => "done",
                Expr::Next(_) => "next",
                _ => "try",
            };
            o.push_str(&format!("{{\"k\":\"{k}\",\"v\":"));
            expr(o, v, ids);
            o.push('}');
        }
        Expr::Agent(a) => {
            let mut args = String::new();
            list(&mut args, &a.args, |o, e| expr(o, e, ids));
            let id = *ids;
            *ids += 1;
            o.push_str(&format!(
                "{{\"k\":\"agent\",\"id\":{id},\"model\":{},\"prompt\":{},\"args\":{args},\"tools\":",
                a.model, a.prompt
            ));
            list(o, &a.tools, |o, t| o.push_str(&t.to_string()));
            if a.bound.iter().any(|b| !b.is_empty()) {
                o.push_str(",\"bound\":");
                list(o, &a.bound, |o, b| {
                    list(o, b, |o, (p, v)| {
                        o.push_str(&format!("{{\"param\":{p},\"v\":"));
                        expr(o, v, ids);
                        o.push('}');
                    })
                });
            }
            o.push_str(&format!(
                ",\"max_turns\":{},\"on_turn_limit\":",
                a.max_turns
            ));
            opt_string(o, a.on_turn_limit.as_deref());
            o.push_str(",\"on_stuck\":");
            opt_string(o, a.on_stuck.as_deref());
            o.push('}');
        }
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
                args: vec![
                    Expr::Field(Box::new(Expr::Node(crate::NodeId(2))), "q".into()),
                    Expr::Tool {
                        tool: 0,
                        args: vec![Expr::Item],
                        requires: Vec::new(),
                    },
                ],
            },
            &mut 0,
        );
        // The tool (an argument) gets id 0, the model call id 1.
        assert_eq!(
            o,
            r#"{"k":"model","id":1,"model":0,"prompt":1,"args":[{"k":"field","base":{"k":"node","i":2},"name":"q"},{"k":"tool","id":0,"tool":0,"args":[{"k":"item"}]}]}"#
        );
    }
}
