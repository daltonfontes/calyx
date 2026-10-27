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
//!
//! Over stdio the caller is whoever started the process. With `--http
//! ADDR` the program is served over Streamable HTTP (POST /mcp, answers as
//! JSON, a session id from `initialize`), to callers declared in
//! calyx.toml, each with a token (from the environment) and the graphs it
//! may call:
//!
//! ```toml
//! [serve.callers.suporte]
//! token_env = "CALYX_TOKEN_SUPORTE"   # Authorization: Bearer <token>
//! graphs = ["support"]                # or ["*"]
//! ```
//!
//! A request without a known token gets 401; `tools/list` shows a caller
//! only its graphs, and a call of another is refused. Idempotency keys are
//! per caller: one caller cannot get another's answer by sending its key.
//! A request with an `Origin` not in `[serve] allowed_origins` gets 403
//! (a web page cannot drive the server from someone's browser). There is no
//! TLS: bind to 127.0.0.1 (the default) and put a proxy with TLS in front
//! for other machines.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use calyx_runtime::config::Caller;
use serde_json::{Map, Value, json};

/// The largest request body accepted over HTTP.
const MAX_BODY: usize = 4 << 20;

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
    let mut http = None;
    let mut config_file = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--fake-models" => flags.push(arg.clone()),
            "--config" => match it.next() {
                Some(c) => {
                    // Children run next to the program: an absolute path.
                    let abs = std::fs::canonicalize(c).unwrap_or_else(|_| PathBuf::from(c));
                    config_file = Some(abs.clone());
                    flags.extend([arg.clone(), abs.display().to_string()]);
                }
                None => return usage("--config expects a file"),
            },
            "--http" => match it.next() {
                Some(a) => http = Some(a.clone()),
                None => return usage("--http expects an address, e.g. 127.0.0.1:8080"),
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
    let text = match crate::read_source(Path::new(&file)) {
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
    if let Some(addr) = http {
        let config = match config_file {
            Some(p) => calyx_runtime::Config::load(&p),
            None => calyx_runtime::Config::discover(Path::new(".")),
        };
        return match config {
            Ok(c) => serve_http(server, &addr, c.serve),
            Err(e) => {
                eprintln!("calyx: {e}");
                ExitCode::from(2)
            }
        };
    }
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
        let Some(out) = server.handle(&msg, None) else {
            continue; // a notification: no answer
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
    eprintln!(
        "calyx: {why}\nusage: calyx serve <file.clyx> [--http ADDR] [--fake-models] [--config FILE]"
    );
    ExitCode::from(2)
}

// ----- HTTP -------------------------------------------------------------------

/// A caller and its token, read from the environment at start.
struct Known {
    caller: Caller,
    token: String,
}

/// Comparison that takes the same time wherever the texts differ.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn serve_http(server: Server, addr: &str, cfg: calyx_runtime::config::ServeConfig) -> ExitCode {
    if cfg.callers.is_empty() {
        eprintln!(
            "calyx: serve --http needs [serve.callers.NAME] in calyx.toml: who may call, with token_env and graphs"
        );
        return ExitCode::from(2);
    }
    let mut known = Vec::new();
    for c in cfg.callers {
        match std::env::var(&c.token_env).ok().filter(|t| t.len() >= 16) {
            Some(token) => known.push(Known { caller: c, token }),
            None => {
                eprintln!(
                    "calyx: caller `{}` needs a token of 16 characters or more in the environment variable {}",
                    c.name, c.token_env
                );
                return ExitCode::from(2);
            }
        }
    }
    for k in &known {
        for g in &k.caller.graphs {
            if g != "*" && !server.graphs.iter().any(|x| &x.name == g) {
                eprintln!(
                    "calyx: caller `{}` names graph `{g}`, which the program does not have",
                    k.caller.name
                );
                return ExitCode::from(2);
            }
        }
    }
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("calyx: cannot listen on {addr}: {e}");
            return ExitCode::from(2);
        }
    };
    let bound = listener
        .local_addr()
        .map_or(addr.to_owned(), |a| a.to_string());
    eprintln!(
        "calyx serve: listening on http://{bound}/mcp ({} caller(s))",
        known.len()
    );
    let shared = Arc::new(Http {
        server,
        known,
        origins: cfg.allowed_origins,
        sessions: Mutex::new(Map::new()),
    });
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let h = Arc::clone(&shared);
        std::thread::spawn(move || h.connection(stream));
    }
    ExitCode::SUCCESS
}

struct Http {
    server: Server,
    known: Vec<Known>,
    origins: Vec<String>,
    /// Session id to the caller that opened it.
    sessions: Mutex<Map<String, Value>>,
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next()?.to_owned(), parts.next()?.to_owned());
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 || headers.len() > 100 {
            return None;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        headers.push((k.trim().to_owned(), v.trim().to_owned()));
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return None;
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

fn respond(mut stream: &TcpStream, status: u16, body: &str, extra: &[(&str, String)]) {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if status == 401 {
        head.push_str("WWW-Authenticate: Bearer\r\n");
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

impl Http {
    fn caller(&self, req: &Request) -> Option<&Caller> {
        let token = req.header("authorization")?.strip_prefix("Bearer ")?.trim();
        self.known
            .iter()
            .find(|k| same(&k.token, token))
            .map(|k| &k.caller)
    }

    fn connection(&self, stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let Some(req) = read_request(&stream) else {
            respond(&stream, 400, r#"{"error":"bad request"}"#, &[]);
            return;
        };
        if req.path.split('?').next() != Some("/mcp") {
            respond(&stream, 404, r#"{"error":"the MCP endpoint is /mcp"}"#, &[]);
            return;
        }
        if let Some(origin) = req.header("origin")
            && !self.origins.iter().any(|o| o == origin)
        {
            respond(&stream, 403, r#"{"error":"origin not allowed"}"#, &[]);
            return;
        }
        let Some(caller) = self.caller(&req) else {
            respond(
                &stream,
                401,
                r#"{"error":"a known Bearer token is needed"}"#,
                &[],
            );
            return;
        };
        let session = req.header("mcp-session-id").map(str::to_owned);
        match req.method.as_str() {
            "POST" => {}
            "DELETE" => {
                if let Some(s) = &session {
                    let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                    if sessions.get(s).and_then(Value::as_str) == Some(caller.name.as_str()) {
                        sessions.remove(s);
                    }
                }
                respond(&stream, 200, "{}", &[]);
                return;
            }
            // No stream from the server: answers come with each POST.
            _ => {
                respond(
                    &stream,
                    405,
                    r#"{"error":"POST JSON-RPC messages to /mcp"}"#,
                    &[("Allow", "POST, DELETE".into())],
                );
                return;
            }
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&req.body) else {
            respond(&stream, 400, r#"{"error":"the body is not JSON"}"#, &[]);
            return;
        };
        let mut extra = Vec::new();
        if msg["method"] == "initialize" {
            let id = new_session();
            self.sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.clone(), json!(caller.name));
            extra.push(("Mcp-Session-Id", id));
        } else {
            // A session belongs to the caller that opened it.
            let ok = session.as_ref().is_some_and(|s| {
                self.sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(s)
                    .and_then(Value::as_str)
                    == Some(caller.name.as_str())
            });
            if !ok {
                respond(
                    &stream,
                    404,
                    r#"{"error":"unknown session: initialize first"}"#,
                    &[],
                );
                return;
            }
        }
        if msg["method"] == "tools/call" {
            eprintln!(
                "calyx serve: {} calls `{}`",
                caller.name,
                msg["params"]["name"].as_str().unwrap_or("?")
            );
        }
        match self.server.handle(&msg, Some(caller)) {
            Some(out) => respond(&stream, 200, &out.to_string(), &extra),
            None => respond(&stream, 202, "", &extra),
        }
    }
}

fn new_session() -> String {
    use std::hash::{BuildHasher, Hasher};
    // Unpredictable enough for a session id (not a secret: the token is).
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    let a = h.finish();
    let mut h2 = std::collections::hash_map::RandomState::new().build_hasher();
    h2.write_u64(a ^ u64::from(std::process::id()));
    format!("{a:016x}{:016x}", h2.finish())
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
    /// One JSON-RPC message; its answer, or `None` for a notification.
    /// `caller`: who sent it, over HTTP; `None` over stdio (all graphs).
    fn handle(&self, msg: &Value, caller: Option<&Caller>) -> Option<Value> {
        let id = msg.get("id").cloned()?;
        let reply = match msg["method"].as_str().unwrap_or_default() {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "calyx", "version": env!("CARGO_PKG_VERSION")},
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": self.tools(caller)})),
            "tools/call" => Ok(self.call(&msg["params"], caller)),
            other => Err(json!({"code": -32601, "message": format!("unknown method {other}")})),
        };
        Some(match reply {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        })
    }

    fn tools(&self, caller: Option<&Caller>) -> Vec<Value> {
        self.graphs
            .iter()
            .filter(|g| caller.is_none_or(|c| c.may_call(&g.name)))
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

    fn call(&self, params: &Value, caller: Option<&Caller>) -> Value {
        let name = params["name"].as_str().unwrap_or_default();
        let Some(graph) = self.graphs.iter().find(|g| g.name == name) else {
            return text(format!("no graph `{name}`"), true);
        };
        if let Some(c) = caller
            && !c.may_call(name)
        {
            return text(
                format!("Forbidden: `{}` may not call `{name}`", c.name),
                true,
            );
        }
        let args = params["arguments"].as_object().cloned().unwrap_or_default();
        let key = params["idempotencyKey"]
            .as_str()
            .or_else(|| params["_meta"]["calyx/idempotency_key"].as_str())
            .map(str::to_owned);
        // Arguments in a fixed order, to compare a repeated key's call.
        let canonical =
            serde_json::to_string(&args.iter().collect::<BTreeMap<_, _>>()).unwrap_or_default();
        // Keys are per caller: another's key finds nothing of yours.
        let entry = key.as_ref().map(|k| match caller {
            Some(c) => format!("{}\u{0}{name}\u{0}{k}", c.name),
            None => format!("{name}\u{0}{k}"),
        });
        let found = entry.as_ref().and_then(|e| keys_get(e));
        if let Some(e) = &found {
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
            if let Some(run) = e["run"].as_str() {
                let mut cmd = vec!["resume".to_owned(), run.to_owned(), "--quiet".to_owned()];
                cmd.extend(self.flags.iter().cloned());
                return self.finish(&cmd, entry.as_deref(), Some(run.to_owned()));
            }
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
            // Two calls with the same key at once: the second waits for
            // nothing and finds the first's entry, as a retry would.
            if !keys_claim(e, json!({"args": canonical, "run": null, "output": null})) {
                return text(
                    "IdempotencyConflict: a call with this key is in progress; call again later"
                        .into(),
                    true,
                );
            }
        }
        self.finish(&cmd, entry.as_deref(), None)
    }

    /// Runs `calyx <cmd>`, records the run under `entry` as soon as its id
    /// is known, and its answer when it finishes.
    fn finish(&self, cmd: &[String], entry: Option<&str>, mut run: Option<String>) -> Value {
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
                        keys_set(e, "run", json!(id.trim()));
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
                    keys_set(e, "output", json!(stdout));
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

/// The key table is read and written under this lock: calls over HTTP
/// run at the same time.
static KEYS_LOCK: Mutex<()> = Mutex::new(());

fn keys_get(entry: &str) -> Option<Value> {
    let _g = KEYS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_keys().get(entry).cloned()
}

/// Adds `entry` unless it is there: whether it was added.
fn keys_claim(entry: &str, value: Value) -> bool {
    let _g = KEYS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut keys = load_keys();
    if keys.contains_key(entry) {
        return false;
    }
    keys.insert(entry.to_owned(), value);
    save_keys(&keys);
    true
}

fn keys_set(entry: &str, field: &str, value: Value) {
    let _g = KEYS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut keys = load_keys();
    if let Some(e) = keys.get_mut(entry) {
        e[field] = value;
        save_keys(&keys);
    }
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
