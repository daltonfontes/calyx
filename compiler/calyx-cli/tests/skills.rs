//! Skills (progressive disclosure): an agent sees each skill's description
//! in a built-in tool, `load_skill`, and gets the skill's text when it
//! calls it.
//!
//! The model is `fake-obedient`, which does what the texts it reads say
//! (`CALL tool {args}`): the task says to load the skill, and only the
//! skill's text says to search "refund rules". So a search for "refund
//! rules" shows that the text reached the model.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROGRAM: &str = r#"
model m = "fake-obedient"

tool web_search(query: Text) -> Text:
    effect read
    max_output 2000 tokens

skill refunds:
    description "How to handle a refund request."
    """
    Check the rules first: CALL web_search {"query": "refund rules"}
    """

skill shipping:
    description "How to answer where an order is."
    """
    Never promise a date the carrier did not give.
    """

prompt help(msg: Text) -> Text:
    """
    {msg}
    """

graph support(msg: Text) -> Text:
    answer = agent m:
        tools [web_search]
        skills [refunds, shipping]
        max_turns 5
        task help(msg)
        on turn_limit: final_answer
        on stuck: final_answer
    return answer
"#;

const LOAD: &str = r#"I want a refund. CALL load_skill {"name": "refunds"}"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("calyx-skills-{name}-{}", std::process::id()));
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

    fn run(&self, msg: &str, crash_after: Option<u32>) -> Output {
        self.calyx(
            &["run", "p.clyx", "--fake-models", "--msg", msg],
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

    fn journal(&self, id: &str) -> String {
        std::fs::read_to_string(self.0.join(".calyx/runs").join(id).join("journal.jsonl")).unwrap()
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
fn an_agent_loads_a_skill_and_follows_it() {
    let d = Dir::new("load");
    let o = d.run(LOAD, None);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let err = text(&o.stderr);
    assert!(err.contains("skill refunds"), "{err}");
    // The search only the skill's text asks for.
    assert!(d.journal(&d.only_run()).contains("refund rules"), "{err}");

    // The model sees the skills' names and descriptions, not their texts.
    let ir = d.calyx(&["check", "--ir-json", "p.clyx"], None);
    let ir: serde_json::Value = serde_json::from_slice(&ir.stdout).unwrap();
    assert_eq!(ir["skills"][1]["name"], "shipping");
    assert_eq!(
        ir["skills"][0]["text"],
        r#"Check the rules first: CALL web_search {"query": "refund rules"}"#
    );
}

#[test]
fn a_skill_that_does_not_exist_is_an_observation_not_a_failure() {
    let d = Dir::new("unknown");
    let o = d.run(r#"CALL load_skill {"name": "taxes"}"#, None);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(
        text(&o.stderr).contains("skill taxes: no such skill"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn a_run_that_crashes_after_loading_a_skill_resumes_the_same() {
    let full = Dir::new("full");
    let o = full.run(LOAD, None);
    assert!(o.status.success(), "{}", text(&o.stderr));
    for crash in 1..=3 {
        let d = Dir::new(&format!("crash{crash}"));
        let crashed = d.run(LOAD, Some(crash));
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
        assert_eq!(
            d.journal(&id).matches("\"type\":\"call\"").count(),
            full.journal(&full.only_run())
                .matches("\"type\":\"call\"")
                .count(),
            "crash {crash}"
        );
    }
}
