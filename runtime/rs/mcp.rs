//! Tools run as MCP servers (decision D34). This is a minimal MCP client:
//! JSON-RPC 2.0, `initialize`, `tools/list` and `tools/call`, over one of
//! two transports.
//!
//! - **stdio**: one process per distinct command, started on first use and
//!   kept for the whole run, newline-delimited JSON. Several calls to one
//!   server are in flight at once, as the protocol allows: each request has
//!   its own id, and a reader thread hands each answer to the call that
//!   waits for it (with a timeout). Only writing a request's line to the
//!   server's stdin takes turns.
//! - **Streamable HTTP** (a remote server, `url` in calyx.toml): each
//!   request is a POST that accepts the answer as JSON or as a stream of
//!   server-sent events; the session id the server gives at `initialize`
//!   (`Mcp-Session-Id`) and the protocol version go with every request
//!   after it. A session the server forgot (HTTP 404) is started again,
//!   once. Calls are concurrent: one HTTP request each. A token
//!   (`token_env`) goes as `Authorization: Bearer`, and other headers
//!   (`headers_env`) come from the environment too.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::ToolServer;
use crate::io::IoError;

const PROTOCOL_VERSION: &str = "2025-06-18";
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// Error kinds a tool may report by starting its error text with `Kind:`.
const TEMPORARY: [&str; 4] = ["Timeout", "Unavailable", "RateLimit", "Network"];

/// The calls waiting for an answer, by request id. `None` once the server
/// has exited: no answer will come.
type Waiting = Arc<Mutex<Option<HashMap<u64, Sender<Value>>>>>;

/// How requests reach the server.
enum Transport {
    Stdio {
        child: Mutex<Child>,
        stdin: Mutex<ChildStdin>,
        waiting: Waiting,
    },
    Http(Http),
}

struct Http {
    url: String,
    headers: Vec<(String, String)>,
    agent: ureq::Agent,
    /// `Mcp-Session-Id`, once the server gave one.
    session: Mutex<Option<String>>,
    /// The version agreed at `initialize`, sent as `MCP-Protocol-Version`.
    protocol: Mutex<Option<String>>,
}

pub struct Server {
    transport: Transport,
    next_id: AtomicU64,
    /// Tool names the server offers.
    pub tools: Vec<String>,
    /// What the server says about each tool (`annotations`), if anything.
    pub annotations: HashMap<String, Value>,
}

pub struct ToolAnswer {
    pub text: String,
    /// `structuredContent`, when the server sends it.
    pub json: Option<Value>,
    pub ms: u64,
}

impl Server {
    pub fn start(cfg: &ToolServer) -> Result<Server, IoError> {
        let transport = match &cfg.url {
            Some(url) => Transport::Http(Http::new(url, cfg)?),
            None => Self::spawn(cfg)?,
        };
        let mut server = Server {
            transport,
            next_id: AtomicU64::new(1),
            tools: Vec::new(),
            annotations: HashMap::new(),
        };
        server.handshake()?;
        let list = server.request("tools/list", json!({}), START_TIMEOUT)?;
        server.tools = list["tools"]
            .as_array()
            .map(|ts| {
                ts.iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        server.annotations = list["tools"]
            .as_array()
            .map(|ts| {
                ts.iter()
                    .filter_map(|t| {
                        Some((
                            t["name"].as_str()?.to_owned(),
                            t.get("annotations")?.clone(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(server)
    }

    /// `initialize`, then `notifications/initialized`.
    fn handshake(&self) -> Result<(), IoError> {
        if let Transport::Http(h) = &self.transport {
            *h.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
            *h.protocol.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        let init = self.request_once(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "calyx", "version": env!("CARGO_PKG_VERSION")},
            }),
            START_TIMEOUT,
        )?;
        if let (Transport::Http(h), Some(v)) = (&self.transport, init["protocolVersion"].as_str()) {
            *h.protocol.lock().unwrap_or_else(|e| e.into_inner()) = Some(v.to_owned());
        }
        self.notify("notifications/initialized")
    }

    fn spawn(cfg: &ToolServer) -> Result<Transport, IoError> {
        let (program, args) = cfg.command.split_first().expect("non-empty command");
        let mut child = Command::new(program)
            .args(args)
            .current_dir(&cfg.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                IoError::new(
                    "Config",
                    format!("cannot start MCP server `{}`: {e}", cfg.command.join(" ")),
                )
            })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let waiting: Waiting = Arc::new(Mutex::new(Some(HashMap::new())));
        let readers = Arc::clone(&waiting);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                // Not JSON-RPC: servers sometimes log to stdout. Ignore it.
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                // Notifications and requests from the server are not answers.
                let Some(id) = msg["id"].as_u64() else {
                    continue;
                };
                let mut w = readers.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(tx) = w.as_mut().and_then(|w| w.remove(&id)) {
                    let _ = tx.send(msg);
                }
            }
            // The server exited: every call still waiting fails now.
            *readers.lock().unwrap_or_else(|e| e.into_inner()) = None;
        });
        Ok(Transport::Stdio {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            waiting,
        })
    }

    /// Calls a tool. `meta` goes in the request's `_meta` (the idempotency
    /// key and the preconditions, decisions D2 and D29), when not empty.
    pub fn call(
        &self,
        tool: &str,
        args: Value,
        meta: serde_json::Map<String, Value>,
        timeout: Duration,
    ) -> Result<ToolAnswer, IoError> {
        if !self.tools.iter().any(|t| t == tool) {
            return Err(IoError::new(
                "Config",
                format!(
                    "the MCP server has no tool `{tool}` (it offers: {})",
                    self.tools.join(", ")
                ),
            ));
        }
        let started = Instant::now();
        let mut params = json!({"name": tool, "arguments": args});
        if !meta.is_empty() {
            params["_meta"] = Value::Object(meta);
        }
        let result = self.request("tools/call", params, timeout)?;
        let text = result["content"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|c| c["type"] == "text")
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if result["isError"] == true {
            // A precondition that does not hold (D29): the tool did nothing.
            if let Some(rest) = text.strip_prefix("PreconditionFailed:") {
                return Err(IoError::new("PreconditionFailed", rest.trim().to_owned()));
            }
            // A server in front of another service passes on its temporary
            // errors: a timeout there may still have done the call.
            for kind in TEMPORARY {
                if let Some(rest) = text.strip_prefix(kind).and_then(|r| r.strip_prefix(':')) {
                    return Err(IoError::new(kind, rest.trim().to_owned()));
                }
            }
            return Err(IoError::new("ToolError", text));
        }
        Ok(ToolAnswer {
            text,
            json: result.get("structuredContent").cloned(),
            ms: started.elapsed().as_millis() as u64,
        })
    }

    fn send(stdin: &Mutex<ChildStdin>, msg: &Value) -> Result<(), IoError> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut stdin = stdin.lock().unwrap_or_else(|e| e.into_inner());
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| IoError::new("Unavailable", format!("MCP server closed: {e}")))
    }

    fn notify(&self, method: &str) -> Result<(), IoError> {
        let msg = json!({"jsonrpc": "2.0", "method": method});
        match &self.transport {
            Transport::Stdio { stdin, .. } => Self::send(stdin, &msg),
            Transport::Http(h) => h.post(&msg, START_TIMEOUT, None).map(|_| ()),
        }
    }

    /// A request; over HTTP, a session the server forgot is started again
    /// and the request sent once more.
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, IoError> {
        match self.request_once(method, params.clone(), timeout) {
            Err(e) if e.kind == "SessionExpired" => {
                self.handshake()?;
                self.request_once(method, params, timeout)
            }
            other => other,
        }
    }

    fn request_once(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, IoError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let msg = match &self.transport {
            Transport::Http(h) => h.post(&req, timeout, Some(id))?.ok_or_else(|| {
                IoError::new(
                    "Unavailable",
                    format!("MCP `{method}`: the server sent no answer"),
                )
            })?,
            Transport::Stdio { stdin, waiting, .. } => {
                Self::stdio_request(stdin, waiting, id, method, &req, timeout)?
            }
        };
        if let Some(err) = msg.get("error") {
            return Err(IoError::new(
                "ToolError",
                err["message"].as_str().unwrap_or("MCP error").to_owned(),
            ));
        }
        Ok(msg["result"].clone())
    }

    fn stdio_request(
        stdin: &Mutex<ChildStdin>,
        waiting: &Waiting,
        id: u64,
        method: &str,
        req: &Value,
        timeout: Duration,
    ) -> Result<Value, IoError> {
        let (tx, rx) = channel();
        {
            let mut w = waiting.lock().unwrap_or_else(|e| e.into_inner());
            match w.as_mut() {
                Some(w) => w.insert(id, tx),
                None => return Err(IoError::new("Unavailable", "MCP server exited")),
            };
        }
        let forget = || {
            if let Some(w) = waiting.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                w.remove(&id);
            }
        };
        if let Err(e) = Self::send(stdin, req) {
            forget();
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(m) => Ok(m),
            Err(RecvTimeoutError::Timeout) => {
                forget();
                Err(IoError::new(
                    "Timeout",
                    format!("MCP `{method}` took more than {} ms", timeout.as_millis()),
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err(IoError::new("Unavailable", "MCP server exited"))
            }
        }
    }

    /// The server is in an unknown state (e.g. after a timeout): stop it.
    /// A remote server's session is ended.
    pub fn kill(&self) {
        match &self.transport {
            Transport::Stdio { child, .. } => {
                let mut child = child.lock().unwrap_or_else(|e| e.into_inner());
                let _ = child.kill();
                let _ = child.wait();
            }
            Transport::Http(h) => h.end(),
        }
    }
}

impl Http {
    fn new(url: &str, cfg: &ToolServer) -> Result<Http, IoError> {
        let var = |name: &str, what: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    IoError::new(
                        "Config",
                        format!(
                            "the MCP server at {url} needs the environment variable {name} ({what})"
                        ),
                    )
                })
        };
        let mut headers = Vec::new();
        if let Some(t) = &cfg.token_env {
            headers.push((
                "Authorization".to_owned(),
                format!("Bearer {}", var(t, "its token")?),
            ));
        }
        for (h, v) in &cfg.headers_env {
            headers.push((h.clone(), var(v, &format!("header {h}"))?));
        }
        Ok(Http {
            url: url.to_owned(),
            headers,
            agent: crate::llm::agent(),
            session: Mutex::new(None),
            protocol: Mutex::new(None),
        })
    }

    /// Sends one JSON-RPC message. For a request (`id`), its answer, from a
    /// JSON body or from the event stream; for a notification, nothing.
    fn post(
        &self,
        msg: &Value,
        timeout: Duration,
        id: Option<u64>,
    ) -> Result<Option<Value>, IoError> {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let protocol = self
            .protocol
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut req = self
            .agent
            .post(&self.url)
            .config()
            .timeout_global(Some(timeout))
            .build()
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        if let Some(s) = &session {
            req = req.header("Mcp-Session-Id", s);
        }
        if let Some(p) = &protocol {
            req = req.header("MCP-Protocol-Version", p);
        }
        let mut resp = req.send(msg.to_string()).map_err(transport_error)?;
        let status = resp.status().as_u16();
        if let Some(s) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.to_owned());
        }
        if !(200..300).contains(&status) {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            let body: String = body.chars().take(300).collect();
            let at = &self.url;
            return Err(match status {
                404 if session.is_some() => {
                    IoError::new("SessionExpired", "the MCP server forgot the session")
                }
                401 | 403 => IoError::new(
                    "Auth",
                    format!(
                        "the MCP server at {at} refused the credentials (HTTP {status}): {body}"
                    ),
                ),
                408 => IoError::new("Timeout", format!("HTTP 408 from {at}")),
                429 => {
                    let mut e = IoError::new("RateLimit", format!("HTTP 429 from {at}: {body}"));
                    e.retry_after_ms = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(|s| s * 1000);
                    e
                }
                500..=599 => {
                    IoError::new("Unavailable", format!("HTTP {status} from {at}: {body}"))
                }
                _ => IoError::new("ToolError", format!("HTTP {status} from {at}: {body}")),
            });
        }
        let Some(id) = id else { return Ok(None) };
        let sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("text/event-stream"));
        let reader = BufReader::new(resp.body_mut().as_reader());
        if sse {
            return answer_in_events(reader, id).map(Some);
        }
        let v: Value = serde_json::from_reader(reader).map_err(|e| {
            IoError::new(
                "Unavailable",
                format!("the MCP server's answer is not JSON: {e}"),
            )
        })?;
        let found = match v {
            Value::Array(items) => items.into_iter().find(|m| m["id"].as_u64() == Some(id)),
            m if m["id"].as_u64() == Some(id) => Some(m),
            _ => None,
        };
        found
            .map(Some)
            .ok_or_else(|| IoError::new("Unavailable", "the MCP server answered another request"))
    }

    /// Ends the session, if there is one (best effort).
    fn end(&self) {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(s) = session {
            let mut req = self
                .agent
                .delete(&self.url)
                .config()
                .timeout_global(Some(Duration::from_secs(2)))
                .build()
                .header("Mcp-Session-Id", &s);
            for (k, v) in &self.headers {
                req = req.header(k, v);
            }
            let _ = req.call();
        }
    }
}

/// The answer to request `id` in a stream of server-sent events: each
/// event's `data:` lines are one JSON-RPC message. Messages that are not
/// that answer (notifications, the server's own requests) are skipped.
fn answer_in_events(reader: impl BufRead, id: u64) -> Result<Value, IoError> {
    let mut data = String::new();
    for line in reader.lines() {
        let line = line.map_err(|e| IoError::new("Network", format!("MCP event stream: {e}")))?;
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        } else if line.is_empty() && !data.is_empty() {
            if let Ok(m) = serde_json::from_str::<Value>(&data)
                && m["id"].as_u64() == Some(id)
                && (m.get("result").is_some() || m.get("error").is_some())
            {
                return Ok(m);
            }
            data.clear();
        }
    }
    if let Ok(m) = serde_json::from_str::<Value>(&data)
        && m["id"].as_u64() == Some(id)
    {
        return Ok(m);
    }
    Err(IoError::new(
        "Unavailable",
        "the MCP server's event stream ended without the answer",
    ))
}

fn transport_error(e: ureq::Error) -> IoError {
    match e {
        ureq::Error::Timeout(_) => IoError::new("Timeout", e.to_string()),
        other => IoError::new("Network", other.to_string()),
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
    }
}
