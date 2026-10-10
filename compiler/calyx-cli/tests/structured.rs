//! Tools that return a type other than `Text`: the server's answer is kept
//! to the fields the type declares, checked, and only then cut at
//! `max_output`; the journal keeps that value, not the server's answer.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const PROGRAM: &str = r#"
type Release:
    tag_name: Text
    body: Text

type Pull:
    number: Nat
    title: Text

tool release(query: Text) -> Release:
    effect read
    max_output 3000 tokens

tool pulls(query: Text) -> List[Pull]:
    effect read
    max_output 100 tokens

graph latest(query: Text) -> Release:
    return release(query)

graph recent(query: Text) -> List[Pull]:
    return pulls(query)
"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir =
            std::env::temp_dir().join(format!("calyx-structured-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.clyx"), PROGRAM).unwrap();
        let search = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/tools/fake_search.py")
            .canonicalize()
            .unwrap();
        let toml: String = ["release", "pulls"]
            .iter()
            .map(|t| {
                format!(
                    "[tools.{t}]\ncommand = [\"python3\", \"{}\"]\nname = \"web_search\"\n",
                    search.display()
                )
            })
            .collect();
        std::fs::write(dir.join("calyx.toml"), toml).unwrap();
        Dir(dir)
    }

    fn calyx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(args)
            .current_dir(&self.0)
            .env_remove("CALYX_DATABASE_URL")
            .output()
            .unwrap()
    }

    /// The `call` entries of the only run's journal, with their blobs read.
    fn calls(&self) -> Vec<Value> {
        let runs = self.0.join(".calyx/runs");
        let run = std::fs::read_dir(&runs)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::read_to_string(run.join("journal.jsonl"))
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|l| l["type"] == "call")
            .map(|mut l| {
                if let Some(h) = l["blob"].as_str() {
                    let blob = std::fs::read_to_string(run.join("blobs").join(h)).unwrap();
                    l["ok"] = serde_json::from_str(&blob).unwrap();
                }
                l
            })
            .collect()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn only_the_declared_fields_are_kept_and_recorded() {
    let d = Dir::new("fields");
    // The server's answer is ~9 KB: 60 assets the type does not declare.
    let out = d.calyx(&[
        "run",
        "p.clyx",
        "--graph",
        "latest",
        "--query",
        "__release__",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"tag_name": "v9.9.9", "body": "notas da versão"})
    );
    assert!(
        !text(&out.stderr).contains("cut at max_output"),
        "{}",
        text(&out.stderr)
    );
    let calls = d.calls();
    assert_eq!(calls[0]["ok"]["json"], v, "the journal keeps the value");
    assert!(calls[0]["ok"]["text"].is_null(), "not the server's answer");
    assert!(
        calls[0].get("blob").is_none(),
        "small enough to stay in the line"
    );

    let id = std::fs::read_dir(d.0.join(".calyx/runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    let again = d.calyx(&["replay", id.to_str().unwrap(), "--quiet"]);
    assert_eq!(serde_json::from_slice::<Value>(&again.stdout).unwrap(), v);
}

#[test]
fn max_output_cuts_what_is_left_not_the_json() {
    let d = Dir::new("cut");
    // 50 pulls of ~600 bytes each; kept to `number` and `title`, ~30 bytes
    // each, and cut at 100 tokens (400 bytes): whole items, valid JSON.
    let out = d.calyx(&["run", "p.clyx", "--graph", "recent", "--query", "__prs__"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("cut at max_output"),
        "{}",
        text(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let pulls = v.as_array().unwrap();
    assert!(pulls.len() > 5 && pulls.len() < 50, "{}", pulls.len());
    assert_eq!(pulls[0], serde_json::json!({"number": 1, "title": "PR 1"}));
    assert!(serde_json::to_string(&v).unwrap().len() <= 400);
    assert_eq!(d.calls()[0]["ok"]["truncated"], true);
}

#[test]
fn an_answer_without_a_declared_field_is_a_decode_failure() {
    let d = Dir::new("missing");
    let out = d.calyx(&[
        "run",
        "p.clyx",
        "--graph",
        "latest",
        "--query",
        "__nobody__",
    ]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("not JSON of its declared type"),
        "{}",
        text(&out.stderr)
    );
}
