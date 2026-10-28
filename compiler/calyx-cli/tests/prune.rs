//! `calyx prune`: runs that ended go, runs that may still go on stay.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROGRAM: &str = r#"
model m = "fake-model"

message Approval = Approved | Denied(reason: Text)

prompt say(x: Text) -> Text:
    """{x}"""

graph ok(x: Text) -> Text:
    a = m(say(x))
    return a

graph bad(x: Text) -> Text:
    a = m(say(x))
    b = boom(a)
    return b

tool boom(x: Text) -> Text:
    effect read

graph wait(x: Text) -> Text:
    approval = receive Approval, timeout 3 days:
        on timeout: Denied(reason="late")
    return match approval:
        case Approved: "sim"
        case Denied(reason): reason
"#;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn calyx(d: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(args)
        .current_dir(d)
        .env_remove("CALYX_DATABASE_URL")
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn run(d: &Path, graph: &str) -> String {
    let out = calyx(
        d,
        &["run", "p.clyx", "--graph", graph, "--quiet", "--x", "oi"],
    );
    let err = text(&out.stderr);
    err.lines()
        .find_map(|l| l.strip_prefix("calyx: run "))
        .unwrap_or_else(|| panic!("{err}"))
        .trim()
        .to_owned()
}

#[test]
fn prune_deletes_the_runs_that_ended_and_keeps_the_waiting_one() {
    let d = Dir(std::env::temp_dir().join(format!("calyx-prune-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    std::fs::create_dir_all(&d.0).unwrap();
    std::fs::write(d.0.join("p.clyx"), PROGRAM).unwrap();
    // `boom` has no server: the run fails.
    std::fs::write(
        d.0.join("calyx.toml"),
        "[tools.boom]\ncommand = [\"no-such-program\"]\n",
    )
    .unwrap();
    let ok = run(&d.0, "ok");
    let bad = run(&d.0, "bad");
    let waiting = run(&d.0, "wait");
    let runs = text(&calyx(&d.0, &["runs"]).stdout);
    for (id, status) in [(&ok, "finished"), (&bad, "failed"), (&waiting, "waiting")] {
        assert!(
            runs.lines()
                .any(|l| l.starts_with(id.as_str()) && l.contains(status)),
            "{runs}"
        );
    }

    // A decision, not a habit: no default age.
    assert_eq!(calyx(&d.0, &["prune"]).status.code(), Some(2));
    // Younger than a day: nothing.
    let out = calyx(&d.0, &["prune", "--older-than", "1"]);
    assert!(
        text(&out.stdout).contains("deleted 0 run(s)"),
        "{}",
        text(&out.stdout)
    );
    // A dry run says what would go, and deletes nothing.
    let out = calyx(&d.0, &["prune", "--older-than", "0", "--dry-run"]);
    let said = text(&out.stdout);
    assert!(
        said.contains(&format!("would delete {ok} (finished)")),
        "{said}"
    );
    assert!(
        said.contains(&format!("would delete {bad} (failed)")),
        "{said}"
    );
    assert!(!said.contains(&waiting), "{said}");
    assert!(d.0.join(".calyx/runs").join(&ok).exists());

    let out = calyx(&d.0, &["prune", "--older-than", "0"]);
    assert!(
        text(&out.stdout).contains("deleted 2 run(s)"),
        "{}",
        text(&out.stdout)
    );
    assert!(!d.0.join(".calyx/runs").join(&ok).exists());
    assert!(!d.0.join(".calyx/runs").join(&bad).exists());
    // The waiting run is still there, and still goes on.
    let out = calyx(&d.0, &["deliver", &waiting, "Approval", "Approved"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = calyx(&d.0, &["resume", &waiting, "--quiet"]);
    assert_eq!(text(&out.stdout).trim(), "sim", "{}", text(&out.stderr));
}
