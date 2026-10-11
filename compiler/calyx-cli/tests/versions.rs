//! Runs and versions of Calyx (D23, docs/operacao.md).
//!
//! A run finishes on the version that started it, or on a newer one that
//! compiles its program to the same thing: the journal records the
//! program's hash. These tests pin the hash of every example, so a change
//! to the compiler that would stop new versions from resuming old runs is
//! seen here, not by someone whose runs wait for an approval.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const PINNED: &str = "tests/program_hashes.txt";

/// If this fails, the compiler now compiles an example to something else,
/// and a run of it started by the previous version cannot be resumed by
/// this one (it goes to the binary of the version that started it, if it
/// is kept). A field added to the IR is `null` when unused, so adding one
/// keeps the hashes. When the change is meant, write the new hashes with
/// `CALYX_UPDATE_HASHES=1 cargo test -p calyx-cli --test versions` and say
/// it in the release notes (docs/operacao.md, "Atualizar").
#[test]
fn the_examples_compile_to_the_programs_older_versions_did() {
    let mut examples: Vec<PathBuf> = ["examples", "tests/programs"]
        .iter()
        .flat_map(|d| std::fs::read_dir(root().join(d)).unwrap())
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "clyx"))
        .collect();
    examples.sort();
    let mut now = String::new();
    for p in &examples {
        let out = Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(["check", "--ir-json"])
            .arg(p)
            .output()
            .unwrap();
        // Programs with errors (on purpose, in tests/programs) have none.
        if !out.status.success() {
            continue;
        }
        let name = p.strip_prefix(root()).unwrap().display().to_string();
        let hash = calyx_runtime::program_hash(&text(&out.stdout)).unwrap();
        now += &format!("{name} {hash}\n");
    }
    let pinned = Path::new(env!("CARGO_MANIFEST_DIR")).join(PINNED);
    if std::env::var_os("CALYX_UPDATE_HASHES").is_some() {
        std::fs::write(&pinned, &now).unwrap();
        return;
    }
    let was = std::fs::read_to_string(&pinned).unwrap_or_default();
    let changed: Vec<&str> = now
        .lines()
        .filter(|l| !was.lines().any(|w| w == *l))
        .collect();
    assert!(
        changed.is_empty(),
        "these examples now compile to another program, so runs of them started by the \
         previous version cannot be resumed by this one: {changed:?} (see this test's comment)"
    );
}

/// An IR field that is `null` does not count: a version that adds one
/// resumes the runs of the version before.
#[test]
fn null_fields_do_not_change_the_hash() {
    let a = calyx_runtime::program_hash(r#"{"version":1,"graphs":[{"name":"g"}]}"#).unwrap();
    let b =
        calyx_runtime::program_hash(r#"{"version":1,"graphs":[{"name":"g","decreases":null}]}"#)
            .unwrap();
    let c = calyx_runtime::program_hash(r#"{"version":1,"graphs":[{"name":"h"}]}"#).unwrap();
    assert_eq!(a, b);
    assert_ne!(a, c);
}

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(name: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("calyx-versions-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::copy(
        root().join("examples/approval.clyx"),
        d.join("approval.clyx"),
    )
    .unwrap();
    Dir(d)
}

fn calyx(d: &Path, versions: Option<&Path>, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_calyx"));
    c.args(args).current_dir(d).env_remove("CALYX_DATABASE_URL");
    // Only the directory the test gives: not the machine's ~/.calyx.
    c.env("HOME", d);
    match versions {
        Some(v) => c.env("CALYX_VERSIONS_DIR", v),
        None => c.env_remove("CALYX_VERSIONS_DIR"),
    };
    c.output().unwrap()
}

/// Starts a run of approval.clyx that waits, and returns its id.
fn waiting_run(d: &Path) -> String {
    let out = calyx(
        d,
        None,
        &[
            "run",
            "approval.clyx",
            "--fake-models",
            "--quiet",
            "--request",
            "notebook",
        ],
    );
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    let id = std::fs::read_dir(d.join(".calyx/runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .into_string()
        .unwrap();
    let journal = d.join(".calyx/runs").join(&id).join("journal.jsonl");
    let header = std::fs::read_to_string(&journal).unwrap();
    assert!(
        header.contains(&format!("\"calyx\":\"{}\"", env!("CARGO_PKG_VERSION"))),
        "{header}"
    );
    id
}

/// Says the run was started by `version`, as if an older Calyx had.
fn started_by(d: &Path, id: &str, version: &str) {
    let journal = d.join(".calyx/runs").join(id).join("journal.jsonl");
    let text = std::fs::read_to_string(&journal).unwrap();
    let mine = format!("\"calyx\":\"{}\"", env!("CARGO_PKG_VERSION"));
    std::fs::write(
        &journal,
        text.replacen(&mine, &format!("\"calyx\":\"{version}\""), 1),
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn a_run_goes_to_the_binary_of_the_version_that_started_it() {
    use std::os::unix::fs::PermissionsExt;
    let d = dir("delegate");
    let id = waiting_run(&d.0);
    started_by(&d.0, &id, "0.1.0");
    // The "old version": records how it was called.
    let versions = d.0.join("versions");
    std::fs::create_dir_all(versions.join("0.1.0")).unwrap();
    let old = versions.join("0.1.0/calyx");
    std::fs::write(
        &old,
        format!(
            "#!/bin/sh\necho \"$@\" > {}/called\nexit 4\n",
            d.0.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = calyx(
        &d.0,
        Some(&versions),
        &["resume", &id, "--fake-models", "--quiet"],
    );
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("was started by Calyx 0.1.0"));
    let called = std::fs::read_to_string(d.0.join("called")).unwrap();
    assert_eq!(called.trim(), format!("resume {id} --fake-models --quiet"));
    // `calyx runs` says which version each run needs.
    let runs = text(&calyx(&d.0, None, &["runs"]).stdout);
    assert!(runs.contains("0.1.0"), "{runs}");
}

#[test]
fn without_that_binary_a_version_that_compiles_the_same_program_resumes_it() {
    let d = dir("same");
    let id = waiting_run(&d.0);
    started_by(&d.0, &id, "0.1.0");
    let out = calyx(
        &d.0,
        None,
        &["deliver", &id, "Approval", r#"{"kind":"Approved"}"#],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = calyx(&d.0, None, &["resume", &id, "--fake-models", "--quiet"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("aprovado"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn a_run_this_version_cannot_resume_says_which_version_can() {
    let d = dir("refused");
    let id = waiting_run(&d.0);
    started_by(&d.0, &id, "0.1.0");
    // Another program under the run, as an older compiler would have made.
    let journal = d.0.join(".calyx/runs").join(&id).join("journal.jsonl");
    let text0 = std::fs::read_to_string(&journal).unwrap();
    let at = text0.find("\"ir_hash\":\"").unwrap() + 11;
    let mut changed = text0.clone();
    changed.replace_range(at..at + 64, &"0".repeat(64));
    std::fs::write(&journal, changed).unwrap();
    let out = calyx(&d.0, None, &["resume", &id, "--fake-models", "--quiet"]);
    assert_eq!(out.status.code(), Some(3));
    let err = text(&out.stderr);
    assert!(err.contains("started by Calyx 0.1.0"), "{err}");
    assert!(err.contains("~/.calyx/versions/0.1.0/calyx"), "{err}");
}
