//! A tool server that does not write UTF-8 (Python on Windows writes the
//! console's code page to a pipe): its answers are still read, with U+FFFD
//! for what is not UTF-8, and the run says so once, instead of taking the
//! server for dead at the first accented letter.

use std::process::Command;

const SERVER: &str = r#"
import json, sys
out = sys.stdout.buffer
for line in sys.stdin.buffer:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    if msg["method"] == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "latin1", "version": "0"}}
    elif msg["method"] == "tools/list":
        result = {"tools": [{"name": "lookup", "description": "Descrição",
                             "inputSchema": {"type": "object"}}]}
    else:
        result = {"content": [{"type": "text", "text": "ação"}]}
    text = json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}, ensure_ascii=False)
    out.write(text.encode("latin-1") + b"\r\n")
    out.flush()
"#;

#[test]
fn a_server_that_writes_latin1_is_still_read() {
    let d = std::env::temp_dir().join(format!("calyx-latin1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("server.py"), SERVER).unwrap();
    std::fs::write(
        d.join("calyx.toml"),
        "[tools.lookup]\ncommand = [\"python3\", \"server.py\"]\n",
    )
    .unwrap();
    std::fs::write(
        d.join("p.clyx"),
        "tool lookup(q: Text) -> Text:\n    effect read\n\ngraph g(q: Text) -> Text:\n    a = lookup(q)\n    return a\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(["run", "p.clyx", "--quiet", "--no-journal", "--q", "x"])
        .current_dir(&d)
        .env("PYTHONIOENCODING", "latin-1")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "a\u{fffd}\u{fffd}o"
    );
    assert_eq!(
        err.matches("writes text that is not UTF-8").count(),
        1,
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&d);
}
