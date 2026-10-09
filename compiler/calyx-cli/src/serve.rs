//! `calyx serve`: a program as an MCP server (stdio). Each graph is a tool.
//!
//! An agent (Claude Desktop, an IDE, another Calyx program) calls a graph
//! as it would any MCP tool, and gets what `calyx run` gives: every call
//! journaled, independent calls in parallel, effects under their contracts.
//!
//! Annotations come from the program, not from a person: a graph none of
//! whose steps writes is `readOnlyHint: true`; any other graph is
//! `idempotencyKeyHint: true` (docs/mcp/idempotency-key-hint.md), and that is
//! kept by construction: a call with an idempotency key (`idempotencyKey` in
//! the request, or `calyx/idempotency_key` in its `_meta`) is tied to one run.
//! The same key again returns that run's answer if it finished, or resumes
//! it if it did not (a crash, a failure), so its effects are not repeated;
//! the same key with other arguments is refused.
//!
//! Each call runs `calyx run` (or `calyx resume`) as a child process; the
//! key-to-run table is `.calyx/serve/keys.json`, written before the run
//! goes past its first step.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use serde_json::{Map, Value, json};

const KEYS: &str = ".calyx/serve/keys.json";

struct Graph {
    name: String,
    params: Vec<(String, String)>,
    ret: String,
    read_only: bool,
}

struct Server {
    exe: PathBuf,
    file: String,
    /// Passed on to every `calyx run` and `calyx resume`.
    flags: Vec<String>,
    graphs: Vec<Graph>,
}

pub fn serve(args: &[String]) -> ExitCode {
    let mut file = None;
    let mut flags = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--fake-models" => flags.push(arg.clone()),
            "--config" => match it.next() {
                Some(c) => flags.extend([arg.clone(), c.clone()]),
                None => return usage("--config expects a file"),
            },
            a if a.starts_with('-') => return usage(&format!("unknown option `{a}`")),
            a if file.is_none() => file = Some(a.to_owned()),
            _ => return usage("serve takes a single file"),
        }
    }
    let Some(file) = file else {
        return usage("serve needs a file");
    };
    // MCP clients start servers from anywhere: work next to the program,
    // where its calyx.toml and its runs are.
    let path = std::fs::canonicalize(&file).unwrap_or_else(|_| PathBuf::from(&file));
    if let Some(dir) = path.parent()
        && std::env::set_current_dir(dir).is_err()
    {
        eprintln!("calyx: cannot work in `{}`", dir.display());
        return ExitCode::from(2);
    }
    let file = path
        .file_name()
        .map_or(file, |n| n.to_string_lossy().into_owned());
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("calyx: cannot read `{file}`: {e}");
            return ExitCode::from(2);
        }
    };
    let report = calyx_check::check(&file, &text);
    if report.has_errors() {
        // stdout is the protocol: diagnostics go to stderr.
        eprint!("{}", report.render());
        return ExitCode::from(1);
    }
    let ir: Value = serde_json::from_str(&report.ir.to_json()).unwrap_or_default();
    let server = Server {
        exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("calyx")),
        file,
        flags,
        graphs: graphs(&ir),
    };
    eprintln!(
        "calyx serve: {} graph(s) as MCP tools: {}",
        server.graphs.len(),
        server
            .graphs
            .iter()
            .map(|g| g.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            continue; // a notification: no answer
        };
        let reply = match msg["method"].as_str().unwrap_or_default() {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "calyx", "version": env!("CARGO_PKG_VERSION")},
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": server.tools()})),
            "tools/call" => Ok(server.call(&msg["params"])),
            other => Err(json!({"code": -32601, "message": format!("unknown method {other}")})),
        };
        let out = match reply {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        };
        if writeln!(stdout, "{out}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            break;
        }
    }
    ExitCode::SUCCESS
}

fn usage(why: &str) -> ExitCode {
    eprintln!("calyx: {why}\nusage: calyx serve <file.clyx> [--fake-models] [--config FILE]");
    ExitCode::from(2)
}

/// The program's graphs, with whether any step writes.
fn graphs(ir: &Value) -> Vec<Graph> {
    let empty = Vec::new();
    ir["graphs"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|g| {
            let names = g["params"].as_array()?;
            let types = g["param_types"].as_array()?;
            Some(Graph {
                name: g["name"].as_str()?.to_owned(),
                params: names
                    .iter()
                    .zip(types)
                    .filter_map(|(n, t)| Some((n.as_str()?.to_owned(), t.as_str()?.to_owned())))
                    .collect(),
                ret: g["ret"].as_str().unwrap_or("Unit").to_owned(),
                read_only: g["nodes"].as_array().is_some_and(|ns| {
                    ns.iter()
                        .all(|n| matches!(n["effect"].as_str(), Some("pure" | "llm" | "read")))
                }),
            })
        })
        .collect()
}

/// A JSON Schema for a Calyx type, as far as the name tells.
fn schema(ty: &str) -> Value {
    match ty {
        "Text" => json!({"type": "string"}),
        "Int" => json!({"type": "integer"}),
        "Float" => json!({"type": "number"}),
        "Bool" => json!({"type": "boolean"}),
        t if t.starts_with("List[") => json!({"type": "array"}),
        _ => json!({}),
    }
}

impl Server {
    fn tools(&self) -> Vec<Value> {
        self.graphs
            .iter()
            .map(|g| {
                let props: Map<String, Value> =
                    g.params.iter().map(|(n, t)| (n.clone(), schema(t))).collect();
                let annotations = if g.read_only {
                    json!({"readOnlyHint": true})
                } else {
                    json!({"readOnlyHint": false, "idempotentHint": false, "idempotencyKeyHint": true})
                };
                json!({
                    "name": g.name,
                    "description": format!(
                        "The Calyx graph `{}` (returns {}). Every call is journaled; {}",
                        g.name,
                        g.ret,
                        if g.read_only {
                            "it changes nothing outside the run."
                        } else {
                            "send an idempotency key to make a retry return the first call's answer, or resume it, instead of running it again."
                        }
                    ),
                    "inputSchema": {
                        "type": "object",
                        "properties": props,
                        "required": g.params.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
                    },
                    "annotations": annotations,
                })
            })
            .collect()
    }

    fn call(&self, params: &Value) -> Value {
        let name = params["name"].as_str().unwrap_or_default();
        let Some(graph) = self.graphs.iter().find(|g| g.name == name) else {
            return text(format!("no graph `{name}`"), true);
        };
        let args = params["arguments"].as_object().cloned().unwrap_or_default();
        let key = params["idempotencyKey"]
            .as_str()
            .or_else(|| params["_meta"]["calyx/idempotency_key"].as_str())
            .map(str::to_owned);
        // Arguments in a fixed order, to compare a repeated key's call.
        let canonical =
            serde_json::to_string(&args.iter().collect::<BTreeMap<_, _>>()).unwrap_or_default();
        let mut keys = load_keys();
        let entry = key.as_ref().map(|k| format!("{name}\u{0}{k}"));
        if let Some(e) = entry.as_ref().and_then(|e| keys.get(e)) {
            if e["args"].as_str() != Some(canonical.as_str()) {
                return text(
                    "IdempotencyConflict: this key was used with other arguments".into(),
                    true,
                );
            }
            if let Some(out) = e["output"].as_str() {
                let mut r = text(out.to_owned(), false);
                r["_meta"] = json!({"calyx/replayed": true, "calyx/run": e["run"]});
                return r;
            }
            let run = e["run"].as_str().unwrap_or_default().to_owned();
            let mut cmd = vec!["resume".to_owned(), run.clone(), "--quiet".to_owned()];
            cmd.extend(self.flags.iter().cloned());
            return self.finish(&cmd, entry.as_deref(), &mut keys, Some(run));
        }
        let mut cmd = vec![
            "run".to_owned(),
            self.file.clone(),
            "--graph".to_owned(),
            name.to_owned(),
            "--quiet".to_owned(),
        ];
        cmd.extend(self.flags.iter().cloned());
        for (p, _) in &graph.params {
            let Some(v) = args.get(p) else {
                return text(format!("missing argument `{p}`"), true);
            };
            cmd.push(format!("--{p}"));
            cmd.push(match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        }
        if let Some(e) = &entry {
            keys.insert(
                e.clone(),
                json!({"args": canonical, "run": null, "output": null}),
            );
        }
        self.finish(&cmd, entry.as_deref(), &mut keys, None)
    }

    /// Runs `calyx <cmd>`, records the run under `entry` as soon as its id
    /// is known, and its answer when it finishes.
    fn finish(
        &self,
        cmd: &[String],
        entry: Option<&str>,
        keys: &mut Map<String, Value>,
        mut run: Option<String>,
    ) -> Value {
        let child = Command::new(&self.exe)
            .args(cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => return text(format!("Unavailable: cannot start calyx: {e}"), true),
        };
        let mut err = String::new();
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if run.is_none()
                    && let Some(id) = line.strip_prefix("calyx: run ")
                {
                    run = Some(id.trim().to_owned());
                    // Before the run goes on: a crash of this server from
                    // here on leaves a run to resume, not one to repeat.
                    if let Some(e) = entry {
                        keys[e]["run"] = json!(id.trim());
                        save_keys(keys);
                    }
                }
                err.push_str(&line);
                err.push('\n');
            }
        }
        let out = child.wait_with_output();
        let (code, stdout) = match out {
            Ok(o) => (
                o.status.code(),
                String::from_utf8_lossy(&o.stdout).trim().to_owned(),
            ),
            Err(e) => (None, e.to_string()),
        };
        let meta = json!({"calyx/run": run});
        let mut r = match code {
            Some(0) => {
                if let Some(e) = entry {
                    keys[e]["output"] = json!(stdout);
                    save_keys(keys);
                }
                text(stdout, false)
            }
            // Waiting for a message (`receive`): not a failure.
            Some(4) => text(err.trim().to_owned(), false),
            _ => text(
                format!(
                    "the run failed; call again with the same idempotency key to resume it\n{}",
                    err.trim()
                ),
                true,
            ),
        };
        r["_meta"] = meta;
        r
    }
}

fn text(t: String, error: bool) -> Value {
    let mut r = json!({"content": [{"type": "text", "text": t}]});
    if error {
        r["isError"] = json!(true);
    }
    r
}

fn load_keys() -> Map<String, Value> {
    std::fs::read_to_string(KEYS)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Written whole and renamed, and synced: the table is what keeps a retry
/// from becoming a second run.
fn save_keys(keys: &Map<String, Value>) {
    let path = Path::new(KEYS);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(Value::Object(keys.clone()).to_string().as_bytes())?;
        f.sync_all()
    });
    if written.is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}
