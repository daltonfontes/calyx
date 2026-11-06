//! `calyx lsp`: a language server (LSP, over stdio), for editors.
//!
//! - **Diagnostics** as you type: the same checks as `calyx check`, with
//!   their code, what was expected and what was observed, on every change
//!   of an open file.
//! - **Outline** (document symbols): the file's models, tools, types,
//!   prompts, graphs, entities, defs, routers and policies.
//! - **Go to definition** and **hover** for the names declared in the file.
//!
//! Files are synced whole on every change (a check takes milliseconds).
//! Positions are counted in UTF-16 code units, as LSP clients expect.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::process::ExitCode;

use calyx_syntax::ast::Decl;
use calyx_syntax::{Severity, Source, Span};
use serde_json::{Value, json};

pub fn lsp(args: &[String]) -> ExitCode {
    if let Some(a) = args.first() {
        eprintln!("calyx: lsp takes no arguments, got `{a}`");
        return ExitCode::from(2);
    }
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout().lock();
    let mut docs: HashMap<String, String> = HashMap::new();
    let mut shutdown = false;
    while let Some(msg) = read_message(&mut input) {
        let method = msg["method"].as_str().unwrap_or_default();
        let id = msg.get("id").cloned();
        let params = &msg["params"];
        let uri = params["textDocument"]["uri"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let result = match method {
            "initialize" => Some(json!({
                "capabilities": {
                    "textDocumentSync": {"openClose": true, "change": 1},
                    "documentSymbolProvider": true,
                    "definitionProvider": true,
                    "hoverProvider": true,
                },
                "serverInfo": {"name": "calyx", "version": env!("CARGO_PKG_VERSION")},
            })),
            "textDocument/didOpen" => {
                let text = normalize(params["textDocument"]["text"].as_str().unwrap_or_default());
                publish(&mut out, &uri, &text);
                docs.insert(uri, text);
                None
            }
            "textDocument/didChange" => {
                // Full sync: the last change is the whole text.
                if let Some(text) = params["contentChanges"]
                    .as_array()
                    .and_then(|c| c.last())
                    .and_then(|c| c["text"].as_str())
                {
                    let text = normalize(text);
                    publish(&mut out, &uri, &text);
                    docs.insert(uri, text);
                }
                None
            }
            "textDocument/didClose" => {
                docs.remove(&uri);
                send(
                    &mut out,
                    &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                            "params": {"uri": uri, "diagnostics": []}}),
                );
                None
            }
            "textDocument/documentSymbol" => Some(docs.get(&uri).map_or(json!([]), |t| symbols(t))),
            "textDocument/definition" => Some(
                docs.get(&uri)
                    .and_then(|t| declaration_at(t, &params["position"]))
                    .map_or(
                        Value::Null,
                        |(src, d)| json!({"uri": uri, "range": range(&src, d.name().span)}),
                    ),
            ),
            "textDocument/hover" => Some(
                docs.get(&uri)
                    .and_then(|t| declaration_at(t, &params["position"]))
                    .map_or(Value::Null, |(src, d)| hover(&src, &d)),
            ),
            "shutdown" => {
                shutdown = true;
                Some(Value::Null)
            }
            "exit" => break,
            _ => {
                if let Some(id) = &id {
                    send(
                        &mut out,
                        &json!({"jsonrpc": "2.0", "id": id,
                                "error": {"code": -32601, "message": format!("no method {method}")}}),
                    );
                }
                None
            }
        };
        if let (Some(id), Some(result)) = (id, result) {
            send(
                &mut out,
                &json!({"jsonrpc": "2.0", "id": id, "result": result}),
            );
        }
    }
    if shutdown {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// One message: `Content-Length` headers, a blank line, the JSON body.
fn read_message(input: &mut impl BufRead) -> Option<Value> {
    loop {
        let mut len = None;
        loop {
            let mut line = String::new();
            if input.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse::<usize>().ok();
            }
        }
        let Some(len) = len else { continue };
        let mut body = vec![0; len];
        input.read_exact(&mut body).ok()?;
        if let Ok(v) = serde_json::from_slice(&body) {
            return Some(v);
        }
    }
}

fn send(out: &mut impl Write, msg: &Value) {
    let body = msg.to_string();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

/// As `calyx check` reads files: CRLF line ends count as LF.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// The file's name, for the diagnostics' own text.
fn file_name(uri: &str) -> String {
    uri.rsplit('/').next().unwrap_or(uri).replace("%20", " ")
}

fn publish(out: &mut impl Write, uri: &str, text: &str) {
    let report = calyx_check::check(&file_name(uri), text);
    let diagnostics: Vec<Value> = report
        .diagnostics
        .iter()
        .map(|d| {
            let mut message = d.message.clone();
            if let Some(e) = &d.expected {
                message.push_str(&format!("\nexpected: {e}"));
            }
            if let Some(o) = &d.observed {
                message.push_str(&format!("\nobserved: {o}"));
            }
            json!({
                "range": range(&report.source, d.span),
                "severity": if d.severity == Severity::Error { 1 } else { 2 },
                "code": d.code,
                "source": "calyx",
                "message": message,
            })
        })
        .collect();
    send(
        out,
        &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                "params": {"uri": uri, "diagnostics": diagnostics}}),
    );
}

/// A byte offset as an LSP position: 0-based line, UTF-16 column.
fn position(src: &Source, offset: u32) -> Value {
    let at = src.line_col(offset);
    let line_start = src.text[..offset as usize].rfind('\n').map_or(0, |i| i + 1);
    let col: usize = src.text[line_start..offset as usize]
        .chars()
        .map(char::len_utf16)
        .sum();
    json!({"line": at.line - 1, "character": col})
}

fn range(src: &Source, span: Span) -> Value {
    let end = span.end.max(span.start);
    json!({"start": position(src, span.start), "end": position(src, end)})
}

/// An LSP position as a byte offset, if it is in the text.
fn offset(text: &str, pos: &Value) -> Option<usize> {
    let line = pos["line"].as_u64()? as usize;
    let character = pos["character"].as_u64()? as usize;
    let start = if line == 0 {
        0
    } else {
        text.match_indices('\n').nth(line - 1)?.0 + 1
    };
    let mut units = 0;
    for (i, c) in text[start..].char_indices() {
        if units >= character || c == '\n' {
            return Some(start + i);
        }
        units += c.len_utf16();
    }
    Some(text.len())
}

/// The declaration named by the word at `pos`, if the file declares it.
fn declaration_at(text: &str, pos: &Value) -> Option<(Source, Decl)> {
    let at = offset(text, pos)?;
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(at, |(i, _)| i);
    let end = text[at..]
        .char_indices()
        .find(|(_, c)| !is_word(*c))
        .map_or(text.len(), |(i, _)| at + i);
    let word = &text[start..end];
    if word.is_empty() {
        return None;
    }
    let (program, _) = calyx_syntax::parse(text);
    let decl = program
        .decls
        .into_iter()
        .find(|d| !matches!(d, Decl::Policy(_)) && d.name().name == word)?;
    Some((Source::new("", text), decl))
}

fn kind(d: &Decl) -> (&'static str, u32) {
    // LSP SymbolKind: Class 5, Method 6, Interface 11, Function 12,
    // String 15, Object 19, Struct 23.
    match d {
        Decl::Model(_) => ("model", 19),
        Decl::Tool(_) => ("tool", 6),
        Decl::Type(_) => ("type", 23),
        Decl::Prompt(_) => ("prompt", 15),
        Decl::Graph(_) => ("graph", 12),
        Decl::Entity(_) => ("entity", 5),
        Decl::Def(_) => ("def", 12),
        Decl::Router(_) => ("router", 19),
        Decl::Policy(_) => ("policy", 11),
        Decl::Skill(_) => ("skill", 15),
    }
}

fn span_of(d: &Decl) -> Span {
    match d {
        Decl::Model(x) => x.span,
        Decl::Tool(x) => x.span,
        Decl::Type(x) => x.span,
        Decl::Prompt(x) => x.span,
        Decl::Graph(x) => x.span,
        Decl::Entity(x) => x.span,
        Decl::Def(x) => x.span,
        Decl::Router(x) => x.span,
        Decl::Policy(x) => x.span,
        Decl::Skill(x) => x.span,
    }
}

fn symbols(text: &str) -> Value {
    let src = Source::new("", text);
    let (program, _) = calyx_syntax::parse(text);
    let list: Vec<Value> = program
        .decls
        .iter()
        .map(|d| {
            let (what, k) = kind(d);
            let name = match d {
                Decl::Policy(p) => format!("policy {}", p.tool.name),
                _ => d.name().name.clone(),
            };
            let whole = span_of(d);
            let whole = Span {
                start: whole.start.min(d.name().span.start),
                end: whole.end.max(d.name().span.end),
            };
            json!({
                "name": name,
                "detail": what,
                "kind": k,
                "range": range(&src, whole),
                "selectionRange": range(&src, d.name().span),
            })
        })
        .collect();
    json!(list)
}

/// The declaration's text: whole for short ones (a tool and its contract),
/// the first line for graphs, entities and defs.
fn hover(src: &Source, d: &Decl) -> Value {
    let (what, _) = kind(d);
    let text = src.slice(span_of(d));
    let shown: Vec<&str> = match d {
        Decl::Graph(_) | Decl::Entity(_) | Decl::Def(_) => text.lines().take(1).collect(),
        _ => text.lines().take(14).collect(),
    };
    let more = if text.lines().count() > shown.len() {
        "\n    ..."
    } else {
        ""
    };
    json!({
        "contents": {
            "kind": "markdown",
            "value": format!("```python\n{}{more}\n```\n*{what}*", shown.join("\n")),
        },
        "range": range(src, d.name().span),
    })
}
