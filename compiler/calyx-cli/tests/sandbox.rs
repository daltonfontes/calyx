//! Sandboxes through the CLI (milestone M6): the run works on a copy, the
//! steps that share a sandbox are ordered by the compiler, a failed call
//! leaves no trace, and resuming puts the sandbox back where the journal
//! left it (decisions D13, D26).
//!
//! The tools run on examples/tools/sandbox_tools.py. Programs call them
//! directly (no model), so every run does the same thing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROGRAM: &str = r#"
tool read_file(box: reads Sandbox, path: Text) -> Text:
    effect read

tool edit_file(box: edits Sandbox, path: Text, old: Text, new: Text) -> Text:
    effect sandbox

tool run_tests(box: reads Sandbox) -> Text:
    effect read

graph fix(repo: Sandbox, new: Text) -> Text:
    # No data flows from `fixed` to `tests`: the order comes from the
    # borrows of `repo`, in the order the steps are written.
    fixed = edit_file(edits repo, "calc.py", "(len(values) - 1)", new)
    tests = run_tests(reads repo)
    return tests

graph check_first(repo: Sandbox) -> Text:
    tests = run_tests(reads repo)
    fixed = edit_file(edits repo, "calc.py", "(len(values) - 1)", "len(values)")
    return tests

graph attempt(repo: Sandbox, new: Text) -> Text:
    fixed = edit_file(edits repo, "calc.py", "(len(values) - 1)", new)
    return read_file(reads repo, "calc.py")

# `fork repo`: each attempt edits a copy of its own; the original is only read.
graph variants(repo: Sandbox) -> List[Text]:
    both = race first 2:
        right: attempt(fork repo, "len(values)")
        other: attempt(fork repo, "(len(values) + 1)")
        on none: fail "nenhuma"
    original = read_file(reads repo, "calc.py")
    return both + [original]

graph twice(repo: Sandbox) -> Text:
    first = edit_file(edits repo, "calc.py", "(len(values) - 1)", "len(values)")
    second = edit_file(edits repo, "calc.py", "if not values:", "if len(values) == 0:")
    return read_file(reads repo, "calc.py")
"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir =
            std::env::temp_dir().join(format!("calyx-sandbox-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo")).unwrap();
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        for f in ["calc.py", "test_calc.py"] {
            std::fs::copy(
                examples.join("sample_repo").join(f),
                dir.join("repo").join(f),
            )
            .unwrap();
        }
        let tools = examples
            .join("tools/sandbox_tools.py")
            .canonicalize()
            .unwrap();
        let cmd = format!("command = [\"python3\", \"{}\"]\n", tools.display());
        let toml: String = ["read_file", "edit_file", "run_tests"]
            .iter()
            .map(|t| format!("[tools.{t}]\n{cmd}\n"))
            .collect();
        std::fs::write(dir.join("calyx.toml"), toml).unwrap();
        std::fs::write(dir.join("p.clyx"), PROGRAM).unwrap();
        Dir(dir)
    }

    fn calyx(&self, args: &[&str], crash_after: Option<u32>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_calyx"));
        cmd.args(args)
            .current_dir(&self.0)
            .env("CALYX_RETRY_BASE_MS", "1");
        if let Some(n) = crash_after {
            cmd.env("CALYX_CRASH_AFTER", n.to_string());
        }
        cmd.output().unwrap()
    }

    /// The sandbox of a run, from what the CLI printed.
    fn sandbox(&self, out: &Output) -> PathBuf {
        let err = text(&out.stderr);
        let line = err
            .lines()
            .find_map(|l| l.strip_prefix("calyx: sandbox `repo` is at "))
            .unwrap_or_else(|| panic!("no sandbox in: {err}"));
        PathBuf::from(line)
    }

    fn run_id(&self, out: &Output) -> String {
        let err = text(&out.stderr);
        err.lines()
            .find_map(|l| l.strip_prefix("calyx: run "))
            .unwrap_or_else(|| panic!("no run id in: {err}"))
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

fn calc(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("calc.py")).unwrap()
}

#[test]
fn the_run_edits_a_copy_in_the_order_the_borrows_give() {
    let d = Dir::new("order");
    let out = d.calyx(
        &[
            "run",
            "p.clyx",
            "--graph",
            "fix",
            "--quiet",
            "--repo",
            "repo",
            "--new",
            "len(values)",
        ],
        None,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    // The tests ran after the edit, and passed.
    let report = text(&out.stdout);
    assert!(
        report.contains("Ran 2 tests") && report.contains("OK"),
        "{report}"
    );
    // The copy changed; the original did not.
    assert!(calc(&d.sandbox(&out)).contains("sum(values) / len(values)"));
    assert!(calc(&d.0.join("repo")).contains("(len(values) - 1)"));

    // Written the other way around, the tests run first, on the old code.
    let out = d.calyx(
        &[
            "run",
            "p.clyx",
            "--graph",
            "check_first",
            "--quiet",
            "--repo",
            "repo",
        ],
        None,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("FAILED (failures=1)"));
    assert!(calc(&d.sandbox(&out)).contains("sum(values) / len(values)"));
}

#[test]
fn a_call_that_fails_half_way_is_undone_before_it_is_repeated() {
    let d = Dir::new("undo");
    // The tool writes the change, then crashes (the first time only). If the
    // half-done change stayed, the retry would not find `old` and fail.
    let out = d.calyx(
        &[
            "run",
            "p.clyx",
            "--graph",
            "fix",
            "--quiet",
            "--repo",
            "repo",
            "--new",
            "len(values)  # __crash_once__",
        ],
        None,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let after = calc(&d.sandbox(&out));
    assert_eq!(after.matches("__crash_once__").count(), 1, "{after}");
}

#[test]
fn resuming_puts_the_sandbox_back_where_the_journal_left_it() {
    let d = Dir::new("resume");
    // Crash right after the first edit reached the journal.
    let out = d.calyx(
        &[
            "run", "p.clyx", "--graph", "twice", "--quiet", "--repo", "repo",
        ],
        Some(1),
    );
    assert!(!out.status.success());
    let id = d.run_id(&out);
    let sandbox = d.0.join(".calyx/runs").join(&id).join("sandboxes/repo");
    assert!(calc(&sandbox).contains("sum(values) / len(values)"));
    // Damage it, as a second edit cut short by the crash would.
    let damaged = calc(&sandbox).replace("if not values:", "if len(values) == 0:  # pela metade");
    std::fs::write(sandbox.join("calc.py"), damaged).unwrap();
    std::fs::write(sandbox.join("lixo.tmp"), "x").unwrap();

    let out = d.calyx(&["resume", &id, "--quiet"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let fixed = text(&out.stdout);
    assert!(fixed.contains("if len(values) == 0:\n"), "{fixed}");
    assert!(!fixed.contains("pela metade"), "{fixed}");
    assert!(fixed.contains("sum(values) / len(values)"), "{fixed}");
    assert!(!sandbox.join("lixo.tmp").exists());
}

#[test]
fn a_sandbox_argument_must_be_a_directory() {
    let d = Dir::new("arg");
    let out = d.calyx(
        &[
            "run",
            "p.clyx",
            "--graph",
            "fix",
            "--repo",
            "nao-existe",
            "--new",
            "x",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("expected a directory for a `Sandbox`"));
}

#[test]
fn each_fork_edits_a_copy_of_its_own() {
    let d = Dir::new("fork");
    let args = [
        "run", "p.clyx", "--graph", "variants", "--repo", "repo", "--quiet",
    ];
    let out = d.calyx(&args, None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v[0].contains("/ len(values)") && !v[0].contains("- 1)"),
        "{}",
        v[0]
    );
    assert!(v[1].contains("(len(values) + 1)"), "{}", v[1]);
    assert!(
        v[2].contains("(len(values) - 1)"),
        "the original is untouched: {}",
        v[2]
    );

    // Killed after the first edit reached the journal: the resumed run puts
    // that fork back to its snapshot and finishes, with the same result.
    let d2 = Dir::new("fork-crash");
    let crashed = d2.calyx(&args, Some(1));
    assert!(!crashed.status.success());
    let id = text(&crashed.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("calyx: run "))
        .unwrap()
        .trim()
        .to_owned();
    let resumed = d2.calyx(&["resume", &id, "--quiet"], None);
    assert!(resumed.status.success(), "{}", text(&resumed.stderr));
    assert_eq!(
        serde_json::from_slice::<Vec<String>>(&resumed.stdout).unwrap(),
        v
    );
}
