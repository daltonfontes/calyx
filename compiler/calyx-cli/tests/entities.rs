//! Entities through the CLI (milestone M6): state kept between runs, one
//! owner per key across processes, and each message applied exactly once
//! (decisions D15, D21).

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

const PROGRAM: &str = r#"
model m = "fake-model"

entity Counter(key name: Text):
    state count: Nat = 0
    state notes: List[Text] = []

    on Get() -> Nat:
        return count

    on Add(n: Nat, note: Text):
        next count = count + n
        next notes = notes + [note]

prompt say(n: Nat) -> Text:
    """{n}"""

graph bump(name: Text, note: Text) -> Text:
    before = ask Counter(name).Get()
    send Counter(name).Add(1, note)
    return m(say(before))

graph read(name: Text) -> Nat:
    return ask Counter(name).Get()
"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir =
            std::env::temp_dir().join(format!("calyx-entities-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.clyx"), PROGRAM).unwrap();
        Dir(dir)
    }

    fn calyx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }

    fn bump(&self, name: &str, note: &str) -> Output {
        self.calyx(&[
            "run",
            "p.clyx",
            "--graph",
            "bump",
            "--fake-models",
            "--quiet",
            "--name",
            name,
            "--note",
            note,
        ])
    }

    fn count(&self, name: &str) -> String {
        let out = self.calyx(&[
            "run",
            "p.clyx",
            "--graph",
            "read",
            "--quiet",
            "--no-journal",
            "--name",
            name,
        ]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        text(&out.stdout).trim().to_owned()
    }

    fn state(&self) -> Value {
        let dir = self.0.join(".calyx/entities/Counter");
        let entry = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        serde_json::from_str(&std::fs::read_to_string(entry.join("entity.json")).unwrap()).unwrap()
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
fn state_lives_between_runs_one_per_key() {
    let d = Dir::new("between");
    assert!(d.bump("a", "um").status.success());
    assert!(d.bump("a", "dois").status.success());
    assert!(d.bump("b", "outro").status.success());
    assert_eq!(d.count("a"), "2");
    assert_eq!(d.count("b"), "1");
    assert_eq!(
        d.count("c"),
        "0",
        "a new key starts from the initial values"
    );
}

#[test]
fn concurrent_runs_lose_no_update() {
    let d = Dir::new("concurrent");
    // Eight processes started together, all sending to the same key.
    let children: Vec<_> = (0..8)
        .map(|i| {
            Command::new(env!("CARGO_BIN_EXE_calyx"))
                .args([
                    "run",
                    "p.clyx",
                    "--graph",
                    "bump",
                    "--fake-models",
                    "--quiet",
                    "--name",
                    "x",
                    "--note",
                    &format!("n{i}"),
                ])
                .current_dir(&d.0)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
    assert_eq!(d.count("x"), "8");
    assert_eq!(d.state()["state"]["notes"].as_array().unwrap().len(), 8);
}

#[test]
fn a_resumed_run_does_not_apply_its_message_twice() {
    let d = Dir::new("once");
    assert!(d.bump("a", "um").status.success());
    let runs = d.0.join(".calyx/runs");
    let id = std::fs::read_dir(&runs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    let journal = runs.join(&id).join("journal.jsonl");
    // As if the run died after the entity applied the message and before
    // the journal recorded it: the resumed run sends it again.
    let text_j = std::fs::read_to_string(&journal).unwrap();
    let kept: Vec<&str> = text_j
        .lines()
        .filter(|l| !(l.contains("\"key\":\"bump/send_0#0\"") || l.contains("\"type\":\"end\"")))
        .collect();
    assert!(text_j.contains("\"key\":\"bump/send_0#0\""), "{text_j}");
    std::fs::write(&journal, kept.join("\n") + "\n").unwrap();

    let out = d.calyx(&["resume", id.to_str().unwrap(), "--fake-models"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("already applied"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(d.count("a"), "1");
}

#[test]
fn replay_takes_entity_answers_from_the_journal() {
    let d = Dir::new("replay");
    assert!(d.bump("a", "um").status.success());
    let id = std::fs::read_dir(d.0.join(".calyx/runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    // The entity changes after the run; the replay still sees what the run saw.
    assert!(d.bump("a", "dois").status.success());
    let out = d.calyx(&["replay", id.to_str().unwrap(), "--quiet"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(d.count("a"), "2", "replay sends nothing");
}
