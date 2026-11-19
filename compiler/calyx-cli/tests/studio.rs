//! `calyx studio`: a workspace folder in the browser. The page, each
//! program as a graph, live checks, creating, saving, renaming and
//! deleting programs, and that only its own page on this machine can drive
//! it, and only on the `.clyx` files inside the folder.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const PROGRAM: &str = r#"model m = "fake-model"

prompt plan(goal: Text) -> List[Text] max 3:
    """Split: {goal}"""

prompt say(x: Text) -> Text:
    """{x}"""

graph main(goal: Text) -> List[Text]:
    parts = m(plan(goal))
    answers = for each p in parts:
        first = m(say(p))
        m(say(first))
    return answers
"#;

struct Studio {
    child: Child,
    /// Kept open: the studio's stderr.
    _err: BufReader<std::process::ChildStderr>,
    port: u16,
    dir: PathBuf,
}

impl Studio {
    /// The studio on a new folder with `p.clyx`, started with `args`.
    fn start(name: &str, args: &[&str]) -> Studio {
        let dir = std::env::temp_dir().join(format!("calyx-studio-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ws/sub")).unwrap();
        std::fs::write(dir.join("ws/p.clyx"), PROGRAM).unwrap();
        std::fs::write(dir.join("ws/sub/q.clyx"), PROGRAM).unwrap();
        std::fs::write(dir.join("secret.clyx"), "outside").unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(["studio"])
            .args(args)
            .args(["--port", "0", "--no-open"])
            .current_dir(dir.join("ws"))
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut err = BufReader::new(child.stderr.take().unwrap());
        let mut line = String::new();
        err.read_line(&mut line).unwrap();
        let port = line
            .split("http://127.0.0.1:")
            .nth(1)
            .and_then(|r| r.trim_end().trim_end_matches('/').parse().ok())
            .unwrap_or_else(|| panic!("no address in {line:?}"));
        Studio {
            child,
            _err: err,
            port,
            dir,
        }
    }

    fn ws(&self, rel: &str) -> PathBuf {
        self.dir.join("ws").join(rel)
    }

    /// One request; `(status, body)`.
    fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(body);
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out)
            .unwrap_or_else(|e| panic!("{method} {path}: {e}"));
        let status = out[9..12].parse().unwrap();
        let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_owned();
        (status, body)
    }

    fn host(&self) -> (&'static str, String) {
        ("Host", format!("127.0.0.1:{}", self.port))
    }

    fn get(&self, path: &str) -> (u16, String) {
        self.request("GET", path, &[self.host()], "")
    }

    /// A POST as the studio's own page sends it.
    fn post(&self, path: &str, body: &str) -> (u16, String) {
        let origin = ("Origin", format!("http://127.0.0.1:{}", self.port));
        self.request(
            "POST",
            path,
            &[self.host(), origin, ("X-Calyx-Studio", "1".into())],
            body,
        )
    }
}

impl Drop for Studio {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
}

#[test]
fn without_a_file_it_opens_the_folder_with_its_programs_and_templates() {
    let s = Studio::start("folder", &[]);
    let (status, page) = s.get("/");
    assert_eq!(status, 200);
    assert!(page.contains("<title>Calyx Studio</title>"));
    assert!(
        !page.contains("https://"),
        "the page loads nothing from the network"
    );

    let w = json(&s.get("/api/workspace").1);
    assert_eq!(w["files"], serde_json::json!(["p.clyx", "sub/q.clyx"]));
    assert_eq!(w["open"], Value::Null);
    let templates = w["templates"].as_array().unwrap();
    assert!(templates.len() >= 2);
    // Every template checks.
    for t in templates {
        let (status, body) = s.post("/api/check?path=t.clyx", t["text"].as_str().unwrap());
        assert_eq!(status, 200);
        assert_eq!(json(&body)["ok"], true, "template {}", t["id"]);
    }
    // A file names its folder, and is opened first.
    let s = Studio::start("file", &["p.clyx"]);
    assert_eq!(json(&s.get("/api/workspace").1)["open"], "p.clyx");
}

#[test]
fn a_program_as_a_graph_and_followed_on_disk() {
    let s = Studio::start("graph", &[]);
    let (status, body) = s.get("/api/program?path=sub%2Fq.clyx");
    assert_eq!(status, 200);
    let p = json(&body);
    assert_eq!(p["ok"], true);
    assert_eq!(p["text"], PROGRAM);
    let g = &p["graphs"][0];
    assert_eq!(g["name"], "main");
    assert_eq!(g["line"], 8, "lines from 0");
    let nodes = g["nodes"].as_array().unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["parts", "answers"]);
    assert_eq!(g["output"], 1, "`return answers` is the step `answers`");
    let answers = &nodes[1];
    assert_eq!(answers["effect"], "llm");
    assert_eq!(answers["fan_out"], "p");
    assert_eq!(answers["over"], "parts");
    assert_eq!(answers["inputs"], serde_json::json!([0]));
    assert_eq!(answers["line"], 10);
    assert_eq!(answers["inner"][0]["name"], "first");
    assert_eq!(answers["inner"][0]["code"], "m(say(p))");
    assert_eq!(answers["inner"][0]["line"], 11);
    assert_eq!(nodes[0]["kind"], "model m(plan)");

    // Unchanged since that version: nothing to send.
    let hash = p["hash"].as_str().unwrap();
    assert_eq!(
        s.get(&format!("/api/program?path=sub/q.clyx&since={hash}"))
            .0,
        204
    );
    std::fs::write(s.ws("sub/q.clyx"), PROGRAM.replace("max 3", "max 4")).unwrap();
    let (status, body) = s.get(&format!("/api/program?path=sub/q.clyx&since={hash}"));
    assert_eq!(status, 200, "the file changed on disk");
    assert_eq!(
        json(&body)["graphs"][0]["nodes"][0]["type"],
        "List[Text] max 4"
    );
    assert_eq!(s.get("/api/program?path=gone.clyx").0, 404);
}

#[test]
fn checks_as_typed_creates_saves_renames_and_deletes() {
    let s = Studio::start("files", &[]);
    let broken = PROGRAM.replace("m(say(first))", "m(say(frist))");
    let (status, body) = s.post("/api/check?path=p.clyx", &broken);
    assert_eq!(status, 200);
    let p = json(&body);
    assert_eq!(p["ok"], false);
    assert_eq!(p["graphs"], serde_json::json!([]));
    assert_eq!(p["diagnostics"][0]["start"]["line"], 12);
    assert_eq!(
        std::fs::read_to_string(s.ws("p.clyx")).unwrap(),
        PROGRAM,
        "a check does not write"
    );

    // New: only where there is no file yet, folders made on the way.
    assert_eq!(
        s.post(
            "/api/save?new=1&path=agents%2Fnew.clyx",
            "graph g() -> Text:\n    return \"x\"\n"
        )
        .0,
        200
    );
    assert!(s.ws("agents/new.clyx").is_file());
    assert_eq!(s.post("/api/save?new=1&path=p.clyx", "x").0, 409);
    assert_eq!(std::fs::read_to_string(s.ws("p.clyx")).unwrap(), PROGRAM);

    // Save over it.
    let fixed = PROGRAM.replace("max 3", "max 5");
    let (status, body) = s.post("/api/save?path=p.clyx", &fixed);
    assert_eq!(status, 200);
    assert_eq!(json(&body)["ok"], true);
    assert_eq!(std::fs::read_to_string(s.ws("p.clyx")).unwrap(), fixed);

    // Rename, never over another file.
    assert_eq!(s.post("/api/rename?path=p.clyx&to=sub/q.clyx", "").0, 409);
    assert_eq!(s.post("/api/rename?path=p.clyx&to=main.clyx", "").0, 200);
    assert!(!s.ws("p.clyx").exists() && s.ws("main.clyx").is_file());

    assert_eq!(s.post("/api/delete?path=main.clyx", "").0, 200);
    assert!(!s.ws("main.clyx").exists());
    let w = json(&s.get("/api/workspace").1);
    assert_eq!(
        w["files"],
        serde_json::json!(["agents/new.clyx", "sub/q.clyx"])
    );
    let left: Vec<String> = std::fs::read_dir(s.dir.join("ws"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!left.iter().any(|n| n.contains("studio-tmp")), "{left:?}");
}

#[test]
fn only_its_own_page_on_this_machine_drives_it() {
    let s = Studio::start("guard", &[]);
    // DNS rebinding: a name that resolves here, but is not this machine's.
    let (status, _) = s.request(
        "GET",
        "/api/workspace",
        &[("Host", "evil.example".into())],
        "",
    );
    assert_eq!(status, 403);
    // Another site in the same browser: its Origin, or no header of ours.
    let host = s.host();
    for headers in [
        vec![
            host.clone(),
            ("Origin", "https://evil.example".into()),
            ("X-Calyx-Studio", "1".into()),
        ],
        vec![
            host.clone(),
            ("Origin", format!("http://127.0.0.1:{}", s.port)),
        ],
        vec![host.clone()],
    ] {
        let (status, _) = s.request("POST", "/api/save?path=p.clyx", &headers, "pwned");
        assert_eq!(status, 403, "{headers:?}");
        let (status, _) = s.request("POST", "/api/delete?path=p.clyx", &headers, "");
        assert_eq!(status, 403, "{headers:?}");
    }
    assert_eq!(std::fs::read_to_string(s.ws("p.clyx")).unwrap(), PROGRAM);
}

#[test]
fn only_clyx_files_inside_the_folder() {
    let s = Studio::start("paths", &[]);
    for bad in [
        "../secret.clyx",
        "..%2Fsecret.clyx",
        "sub/../../secret.clyx",
        "/etc/passwd",
        "%2Fetc%2Fhosts.clyx",
        ".hidden/x.clyx",
        ".git/config",
        "notes.txt",
        "calyx.toml",
        "C:%5Cx.clyx",
    ] {
        assert_eq!(
            s.get(&format!("/api/program?path={bad}")).0,
            400,
            "read {bad}"
        );
        assert_eq!(
            s.post(&format!("/api/save?path={bad}"), "x").0,
            400,
            "write {bad}"
        );
        assert_eq!(
            s.post(&format!("/api/save?new=1&path={bad}"), "x").0,
            400,
            "create {bad}"
        );
        assert_eq!(
            s.post(&format!("/api/delete?path={bad}"), "").0,
            400,
            "delete {bad}"
        );
        assert_eq!(
            s.post(&format!("/api/rename?path=p.clyx&to={bad}"), "").0,
            400,
            "rename to {bad}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(s.dir.join("secret.clyx")).unwrap(),
        "outside"
    );
    assert!(s.ws("p.clyx").is_file());

    // A link inside the folder that leads out of it.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&s.dir, s.ws("out")).unwrap();
        std::os::unix::fs::symlink(s.dir.join("secret.clyx"), s.ws("link.clyx")).unwrap();
        assert_eq!(s.get("/api/program?path=out/secret.clyx").0, 400);
        assert_eq!(s.post("/api/save?path=out/secret.clyx", "x").0, 400);
        assert_eq!(s.get("/api/program?path=link.clyx").0, 400);
        assert_eq!(s.post("/api/save?path=link.clyx", "x").0, 400);
        assert_eq!(
            std::fs::read_to_string(s.dir.join("secret.clyx")).unwrap(),
            "outside"
        );
        let w = json(&s.get("/api/workspace").1);
        assert_eq!(
            w["files"],
            serde_json::json!(["p.clyx", "sub/q.clyx"]),
            "links are not listed"
        );
    }
}

/// Runs `body` as `POST /api/run`, then waits until it is no longer running.
fn run_until_done(s: &Studio, path: &str, body: &str) -> Value {
    let (status, out) = s.post(&format!("/api/run?path={path}"), body);
    assert_eq!(status, 200, "{out}");
    let key = json(&out)["key"].as_u64().unwrap();
    for _ in 0..300 {
        let state = json(&s.get(&format!("/api/run?key={key}")).1);
        if state["status"] != "running" {
            return state;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the run did not end");
}

#[test]
fn runs_a_graph_and_shows_each_call_from_the_journal() {
    let s = Studio::start("run", &[]);
    let state = run_until_done(
        &s,
        "sub/q.clyx",
        r#"{"graph": "main", "args": {"goal": "plan a trip"}, "fake": true}"#,
    );
    assert_eq!(state["status"], "finished", "{state}");
    assert_eq!(state["exit"], 0);
    let id = state["id"].as_str().unwrap();
    assert!(
        s.ws(&format!("sub/.calyx/runs/{id}/journal.jsonl"))
            .is_file(),
        "the run is in the file's folder"
    );
    let output: Value = serde_json::from_str(state["output"].as_str().unwrap()).unwrap();
    assert!(output.is_array());
    let calls = state["calls"].as_array().unwrap();
    assert_eq!(calls[0]["step"], "parts");
    assert_eq!(calls[0]["effect"], "llm");
    assert!(calls[0]["input_tokens"].is_number());
    let items: Vec<&Value> = calls.iter().filter(|c| c["step"] == "answers").collect();
    assert_eq!(
        items.len(),
        2 * output.as_array().unwrap().len(),
        "two calls per item"
    );
    assert!(items.iter().all(|c| c["item"].is_string()));
    assert!(
        state["trace"]
            .as_str()
            .unwrap()
            .contains("running graph `main`")
    );
}

#[test]
fn a_failed_run_names_its_step_and_a_run_can_be_stopped() {
    let s = Studio::start("fail", &[]);
    std::fs::write(
        s.ws("bad.clyx"),
        "graph main(xs: List[Text]) -> Text:\n    first = xs[3]\n    return first\n",
    )
    .unwrap();
    let state = run_until_done(&s, "bad.clyx", r#"{"args": {"xs": ["a"]}}"#);
    assert_eq!(state["status"], "failed", "{state}");
    assert_eq!(state["failed_step"], "first");
    assert!(state["trace"].as_str().unwrap().contains("no item 3"));

    // A run whose tool never answers: stopped from the page.
    #[cfg(unix)]
    {
        std::fs::write(
            s.ws("calyx.toml"),
            "[tools.slow]\ncommand = [\"sleep\", \"30\"]\n",
        )
        .unwrap();
        std::fs::write(
            s.ws("hang.clyx"),
            "tool slow(x: Text) -> Text:\n    effect read\n    timeout 60 s\n\ngraph main() -> Text:\n    return slow(\"x\")\n",
        )
        .unwrap();
        let (status, out) = s.post("/api/run?path=hang.clyx", "{}");
        assert_eq!(status, 200, "{out}");
        let key = json(&out)["key"].as_u64().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(
            json(&s.get(&format!("/api/run?key={key}")).1)["status"],
            "running"
        );
        assert_eq!(s.post(&format!("/api/stop?key={key}"), "").0, 200);
        let mut stopped = false;
        for _ in 0..100 {
            if json(&s.get(&format!("/api/run?key={key}")).1)["status"] == "stopped" {
                stopped = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(stopped, "Stop ends the run");
        // And the tool's server it started.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let left = Command::new("pgrep")
            .args(["-f", "^sleep 30$"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&left.stdout).trim().is_empty(),
            "the tool's server is still running"
        );
        std::fs::remove_file(s.ws("calyx.toml")).unwrap();
    }

    // A run that waits for a message ends its process.
    std::fs::write(
        s.ws("slow.clyx"),
        "message Go:\n    ok: Bool\n\ngraph main() -> Bool:\n    go = receive Go, timeout 1 days:\n        on timeout: Go(ok = false)\n    return go.ok\n",
    )
    .unwrap();
    let state = run_until_done(&s, "slow.clyx", "{}");
    assert_eq!(state["status"], "waiting", "{state}");

    // Parameters are names, never options.
    let (status, _) = s.post(
        "/api/run?path=p.clyx",
        r#"{"args": {"config=/etc/x": "y"}}"#,
    );
    assert_eq!(status, 400);
    assert_eq!(s.post("/api/run?path=../secret.clyx", "{}").0, 400);
    assert_eq!(s.get("/api/run?key=99").0, 404);
}

/// Waits until run `key` (from `/api/run` or `/api/resume`) is no longer running.
fn wait_key(s: &Studio, key: u64) -> Value {
    for _ in 0..300 {
        let state = json(&s.get(&format!("/api/run?key={key}")).1);
        if state["status"] != "running" {
            return state;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the run did not end");
}

#[test]
fn past_runs_are_listed_reopened_given_their_message_and_resumed() {
    let s = Studio::start("history", &[]);
    std::fs::write(
        s.ws("approve.clyx"),
        r#"model m = "fake-model"

message Approval = Approved | Denied(reason: Text)

prompt propose(request: Text) -> Text:
    """Propose: {request}"""

graph approve(request: Text) -> Text:
    proposal = m(propose(request))
    approval = receive Approval about proposal, timeout 3 days:
        on timeout: Denied(reason = "late")
    return match approval:
        case Approved: "approved"
        case Denied(reason): "denied: {reason}"
"#,
    )
    .unwrap();
    // The message's type goes to the page, for its form.
    let p = json(&s.get("/api/program?path=approve.clyx").1);
    assert_eq!(p["messages"][0]["name"], "Approval");
    assert!(p["messages"][0]["schema"]["anyOf"].is_array());

    let state = run_until_done(&s, "approve.clyx", r#"{"args": {"request": "a refund"}}"#);
    assert_eq!(state["status"], "waiting", "{state}");
    let id = state["id"].as_str().unwrap().to_owned();
    assert_eq!(state["waits"][0]["message"], "Approval");
    assert_eq!(state["waits"][0]["delivered"], false);

    // Listed with what it waits for; only this file's runs.
    let _ = run_until_done(&s, "p.clyx", r#"{"args": {"goal": "x"}}"#);
    let runs = json(&s.get("/api/runs?path=approve.clyx").1);
    let runs = runs.as_array().unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["id"], id.as_str());
    assert_eq!(runs[0]["status"], "waiting");
    assert_eq!(runs[0]["graph"], "approve");
    assert_eq!(runs[0]["waits"][0]["message"], "Approval");

    // Reopened from its journal.
    let j = json(&s.get(&format!("/api/journal?path=approve.clyx&id={id}")).1);
    assert_eq!(j["status"], "waiting");
    assert_eq!(j["calls"][0]["step"], "proposal");
    assert_eq!(
        s.get(&format!("/api/journal?path=p.clyx&id={id}")).0,
        404,
        "not that file's run"
    );

    // A message that is not of its type is refused, and nothing changes.
    let (status, why) = s.post(
        &format!("/api/deliver?path=approve.clyx&id={id}&message=Approval"),
        r#"{"kind": "Maybe"}"#,
    );
    assert_eq!(status, 400, "{why}");
    // Delivered, then resumed: the proposal comes from the journal.
    let (status, why) = s.post(
        &format!("/api/deliver?path=approve.clyx&id={id}&message=Approval"),
        r#"{"kind": "Denied", "reason": "too much"}"#,
    );
    assert_eq!(status, 200, "{why}");
    let runs = json(&s.get("/api/runs?path=approve.clyx").1);
    assert_eq!(runs[0]["waits"][0]["delivered"], true);
    let (status, out) = s.post(
        &format!("/api/resume?path=approve.clyx&id={id}"),
        r#"{"fake": true}"#,
    );
    assert_eq!(status, 200, "{out}");
    let state = wait_key(&s, json(&out)["key"].as_u64().unwrap());
    assert_eq!(state["status"], "finished", "{state}");
    assert_eq!(state["id"], id.as_str());
    assert_eq!(state["output"].as_str().unwrap().trim(), "denied: too much");
    let runs = json(&s.get("/api/runs?path=approve.clyx").1);
    assert_eq!(runs[0]["status"], "finished");
    assert_eq!(runs[0]["resumes"], 1);
    let j = json(&s.get(&format!("/api/journal?path=approve.clyx&id={id}")).1);
    assert_eq!(j["output"], "denied: too much");

    // Names that are not runs or messages.
    assert_eq!(s.get("/api/journal?path=approve.clyx&id=..%2F..").0, 400);
    assert_eq!(s.post("/api/resume?path=approve.clyx&id=nope", "{}").0, 404);
    assert_eq!(
        s.post(
            &format!("/api/deliver?path=approve.clyx&id={id}&message=--as"),
            "{}"
        )
        .0,
        400
    );
}

#[test]
fn the_page_gets_what_edits_from_the_graph_need() {
    let s = Studio::start("edits", &[]);
    std::fs::write(
        s.ws("e.clyx"),
        r#"model gemini = "gemini-3.5-flash-lite"

tool lookup(q: Text, n: Nat) -> Text:
    effect read

prompt summarize(text: Text) -> Text:
    """Summarize: {text}"""

graph main(question: Text, items: List[Text]) -> Text:
    found = lookup(question, 3)
    all = for each item in items:
        one = gemini(summarize(item))
        one

    return gemini(summarize(found))
"#,
    )
    .unwrap();
    let p = json(&s.get("/api/program?path=e.clyx").1);
    assert_eq!(p["ok"], true, "{p}");
    // What the program declares, with the types of the parameters.
    assert_eq!(p["decls"]["models"], serde_json::json!(["gemini"]));
    assert_eq!(
        p["decls"]["tools"][0]["params"],
        serde_json::json!(["q", "n"])
    );
    assert_eq!(
        p["decls"]["tools"][0]["types"],
        serde_json::json!(["Text", "Nat"])
    );
    assert_eq!(
        p["decls"]["prompts"][0]["types"],
        serde_json::json!(["Text"])
    );
    // Each step as written, with its first and last lines, `return` too.
    let g = &p["graphs"][0];
    assert_eq!(
        (g["line"].as_u64(), g["end_line"].as_u64()),
        (Some(8), Some(14))
    );
    let steps: Vec<(String, u64, u64)> = g["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_owned(),
                s["line"].as_u64().unwrap(),
                s["end_line"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(steps[0], ("found".into(), 9, 9));
    assert_eq!((steps[1].0.as_str(), steps[1].1), ("all", 10));
    assert!(
        (12..=13).contains(&steps[1].2),
        "the for each ends after its body: {steps:?}"
    );
    assert_eq!(steps[2], ("return".into(), 14, 14));
}
