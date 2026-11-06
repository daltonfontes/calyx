//! `calyx lsp`, driven as an editor drives it: JSON-RPC with
//! `Content-Length` frames over stdio.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Editor {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    child: std::process::Child,
    next: u64,
}

impl Editor {
    fn start() -> Editor {
        let mut child = Command::new(env!("CARGO_BIN_EXE_calyx"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        Editor {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 0,
        }
    }

    fn send(&mut self, msg: &Value) {
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut len = 0;
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "the server stopped"
            );
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        self.stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.read();
            if m["id"] == id {
                return m;
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// The next diagnostics the server publishes.
    fn diagnostics(&mut self) -> Vec<Value> {
        loop {
            let m = self.read();
            if m["method"] == "textDocument/publishDiagnostics" {
                return m["params"]["diagnostics"].as_array().unwrap().clone();
            }
        }
    }
}

const URI: &str = "file:///tmp/agente.clyx";

/// `ação` is two UTF-16 units shorter in bytes than in characters: the
/// columns must count UTF-16 units, as editors do.
const BROKEN: &str = "model m = \"gemini-3.5-flash-lite\"\n\nprompt ação(q: Text) -> Text:\n    \"\"\"\n    {q}\n    \"\"\"\n\ngraph main(q: Text) -> Text:\n    a = m(ação(q))\n    return desconhecido\n";

const FIXED: &str = "model m = \"gemini-3.5-flash-lite\"\n\nprompt ação(q: Text) -> Text:\n    \"\"\"\n    {q}\n    \"\"\"\n\ngraph main(q: Text) -> Text:\n    a = m(ação(q))\n    return a\n";

#[test]
fn an_editor_gets_diagnostics_outline_definitions_and_hovers() {
    let mut e = Editor::start();
    let init = e.request(
        "initialize",
        json!({"processId": null, "rootUri": null, "capabilities": {}}),
    );
    let caps = &init["result"]["capabilities"];
    assert_eq!(caps["textDocumentSync"]["change"], 1, "{init}");
    assert_eq!(caps["definitionProvider"], true);
    e.notify("initialized", json!({}));

    // Opening a file with an error: the diagnostic, where it is.
    e.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": URI, "languageId": "calyx", "version": 1, "text": BROKEN}}),
    );
    let d = e.diagnostics();
    let err = d
        .iter()
        .find(|x| x["severity"] == 1)
        .unwrap_or_else(|| panic!("{d:?}"));
    assert_eq!(err["code"], "E0602", "{err}");
    assert_eq!(err["source"], "calyx");
    assert_eq!(
        err["range"]["start"],
        json!({"line": 9, "character": 11}),
        "{err}"
    );
    assert!(
        err["message"].as_str().unwrap().contains("desconhecido"),
        "{err}"
    );

    // Fixing it: no errors left (CRLF, as from Windows, is fine).
    e.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": URI, "version": 2},
               "contentChanges": [{"text": FIXED.replace('\n', "\r\n")}]}),
    );
    let d = e.diagnostics();
    assert!(d.iter().all(|x| x["severity"] != 1), "{d:?}");

    // The outline.
    let s = e.request(
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": URI}}),
    );
    let names: Vec<&str> = s["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["m", "ação", "main"], "{s}");

    // Go to definition from the call `ação(q)` (line 8): to the prompt's
    // name, line 2, after `prompt `. Columns in UTF-16 units.
    let at = json!({"textDocument": {"uri": URI}, "position": {"line": 8, "character": 11}});
    let def = e.request("textDocument/definition", at.clone());
    assert_eq!(
        def["result"]["range"],
        json!({"start": {"line": 2, "character": 7}, "end": {"line": 2, "character": 11}}),
        "{def}"
    );

    // Hover: the declaration.
    let h = e.request("textDocument/hover", at);
    let shown = h["result"]["contents"]["value"].as_str().unwrap();
    assert!(shown.contains("prompt ação(q: Text) -> Text:"), "{shown}");
    assert!(shown.contains("*prompt*"), "{shown}");

    // Nothing to define on a blank: null, not an error.
    let none = e.request(
        "textDocument/definition",
        json!({"textDocument": {"uri": URI}, "position": {"line": 1, "character": 0}}),
    );
    assert_eq!(none["result"], Value::Null, "{none}");

    // Closing clears the diagnostics; shutdown, then exit, ends it cleanly.
    e.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": URI}}),
    );
    assert!(e.diagnostics().is_empty());
    let r = e.request("shutdown", Value::Null);
    assert_eq!(r["result"], Value::Null);
    e.notify("exit", Value::Null);
    drop(e.stdin);
    assert!(e.child.wait().unwrap().success());
}
