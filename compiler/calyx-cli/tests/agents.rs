//! Agents through the CLI: a run killed in the middle of an agent's turns
//! resumes where the journal left it, and gives the same answer as a run
//! that was never killed.

use std::path::PathBuf;
use std::process::{Command, Output};

const PROGRAM: &str = r#"
model busy = "fake-busy"

tool web_search(query: Text) -> Text:
    effect read
    max_output 4000 tokens

prompt investigate(question: Text) -> Text:
    """Responda: {question}"""

graph ask(question: Text) -> Text:
    answer = agent busy:
        tools [web_search]
        max_turns 6
        task investigate(question)
        on turn_limit: final_answer
        on stuck: fail "stuck"
    return answer
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("calyx-agents-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("p.clyx"), PROGRAM).unwrap();
    let search =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/tools/fake_search.py");
    std::fs::write(
        d.join("calyx.toml"),
        format!(
            "[tools.web_search]\ncommand = [\"python3\", {:?}]\n",
            search.to_str().unwrap()
        ),
    )
    .unwrap();
    d
}

fn calyx(d: &PathBuf, args: &[&str], crash_after: Option<u32>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_calyx"));
    c.args(args).current_dir(d);
    if let Some(k) = crash_after {
        c.env("CALYX_CRASH_AFTER", k.to_string());
    }
    c.output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn an_agent_killed_between_turns_resumes_where_it_was() {
    let run = ["run", "p.clyx", "--question", "x"];
    let whole = dir("whole");
    let expected = calyx(&whole, &run, None);
    assert!(expected.status.success(), "{}", text(&expected.stderr));

    // Six turns of a model call and a search each: killed after the 7th
    // entry, in the middle of the fourth turn.
    let d = dir("killed");
    let first = calyx(&d, &run, Some(7));
    assert_eq!(first.status.code(), Some(137), "{}", text(&first.stderr));
    let id = text(&first.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("calyx: run ").map(str::to_owned))
        .unwrap();
    let resumed = calyx(&d, &["resume", &id], None);
    assert!(resumed.status.success(), "{}", text(&resumed.stderr));
    assert_eq!(text(&resumed.stdout), text(&expected.stdout));
    assert!(
        text(&resumed.stderr).contains("7 taken from the journal"),
        "{}",
        text(&resumed.stderr)
    );
    let _ = std::fs::remove_dir_all(&whole);
    let _ = std::fs::remove_dir_all(&d);
}
