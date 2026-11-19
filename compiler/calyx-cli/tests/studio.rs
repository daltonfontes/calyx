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
