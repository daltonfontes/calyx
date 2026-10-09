//! The effect a program declares for a tool, against what the tool's MCP
//! server says about it (`annotations` in `tools/list`: `readOnlyHint`,
//! `idempotentHint`, `destructiveHint`).
//!
//! The declaration is what the runtime acts on: it retries reads and keyed
//! writes, never repeats a `write once`. The server's hints cannot replace
//! it (they are hints, and say nothing about keys or what to do when a call
//! may have happened), but they can contradict it, and that is worth a
//! warning: a tool declared `read` that the server says changes something
//! is repeated by every retry and every replay.
//!
//! Only tools whose server sends annotations are checked: without them the
//! MCP defaults (not read-only, not idempotent) would flag every read tool
//! of a server that simply says nothing.

use std::collections::HashMap;

use serde_json::Value;

use crate::config::Config;
use crate::mcp;

/// A tool as the program declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub name: String,
    /// `read`, `write`, `write once` or `sandbox`.
    pub effect: String,
    /// It has an `idempotency_key`.
    pub keyed: bool,
}

/// A contradiction between a declaration and its server, or a tool the
/// server does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub code: &'static str,
    pub error: bool,
    pub tool: String,
    pub message: String,
    pub expected: String,
    pub observed: String,
}

impl Finding {
    /// One line, for the run's warnings.
    pub fn line(&self) -> String {
        format!(
            "calyx: {}[{}]: {} ({})",
            if self.error { "error" } else { "warning" },
            self.code,
            self.message,
            self.observed
        )
    }

    /// The compiler's diagnostic format, for `calyx check --tools`.
    pub fn render(&self) -> String {
        format!(
            "{}[{}]: {}\n- expected : {}\n- observed : {}\nLocation: [tools.{}] in calyx.toml\n",
            if self.error { "error" } else { "warning" },
            self.code,
            self.message,
            self.expected,
            self.observed,
            self.tool
        )
    }
}

/// The tools a compiled program declares (its IR in JSON).
pub fn declared(ir_json: &str) -> Vec<Declared> {
    let ir: Value = serde_json::from_str(ir_json).unwrap_or_default();
    ir["tools"]
        .as_array()
        .map(|ts| {
            ts.iter()
                .filter_map(|t| {
                    Some(Declared {
                        name: t["name"].as_str()?.to_owned(),
                        effect: t["effect"].as_str()?.to_owned(),
                        keyed: t["idempotency_key"].is_u64(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn hint(ann: &Value, name: &str) -> String {
    match ann.get(name) {
        Some(v) => format!("{name}: {v}"),
        None => format!("no {name}"),
    }
}

/// What contradicts `d` in the annotations its server sent, if anything.
pub fn mismatch(d: &Declared, annotations: Option<&Value>) -> Option<Finding> {
    let ann = annotations.filter(|a| a.is_object())?;
    let read_only = ann["readOnlyHint"] == true;
    let idempotent = ann["idempotentHint"] == true;
    let finding = |code, message: String, expected: &str, observed: String| Finding {
        code,
        error: false,
        tool: d.name.clone(),
        message,
        expected: expected.to_owned(),
        observed,
    };
    match d.effect.as_str() {
        "read" if !read_only => Some(finding(
            "W0701",
            format!(
                "tool `{}` is declared `read`, but its server does not say it is read-only",
                d.name
            ),
            "`readOnlyHint: true`; or declare the tool as a write: reads are retried and replayed freely",
            format!(
                "{}, {}",
                hint(ann, "readOnlyHint"),
                hint(ann, "destructiveHint")
            ),
        )),
        "write" if !d.keyed && !idempotent && !read_only => Some(finding(
            "W0702",
            format!(
                "tool `{}` is a `write` without a key, but its server does not say it is idempotent",
                d.name
            ),
            "`idempotentHint: true`; or an `idempotency_key`, or `effect write once`: the runtime repeats a `write` after failures",
            hint(ann, "idempotentHint"),
        )),
        _ => None,
    }
}

/// Starts the servers of the program's tools (as `calyx run` would) and
/// compares each declaration with what its server says.
pub fn inspect(config: &Config, tools: &[Declared]) -> Vec<Finding> {
    let mut servers: HashMap<Vec<String>, Result<mcp::Server, String>> = HashMap::new();
    let mut out = Vec::new();
    for d in tools {
        let Some(cfg) = config.tools.get(&d.name) else {
            out.push(Finding {
                code: "E0701",
                error: true,
                tool: d.name.clone(),
                message: format!("no MCP server for tool `{}`", d.name),
                expected: format!("`[tools.{}]` with `command = [...]` in calyx.toml", d.name),
                observed: "no such section".into(),
            });
            continue;
        };
        let server = servers
            .entry(cfg.command.clone())
            .or_insert_with(|| mcp::Server::start(cfg).map_err(|e| e.message));
        let remote = cfg.remote_name.clone().unwrap_or_else(|| d.name.clone());
        match server {
            Err(e) => out.push(Finding {
                code: "E0702",
                error: true,
                tool: d.name.clone(),
                message: format!("the MCP server of tool `{}` did not start", d.name),
                expected: "a server that answers `initialize` and `tools/list`".into(),
                observed: e.clone(),
            }),
            Ok(s) if !s.tools.contains(&remote) => out.push(Finding {
                code: "E0703",
                error: true,
                tool: d.name.clone(),
                message: format!("the MCP server has no tool `{remote}`"),
                expected: format!("`{remote}` in its `tools/list`"),
                observed: format!("it offers: {}", s.tools.join(", ")),
            }),
            Ok(s) => out.extend(mismatch(d, s.annotations.get(&remote))),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(effect: &str, keyed: bool) -> Declared {
        Declared {
            name: "t".into(),
            effect: effect.into(),
            keyed,
        }
    }

    fn code(d: &Declared, ann: Value) -> Option<&'static str> {
        mismatch(d, Some(&ann)).map(|f| f.code)
    }

    #[test]
    fn a_read_the_server_says_changes_things_is_a_warning() {
        let read = tool("read", false);
        assert_eq!(code(&read, json!({"readOnlyHint": false})), Some("W0701"));
        assert_eq!(code(&read, json!({"destructiveHint": true})), Some("W0701"));
        assert_eq!(code(&read, json!({"readOnlyHint": true})), None);
    }

    #[test]
    fn an_unkeyed_write_needs_an_idempotent_tool() {
        assert_eq!(
            code(&tool("write", false), json!({"idempotentHint": false})),
            Some("W0702")
        );
        assert_eq!(
            code(&tool("write", false), json!({"idempotentHint": true})),
            None
        );
        // A key, or `write once`: the runtime does not repeat it blindly.
        assert_eq!(
            code(&tool("write", true), json!({"idempotentHint": false})),
            None
        );
        assert_eq!(
            code(&tool("write once", false), json!({"readOnlyHint": false})),
            None
        );
    }

    #[test]
    fn a_server_that_says_nothing_is_not_judged() {
        assert!(mismatch(&tool("read", false), None).is_none());
        assert!(mismatch(&tool("write", false), Some(&json!(null))).is_none());
    }
}
