//! Tools on a remote MCP server (Streamable HTTP): examples/tools/remote_store.py,
//! started by each test on a free port.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use serde_json::Value;

struct Remote {
    child: Child,
    port: u16,
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn remote(log: &Path, flags: &[&str]) -> Remote {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/tools/remote_store.py");
    let mut child = Command::new("python3")
        .arg(script)
        .args(flags)
        .env("CALYX_REMOTE_LOG", log)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line
        .trim()
        .strip_prefix("listening on ")
        .unwrap()
        .parse()
        .unwrap();
    Remote { child, port }
}

const PROGRAM: &str = r#"
type Order:
    id: Text
    email: Text
    total: Float

tool get_order(id: Text) -> Order:
    effect read

tool refund(request: Text, order: Text, amount: Float) -> Unit:
    effect write
    idempotency_key request

tool slow(ms: Nat) -> Text:
    effect read

graph pay(request: Text, order: Text) -> Text:
    o = get_order(order)
    paid = refund(request, o.id, 10)
    return "paid {o.email}"

graph both(ms: Nat) -> List[Text]:
    a = slow(ms)
    b = slow(ms)
    return [a, b]
"#;

fn dir(name: &str, port: u16, extra: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("calyx-remote-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("p.clyx"), PROGRAM).unwrap();
    let mut toml = String::new();
    for t in ["get_order", "refund", "slow"] {
        toml += &format!(
            "[tools.{t}]\nurl = \"http://127.0.0.1:{port}/mcp\"\ntoken_env = \"REMOTE_TOKEN\"\nheaders_env = {{ \"X-Tenant\" = \"REMOTE_TENANT\" }}\n{extra}\n"
        );
    }
    std::fs::write(d.join("calyx.toml"), toml).unwrap();
    d
}

fn calyx(d: &Path, token: Option<&str>, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_calyx"));
    c.args(args)
        .current_dir(d)
        .env("REMOTE_TENANT", "loja-1")
        .env_remove("REMOTE_TOKEN");
    if let Some(t) = token {
        c.env("REMOTE_TOKEN", t);
    }
    c.output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn log(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn pay(sse: bool) {
    let name = if sse { "sse" } else { "json" };
    let base = std::env::temp_dir().join(format!("calyx-remote-log-{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&base);
    let mut flags = vec!["--token", "s3cret"];
    if sse {
        flags.push("--sse");
    }
    let r = remote(&base, &flags);
    let d = dir(name, r.port, "");
    let out = calyx(
        &d,
        Some("s3cret"),
        &[
            "run",
            "p.clyx",
            "--graph",
            "pay",
            "--request",
            "r1",
            "--order",
            "A100",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout).trim(), "paid ana@exemplo.org");
    let seen = log(&base);
    let methods: Vec<&str> = seen.iter().filter_map(|e| e["method"].as_str()).collect();
    assert_eq!(
        &methods[..3],
        ["initialize", "notifications/initialized", "tools/list"]
    );
    let calls: Vec<&Value> = seen
        .iter()
        .filter(|e| e["method"] == "tools/call")
        .collect();
    assert_eq!(calls.len(), 2, "{seen:?}");
    for c in &calls {
        // The session the server gave, the agreed version, the header.
        assert!(c["session"].is_string());
        assert_eq!(c["protocol"], "2025-06-18");
        assert_eq!(c["custom"], "loja-1");
        assert!(c["accept"].as_str().unwrap().contains("text/event-stream"));
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn tools_on_a_remote_server_answering_json() {
    pay(false);
}

#[test]
fn tools_on_a_remote_server_answering_events() {
    pay(true);
}

#[test]
fn a_wrong_or_missing_token_is_said_plainly() {
    let base = std::env::temp_dir().join(format!("calyx-remote-log-auth-{}", std::process::id()));
    let r = remote(&base, &["--token", "s3cret"]);
    let d = dir("auth", r.port, "");
    let args = [
        "run",
        "p.clyx",
        "--graph",
        "pay",
        "--request",
        "r2",
        "--order",
        "A100",
    ];
    let out = calyx(&d, Some("wrong"), &args);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("refused the credentials (HTTP 401)"),
        "{}",
        text(&out.stderr)
    );
    let out = calyx(&d, None, &args);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("needs the environment variable REMOTE_TOKEN"),
        "{}",
        text(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_forgotten_session_is_started_again() {
    let base = std::env::temp_dir().join(format!("calyx-remote-log-forget-{}", std::process::id()));
    let _ = std::fs::remove_file(&base);
    // The session is forgotten after 2 requests: the second call gets a 404.
    let r = remote(&base, &["--token", "s3cret", "--forget-after", "2"]);
    let d = dir("forget", r.port, "");
    let out = calyx(
        &d,
        Some("s3cret"),
        &[
            "run",
            "p.clyx",
            "--graph",
            "pay",
            "--request",
            "r3",
            "--order",
            "A100",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let seen = log(&base);
    let inits = seen.iter().filter(|e| e["method"] == "initialize").count();
    assert!(inits >= 2, "{seen:?}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn calls_to_one_remote_server_run_at_the_same_time() {
    let base = std::env::temp_dir().join(format!("calyx-remote-log-par-{}", std::process::id()));
    let _ = std::fs::remove_file(&base);
    let r = remote(&base, &["--token", "s3cret"]);
    let d = dir("par", r.port, "");
    let out = calyx(
        &d,
        Some("s3cret"),
        &["run", "p.clyx", "--graph", "both", "--ms", "700"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    // Both calls reached the server before the first one ended.
    let seen = log(&base);
    let t = |e: &Value| e["t"].as_f64().unwrap();
    let starts: Vec<f64> = seen
        .iter()
        .filter(|e| e["tool"] == "slow" && e["method"] == "tools/call")
        .map(t)
        .collect();
    let first_end = seen
        .iter()
        .filter(|e| e["method"] == "done")
        .map(t)
        .fold(f64::MAX, f64::min);
    assert_eq!(starts.len(), 2, "{seen:?}");
    assert!(
        starts.iter().all(|s| *s < first_end),
        "not concurrent: {seen:?}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn check_tools_compares_with_the_remote_annotations() {
    let base = std::env::temp_dir().join(format!("calyx-remote-log-check-{}", std::process::id()));
    let r = remote(&base, &["--token", "s3cret"]);
    let d = dir("check", r.port, "");
    let out = calyx(&d, Some("s3cret"), &["check", "p.clyx", "--tools"]);
    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    // `refund` is a keyed write the server calls neither read-only nor
    // idempotent: nothing to warn about. `slow` and `get_order` are reads.
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("W0701"), "{all}");
    assert!(!all.contains("E0702"), "{all}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_tool_needs_a_command_or_a_url() {
    let d = std::env::temp_dir().join(format!("calyx-remote-cfg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("p.clyx"), PROGRAM).unwrap();
    std::fs::write(
        d.join("calyx.toml"),
        "[tools.slow]\nurl = \"http://x\"\ncommand = [\"y\"]\n",
    )
    .unwrap();
    let out = calyx(&d, None, &["run", "p.clyx", "--graph", "both", "--ms", "1"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("`command` or `url`, not both"),
        "{}",
        text(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&d);
}
