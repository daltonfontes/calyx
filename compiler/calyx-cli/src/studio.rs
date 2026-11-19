//! `calyx studio [folder | file.clyx]`: a workspace for Calyx programs in
//! the browser. Each program's graphs are drawn next to its code, with its
//! problems as you type; programs are created (from a template), imported,
//! saved, renamed, deleted and downloaded there.
//!
//! A small web server on 127.0.0.1 serves one page (`studio.html`, inside
//! the binary: nothing to install, nothing fetched from the network) and
//! these calls, all on the workspace folder (the current one by default):
//!
//! - `GET /api/workspace`: the folder, its `.clyx` files, the file to open
//!   first, and the templates.
//! - `GET /api/program?path=P[&since=HASH]`: a file as it is on disk,
//!   checked: its text, its problems, and each graph's steps and how they
//!   depend on each other (what `calyx check --ir` shows), with the line of
//!   each step. With `since`, an empty answer (204) while the file has not
//!   changed, so the page follows a file edited in another editor.
//! - `POST /api/check?path=P`: the same for a text the page sends, as it is
//!   typed. Nothing is written.
//! - `POST /api/save?path=P[&new=1]`: writes the text to the file (with
//!   `new`, only if there is none yet), and checks it.
//! - `POST /api/rename?path=P&to=Q`, `POST /api/delete?path=P`.
//! - `POST /api/run?path=P` (`{"graph", "args", "fake"}`): runs a graph of
//!   the file as it is on disk, with this same `calyx` (`calyx run`, in the
//!   file's folder, so its `calyx.toml` and journal are the usual ones).
//!   `GET /api/run?key=K`: how it goes: running, finished, failed or
//!   waiting; its output, its trace, and each call from its journal (step,
//!   item, effect, time, tokens, cost, answer). `POST /api/stop?key=K`.
//!
//! The code is the source of truth: the graph is drawn from it, never the
//! other way round.
//!
//! Only this machine reaches it, and only the page it serves drives it: a
//! request whose `Host` is not 127.0.0.1 or localhost (DNS rebinding) is
//! refused, and so is a `POST` without the page's `Origin` and its header
//! `X-Calyx-Studio` (another site in the same browser). It reads and writes
//! only `.clyx` files inside the workspace: no `..`, no absolute paths, no
//! hidden directories, and no symbolic link that leads out of it.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use calyx_syntax::Severity;
use calyx_syntax::ast::{Decl, Expr, ExprKind, Ident, Stmt};
use serde_json::{Value, json};

const PAGE: &str = include_str!("studio.html");
const DEFAULT_PORT: u16 = 4180;

/// Programs to start from: (id, title, what it shows, text).
const TEMPLATES: &[(&str, &str, &str, &str)] = &[
    (
        "blank",
        "Empty",
        "A model and one graph that calls it.",
        "model gemini = \"gemini-3.5-flash-lite\"\n\nprompt answer(question: Text) -> Text:\n    \"\"\"\n    Answer in one sentence: {question}\n    \"\"\"\n\ngraph main(question: Text) -> Text:\n    return gemini(answer(question))\n",
    ),
    (
        "hello",
        "Typed answer",
        "A prompt whose answer is checked against a type.",
        include_str!("../../../examples/readme/hello.clyx"),
    ),
    (
        "team",
        "Team of agents",
        "Helpers at the same time around a task board (an entity).",
        include_str!("../../../examples/team.clyx"),
    ),
];

/// A run started from the page.
struct Run {
    /// The file's folder, where its journal is.
    dir: PathBuf,
    child: Mutex<std::process::Child>,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
}

static RUNS: Mutex<Vec<Arc<Run>>> = Mutex::new(Vec::new());

struct Workspace {
    /// The folder, canonical.
    root: PathBuf,
    /// The file to open first, relative to the folder.
    open: Option<String>,
    port: u16,
}

pub fn studio(args: &[String]) -> ExitCode {
    let mut target = None;
    let mut port = None;
    let mut open_browser = true;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => match it.next().and_then(|p| p.parse::<u16>().ok()) {
                Some(p) => port = Some(p),
                None => return usage("--port takes a number"),
            },
            "--no-open" => open_browser = false,
            f if !f.starts_with('-') && target.is_none() => target = Some(PathBuf::from(f)),
            other => return usage(&format!("unknown option `{other}`")),
        }
    }
    let target = target.unwrap_or_else(|| PathBuf::from("."));
    // A file opens its folder, with it open; a folder opens as it is.
    let (folder, open) = if target.is_dir() {
        (target.clone(), None)
    } else if target.extension().is_some_and(|e| e == "clyx") {
        if !target.is_file() {
            eprintln!("calyx: studio: no file {}", target.display());
            return ExitCode::from(2);
        }
        let folder = match target.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned());
        (folder, name)
    } else {
        return usage(&format!(
            "{} is not a folder nor a .clyx file",
            target.display()
        ));
    };
    let root = match folder.canonicalize() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("calyx: studio: {}: {e}", folder.display());
            return ExitCode::from(2);
        }
    };
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
    let url = format!("http://127.0.0.1:{port}/");
    // One write, and never a panic if nobody reads it any more.
    let _ = write!(
        std::io::stderr(),
        "calyx: studio for {} at {url}\ncalyx: Ctrl+C stops it\n",
        root.display()
    );
    if open_browser {
        crate::open_browser(&url);
    }
    let ws: &'static Workspace = Box::leak(Box::new(Workspace { root, open, port }));
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || handle(stream, ws));
    }
    ExitCode::SUCCESS
}

fn usage(why: &str) -> ExitCode {
    eprintln!(
        "calyx: studio: {why}\nusage: calyx studio [folder | file.clyx] [--port N] [--no-open]"
    );
    ExitCode::from(2)
}

/// `%xx` and `+` decoded.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn param(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| decode(v))
}

/// The file `rel` names in the workspace, if it may be read or written
/// there: a relative path to a `.clyx` file, with no `..`, no hidden part,
/// and whose folder, links followed, is inside the workspace. With
/// `create`, missing folders on the way are made.
fn resolve(ws: &Workspace, rel: &str, create: bool) -> Result<PathBuf, String> {
    let bad = |why: &str| Err(format!("`{rel}`: {why}"));
    if rel.is_empty()
        || rel.len() > 200
        || rel.chars().any(|c| c.is_control() || c == '\\' || c == ':')
    {
        return bad("not a file name");
    }
    let p = Path::new(rel);
    if p.extension().is_none_or(|e| e != "clyx") {
        return bad("a program's name ends in .clyx");
    }
    for c in p.components() {
        match c {
            Component::Normal(n) if !n.to_string_lossy().starts_with('.') => {}
            _ => return bad("only names inside the workspace, without `..` or hidden folders"),
        }
    }
    let full = ws.root.join(p);
    let parent = full.parent().unwrap_or(&ws.root);
    if create && !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|e| format!("`{rel}`: {e}"))?;
    }
    let real = parent.canonicalize().map_err(|e| format!("`{rel}`: {e}"))?;
    if !real.starts_with(&ws.root) {
        return bad("outside the workspace");
    }
    if std::fs::symlink_metadata(&full).is_ok_and(|m| m.file_type().is_symlink()) {
        return bad("a symbolic link");
    }
    Ok(real.join(full.file_name().unwrap_or_default()))
}

/// The workspace's `.clyx` files, relative and sorted; not in hidden
/// folders, `target` or `node_modules`, and not through links.
fn list(ws: &Workspace) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ty) = e.file_type() else { continue };
            if name.starts_with('.') || out.len() >= 500 {
                continue;
            }
            if ty.is_dir() && depth < 6 && name != "target" && name != "node_modules" {
                walk(root, &e.path(), depth + 1, out);
            } else if ty.is_file()
                && name.ends_with(".clyx")
                && let Ok(rel) = e.path().strip_prefix(root)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(&ws.root, &ws.root, 0, &mut out);
    out.sort();
    out
}

fn handle(mut stream: TcpStream, ws: &Workspace) {
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
    let port = ws.port;
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
    let rel = param(query, "path").unwrap_or_default();
    let fail = |stream: &mut TcpStream, status: u16, why: String| {
        respond(stream, status, "text/plain", &why)
    };
    match (req.method.as_str(), path) {
        ("GET", "/") => respond(&mut stream, 200, "text/html; charset=utf-8", PAGE),
        ("GET", "/api/workspace") => {
            let body = json!({
                "name": ws.root.file_name().map_or_else(|| ws.root.display().to_string(), |n| n.to_string_lossy().into_owned()),
                "dir": ws.root.display().to_string(),
                "files": list(ws),
                "open": ws.open,
                "templates": TEMPLATES.iter().map(|(id, title, about, text)| json!({
                    "id": id, "title": title, "about": about, "text": text,
                })).collect::<Vec<_>>(),
            });
            respond(&mut stream, 200, "application/json", &body.to_string());
        }
        ("GET", "/api/program") => {
            let file = match resolve(ws, &rel, false) {
                Ok(f) => f,
                Err(e) => return fail(&mut stream, 400, e),
            };
            match std::fs::read_to_string(&file) {
                Ok(text) => {
                    let since = param(query, "since").unwrap_or_default();
                    if !since.is_empty() && since == hash(&text) {
                        respond(&mut stream, 204, "text/plain", "")
                    } else {
                        let a = analyze(&rel, &text);
                        respond(&mut stream, 200, "application/json", &a.to_string())
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => fail(
                    &mut stream,
                    404,
                    format!("`{rel}` is not in the workspace any more"),
                ),
                Err(e) => fail(&mut stream, 500, format!("`{rel}`: {e}")),
            }
        }
        ("POST", "/api/check" | "/api/save") => {
            let Ok(text) = String::from_utf8(req.body) else {
                return fail(&mut stream, 400, "the text is not UTF-8".into());
            };
            if path == "/api/save" {
                let file = match resolve(ws, &rel, true) {
                    Ok(f) => f,
                    Err(e) => return fail(&mut stream, 400, e),
                };
                if param(query, "new").is_some() {
                    // Only a new file: never over one already there.
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&file)
                    {
                        Ok(mut f) => {
                            if let Err(e) = f.write_all(text.as_bytes()) {
                                return fail(&mut stream, 500, format!("`{rel}`: {e}"));
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                            return fail(&mut stream, 409, format!("`{rel}` already exists"));
                        }
                        Err(e) => return fail(&mut stream, 500, format!("`{rel}`: {e}")),
                    }
                } else {
                    let tmp = file.with_extension("clyx.studio-tmp");
                    if let Err(e) =
                        std::fs::write(&tmp, &text).and_then(|()| std::fs::rename(&tmp, &file))
                    {
                        let _ = std::fs::remove_file(&tmp);
                        return fail(&mut stream, 500, format!("`{rel}`: {e}"));
                    }
                }
            }
            let a = analyze(&rel, &text);
            respond(&mut stream, 200, "application/json", &a.to_string());
        }
        ("POST", "/api/rename") => {
            let to = param(query, "to").unwrap_or_default();
            let (from_file, to_file) = match (resolve(ws, &rel, false), resolve(ws, &to, true)) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(e), _) | (_, Err(e)) => return fail(&mut stream, 400, e),
            };
            if !from_file.is_file() {
                return fail(
                    &mut stream,
                    404,
                    format!("`{rel}` is not in the workspace any more"),
                );
            }
            if to_file.exists() {
                return fail(&mut stream, 409, format!("`{to}` already exists"));
            }
            match std::fs::rename(&from_file, &to_file) {
                Ok(()) => respond(
                    &mut stream,
                    200,
                    "application/json",
                    &json!({"path": to}).to_string(),
                ),
                Err(e) => fail(&mut stream, 500, format!("`{rel}`: {e}")),
            }
        }
        ("POST", "/api/delete") => {
            let file = match resolve(ws, &rel, false) {
                Ok(f) => f,
                Err(e) => return fail(&mut stream, 400, e),
            };
            match std::fs::remove_file(&file) {
                Ok(()) => respond(&mut stream, 200, "application/json", "{}"),
                Err(e) => fail(&mut stream, 500, format!("`{rel}`: {e}")),
            }
        }
        ("POST", "/api/run") => {
            let file = match resolve(ws, &rel, false) {
                Ok(f) => f,
                Err(e) => return fail(&mut stream, 400, e),
            };
            let Ok(req) = serde_json::from_slice::<Value>(&req.body) else {
                return fail(&mut stream, 400, "expected {graph, args, fake}".into());
            };
            match start_run(&file, &req) {
                Ok(key) => respond(
                    &mut stream,
                    200,
                    "application/json",
                    &json!({"key": key}).to_string(),
                ),
                Err(e) => fail(&mut stream, 400, e),
            }
        }
        ("GET", "/api/run") => {
            let run = param(query, "key")
                .and_then(|k| k.parse::<usize>().ok())
                .and_then(|k| RUNS.lock().ok()?.get(k).cloned());
            match run {
                Some(r) => respond(
                    &mut stream,
                    200,
                    "application/json",
                    &run_state(&r).to_string(),
                ),
                None => fail(&mut stream, 404, "no such run".into()),
            }
        }
        ("POST", "/api/stop") => {
            let run = param(query, "key")
                .and_then(|k| k.parse::<usize>().ok())
                .and_then(|k| RUNS.lock().ok()?.get(k).cloned());
            match run {
                Some(r) => {
                    if let Ok(mut c) = r.child.lock()
                        && c.try_wait().ok().flatten().is_none()
                    {
                        // The run and every process it started.
                        #[cfg(unix)]
                        let _ = std::process::Command::new("kill")
                            .args(["-KILL", "--", &format!("-{}", c.id())])
                            .status();
                        let _ = c.kill();
                    }
                    respond(&mut stream, 200, "application/json", "{}")
                }
                None => fail(&mut stream, 404, "no such run".into()),
            }
        }
        _ => respond(&mut stream, 404, "text/plain", "not found"),
    }
}

/// Starts `calyx run` on `file` (`{"graph", "args", "fake"}`): its key.
fn start_run(file: &Path, req: &Value) -> Result<usize, String> {
    let exe = std::env::current_exe().map_err(|e| format!("calyx: {e}"))?;
    let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run")
        .arg(file.file_name().unwrap_or_default())
        .current_dir(&dir);
    if let Some(g) = req["graph"].as_str() {
        cmd.args(["--graph", g]);
    }
    if req["fake"].as_bool().unwrap_or(true) {
        cmd.arg("--fake-models");
    }
    if let Some(args) = req["args"].as_object() {
        for (name, value) in args {
            if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return Err(format!("`{name}` is not a parameter"));
            }
            // Texts as they are; anything else as JSON.
            let v = match value {
                Value::String(t) => t.clone(),
                other => other.to_string(),
            };
            cmd.arg(format!("--{name}")).arg(v);
        }
    }
    // Its own process group, so Stop also ends the tools it started.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start calyx run: {e}"))?;
    // Read both outputs as they come, so the page sees the trace live.
    let collect = |from: Box<dyn std::io::Read + Send>| {
        let buf = Arc::new(Mutex::new(String::new()));
        let into = Arc::clone(&buf);
        std::thread::spawn(move || {
            let mut r = std::io::BufReader::new(from);
            let mut chunk = [0u8; 4096];
            while let Ok(n) = std::io::Read::read(&mut r, &mut chunk) {
                if n == 0 {
                    break;
                }
                if let Ok(mut b) = into.lock()
                    && b.len() < 4 << 20
                {
                    b.push_str(&String::from_utf8_lossy(&chunk[..n]));
                }
            }
        });
        buf
    };
    let stdout = collect(Box::new(child.stdout.take().expect("piped")));
    let stderr = collect(Box::new(child.stderr.take().expect("piped")));
    let run = Arc::new(Run {
        dir,
        child: Mutex::new(child),
        stdout,
        stderr,
    });
    let mut runs = RUNS.lock().map_err(|_| "studio: runs".to_owned())?;
    runs.push(run);
    Ok(runs.len() - 1)
}

/// A run as the page shows it.
fn run_state(r: &Run) -> Value {
    let exit = r
        .child
        .lock()
        .ok()
        .and_then(|mut c| c.try_wait().ok().flatten());
    let stderr = r.stderr.lock().map(|s| s.clone()).unwrap_or_default();
    let stdout = r.stdout.lock().map(|s| s.clone()).unwrap_or_default();
    let id = stderr
        .lines()
        .find_map(|l| l.strip_prefix("calyx: run "))
        .map(|s| s.trim().to_owned());
    let status = match exit.map(|e| e.code()) {
        None => "running",
        Some(Some(0)) => "finished",
        Some(Some(4)) => "waiting",
        Some(None) => "stopped",
        Some(Some(_)) => "failed",
    };
    // The step that failed, from `... node `x`: ...`.
    let failed_step = stderr
        .lines()
        .find(|l| l.contains("execution failed"))
        .and_then(|l| l.split("node `").nth(1))
        .and_then(|r| r.split('`').next())
        .map(|n| n.split(['[', '#']).next().unwrap_or(n).to_owned());
    let mut calls = Vec::new();
    if let Some(id) = &id
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && let Ok(text) =
            std::fs::read_to_string(r.dir.join(".calyx/runs").join(id).join("journal.jsonl"))
    {
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if v["type"] == "call" {
                calls.push(call_summary(&v));
            }
        }
    }
    let short = |s: &str| {
        if s.len() > 200_000 {
            let mut end = 200_000;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}\n…", &s[..end])
        } else {
            s.to_owned()
        }
    };
    json!({
        "status": status,
        "id": id,
        "exit": exit.and_then(|e| e.code()),
        "output": short(&stdout),
        "trace": short(&stderr),
        "failed_step": failed_step,
        "calls": calls,
    })
}

/// One journaled call: its step (`graph/step[item]#n`), what it did and
/// gave, its answer cut to a size a page can show.
fn call_summary(v: &Value) -> Value {
    let key = v["key"].as_str().unwrap_or("");
    let path = key.split_once('/').map_or(key, |(_, rest)| rest);
    let step = path.split(['[', '#', '/']).next().unwrap_or(path);
    let item = path
        .split_once('[')
        .and_then(|(_, r)| r.split(']').next())
        .map(str::to_owned);
    let ok = &v["ok"];
    let answer = if ok.is_null() {
        Value::Null
    } else if !ok["json"].is_null() {
        ok["json"].clone()
    } else if ok["text"].is_string() {
        ok["text"].clone()
    } else if ok["value"].is_null() {
        ok.clone()
    } else {
        ok["value"].clone()
    };
    let mut answer = answer.to_string();
    if answer.len() > 4000 {
        let mut end = 4000;
        while !answer.is_char_boundary(end) {
            end -= 1;
        }
        answer.truncate(end);
        answer.push('…');
    }
    json!({
        "key": key,
        "step": step,
        "item": item,
        "effect": v["effect"],
        "ms": ok["ms"],
        "input_tokens": ok["input_tokens"],
        "output_tokens": ok["output_tokens"],
        "cost_usd": ok["cost_usd"],
        "answer": if ok.is_null() { Value::Null } else { Value::String(answer) },
        "error": v.get("err").cloned().unwrap_or(Value::Null),
    })
}

fn respond(stream: &mut TcpStream, status: u16, kind: &str, body: &str) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        409 => "Conflict",
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
                    let (name, at, value, each) = match s {
                        Stmt::Node {
                            name,
                            value,
                            fan_out,
                        } => (name.name.clone(), name.span.start, value, fan_out.is_some()),
                        Stmt::Return(e) => ("return".to_owned(), e.span.start, e, false),
                        _ => return None,
                    };
                    // A `for each` of steps: its steps are listed, not its
                    // first line.
                    let first = if each && matches!(value.kind, ExprKind::Block { .. }) {
                        String::new()
                    } else {
                        code(value)
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
                        "code": first,
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
