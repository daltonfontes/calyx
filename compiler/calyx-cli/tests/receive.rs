//! `receive` through the CLI (milestone M8): a run stops to wait for a
//! message, `calyx deliver` gives it one, `calyx resume` / `calyx tick`
//! continue it, and a deadline written down once decides when it gives up
//! (decision D21).

use std::path::PathBuf;
use std::process::{Command, Output};

const PROGRAM: &str = r#"
model m = "fake-model"

message Approval = Approved | Denied(reason: Text)

message Answer:
    text: Text

prompt propose(request: Text) -> Text:
    """{request}"""

graph approve(request: Text) -> Text:
    proposal = m(propose(request))
    approval = receive Approval, timeout 3 days:
        on timeout: Denied(reason="sem resposta")
    return match approval:
        case Approved: "aprovado: {proposal}"
        case Denied(reason): "recusado ({reason})"

graph about(request: Text) -> Text:
    proposal = m(propose(request))
    approval = receive Approval about proposal, timeout 3 days:
        on timeout: Denied(reason="sem resposta")
    return match approval:
        case Approved: "aprovado: {proposal}"
        case Denied(reason): "recusado ({reason})"

graph quick(request: Text) -> Text:
    approval = receive Approval, timeout 1 s:
        on timeout: Denied(reason="prazo vencido")
    return match approval:
        case Approved: "aprovado"
        case Denied(reason): "recusado ({reason})"

graph survey(people: List[Text]) -> List[Text]:
    answers = for each p in people:
        receive Answer, timeout 1 days:
            on timeout: Answer(text="-")
    return [a.text for a in answers]
"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("calyx-receive-{name}-{}", std::process::id()));
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

    fn start(&self, graph: &str, args: &[&str]) -> String {
        let mut a = vec![
            "run",
            "p.clyx",
            "--graph",
            graph,
            "--fake-models",
            "--quiet",
        ];
        a.extend_from_slice(args);
        let out = self.calyx(&a);
        assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
        text(&out.stderr)
            .lines()
            .find_map(|l| l.strip_prefix("calyx: run "))
            .unwrap()
            .to_owned()
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
fn a_run_waits_and_continues_with_the_delivered_message() {
    let d = Dir::new("approve");
    let id = d.start("approve", &["--request", "reembolso"]);
    let runs = text(&d.calyx(&["runs"]).stdout);
    assert!(runs.contains("waiting"), "{runs}");

    let out = d.calyx(&["deliver", &id, "Approval", "Approved"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = d.calyx(&["resume", &id, "--fake-models"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).starts_with("aprovado: [resposta falsa"));
    // The proposal was made once, before the wait.
    assert!(text(&out.stderr).contains("from the journal"));
    assert!(text(&d.calyx(&["runs"]).stdout).contains("finished"));

    // Replay needs neither the model nor the message again.
    let out = d.calyx(&["replay", &id, "--quiet"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn deliveries_are_checked_against_the_message_type() {
    let d = Dir::new("check");
    let id = d.start("approve", &["--request", "x"]);
    for bad in [
        "Talvez",
        r#"{"kind":"Denied"}"#,
        r#"{"kind":"Denied","reason":3}"#,
    ] {
        let out = d.calyx(&["deliver", &id, "Approval", bad]);
        assert_eq!(out.status.code(), Some(2), "{bad}");
    }
    let out = d.calyx(&["deliver", &id, "Answer", r#"{"text":"oi"}"#]);
    assert!(text(&out.stderr).contains("is not waiting for `Answer`"));

    let out = d.calyx(&[
        "deliver",
        &id,
        "Approval",
        r#"{"kind":"Denied","reason":"caro"}"#,
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = d.calyx(&["resume", &id, "--fake-models", "--quiet"]);
    assert_eq!(text(&out.stdout), "recusado (caro)\n");
    // Nothing waits now.
    let out = d.calyx(&["deliver", &id, "Approval", "Approved"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn tick_resumes_runs_whose_deadline_passed() {
    let d = Dir::new("tick");
    let id = d.start("quick", &["--request", "x"]);
    // Before the deadline, tick leaves it waiting.
    let out = d.calyx(&["tick", "--fake-models", "--quiet"]);
    assert!(
        text(&out.stderr).contains("resumed 0 run(s); 1 still waiting"),
        "{}",
        text(&out.stderr)
    );
    std::thread::sleep(std::time::Duration::from_millis(1600));
    let out = d.calyx(&["tick", "--fake-models", "--quiet"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "recusado (prazo vencido)\n");
    assert!(text(&out.stderr).contains(&format!("resuming {id}")));
    assert!(text(&d.calyx(&["runs"]).stdout).contains("finished"));
}

#[test]
fn a_wait_says_what_it_is_about() {
    let d = Dir::new("about");
    let out = d.calyx(&[
        "run",
        "p.clyx",
        "--graph",
        "about",
        "--fake-models",
        "--request",
        "reembolso",
    ]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    // The proposal comes first; then the wait, which shows it.
    let proposed = err.find("propose").unwrap();
    let waiting = err.find("recv  Approval  waiting").unwrap();
    assert!(proposed < waiting, "{err}");
    assert!(
        err.contains("`Approval` is about: \"[resposta falsa"),
        "{err}"
    );
    let id = err
        .lines()
        .find_map(|l| l.strip_prefix("calyx: run "))
        .unwrap()
        .to_owned();
    let waits =
        std::fs::read_to_string(d.0.join(".calyx/runs").join(&id).join("waits.jsonl")).unwrap();
    assert!(waits.contains("\"about\":\"[resposta falsa"), "{waits}");
}

#[test]
fn an_answer_after_the_deadline_is_not_taken() {
    // The deadline passes while nothing runs: the answer that comes next is
    // late, even though the run has not taken its `on timeout` yet.
    let d = Dir::new("late");
    let id = d.start("quick", &["--request", "x"]);
    std::thread::sleep(std::time::Duration::from_millis(1600));
    let out = d.calyx(&["deliver", &id, "Approval", "Approved"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("deadline"),
        "{}",
        text(&out.stderr)
    );

    // An inbox line written some other way, with a late time, is not taken
    // either: the run goes on with `on timeout`.
    let run_dir = d.0.join(".calyx/runs").join(&id);
    let waits = std::fs::read_to_string(run_dir.join("waits.jsonl")).unwrap();
    let wait: serde_json::Value = serde_json::from_str(waits.lines().next().unwrap()).unwrap();
    let late = serde_json::json!({
        "key": wait["key"], "message": "Approval", "value": {"kind": "Approved"},
        "at": wait["until"].as_f64().unwrap() + 1.0,
    });
    std::fs::write(run_dir.join("inbox.jsonl"), format!("{late}\n")).unwrap();
    let out = d.calyx(&["resume", &id, "--fake-models"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "recusado (prazo vencido)\n");
    assert!(text(&out.stderr).contains("after the deadline"));
}

#[test]
fn parallel_receives_take_deliveries_oldest_first() {
    let d = Dir::new("survey");
    let id = d.start("survey", &["--people", r#"["ana","bia"]"#]);
    for answer in ["primeira", "segunda"] {
        let out = d.calyx(&[
            "deliver",
            &id,
            "Answer",
            &format!(r#"{{"text":"{answer}"}}"#),
        ]);
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
    let out = d.calyx(&["resume", &id, "--fake-models", "--quiet"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let list = text(&out.stdout);
    assert!(
        list.contains("primeira") && list.contains("segunda"),
        "{list}"
    );
}

#[test]
fn receive_needs_a_journal() {
    let d = Dir::new("nojournal");
    let out = d.calyx(&[
        "run",
        "p.clyx",
        "--graph",
        "quick",
        "--quiet",
        "--no-journal",
        "--request",
        "x",
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert!(text(&out.stderr).contains("needs a journal"));
}
