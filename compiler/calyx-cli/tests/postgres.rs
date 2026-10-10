//! The journal in PostgreSQL, shared by several machines (D6). Two
//! directories stand for two machines: they share only the database and the
//! fake store (an outside service). Skipped unless CALYX_TEST_DATABASE_URL
//! points to a database (CI starts one).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

fn url() -> Option<String> {
    let u = std::env::var("CALYX_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty());
    if u.is_none() {
        eprintln!("skipped: CALYX_TEST_DATABASE_URL is not set");
    }
    u
}

/// A program, its calyx.toml, and two "machines" (directories).
struct Cluster {
    root: PathBuf,
    url: String,
}

impl Cluster {
    fn new(name: &str, url: String, program: &str) -> Cluster {
        let root = std::env::temp_dir().join(format!("calyx-pg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["prog", "a", "b"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::copy(repo.join(program), root.join("prog/p.clyx")).unwrap();
        let store = repo
            .join("examples/tools/fake_store.py")
            .canonicalize()
            .unwrap();
        let toml: String = ["get_order", "refund", "email", "email_sent"]
            .iter()
            .map(|t| {
                format!(
                    "[tools.{t}]\ncommand = [\"python3\", \"{}\"]\n",
                    store.display()
                )
            })
            .collect();
        std::fs::write(root.join("prog/calyx.toml"), toml).unwrap();
        Cluster { root, url }
    }

    fn program(&self) -> String {
        self.root.join("prog/p.clyx").display().to_string()
    }

    fn cmd(&self, machine: &str, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_calyx"));
        c.args(args)
            .current_dir(self.root.join(machine))
            .env("CALYX_DATABASE_URL", &self.url)
            .env("CALYX_FAKE_STORE", self.root.join("store.json"))
            .envs(env.iter().copied());
        c
    }

    fn calyx(&self, machine: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.cmd(machine, args, env).output().unwrap()
    }

    fn store(&self, list: &str) -> usize {
        let text = std::fs::read_to_string(self.root.join("store.json")).unwrap_or_default();
        let db: Value = serde_json::from_str(&text).unwrap_or_default();
        db[list].as_array().map_or(0, Vec::len)
    }

    fn status(&self, machine: &str, id: &str) -> String {
        let out = text(&self.calyx(machine, &["runs"], &[]).stdout);
        out.lines()
            .find(|l| l.starts_with(id))
            .and_then(|l| l.split_whitespace().nth(2))
            .unwrap_or("?")
            .to_owned()
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn run_id(err: &str) -> String {
    err.lines()
        .find_map(|l| l.strip_prefix("calyx: run "))
        .expect(err)
        .trim()
        .to_owned()
}

const REFUND: [&str; 7] = [
    "--fake-models",
    "--request",
    "R1",
    "--order",
    "A100",
    "--message",
    "chegou quebrado",
];

#[test]
fn another_machine_takes_over_a_run_whose_machine_died() {
    let Some(url) = url() else { return };
    let c = Cluster::new("takeover", url, "examples/refund.clyx");
    let p = c.program();
    let mut args = vec!["run", p.as_str()];
    args.extend(REFUND);
    // Machine A dies right after the refund reached the journal.
    let a = c.calyx("a", &args, &[("CALYX_CRASH_AFTER", "3")]);
    let id = run_id(&text(&a.stderr));
    assert!(!a.status.success());
    assert_eq!(c.status("b", &id), "interrupted");

    // Machine B, with none of A's files, takes it over and finishes it.
    let b = c.calyx("b", &["worker", "--once", "--fake-models"], &[]);
    let err = text(&b.stderr);
    assert!(err.contains(&format!("taking over run {id}")), "{err}");
    assert!(err.contains("3 taken from the journal"), "{err}");
    assert_eq!(c.status("b", &id), "finished");
    assert_eq!((c.store("payments"), c.store("outbox")), (1, 1));
}

#[test]
fn a_run_is_run_by_one_process_at_a_time() {
    let Some(url) = url() else { return };
    // Its model answers in 1 s: the run is still going when B tries.
    let c = Cluster::new("lock", url, "bench/w2_recovery/refund.clyx");
    let p = c.program();
    let mut args = vec!["run", p.as_str()];
    args.extend(&REFUND[1..]);
    let mut a = c
        .cmd("a", &args, &[])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    {
        use std::io::{BufRead, BufReader};
        let mut lines = BufReader::new(a.stderr.as_mut().unwrap());
        lines.read_line(&mut first).unwrap();
    }
    let id = run_id(&first);
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(c.status("b", &id), "running");
    let b = c.calyx("b", &["resume", &id, "--quiet"], &[]);
    assert_eq!(b.status.code(), Some(5), "{}", text(&b.stderr));
    let w = c.calyx("b", &["worker", "--once"], &[]);
    assert!(!text(&w.stderr).contains(&id), "the worker left it alone");

    assert!(a.wait().unwrap().success());
    assert_eq!(c.status("b", &id), "finished");
    assert_eq!((c.store("payments"), c.store("outbox")), (1, 1));
}

#[test]
fn state_kept_on_one_machine_is_refused() {
    let Some(url) = url() else { return };
    let c = Cluster::new("local", url, "examples/memory.clyx");
    let p = c.program();
    let out = c.calyx(
        "a",
        &["run", &p, "--fake-models", "--user", "ana", "--text", "oi"],
        &[],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("entities keep their state in files"),
        "{}",
        text(&out.stderr)
    );
}
