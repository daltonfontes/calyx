//! `calyx serve`: a program as an MCP server, against the fake store.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("calyx-serve-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        std::fs::copy(examples.join("refund.clyx"), dir.join("refund.clyx")).unwrap();
        let store = examples.join("tools/fake_store.py").canonicalize().unwrap();
        let toml: String = ["get_order", "refund", "email", "email_sent"]
            .iter()
            .map(|t| {
                format!(
                    "[tools.{t}]\ncommand = [\"python3\", \"{}\"]\n",
                    store.display()
                )
            })
            .collect();
        std::fs::write(dir.join("calyx.toml"), toml).unwrap();
        Dir(dir)
    }

    fn store(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.0.join("store.json")).unwrap()).unwrap()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Server {
    fn start(d: &Dir, env: &[(&str, &str)]) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(["serve", "refund.clyx", "--fake-models"])
            .current_dir(&d.0)
            .env("CALYX_FAKE_STORE", d.0.join("store.json"))
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Server {
            child,
            stdin,
            stdout,
            next: 0,
        }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()["result"].clone()
    }

    fn refund(&mut self, request: &str, key: Option<&str>) -> Value {
        let mut params = json!({
            "name": "handle_refund",
            "arguments": {"request": request, "order": "A100", "message": "chegou quebrado"},
        });
        if let Some(k) = key {
            params["idempotencyKey"] = json!(k);
        }
        self.rpc("tools/call", params)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn answer(r: &Value) -> &str {
    r["content"][0]["text"].as_str().unwrap_or_default()
}

#[test]
fn each_graph_is_a_tool_annotated_from_the_program() {
    let d = Dir::new("list");
    let mut s = Server::start(&d, &[]);
    let init = s.rpc("initialize", json!({"protocolVersion": "2025-06-18"}));
    assert_eq!(init["serverInfo"]["name"], "calyx");
    let tools = s.rpc("tools/list", json!({}));
    let t = &tools["tools"][0];
    assert_eq!(t["name"], "handle_refund");
    assert_eq!(t["annotations"]["readOnlyHint"], false);
    assert_eq!(t["annotations"]["idempotencyKeyHint"], true);
    assert_eq!(t["inputSchema"]["properties"]["order"]["type"], "string");
}

#[test]
fn a_repeated_key_returns_the_first_answer_without_running_again() {
    let d = Dir::new("replay");
    let mut s = Server::start(&d, &[]);
    let first = s.refund("R1", Some("k1"));
    assert!(first.get("isError").is_none(), "{first}");
    let again = s.refund("R1", Some("k1"));
    assert_eq!(answer(&again), answer(&first));
    assert_eq!(again["_meta"]["calyx/replayed"], true);
    assert_eq!(
        d.store()["outbox"].as_array().unwrap().len(),
        1,
        "one e-mail"
    );

    // The same key with other arguments is refused, and runs nothing.
    let other = s.refund("R2", Some("k1"));
    assert_eq!(other["isError"], true);
    assert!(answer(&other).starts_with("IdempotencyConflict"), "{other}");

    // Without a key, each call is a run of its own: a second e-mail.
    s.refund("R3", None);
    s.refund("R3", None);
    assert_eq!(d.store()["outbox"].as_array().unwrap().len(), 3);
}

#[test]
fn a_run_that_died_is_resumed_by_its_key_not_repeated() {
    let d = Dir::new("resume");
    // The run dies after its fourth step: all but the e-mail, which needs
    // the refund and the reply (the other four, in whatever order they ran).
    let failed = Server::start(&d, &[("CALYX_CRASH_AFTER", "4")]).refund("R1", Some("k1"));
    assert_eq!(failed["isError"], true, "{failed}");
    assert_eq!(d.store()["payments"].as_array().unwrap().len(), 1);
    assert!(d.store()["outbox"].as_array().unwrap().is_empty());

    // A new server (as after a restart): the same key resumes that run.
    let done = Server::start(&d, &[]).refund("R1", Some("k1"));
    assert!(done.get("isError").is_none(), "{done}");
    assert_eq!(done["_meta"]["calyx/run"], failed["_meta"]["calyx/run"]);
    assert_eq!(
        d.store()["payments"].as_array().unwrap().len(),
        1,
        "paid once"
    );
    assert_eq!(d.store()["outbox"].as_array().unwrap().len(), 1);
}
