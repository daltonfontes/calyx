//! `compact with p at n tokens` in an agent: the conversation is summed up
//! when it grows past n, each summary is a call of its own in the journal,
//! and a run that crashes anywhere resumes to the same answer, with no call
//! made twice.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROGRAM: &str = r#"
model m = "fake-model"

tool web_search(query: Text) -> Text:
    effect read
    max_output 2000 tokens
    repeatable

prompt investigate(q: Text) -> Text:
    """
    Answer: {q}. Search as much as you need.
    """

prompt summarize(conversation: Text) -> Text:
    """
    Sum up what was found so far:
    {conversation}
    """

graph ask(q: Text) -> Text:
    answer = agent m:
        tools [web_search]
        max_turns 4
        task investigate(q)
        compact with summarize at 50 tokens
        on turn_limit: final_answer
        on stuck: final_answer
    return answer
"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("calyx-compact-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let search = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/tools/fake_search.py")
            .canonicalize()
            .unwrap();
        std::fs::write(
            dir.join("calyx.toml"),
            format!(
                "[tools.web_search]\ncommand = [\"python3\", '{}']\n",
                search.display()
            ),
        )
        .unwrap();
        std::fs::write(dir.join("p.clyx"), PROGRAM).unwrap();
        Dir(dir)
    }

    fn calyx(&self, args: &[&str], crash_after: Option<u32>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_calyx"));
        cmd.args(args).current_dir(&self.0);
        match crash_after {
            Some(n) => cmd.env("CALYX_CRASH_AFTER", n.to_string()),
            None => cmd.env_remove("CALYX_CRASH_AFTER"),
        };
        cmd.output().unwrap()
    }

    fn run(&self, crash_after: Option<u32>) -> Output {
        self.calyx(
            &["run", "p.clyx", "--fake-models", "--q", "baterias"],
            crash_after,
        )
    }

    fn only_run(&self) -> String {
        let runs: Vec<_> = std::fs::read_dir(self.0.join(".calyx/runs"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(runs.len(), 1, "{runs:?}");
        runs[0].clone()
    }

    /// The keys of the calls in the journal.
    fn keys(&self, id: &str) -> Vec<String> {
        std::fs::read_to_string(self.0.join(".calyx/runs").join(id).join("journal.jsonl"))
            .unwrap()
            .lines()
            .filter(|l| l.contains("\"type\":\"call\""))
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                v["key"].as_str().unwrap().to_owned()
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
fn a_long_conversation_is_summed_up_and_resumes_the_same() {
    let full = Dir::new("full");
    // What the runtime is given: the prompt and the size.
    let ir = full.calyx(&["check", "--ir-json", "p.clyx"], None);
    assert!(
        text(&ir.stdout).contains(r#""compact":{"prompt":1,"at":50}"#),
        "{}",
        text(&ir.stdout)
    );
    let o = full.run(None);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let keys = full.keys(&full.only_run());
    // Turns 1 to 3, and the final answer at the turn limit, start over 50
    // tokens: each is summed up first.
    let compacts: Vec<&String> = keys.iter().filter(|k| k.ends_with(".compact")).collect();
    assert_eq!(compacts.len(), 4, "{keys:?}");
    assert!(
        text(&o.stderr).contains("(summarize)  compact"),
        "{}",
        text(&o.stderr)
    );

    // A crash after each call, then a resume: the same answer, and every
    // call in the journal once.
    for crash in 1..keys.len() as u32 {
        let d = Dir::new(&format!("crash{crash}"));
        let crashed = d.run(Some(crash));
        assert_eq!(
            crashed.status.code(),
            Some(137),
            "{}",
            text(&crashed.stderr)
        );
        let id = d.only_run();
        let resumed = d.calyx(&["resume", &id, "--fake-models"], None);
        assert!(
            resumed.status.success(),
            "crash {crash}: {}",
            text(&resumed.stderr)
        );
        assert_eq!(text(&resumed.stdout), text(&o.stdout), "crash {crash}");
        let mut got = d.keys(&id);
        let mut want = keys.clone();
        got.sort();
        want.sort();
        assert_eq!(got, want, "crash {crash}");
    }
}
