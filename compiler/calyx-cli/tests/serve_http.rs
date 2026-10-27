//! `calyx serve --http`: the program over Streamable HTTP, to the callers
//! calyx.toml declares, each with its token and the graphs it may call.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};

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

graph consulta(order: Text) -> Text:
    o = get_order(order)
    return o.email

graph pagar(request: Text, order: Text) -> Text:
    paid = refund(request, order, 10)
    return "pago {request}"
"#;

const SUPORTE: &str = "token-do-suporte-0123456789";
const OPS: &str = "token-de-operacoes-987654321";

struct Served {
    child: Child,
    port: u16,
    dir: PathBuf,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn dir(name: &str, serve_toml: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("calyx-serve-http-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("p.clyx"), PROGRAM).unwrap();
    let store = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/tools/fake_store.py")
        .canonicalize()
        .unwrap();
    let mut toml: String = ["get_order", "refund"]
        .iter()
        .map(|t| {
            format!(
                "[tools.{t}]\ncommand = [\"python3\", '{}']\n",
                store.display()
            )
        })
        .collect();
    toml.push_str(serve_toml);
    std::fs::write(d.join("calyx.toml"), toml).unwrap();
    d
}

const CALLERS: &str = r#"
[serve.callers.suporte]
token_env = "TOKEN_SUPORTE"
graphs = ["consulta", "pagar"]

[serve.callers.leitura]
token_env = "TOKEN_OPS"
graphs = ["consulta"]
"#;

fn serve(name: &str) -> Served {
    let d = dir(name, CALLERS);
    let mut child = Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(["serve", "p.clyx", "--http", "127.0.0.1:0"])
        .current_dir(&d)
        .env("CALYX_FAKE_STORE", d.join("store.json"))
        .env("TOKEN_SUPORTE", SUPORTE)
        .env("TOKEN_OPS", OPS)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let port = loop {
        let mut line = String::new();
        assert!(err.read_line(&mut line).unwrap() > 0, "the server stopped");
        if let Some(rest) = line
            .trim()
            .strip_prefix("calyx serve: listening on http://127.0.0.1:")
        {
            break rest.split('/').next().unwrap().parse().unwrap();
        }
    };
    // Keep reading stderr, so the server never blocks on it.
    std::thread::spawn(move || for _ in err.lines() {});
    Served {
        child,
        port,
        dir: d,
    }
}

/// One HTTP request: status, headers (lowercase), body as JSON (or null).
fn http(
    port: u16,
    method: &str,
    headers: &[(&str, &str)],
    body: &Value,
) -> (u16, Vec<(String, String)>, Value) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let body = body.to_string();
    let mut req = format!(
        "{method} /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, rest) = out.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
        .collect();
    (
        status,
        headers,
        serde_json::from_str(rest).unwrap_or(Value::Null),
    )
}

/// A client with a session, as `caller`.
struct Client {
    port: u16,
    token: String,
    session: String,
    next: u64,
}

impl Client {
    fn open(port: u16, token: &str) -> Client {
        let auth = format!("Bearer {token}");
        let (status, headers, _) = http(
            port,
            "POST",
            &[("Authorization", &auth)],
            &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                    "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                               "clientInfo": {"name": "t", "version": "0"}}}),
        );
        assert_eq!(status, 200);
        let session = headers
            .iter()
            .find(|(k, _)| k == "mcp-session-id")
            .map(|(_, v)| v.clone())
            .unwrap();
        Client {
            port,
            token: token.to_owned(),
            session,
            next: 0,
        }
    }

    fn rpc(&mut self, method: &str, params: Value) -> (u16, Value) {
        self.next += 1;
        let auth = format!("Bearer {}", self.token);
        let (status, _, body) = http(
            self.port,
            "POST",
            &[("Authorization", &auth), ("Mcp-Session-Id", &self.session)],
            &json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}),
        );
        (status, body["result"].clone())
    }

    fn tools(&mut self) -> Vec<String> {
        let (_, r) = self.rpc("tools/list", json!({}));
        r["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    }
}

fn text_of(r: &Value) -> &str {
    r["content"][0]["text"].as_str().unwrap_or("")
}

#[test]
fn only_known_tokens_get_in() {
    let s = serve("tokens");
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    assert_eq!(http(s.port, "POST", &[], &init).0, 401);
    assert_eq!(
        http(s.port, "POST", &[("Authorization", "Bearer wrong")], &init).0,
        401
    );
    let auth = format!("Bearer {SUPORTE}");
    // A web page, from someone's browser: refused.
    let evil = [
        ("Authorization", auth.as_str()),
        ("Origin", "https://evil.example"),
    ];
    assert_eq!(http(s.port, "POST", &evil, &init).0, 403);
    // A session is the caller's that opened it.
    let mut a = Client::open(s.port, SUPORTE);
    let mut b = Client::open(s.port, OPS);
    b.session = a.session.clone();
    assert_eq!(b.rpc("tools/list", json!({})).0, 404);
    assert_eq!(a.rpc("tools/list", json!({})).0, 200);
}

#[test]
fn a_caller_sees_and_calls_only_its_graphs() {
    let s = serve("graphs");
    let mut suporte = Client::open(s.port, SUPORTE);
    let mut leitura = Client::open(s.port, OPS);
    assert_eq!(suporte.tools(), ["consulta", "pagar"]);
    assert_eq!(leitura.tools(), ["consulta"]);
    let (_, r) = leitura.rpc(
        "tools/call",
        json!({"name": "consulta", "arguments": {"order": "A100"}}),
    );
    assert_eq!(text_of(&r), "ana@exemplo.org", "{r}");
    let (_, r) = leitura.rpc(
        "tools/call",
        json!({"name": "pagar", "arguments": {"request": "r1", "order": "A100"}}),
    );
    assert_eq!(r["isError"], true);
    assert!(
        text_of(&r).contains("Forbidden: `leitura` may not call `pagar`"),
        "{r}"
    );
    let store = std::fs::read_to_string(s.dir.join("store.json")).unwrap_or_default();
    assert!(!store.contains("\"r1\""), "nothing paid: {store}");
}

#[test]
fn idempotency_keys_are_per_caller() {
    let d = dir(
        "keys",
        r#"
[serve.callers.a]
token_env = "TOKEN_SUPORTE"
graphs = ["*"]

[serve.callers.b]
token_env = "TOKEN_OPS"
graphs = ["*"]
"#,
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(["serve", "p.clyx", "--http", "127.0.0.1:0"])
        .current_dir(&d)
        .env("CALYX_FAKE_STORE", d.join("store.json"))
        .env("TOKEN_SUPORTE", SUPORTE)
        .env("TOKEN_OPS", OPS)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let port: u16 = loop {
        let mut line = String::new();
        assert!(err.read_line(&mut line).unwrap() > 0);
        if let Some(rest) = line
            .trim()
            .strip_prefix("calyx serve: listening on http://127.0.0.1:")
        {
            break rest.split('/').next().unwrap().parse().unwrap();
        }
    };
    std::thread::spawn(move || for _ in err.lines() {});
    let s = Served {
        child,
        port,
        dir: d,
    };
    let call = |c: &mut Client, req: &str| {
        c.rpc(
            "tools/call",
            json!({"name": "pagar", "idempotencyKey": "k1",
                   "arguments": {"request": req, "order": "A100"}}),
        )
        .1
    };
    let mut a = Client::open(s.port, SUPORTE);
    let first = call(&mut a, "ra");
    assert_eq!(text_of(&first), "pago ra");
    // a again: the same answer, not a second run.
    let again = call(&mut a, "ra");
    assert_eq!(again["_meta"]["calyx/replayed"], true);
    // b with the same key: its own run, not a's answer.
    let mut b = Client::open(s.port, OPS);
    let other = call(&mut b, "rb");
    assert_eq!(text_of(&other), "pago rb");
    assert!(other["_meta"]["calyx/replayed"].is_null());
    assert_ne!(other["_meta"]["calyx/run"], first["_meta"]["calyx/run"]);
}

/// A Calyx program calling another, served over HTTP, as a remote tool.
#[test]
fn a_calyx_program_calls_a_served_one() {
    let s = serve("dogfood");
    let client =
        std::env::temp_dir().join(format!("calyx-serve-http-client-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&client);
    std::fs::create_dir_all(&client).unwrap();
    std::fs::write(
        client.join("c.clyx"),
        r#"
tool consulta(order: Text) -> Text:
    effect read

graph ask(order: Text) -> Text:
    email = consulta(order)
    return "o cliente é {email}"
"#,
    )
    .unwrap();
    std::fs::write(
        client.join("calyx.toml"),
        format!(
            "[tools.consulta]\nurl = \"http://127.0.0.1:{}/mcp\"\ntoken_env = \"MY_TOKEN\"\n",
            s.port
        ),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(["run", "c.clyx", "--order", "A100"])
        .current_dir(&client)
        .env("MY_TOKEN", OPS)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "o cliente é ana@exemplo.org"
    );
    let _ = std::fs::remove_dir_all(&client);
}

#[test]
fn http_needs_callers_with_tokens() {
    let d = dir("nocallers", "");
    let out = Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(["serve", "p.clyx", "--http", "127.0.0.1:0"])
        .current_dir(&d)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs [serve.callers.NAME]"));
    let d2 = dir("notoken", CALLERS);
    let out = Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(["serve", "p.clyx", "--http", "127.0.0.1:0"])
        .current_dir(&d2)
        .env("TOKEN_SUPORTE", "short")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("16 characters or more"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&d2);
}
