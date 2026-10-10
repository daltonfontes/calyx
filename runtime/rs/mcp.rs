//! Tools run as MCP servers (decision D34). This is a minimal MCP client
//! over stdio: newline-delimited JSON-RPC 2.0, `initialize`, `tools/list`
//! and `tools/call`.
//!
//! One process per distinct command, started on first use and kept for the
//! whole run. Several calls to one server are in flight at once, as the
//! protocol allows: each request has its own id, and a reader thread hands
//! each answer to the call that waits for it (with a timeout). Only writing
//! a request's line to the server's stdin takes turns.

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

pub struct Server {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    waiting: Waiting,
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
        let mut server = Server {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            waiting,
            next_id: AtomicU64::new(1),
            tools: Vec::new(),
            annotations: HashMap::new(),
        };
        server.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "calyx", "version": env!("CARGO_PKG_VERSION")},
            }),
            START_TIMEOUT,
        )?;
        server.notify("notifications/initialized")?;
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

    fn send(&self, msg: &Value) -> Result<(), IoError> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| IoError::new("Unavailable", format!("MCP server closed: {e}")))
    }

    fn notify(&self, method: &str) -> Result<(), IoError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method}))
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, IoError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        {
            let mut w = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
            match w.as_mut() {
                Some(w) => w.insert(id, tx),
                None => return Err(IoError::new("Unavailable", "MCP server exited")),
            };
        }
        let forget = || {
            if let Some(w) = self
                .waiting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_mut()
            {
                w.remove(&id);
            }
        };
        if let Err(e) =
            self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        {
            forget();
            return Err(e);
        }
        let msg = match rx.recv_timeout(timeout) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => {
                forget();
                return Err(IoError::new(
                    "Timeout",
                    format!("MCP `{method}` took more than {} ms", timeout.as_millis()),
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(IoError::new("Unavailable", "MCP server exited"));
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

    /// The server is in an unknown state (e.g. after a timeout): stop it.
    pub fn kill(&self) {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
    }
}
