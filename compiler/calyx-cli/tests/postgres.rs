//! The journal in PostgreSQL, shared by several machines (D6). Two
//! directories stand for two machines: they share only the database and the
//! fake store (an outside service), so what one machine's run needs from the
//! other (its journal, its entities, the messages delivered to it) must come
//! from the database. Skipped unless CALYX_TEST_DATABASE_URL points to a
//! database (CI starts one).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

/// The tests share one database, and a worker takes over any run in it:
/// one test at a time, so a test's worker does not take another's run.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn url() -> Option<std::sync::MutexGuard<'static, ()>> {
    let u = std::env::var("CALYX_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty());
    if u.is_none() {
        eprintln!("skipped: CALYX_TEST_DATABASE_URL is not set");
        return None;
    }
    Some(SERIAL.lock().unwrap_or_else(|e| e.into_inner()))
}

fn database() -> String {
    std::env::var("CALYX_TEST_DATABASE_URL").unwrap()
}

/// A program, its calyx.toml, and two "machines" (directories).
struct Cluster {
    root: PathBuf,
    url: String,
}

impl Cluster {
    fn new(name: &str, url: String, program: &str) -> Cluster {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        Cluster::source(
            name,
            url,
            &std::fs::read_to_string(repo.join(program)).unwrap(),
        )
    }

    fn source(name: &str, url: String, program: &str) -> Cluster {
        let root = std::env::temp_dir().join(format!("calyx-pg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["prog", "a", "b"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::write(root.join("prog/p.clyx"), program).unwrap();
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
    let Some(_one) = url() else { return };
    let url = database();
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
    let Some(_one) = url() else { return };
    let url = database();
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

/// A key no earlier test run used: the database outlives the tests.
fn fresh(name: &str) -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{name}-{}-{t}", std::process::id())
}

const COUNTER: &str = r#"
model m = "fake-model"

entity Counter(key name: Text):
    state count: Nat = 0

    on Get() -> Nat:
        return count

    on Add():
        next count = count + 1

prompt say(n: Nat) -> Text:
    """{n}"""

graph bump(name: Text) -> Text:
    before = ask Counter(name).Get()
    send Counter(name).Add()
    return m(say(before))

graph read(name: Text) -> Nat:
    return ask Counter(name).Get()
"#;

impl Cluster {
    fn count(&self, machine: &str, key: &str) -> String {
        let p = self.program();
        let out = self.calyx(
            machine,
            &["run", &p, "--graph", "read", "--quiet", "--name", key],
            &[],
        );
        assert!(out.status.success(), "{}", text(&out.stderr));
        text(&out.stdout).trim().to_owned()
    }
}

#[test]
fn machines_share_entities_and_lose_no_update() {
    let Some(_one) = url() else { return };
    let url = database();
    let c = Cluster::source("entities", url, COUNTER);
    let p = c.program();
    let key = fresh("ana");
    // Eight runs at once, half on each machine, all to the same entity.
    let children: Vec<_> = (0..8)
        .map(|i| {
            let machine = if i % 2 == 0 { "a" } else { "b" };
            c.cmd(
                machine,
                &[
                    "run",
                    &p,
                    "--graph",
                    "bump",
                    "--fake-models",
                    "--quiet",
                    "--name",
                    &key,
                ],
                &[],
            )
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
        })
        .collect();
    for ch in children {
        let out = ch.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
    assert_eq!(c.count("a", &key), "8");
    assert_eq!(c.count("b", &key), "8");
    assert!(
        !c.root.join("a/.calyx/entities").exists(),
        "the entity is in the database, not in a machine's files"
    );
}

#[test]
fn a_message_applied_before_the_machine_died_is_not_applied_again() {
    let Some(_one) = url() else { return };
    let url = database();
    let c = Cluster::source("send", url, COUNTER);
    let p = c.program();
    let key = fresh("bia");
    // Machine A dies after the entity applied the message and before the
    // journal recorded it.
    let a = c.calyx(
        "a",
        &[
            "run",
            &p,
            "--graph",
            "bump",
            "--fake-models",
            "--name",
            &key,
        ],
        &[("CALYX_CRASH_IN_SEND", "1")],
    );
    assert!(!a.status.success());
    let id = run_id(&text(&a.stderr));
    assert_eq!(c.count("b", &key), "1");

    let b = c.calyx("b", &["worker", "--once", "--fake-models"], &[]);
    let err = text(&b.stderr);
    assert!(err.contains(&format!("taking over run {id}")), "{err}");
    assert_eq!(c.status("b", &id), "finished");
    assert_eq!(c.count("b", &key), "1", "{err}");
}

const APPROVAL: &str = r#"
model m = "fake-model"

message Approval = Approved | Denied(reason: Text)

prompt propose(request: Text) -> Text:
    """{request}"""

graph approve(request: Text) -> Text:
    proposal = m(propose(request))
    approval = receive Approval about proposal, timeout 3 days:
        on timeout: Denied(reason="sem resposta")
    return match approval:
        case Approved: "aprovado"
        case Denied(reason): "recusado ({reason})"
"#;

#[test]
fn a_message_delivered_on_one_machine_reaches_a_run_waiting_on_another() {
    let Some(_one) = url() else { return };
    let url = database();
    let c = Cluster::source("receive", url, APPROVAL);
    let p = c.program();
    let a = c.calyx(
        "a",
        &["run", &p, "--fake-models", "--request", "reembolso de 300"],
        &[],
    );
    assert_eq!(a.status.code(), Some(4), "{}", text(&a.stderr));
    let id = run_id(&text(&a.stderr));
    assert_eq!(c.status("b", &id), "waiting");

    // Nothing to do yet: the worker leaves it waiting.
    let w = c.calyx("b", &["worker", "--once", "--fake-models"], &[]);
    assert!(!text(&w.stderr).contains(&id), "{}", text(&w.stderr));

    // Machine B delivers; a second delivery (from A) finds it taken.
    let d = c.calyx("b", &["deliver", &id, "Approval", "Approved"], &[]);
    assert!(d.status.success(), "{}", text(&d.stderr));
    let again = c.calyx("a", &["deliver", &id, "Approval", "Approved"], &[]);
    assert!(!again.status.success());

    // The worker on B resumes it, with none of A's files.
    let w = c.calyx("b", &["worker", "--once", "--fake-models"], &[]);
    let err = text(&w.stderr);
    assert!(err.contains(&format!("resuming run {id}")), "{err}");
    assert_eq!(c.status("a", &id), "finished");
    let out = c.calyx("a", &["replay", &id, "--quiet"], &[]);
    assert_eq!(
        text(&out.stdout).trim(),
        "aprovado",
        "{}",
        text(&out.stderr)
    );
    assert!(
        !c.root
            .join("a/.calyx/runs")
            .join(&id)
            .join("waits.jsonl")
            .exists()
    );
}

#[test]
fn a_sandbox_kept_on_one_machine_is_refused() {
    let Some(_one) = url() else { return };
    let url = database();
    let c = Cluster::new("local", url, "examples/fix.clyx");
    let p = c.program();
    let out = c.calyx(
        "a",
        &["run", &p, "--fake-models", "--issue", "x", "--repo", "."],
        &[],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("sandboxes are directories on one machine"),
        "{}",
        text(&out.stderr)
    );
}
