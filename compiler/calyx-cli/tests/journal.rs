//! The journal through the CLI: crashes, resume, replay (milestone M3).
//!
//! Each test works in its own directory, where `calyx` keeps `.calyx/runs`.
//! Crashes are simulated with CALYX_CRASH_AFTER, which makes the runtime
//! exit abruptly right after that many calls reached the journal.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROGRAM: &str = r#"
model m = "fake-model"

tool web_search(query: Text) -> Text:
    effect read

tool notify(text: Text) -> Text:
    effect write once
    on_uncertain pause

prompt summarize(q: Text, found: Text) -> Text:
    """{q}: {found}"""

prompt report(items: List[Text]) -> Text:
    """{items}"""

graph g(topics: List[Text]) -> Text:
    answers = for each q in topics:
        m(summarize(q, web_search(q)))
    text = m(report(answers))
    sent = notify(text)
    return sent
"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("calyx-journal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let search = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/tools/fake_search.py")
            .canonicalize()
            .unwrap();
        let search = search.display();
        std::fs::write(
            dir.join("calyx.toml"),
            format!(
                "[tools.web_search]\ncommand = [\"python3\", \"{search}\"]\n\n\
                 [tools.notify]\ncommand = [\"python3\", \"{search}\"]\nname = \"web_search\"\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("p.clyx"), PROGRAM).unwrap();
        Dir(dir)
    }

    fn calyx(&self, args: &[&str], crash_after: Option<u32>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_calyx"));
        cmd.args(args)
            .current_dir(&self.0)
            .env("CALYX_RETRY_BASE_MS", "1");
        match crash_after {
            Some(n) => cmd.env("CALYX_CRASH_AFTER", n.to_string()),
            None => cmd.env_remove("CALYX_CRASH_AFTER"),
        };
        cmd.output().unwrap()
    }

    fn run(&self, topics: &str, crash_after: Option<u32>) -> Output {
        self.calyx(
            &["run", "p.clyx", "--fake-models", "--topics", topics],
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

    fn journal(&self, id: &str) -> PathBuf {
        self.0.join(".calyx/runs").join(id).join("journal.jsonl")
    }

    fn count(&self, id: &str, ty: &str) -> usize {
        std::fs::read_to_string(self.journal(id))
            .unwrap()
            .lines()
            .filter(|l| l.contains(&format!("\"type\":\"{ty}\"")))
            .count()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(o: &[u8]) -> String {
    String::from_utf8_lossy(o).into_owned()
}

const TOPICS: &str = r#"["a", "b", "c"]"#;

#[test]
fn a_resumed_run_redoes_no_finished_call() {
    let reference = Dir::new("reference");
    let full = reference.run(TOPICS, None);
    assert!(full.status.success(), "{}", text(&full.stderr));

    let d = Dir::new("resume");
    let crashed = d.run(TOPICS, Some(4));
    assert_eq!(
        crashed.status.code(),
        Some(137),
        "{}",
        text(&crashed.stderr)
    );
    let id = d.only_run();
    assert_eq!(d.count(&id, "call"), 4);

    let resumed = d.calyx(&["resume", &id, "--fake-models"], None);
    assert!(resumed.status.success(), "{}", text(&resumed.stderr));
    // Same result as the run that never crashed.
    assert_eq!(text(&resumed.stdout), text(&full.stdout));
    let err = text(&resumed.stderr);
    assert!(err.contains("4 taken from the journal"), "{err}");
    // 3 searches + 3 summaries + report + notify, each recorded once.
    assert_eq!(d.count(&id, "call"), 8);
    assert_eq!(d.count(&id, "resume"), 1);
    assert_eq!(d.count(&id, "end"), 1);
}

#[test]
fn replay_calls_nothing() {
    let d = Dir::new("replay");
    assert!(d.run(TOPICS, None).status.success());
    let id = d.only_run();
    // No --fake-models: a real call would need a provider and fail.
    let replay = d.calyx(&["replay", &id], None);
    assert!(replay.status.success(), "{}", text(&replay.stderr));
    assert!(text(&replay.stderr).contains("0 model call(s)"));
    // Replay writes nothing.
    assert_eq!(d.count(&id, "end"), 1);
}

#[test]
fn replay_of_an_interrupted_run_stops_where_the_journal_ends() {
    let d = Dir::new("replay-partial");
    d.run(TOPICS, Some(2));
    let id = d.only_run();
    let replay = d.calyx(&["replay", &id], None);
    assert_eq!(replay.status.code(), Some(3));
    // Calls run in parallel, so which ones finished before the crash varies.
    let err = text(&replay.stderr);
    assert!(err.contains("replay: call `g/"), "{err}");
    assert!(err.contains("is not in the journal"), "{err}");
}

#[test]
fn a_changed_program_cannot_resume() {
    let d = Dir::new("changed");
    d.run(TOPICS, Some(2));
    let id = d.only_run();
    let changed = PROGRAM.replace("{q}: {found}", "{q} -> {found}");
    std::fs::write(d.0.join("p.clyx"), changed).unwrap();
    let resumed = d.calyx(&["resume", &id, "--fake-models"], None);
    assert_eq!(resumed.status.code(), Some(3));
    assert!(text(&resumed.stderr).contains("the program changed since this run started"));
}

#[test]
fn a_write_once_call_with_unknown_outcome_is_not_repeated() {
    let d = Dir::new("write-once");
    assert!(d.run(TOPICS, None).status.success());
    let id = d.only_run();
    // As if the process died after `begin` and before the call finished.
    let journal = std::fs::read_to_string(d.journal(&id)).unwrap();
    let kept: Vec<&str> = journal
        .lines()
        .filter(|l| !(l.contains("\"key\":\"g/sent#0\"") && l.contains("\"type\":\"call\"")))
        .filter(|l| !l.contains("\"type\":\"end\""))
        .collect();
    assert!(journal.contains("\"type\":\"begin\",\"key\":\"g/sent#0\""));
    std::fs::write(d.journal(&id), kept.join("\n") + "\n").unwrap();

    let resumed = d.calyx(&["resume", &id, "--fake-models"], None);
    assert_eq!(resumed.status.code(), Some(3));
    let err = text(&resumed.stderr);
    assert!(
        err.contains("`notify` (write once) started before the interruption"),
        "{err}"
    );
}

#[test]
fn a_torn_last_line_is_dropped() {
    let d = Dir::new("torn");
    d.run(TOPICS, Some(2));
    let id = d.only_run();
    // A crash in the middle of writing an entry.
    let mut journal = std::fs::read_to_string(d.journal(&id)).unwrap();
    journal.push_str("{\"type\":\"call\",\"key\":\"g/ans");
    std::fs::write(d.journal(&id), journal).unwrap();

    let resumed = d.calyx(&["resume", &id, "--fake-models"], None);
    assert!(resumed.status.success(), "{}", text(&resumed.stderr));
    let after = std::fs::read_to_string(d.journal(&id)).unwrap();
    assert!(
        after
            .lines()
            .all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
    );
}

#[test]
fn large_answers_are_stored_by_hash() {
    let d = Dir::new("blobs");
    let out = d.run(r#"["__big__"]"#, None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let id = d.only_run();
    let blobs: Vec<_> = std::fs::read_dir(d.0.join(".calyx/runs").join(&id).join("blobs"))
        .unwrap()
        .collect();
    assert_eq!(blobs.len(), 1);
    let replay = d.calyx(&["replay", &id], None);
    assert!(replay.status.success(), "{}", text(&replay.stderr));
    assert_eq!(text(&replay.stdout), text(&out.stdout));
}

#[test]
fn runs_lists_status() {
    let d = Dir::new("list");
    d.run(TOPICS, Some(3));
    let out = text(&d.calyx(&["runs"], None).stdout);
    assert!(out.contains("interrupted"), "{out}");
    assert!(out.contains("      3"), "{out}");
}
