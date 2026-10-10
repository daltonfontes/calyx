//! Graphs that call themselves (decision D17): `decreases p`, a `Nat` that
//! each call to itself makes smaller. Each call has its own keys, so a
//! resumed run takes what the journal has, at every depth.

use std::path::PathBuf;
use std::process::{Command, Output};

const PROGRAM: &str = r#"
model m = "fake-model"

prompt split(t: Text) -> List[Text] max 3:
    """{t}"""

prompt solve(t: Text) -> Text:
    """{t}"""

prompt merge(xs: List[Text]) -> Text:
    """{xs}"""

graph work(task: Text, depth: Nat) -> Text:
    decreases depth
    answer = if depth == 0:
        m(solve(task))
    else:
        parts = m(split(task))
        subs = for each p in parts:
            work(p, depth - 1)
        m(merge(subs))
    return answer

# No base case: the recursion would go on; `decreases` stops it below 0.
graph endless(task: Text, depth: Nat) -> Text:
    decreases depth
    return endless(task, depth - 1)
"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir =
            std::env::temp_dir().join(format!("calyx-recursion-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.clyx"), PROGRAM).unwrap();
        Dir(dir)
    }

    fn calyx(&self, args: &[&str], crash_after: Option<u32>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_calyx"));
        cmd.args(args).current_dir(&self.0);
        if let Some(n) = crash_after {
            cmd.env("CALYX_CRASH_AFTER", n.to_string());
        }
        cmd.output().unwrap()
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

const RUN: [&str; 10] = [
    "run",
    "p.clyx",
    "--graph",
    "work",
    "--fake-models",
    "--quiet",
    "--task",
    "t",
    "--depth",
    "2",
];

#[test]
fn a_recursive_graph_resumes_at_every_depth() {
    let d = Dir::new("resume");
    let whole = d.calyx(&RUN, None);
    assert!(whole.status.success(), "{}", text(&whole.stderr));

    // Killed after 5 calls reached the journal (two levels down): the
    // resumed run takes those 5 and makes only the rest.
    let d2 = Dir::new("resume-crash");
    let crashed = d2.calyx(&RUN, Some(5));
    assert!(!crashed.status.success());
    let id = text(&crashed.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("calyx: run "))
        .unwrap()
        .trim()
        .to_owned();
    let resumed = d2.calyx(&["resume", &id, "--fake-models"], None);
    assert!(resumed.status.success(), "{}", text(&resumed.stderr));
    assert!(
        text(&resumed.stderr).contains("5 taken from the journal"),
        "{}",
        text(&resumed.stderr)
    );
    assert_eq!(text(&resumed.stdout), text(&whole.stdout));
}

#[test]
fn a_recursion_without_a_base_case_stops_below_zero() {
    let d = Dir::new("endless");
    let out = d.calyx(
        &[
            "run", "p.clyx", "--graph", "endless", "--quiet", "--task", "t", "--depth", "3",
        ],
        None,
    );
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("`endless` called with `depth` = -1: below 0"),
        "{}",
        text(&out.stderr)
    );
}
