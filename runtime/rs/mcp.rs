//! Tools run as MCP servers (decision D34). This is a minimal MCP client
//! over stdio: newline-delimited JSON-RPC 2.0, `initialize`, `tools/list`
//! and `tools/call`.
//!
//! One process per distinct command, started on first use and kept for the
//! whole run. A reader thread turns the server's output into a channel, so
//! every call can have a timeout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::ToolServer;
use crate::io::IoError;

const PROTOCOL_VERSION: &str = "2025-06-18";
const START_TIMEOUT: Duration = Duration::from_secs(20);

pub struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    /// Tool names the server offers.
    pub tools: Vec<String>,
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
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Server {
            child,
            stdin,
            lines,
            next_id: 1,
            tools: Vec::new(),
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
        Ok(server)
    }

    /// Calls a tool. `meta` goes in the request's `_meta` (the idempotency
    /// key and the preconditions, decisions D2 and D29), when not empty.
    pub fn call(
        &mut self,
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
            return Err(IoError::new("ToolError", text));
        }
        Ok(ToolAnswer {
            text,
            json: result.get("structuredContent").cloned(),
            ms: started.elapsed().as_millis() as u64,
        })
    }

    fn send(&mut self, msg: &Value) -> Result<(), IoError> {
        let mut line = msg.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| IoError::new("Unavailable", format!("MCP server closed: {e}")))
    }

    fn notify(&mut self, method: &str) -> Result<(), IoError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method}))
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, IoError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(IoError::new(
                        "Timeout",
                        format!("MCP `{method}` took more than {} ms", timeout.as_millis()),
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(IoError::new("Unavailable", "MCP server exited"));
                }
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                // Not JSON-RPC: servers sometimes log to stdout. Ignore it.
                continue;
            };
            // Notifications and requests from the server are not answers.
            if msg["id"] != json!(id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                return Err(IoError::new(
                    "ToolError",
                    err["message"].as_str().unwrap_or("MCP error").to_owned(),
                ));
            }
            return Ok(msg["result"].clone());
        }
    }

    /// The server is in an unknown state (e.g. after a timeout): stop it.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
    }
}
