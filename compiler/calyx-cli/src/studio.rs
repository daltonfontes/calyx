//! `calyx studio <file.clyx>`: the program as a graph, next to its code,
//! in the browser.
//!
//! A small web server on 127.0.0.1 serves one page (`studio.html`, inside
//! the binary: nothing to install, nothing fetched from the network) and
//! three calls:
//!
//! - `GET /api/program[?since=HASH]`: the file as it is on disk, checked:
//!   its text, its problems, and each graph's steps and how they depend on
//!   each other (what `calyx check --ir` shows), with the line of each step.
//!   With `since`, an empty answer (204) while the file has not changed, so
//!   the page follows a file edited in another editor.
//! - `POST /api/check`: the same for a text the page sends, as it is typed.
//! - `POST /api/save`: writes that text to the file, and checks it.
//!
//! The code is the source of truth: the graph is drawn from it, never the
//! other way round.
//!
//! Only this machine reaches it, and only the page it serves drives it: a
//! request whose `Host` is not 127.0.0.1 or localhost (DNS rebinding) is
//! refused, and so is a `POST` without the page's `Origin` and its header
//! `X-Calyx-Studio` (another site in the same browser). It writes only the
//! file it was started with.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use calyx_syntax::Severity;
use calyx_syntax::ast::{Decl, Expr, ExprKind, Ident, Stmt};
use serde_json::{Value, json};

const PAGE: &str = include_str!("studio.html");
const DEFAULT_PORT: u16 = 4180;

pub fn studio(args: &[String]) -> ExitCode {
    let mut file = None;
    let mut port = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => match it.next().and_then(|p| p.parse::<u16>().ok()) {
                Some(p) => port = Some(p),
                None => return usage("--port takes a number"),
            },
            f if !f.starts_with('-') && file.is_none() => file = Some(PathBuf::from(f)),
            other => return usage(&format!("unknown option `{other}`")),
        }
    }
    let Some(file) = file else {
        return usage("which program? `calyx studio <file.clyx>`");
    };
    if let Err(e) = std::fs::read_to_string(&file) {
        eprintln!("calyx: {}: {e}", file.display());
        return ExitCode::from(2);
    }
    // The port asked for, or the first free one from 4180.
    let listener = match port {
        Some(p) => TcpListener::bind(("127.0.0.1", p)).ok(),
        None => {
            (DEFAULT_PORT..DEFAULT_PORT + 20).find_map(|p| TcpListener::bind(("127.0.0.1", p)).ok())
        }
    };
    let Some(listener) = listener else {
        eprintln!("calyx: studio: no free port on 127.0.0.1");
        return ExitCode::FAILURE;
    };
    let port = listener.local_addr().map_or(0, |a| a.port());
    eprintln!(
        "calyx: studio for {} at http://127.0.0.1:{port}/",
        file.display()
    );
    eprintln!("calyx: Ctrl+C stops it");
    let file: &'static PathBuf = Box::leak(Box::new(file));
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || handle(stream, file, port));
    }
    ExitCode::SUCCESS
}

fn usage(why: &str) -> ExitCode {
    eprintln!("calyx: studio: {why}\nusage: calyx studio <file.clyx> [--port N]");
    ExitCode::from(2)
}

fn handle(mut stream: TcpStream, file: &PathBuf, port: u16) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Some(req) = crate::serve::read_request(&mut stream) else {
        return;
    };
    let header = |name: &str| {
        req.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    let ours = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    if !header("host").is_some_and(|h| ours.iter().any(|o| o == h)) {
        return respond(
            &mut stream,
            403,
            "text/plain",
            "calyx studio answers only on 127.0.0.1",
        );
    }
    let (path, query) = req.path.split_once('?').unwrap_or((&req.path, ""));
    if req.method == "POST" {
        let origin_ok =
            header("origin").is_some_and(|o| ours.iter().any(|h| o == format!("http://{h}")));
        if !origin_ok || header("x-calyx-studio").is_none() {
            return respond(
                &mut stream,
                403,
                "text/plain",
                "only the studio's page may do that",
            );
        }
    }
    let name = file.display().to_string();
    match (req.method.as_str(), path) {
        ("GET", "/") => respond(&mut stream, 200, "text/html; charset=utf-8", PAGE),
        ("GET", "/api/program") => match std::fs::read_to_string(file) {
            Ok(text) => {
                let since = query.strip_prefix("since=").unwrap_or("");
                if !since.is_empty() && since == hash(&text) {
                    respond(&mut stream, 204, "text/plain", "")
                } else {
                    let a = analyze(&name, &text);
                    respond(&mut stream, 200, "application/json", &a.to_string())
                }
            }
            Err(e) => respond(&mut stream, 500, "text/plain", &format!("{name}: {e}")),
        },
        ("POST", "/api/check" | "/api/save") => {
            let Ok(text) = String::from_utf8(req.body) else {
                return respond(&mut stream, 400, "text/plain", "the text is not UTF-8");
            };
            if path == "/api/save" {
                let tmp = file.with_extension("clyx.studio-tmp");
                if let Err(e) =
                    std::fs::write(&tmp, &text).and_then(|()| std::fs::rename(&tmp, file))
                {
                    let _ = std::fs::remove_file(&tmp);
                    return respond(&mut stream, 500, "text/plain", &format!("{name}: {e}"));
                }
            }
            let a = analyze(&name, &text);
            respond(&mut stream, 200, "application/json", &a.to_string());
        }
        _ => respond(&mut stream, 404, "text/plain", "not found"),
    }
}

fn respond(stream: &mut TcpStream, status: u16, kind: &str, body: &str) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src data:; frame-ancestors 'none'\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

/// A short fingerprint of the text, to tell whether the file changed.
fn hash(text: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// The text, checked: its problems and, if it has none, its graphs.
pub(crate) fn analyze(name: &str, text: &str) -> Value {
    let report = calyx_check::check(name, text);
    let src = &report.source;
    let at = |offset: u32| crate::lsp::position(src, offset);
    let diagnostics: Vec<Value> = report
        .diagnostics
        .iter()
        .map(|d| {
            json!({
                "severity": if d.severity == Severity::Error { "error" } else { "warning" },
                "code": d.code,
                "message": d.message,
                "expected": d.expected,
                "observed": d.observed,
                "start": at(d.span.start),
                "end": at(d.span.end.max(d.span.start)),
            })
        })
        .collect();
    let ok = !report.has_errors();
    // Where each graph and each of its steps is written.
    let (ast, _) = calyx_syntax::parse(text);
    let line = |offset: u32| src.line_col(offset).line - 1;
    // The first line of what a step computes, as written.
    let code = |e: &Expr| {
        let first = src.slice(e.span).lines().next().unwrap_or("").trim();
        if first.chars().count() > 60 {
            format!("{}…", first.chars().take(59).collect::<String>())
        } else {
            first.to_owned()
        }
    };
    let mut written: Vec<(String, u32, Vec<Value>)> = Vec::new();
    for d in &ast.decls {
        if let Decl::Graph(g) = d {
            let steps = g
                .body
                .iter()
                .filter_map(|s| {
                    let (name, at, value) = match s {
                        Stmt::Node { name, value, .. } => {
                            (name.name.clone(), name.span.start, value)
                        }
                        Stmt::Return(e) => ("return".to_owned(), e.span.start, e),
                        _ => return None,
                    };
                    // Steps inside it (a `for each` or loop body, a `match`).
                    let mut inner = Vec::new();
                    inner_steps(value, 0, &mut |n, v, depth| {
                        inner.push(json!({
                            "name": n.name,
                            "line": line(n.span.start),
                            "code": code(v),
                            "depth": depth,
                        }));
                    });
                    Some(json!({
                        "name": name,
                        "line": line(at),
                        "code": code(value),
                        "inner": inner,
                    }))
                })
                .collect();
            written.push((g.name.name.clone(), line(g.name.span.start), steps));
        }
    }
    let graphs: Vec<Value> = if ok {
        report
            .ir
            .graphs
            .iter()
            .map(|g| {
                let place = written.iter().find(|(n, _, _)| *n == g.name);
                let step = |n: &str| {
                    place
                        .and_then(|(_, _, s)| s.iter().find(|v| v["name"] == n))
                        .cloned()
                        .unwrap_or(Value::Null)
                };
                let over = |e: &calyx_ir::Expr| match e {
                    calyx_ir::Expr::Param(i) => g.params.get(*i).map(|p| p.0.clone()),
                    calyx_ir::Expr::Node(id) => {
                        g.nodes.iter().find(|n| n.id == *id).map(|n| n.name.clone())
                    }
                    _ => None,
                };
                json!({
                    "name": g.name,
                    "params": g.params,
                    "ret": g.ret,
                    "effect": g.effect.map(|e| e.as_str()),
                    "line": place.map(|p| p.1),
                    "output": g.output.map(|o| o.0),
                    "nodes": g.nodes.iter().map(|n| {
                        let s = step(&n.name);
                        json!({
                            "id": n.id.0,
                            "name": n.name,
                            "kind": n.kind.to_string(),
                            "type": n.ty,
                            "effect": n.effect.as_str(),
                            "inputs": n.inputs.iter().map(|i| i.0).collect::<Vec<_>>(),
                            "fan_out": n.fan_out,
                            "over": n.over.as_ref().and_then(over),
                            "line": s["line"],
                            "code": s["code"],
                            "inner": s.get("inner").cloned().unwrap_or(json!([])),
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    json!({
        "file": name,
        "hash": hash(text),
        "text": text,
        "ok": ok,
        "diagnostics": diagnostics,
        "graphs": graphs,
    })
}

/// Calls `f` on each `name = value` step inside `e`, in the order written,
/// with how deep it is (0: directly in the step's body).
fn inner_steps(e: &Expr, depth: usize, f: &mut dyn FnMut(&Ident, &Expr, usize)) {
    match &e.kind {
        ExprKind::Block { steps, tail } => {
            for (n, v) in steps {
                f(n, v, depth);
                inner_steps(v, depth + 1, f);
            }
            inner_steps(tail, depth, f);
        }
        ExprKind::If { then, els, .. } => {
            inner_steps(then, depth, f);
            inner_steps(els, depth, f);
        }
        ExprKind::Match { cases, .. } => {
            for c in cases {
                inner_steps(&c.body, depth, f);
            }
        }
        ExprKind::Loop { body, .. } | ExprKind::Each { body, .. } => inner_steps(body, depth, f),
        ExprKind::Try(v) | ExprKind::Done(v) | ExprKind::Next(v) => inner_steps(v, depth, f),
        ExprKind::Race(r) => {
            for (_, v) in &r.branches {
                inner_steps(v, depth, f);
            }
        }
        _ => {}
    }
}
