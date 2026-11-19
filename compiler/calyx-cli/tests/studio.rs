//! `calyx studio`: the page, the program as a graph, live checks, saving,
//! and that only its own page on this machine can drive it.

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
    port: u16,
    dir: PathBuf,
}

impl Studio {
    fn start(name: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("calyx-studio-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.clyx"), PROGRAM).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(["studio", "p.clyx", "--port", "0"])
            .current_dir(&dir)
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
        Studio { child, port, dir }
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
        s.read_to_string(&mut out).unwrap();
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
fn serves_the_page_and_the_program_as_a_graph() {
    let s = Studio::start("graph");
    let (status, page) = s.get("/");
    assert_eq!(status, 200);
    assert!(page.contains("<title>Calyx Studio</title>"));
    assert!(
        !page.contains("https://"),
        "the page loads nothing from the network"
    );

    let (status, body) = s.get("/api/program");
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
    assert_eq!(s.get(&format!("/api/program?since={hash}")).0, 204);
    std::fs::write(s.dir.join("p.clyx"), PROGRAM.replace("max 3", "max 4")).unwrap();
    let (status, body) = s.get(&format!("/api/program?since={hash}"));
    assert_eq!(status, 200, "the file changed on disk");
    assert_eq!(
        json(&body)["graphs"][0]["nodes"][0]["type"],
        "List[Text] max 4"
    );
}

#[test]
fn checks_text_as_typed_and_saves_only_its_file() {
    let s = Studio::start("check");
    let broken = PROGRAM.replace("m(say(first))", "m(say(frist))");
    let (status, body) = s.post("/api/check", &broken);
    assert_eq!(status, 200);
    let p = json(&body);
    assert_eq!(p["ok"], false);
    assert_eq!(p["graphs"], serde_json::json!([]));
    let d = &p["diagnostics"][0];
    assert_eq!(d["severity"], "error");
    assert_eq!(d["start"]["line"], 12);
    assert_eq!(
        std::fs::read_to_string(s.dir.join("p.clyx")).unwrap(),
        PROGRAM,
        "a check does not write"
    );

    let fixed = PROGRAM.replace("max 3", "max 5");
    let (status, body) = s.post("/api/save", &fixed);
    assert_eq!(status, 200);
    assert_eq!(json(&body)["ok"], true);
    assert_eq!(
        std::fs::read_to_string(s.dir.join("p.clyx")).unwrap(),
        fixed
    );
    let names: Vec<String> = std::fs::read_dir(&s.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["p.clyx"], "no file left beside it");
}

#[test]
fn only_its_own_page_on_this_machine_drives_it() {
    let s = Studio::start("guard");
    // DNS rebinding: a name that resolves here, but is not this machine's.
    let (status, _) = s.request(
        "GET",
        "/api/program",
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
        let (status, _) = s.request(
            "POST",
            "/api/save",
            &headers,
            "graph x() -> Text:\n    return \"pwned\"\n",
        );
        assert_eq!(status, 403, "{headers:?}");
    }
    assert_eq!(
        std::fs::read_to_string(s.dir.join("p.clyx")).unwrap(),
        PROGRAM
    );
    assert_eq!(s.get("/../p.clyx").0, 404);
}
