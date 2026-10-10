//! The C ABI the interpreter calls for effects. Declared for C in
//! `runtime/include/calyx_io.h`.
//!
//! Requests and answers are JSON texts:
//!
//! ```text
//! model: {"model": id, "prompt": text, "schema": {...} | null,
//!         "max_output": n | null, "timeout_ms": n}
//!     -> {"ok": {"text": t, "input_tokens": n, "output_tokens": n, "ms": n}}
//! tool:  {"tool": name, "args": {...}, "max_output": n | null, "timeout_ms": n,
//!         "idempotency_key": k?, "requires": [...]?}
//!     -> {"ok": {"text": t, "json": v | null, "truncated": b, "ms": n}}
//! both:  -> {"error": {"kind": k, "message": m, "retry_after_ms": n | null}}
//! ```
//!
//! Error kinds: `Timeout`, `RateLimit`, `Unavailable`, `Network` (temporary)
//! and `Auth`, `BadRequest`, `Decode`, `ToolError`, `PreconditionFailed`,
//! `Config`.
//!
//! The idempotency key and the preconditions reach the MCP server in the
//! call's `_meta`, as `calyx/idempotency_key` and `calyx/requires`. A tool
//! whose preconditions do not hold answers with an error whose text starts
//! with `PreconditionFailed:`, having done nothing.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::annotations;
use crate::config::Config;
use crate::{llm, mcp, sandbox};

#[derive(Debug)]
pub struct IoError {
    pub kind: &'static str,
    pub message: String,
    /// How long the provider asked to wait before trying again.
    pub retry_after_ms: Option<u64>,
}

impl IoError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> IoError {
        IoError {
            kind,
            message: message.into(),
            retry_after_ms: None,
        }
    }

    fn to_json(&self) -> Value {
        json!({"error": {
            "kind": self.kind,
            "message": self.message,
            "retry_after_ms": self.retry_after_ms,
        }})
    }
}

struct State {
    config: Config,
    /// Every model answers with fake, schema-shaped values.
    fake_models: bool,
    agent: ureq::Agent,
    /// Running MCP servers, by command. A call holds its server, not the
    /// state, and several calls to one server are in flight at once.
    servers: HashMap<Vec<String>, Arc<mcp::Server>>,
    /// The program's tools, to compare with what their servers say.
    declared: HashMap<String, annotations::Declared>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Sets the configuration for the next runs and stops running servers.
pub fn configure(config: Config, fake_models: bool) {
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    *st = Some(State {
        config,
        fake_models,
        agent: llm::agent(),
        servers: HashMap::new(),
        declared: HashMap::new(),
    });
}

/// The program's tools: on its first call, each one is compared with what
/// its server says about it, and a contradiction is a warning.
pub fn declare(tools: Vec<annotations::Declared>) {
    with_state(|st| {
        st.declared = tools.into_iter().map(|d| (d.name.clone(), d)).collect();
    });
}

/// Stops the MCP servers.
pub fn shutdown() {
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = st.as_mut() {
        s.servers.clear();
    }
}

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    // A C program that never configured gets `calyx.toml` from its directory.
    let st = guard.get_or_insert_with(|| State {
        config: Config::discover(Path::new(".")).unwrap_or_else(|_| Config::builtin()),
        fake_models: false,
        agent: llm::agent(),
        servers: HashMap::new(),
        declared: HashMap::new(),
    });
    f(st)
}

fn model_call(req: &Value) -> Result<Value, IoError> {
    let r = llm::ModelRequest {
        model: req["model"].as_str().unwrap_or_default().to_owned(),
        prompt: req["prompt"].as_str().unwrap_or_default().to_owned(),
        messages: req.get("messages").filter(|m| !m.is_null()).cloned(),
        tools: req.get("tools").filter(|t| !t.is_null()).cloned(),
        schema: req.get("schema").filter(|s| !s.is_null()).cloned(),
        max_output: req["max_output"].as_u64(),
        timeout_ms: req["timeout_ms"].as_u64().unwrap_or(300_000),
    };
    // The lock is not held during the network call.
    let target = with_state(|st| {
        if st.fake_models || r.model.starts_with("fake") {
            return Ok(None);
        }
        let provider = st.config.provider_for(&r.model).cloned().ok_or_else(|| {
            IoError::new(
                "Config",
                format!(
                    "no provider for model `{}`: add one to calyx.toml (built-in: gemini-*, gpt-*, nvidia/*), or run with --fake-models",
                    r.model
                ),
            )
        })?;
        Ok(Some((provider, st.agent.clone())))
    })?;
    let answer = match target {
        None => match llm::fake_failure(&r.model) {
            Some(e) => return Err(e),
            None => llm::fake(&r),
        },
        Some((provider, agent)) => llm::call(&agent, &provider, &r)?,
    };
    // Cost for `budget`: null when calyx.toml has no price for the model.
    let cost = with_state(|st| st.config.prices.get(&r.model).copied()).map(|(i, o)| {
        (answer.input_tokens as f64 * i + answer.output_tokens as f64 * o) / 1_000_000.0
    });
    Ok(json!({"ok": {
        "text": answer.text,
        "input_tokens": answer.input_tokens,
        "output_tokens": answer.output_tokens,
        "cost_usd": cost,
        "message": answer.message,
        "tool_calls": answer.tool_calls,
        "ms": answer.ms,
    }}))
}

fn tool_call(req: &Value) -> Result<Value, IoError> {
    // Sandboxes lent to the call (D26): their locks and snapshots wrap it.
    let borrows: Vec<sandbox::Borrow> = req["borrows"]
        .as_array()
        .map(|bs| {
            bs.iter()
                .filter_map(|b| {
                    Some(sandbox::Borrow {
                        path: b["path"].as_str()?.to_owned(),
                        edits: b["mode"] == "edits",
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if borrows.is_empty() {
        return tool_call_unlocked(req);
    }
    let wrap = |m: String| IoError::new("Sandbox", m);
    let (mut ok, snaps) = sandbox::with_borrows(&borrows, wrap, || tool_call_unlocked(req))?;
    if !snaps.is_empty() {
        ok["ok"]["sandbox"] = Value::Array(snaps);
    }
    Ok(ok)
}

fn tool_call_unlocked(req: &Value) -> Result<Value, IoError> {
    let tool = req["tool"].as_str().unwrap_or_default().to_owned();
    let args = req.get("args").cloned().unwrap_or(json!({}));
    let timeout = Duration::from_millis(req["timeout_ms"].as_u64().unwrap_or(30_000));
    let max_output = req["max_output"].as_u64();
    let mut meta = serde_json::Map::new();
    if let Some(k) = req.get("idempotency_key") {
        meta.insert("calyx/idempotency_key".into(), k.clone());
    }
    if let Some(r) = req.get("requires") {
        meta.insert("calyx/requires".into(), r.clone());
    }
    let (key, server, remote, declared) = with_state(|st| {
        let server_cfg = st.config.tools.get(&tool).cloned().ok_or_else(|| {
            let place = st
                .config
                .path
                .as_ref()
                .map_or("a calyx.toml".to_owned(), |p| p.display().to_string());
            IoError::new(
                "Config",
                format!(
                    "no MCP server for tool `{tool}`: add `[tools.{tool}]` with `command = [...]` to {place}"
                ),
            )
        })?;
        let key = server_cfg.command.clone();
        if !st.servers.contains_key(&key) {
            let server = mcp::Server::start(&server_cfg)?;
            st.servers.insert(key.clone(), Arc::new(server));
        }
        let server = Arc::clone(&st.servers[&key]);
        let remote = server_cfg
            .remote_name
            .clone()
            .unwrap_or_else(|| tool.clone());
        // Compared once per run: taken out after the first call.
        let declared = st.declared.remove(&tool);
        Ok::<_, IoError>((key, server, remote, declared))
    })?;
    if let Some(f) =
        declared.and_then(|d| annotations::mismatch(&d, server.annotations.get(&remote)))
    {
        eprintln!("{}", f.line());
    }
    let result = server.call(&remote, args, meta, timeout);
    if let Err(e) = &result
        && matches!(e.kind, "Timeout" | "Unavailable")
    {
        // Unknown state: start a fresh server on the next call.
        with_state(|st| {
            if st
                .servers
                .get(&key)
                .is_some_and(|s| Arc::ptr_eq(s, &server))
            {
                st.servers.remove(&key);
            }
        });
    }
    let answer = result?;
    let (text, truncated) = truncate(&answer.text, max_output);
    Ok(json!({"ok": {
        "text": text,
        "json": answer.json,
        "truncated": truncated,
        "ms": answer.ms,
    }}))
}

/// Cuts a tool's output at its declared `max_output` (decision D16),
/// estimating 4 bytes per token, on a character boundary.
fn truncate(text: &str, max_tokens: Option<u64>) -> (String, bool) {
    let Some(max) = max_tokens else {
        return (text.to_owned(), false);
    };
    let limit = usize::try_from(max.saturating_mul(4)).unwrap_or(usize::MAX);
    if text.len() <= limit {
        return (text.to_owned(), false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// Shared entry point of the C ABI functions.
unsafe fn handle(req: *const c_char, f: fn(&Value) -> Result<Value, IoError>) -> *mut c_char {
    let out = if req.is_null() {
        IoError::new("BadRequest", "null request").to_json()
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        let text = unsafe { CStr::from_ptr(req) }.to_string_lossy();
        match serde_json::from_str::<Value>(&text) {
            Ok(v) => f(&v).unwrap_or_else(|e| e.to_json()),
            Err(e) => IoError::new("BadRequest", format!("invalid request: {e}")).to_json(),
        }
    };
    CString::new(out.to_string()).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Calls a model. Returns JSON to release with `calyx_string_free`.
///
/// # Safety
/// `req` must be NULL or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_io_model_call(req: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller.
    unsafe { handle(req, model_call) }
}

/// Calls a tool on its MCP server. Returns JSON to release with
/// `calyx_string_free`.
///
/// # Safety
/// `req` must be NULL or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_io_tool_call(req: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller.
    unsafe { handle(req, tool_call) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate("abc", None), ("abc".into(), false));
        assert_eq!(truncate("abcdefgh", Some(1)), ("abcd".into(), true));
        // "é" is two bytes; the cut moves back to the boundary.
        assert_eq!(truncate("abcé", Some(1)), ("abc".into(), true));
    }
}
