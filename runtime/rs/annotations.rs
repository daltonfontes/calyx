//! The effect a program declares for a tool, against what the tool's MCP
//! server says about it (`annotations` in `tools/list`: `readOnlyHint`,
//! `idempotentHint`, `destructiveHint`, and `idempotencyKeyHint`, which is
//! not in MCP yet: it is proposed in `docs/mcp/idempotency-key-hint.md`).
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
    /// Its parameters, and which one is the key.
    pub params: Vec<String>,
    pub key: Option<usize>,
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
                        params: t["params"]
                            .as_array()
                            .map(|ps| {
                                ps.iter()
                                    .filter_map(|p| p.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        key: t["idempotency_key"].as_u64().map(|k| k as usize),
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
    // Only an explicit `false`: servers that say nothing about keys are the
    // norm today, and are not judged.
    let ignores_keys = ann["idempotencyKeyHint"] == false;
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
        "write" if d.keyed && ignores_keys && !idempotent && !read_only => Some(finding(
            "W0703",
            format!(
                "tool `{}` is a `write` with an `idempotency_key`, but its server says it does not honour keys",
                d.name
            ),
            "`idempotencyKeyHint: true`; or `effect write once` with `on_uncertain`: a retry with a key the service ignores applies the write again",
            format!(
                "{}, {}",
                hint(ann, "idempotencyKeyHint"),
                hint(ann, "idempotentHint")
            ),
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

/// What a probe found: the effects counted after two calls with one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    pub tool: String,
    pub key: String,
    pub effects: u64,
}

/// `{key}` and `{setup}` in every text of `args`.
fn fill(
    args: &serde_json::Map<String, Value>,
    key: &str,
    setup: &str,
) -> serde_json::Map<String, Value> {
    args.iter()
        .map(|(k, v)| {
            let v = match v {
                Value::String(s) => {
                    Value::String(s.replace("{key}", key).replace("{setup}", setup))
                }
                other => other.clone(),
            };
            (k.clone(), v)
        })
        .collect()
}

/// A count in a tool's answer: a number, a list (its length) or a boolean.
fn count_of(text: &str) -> Option<u64> {
    match serde_json::from_str::<Value>(text.trim()).ok()? {
        Value::Number(n) => n.as_u64(),
        Value::Array(a) => Some(a.len() as u64),
        Value::Bool(b) => Some(b as u64),
        _ => None,
    }
}

/// The conformance test of `docs/mcp/idempotency-key-hint.md` for every
/// keyed `write` with a `probe` in calyx.toml: two calls with the same key,
/// then a count of the effects. One effect: the tool honours the key. Two:
/// it does not (`E0704`), whatever its server says. It makes real writes:
/// run it only against a service's test environment.
pub fn probe(config: &Config, tools: &[Declared]) -> (Vec<Probed>, Vec<Finding>) {
    let mut servers: HashMap<Vec<String>, Result<mcp::Server, String>> = HashMap::new();
    let (mut probed, mut findings) = (Vec::new(), Vec::new());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    for d in tools.iter().filter(|d| d.effect == "write" && d.keyed) {
        let Some(cfg) = config.tools.get(&d.name) else {
            continue;
        };
        let Some(p) = &cfg.probe else { continue };
        let key = format!("calyx-probe-{stamp:x}-{}", d.name);
        let fail = |message: String, observed: String| Finding {
            code: "E0705",
            error: true,
            tool: d.name.clone(),
            message,
            expected: "the probe's tools answer: the write twice, then a count of its effects"
                .into(),
            observed,
        };
        // Calls `tool` (a name in `[tools]`) with `args`, on its own server.
        let mut call = |tool: &str, args: serde_json::Map<String, Value>, meta| {
            let cfg = config
                .tools
                .get(tool)
                .ok_or_else(|| format!("no `[tools.{tool}]` in calyx.toml"))?;
            let server = servers
                .entry(cfg.command.clone())
                .or_insert_with(|| mcp::Server::start(cfg).map_err(|e| e.message));
            let server = server.as_mut().map_err(|e| e.clone())?;
            let remote = cfg.remote_name.clone().unwrap_or_else(|| tool.to_owned());
            server
                .call(
                    &remote,
                    Value::Object(args),
                    meta,
                    std::time::Duration::from_secs(60),
                )
                .map(|a| a.text)
                .map_err(|e| format!("{}: {}", e.kind, e.message))
        };
        let setup = match &p.setup {
            None => Ok(String::new()),
            Some(s) => call(s, Default::default(), Default::default()).map(|t| {
                let t = t.trim();
                serde_json::from_str::<String>(t).unwrap_or_else(|_| t.to_owned())
            }),
        };
        let setup = match setup {
            Ok(s) => s,
            Err(e) => {
                findings.push(fail(
                    format!("the probe of `{}` could not set up", d.name),
                    e,
                ));
                continue;
            }
        };
        let mut args = fill(&p.args, &key, &setup);
        if let Some(param) = d.key.and_then(|k| d.params.get(k)) {
            args.insert(param.clone(), Value::String(key.clone()));
        }
        let mut meta = serde_json::Map::new();
        meta.insert("calyx/idempotency_key".into(), Value::String(key.clone()));
        let mut failed = None;
        for _ in 0..2 {
            if let Err(e) = call(&d.name, args.clone(), meta.clone()) {
                failed = Some(e);
                break;
            }
        }
        if let Some(e) = failed {
            findings.push(fail(format!("the probe could not call `{}`", d.name), e));
            continue;
        }
        let counted = call(
            &p.count,
            fill(&p.count_args, &key, &setup),
            Default::default(),
        );
        let counted = match counted {
            Ok(t) => t,
            Err(e) => {
                findings.push(fail(format!("the probe could not count `{}`", d.name), e));
                continue;
            }
        };
        match count_of(&counted) {
            Some(n) => {
                if n == 0 {
                    findings.push(fail(
                        format!("the probe saw no effect of `{}`", d.name),
                        format!("`{}` counted 0 after two calls", p.count),
                    ));
                } else if n > 1 {
                    findings.push(Finding {
                        code: "E0704",
                        error: true,
                        tool: d.name.clone(),
                        message: format!(
                            "tool `{}` applied a repeated idempotency key {n} times",
                            d.name
                        ),
                        expected: "one effect for two calls with the same key; or declare it `write once` with `on_uncertain`".into(),
                        observed: format!(
                            "`{}` counted {n} after two calls with key `{key}` ({})",
                            p.count,
                            hint_of(&servers, cfg, &d.name)
                        ),
                    });
                }
                probed.push(Probed {
                    tool: d.name.clone(),
                    key,
                    effects: n,
                });
            }
            None => findings.push(fail(
                format!("the probe could not read the count of `{}`", d.name),
                format!("`{}` answered {counted:?}", p.count),
            )),
        }
    }
    (probed, findings)
}

/// What the tool's server claims about keys, for the message.
fn hint_of(
    servers: &HashMap<Vec<String>, Result<mcp::Server, String>>,
    cfg: &crate::config::ToolServer,
    tool: &str,
) -> String {
    let remote = cfg.remote_name.clone().unwrap_or_else(|| tool.to_owned());
    match servers.get(&cfg.command) {
        Some(Ok(s)) => match s.annotations.get(&remote) {
            Some(ann) => hint(ann, "idempotencyKeyHint"),
            None => "no annotations".into(),
        },
        _ => "no server".into(),
    }
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
            params: vec![],
            key: None,
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
    fn a_key_the_server_says_it_ignores_is_a_warning() {
        let keyed = tool("write", true);
        assert_eq!(
            code(&keyed, json!({"idempotencyKeyHint": false})),
            Some("W0703")
        );
        assert_eq!(code(&keyed, json!({"idempotencyKeyHint": true})), None);
        // Saying nothing about keys is not saying they are ignored.
        assert_eq!(code(&keyed, json!({"idempotentHint": false})), None);
        // An idempotent write is safe to repeat, key or not.
        assert_eq!(
            code(
                &keyed,
                json!({"idempotencyKeyHint": false, "idempotentHint": true})
            ),
            None
        );
    }

    #[test]
    fn a_count_is_a_number_a_list_or_a_boolean() {
        assert_eq!(count_of("2"), Some(2));
        assert_eq!(count_of(" [\"re_1\"] "), Some(1));
        assert_eq!(count_of("true"), Some(1));
        assert_eq!(count_of("two"), None);
    }

    #[test]
    fn the_probe_fills_its_placeholders() {
        let args = json!({"order": "{setup}", "request": "{key}", "amount": 1.0});
        let filled = fill(args.as_object().unwrap(), "k1", "pi_1");
        assert_eq!(
            Value::Object(filled),
            json!({"order": "pi_1", "request": "k1", "amount": 1.0})
        );
    }

    #[test]
    fn a_server_that_says_nothing_is_not_judged() {
        assert!(mismatch(&tool("read", false), None).is_none());
        assert!(mismatch(&tool("write", false), Some(&json!(null))).is_none());
    }
}
