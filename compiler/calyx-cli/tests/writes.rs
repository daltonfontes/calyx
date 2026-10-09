//! External writes through the CLI (milestone M6): idempotency keys,
//! preconditions checked by the tool (D29) and the `on_uncertain` policies
//! of `write once` (D2), live and when resuming.
//!
//! The tools run on the fake store (examples/tools/fake_store.py), whose
//! state is a JSON file per test. Words in an e-mail's body simulate
//! failures: `__lost__` (sent, then no answer), `__down__` (crash before
//! sending), `__flaky__` (crash before sending, the first time only),
//! `__gateway__` (sent, then an error starting `Timeout:`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn program(policy: &str) -> String {
    format!(
        r#"
type Status = Pending | Delivered | Cancelled

type OrderState:
    status: Status
    total: Float
    refunded: Float

tool refund(request: Text, order: Text, amount: Float) -> Unit:
    effect write
    idempotency_key request
    checks OrderState

tool email_sent(to: Text, subject: Text) -> Bool:
    effect read

tool email(to: Text, subject: Text, body: Text) -> Unit:
    effect write once
    timeout 1 s
    on_uncertain {policy}

tool mails_to(to: Text, subject: Text) -> List[Text]:
    effect read

tool mail(to: Text, subject: Text, body: Text) -> Text:
    effect write once
    timeout 1 s
    on_uncertain verify(mails_to(to, subject))

graph send(body: Text) -> Text:
    return mail("ana@exemplo.org", "oi", body)

tool post(to: Text, subject: Text, body: Text) -> Text:
    effect write once
    timeout 1 s
    on_uncertain pause

graph post_one(body: Text) -> Text:
    return post("ana@exemplo.org", "oi", body)

tool subjects_sent(to: Text, subjects: List[Text]) -> List[Text]:
    effect read

tool email_many(to: Text, subjects: List[Text]) -> Unit:
    effect write once
    batch subjects
    on_uncertain verify(subjects_sent(to, subjects))

graph notify(to: Text) -> Text:
    sent = email_many(to, ["a", "b", "c", "d"])
    return "ok"

graph pay(request: Text, order: Text, amount: Float, body: Text) -> Text:
    paid = refund(request, order, amount):
        requires state.status == Delivered
        requires state.refunded + amount <= state.total
    notice = email("ana@exemplo.org", "pedido {{order}}", body)
    notice after paid
    return "ok"

graph careful(request: Text, order: Text, amount: Float) -> Text:
    paid = try refund(request, order, amount):
        requires state.status == Delivered
    return match paid:
        case Ok(_): "pago"
        case Failed(error): "recusado: {{error}}"
"#
    )
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str, policy: &str) -> Dir {
        Dir::with_program(name, &program(policy))
    }

    fn with_program(name: &str, text: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("calyx-writes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/tools/fake_store.py")
            .canonicalize()
            .unwrap();
        let cmd = format!("command = [\"python3\", \"{}\"]\n", store.display());
        let toml: String = [
            "refund",
            "email",
            "email_sent",
            "mail",
            "mails_to",
            "email_many",
            "subjects_sent",
        ]
        .iter()
        .map(|t| format!("[tools.{t}]\n{cmd}\n"))
        .collect();
        // `post` is the store's `mail`, declared with `on_uncertain pause`.
        let toml = format!("{toml}[tools.post]\n{cmd}name = \"mail\"\n");
        std::fs::write(dir.join("calyx.toml"), toml).unwrap();
        std::fs::write(dir.join("p.clyx"), text).unwrap();
        Dir(dir)
    }

    fn calyx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(args)
            .current_dir(&self.0)
            .env("CALYX_FAKE_STORE", self.0.join("store.json"))
            .env("CALYX_RETRY_BASE_MS", "1")
            .output()
            .unwrap()
    }

    fn pay(&self, request: &str, order: &str, body: &str) -> Output {
        self.calyx(&[
            "run",
            "p.clyx",
            "--graph",
            "pay",
            "--quiet",
            "--request",
            request,
            "--order",
            order,
            "--amount",
            "100",
            "--body",
            body,
        ])
    }

    fn store(&self) -> Value {
        let text = std::fs::read_to_string(self.0.join("store.json")).unwrap_or("{}".into());
        serde_json::from_str(&text).unwrap()
    }

    fn sent(&self) -> usize {
        self.store()["outbox"].as_array().map_or(0, Vec::len)
    }

    /// Without a store file nothing was ever saved: the seed's values.
    fn refunded(&self, order: &str) -> f64 {
        let seed = |o: &str| if o == "A300" { 40.0 } else { 0.0 };
        self.store()["orders"][order]["refunded"]
            .as_f64()
            .unwrap_or_else(|| seed(order))
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

fn run_id(out: &Output) -> String {
    let err = text(&out.stderr);
    err.lines()
        .find_map(|l| l.strip_prefix("calyx: run "))
        .unwrap_or_else(|| panic!("no run id in: {err}"))
        .to_owned()
}

#[test]
fn a_keyed_write_never_applies_twice() {
    let d = Dir::new("keyed", "pause");
    let out = d.pay("R1", "A100", "obrigado");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(d.refunded("A100"), 100.0);
    // A new run with the same request: the tool sees the key and does nothing.
    let out = d.pay("R1", "A100", "obrigado");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(d.refunded("A100"), 100.0);
    assert_eq!(d.sent(), 2, "each run sends its own e-mail");
}

#[test]
fn preconditions_are_checked_by_the_tool_on_the_current_state() {
    let d = Dir::new("requires", "pause");
    // A200 is pending: the tool refuses, pays nothing, and nothing is sent.
    let out = d.pay("R1", "A200", "obrigado");
    assert_eq!(out.status.code(), Some(3));
    let err = text(&out.stderr);
    assert!(
        err.contains("PreconditionFailed: state.status == \"Delivered\""),
        "{err}"
    );
    assert_eq!(d.refunded("A200"), 0.0);
    assert_eq!(d.sent(), 0);
    // A300 has 40 of 50 refunded: 100 more would exceed the total.
    let out = d.pay("R2", "A300", "obrigado");
    assert!(text(&out.stderr).contains("state.refunded + 100 <= state.total"));
    // `try` turns the refusal into a value.
    let out = d.calyx(&[
        "run",
        "p.clyx",
        "--graph",
        "careful",
        "--quiet",
        "--request",
        "R3",
        "--order",
        "A200",
        "--amount",
        "10",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).starts_with("recusado: tool `refund` failed: PreconditionFailed"));
}

#[test]
fn verify_takes_a_lost_answer_as_done_when_the_email_went_out() {
    let d = Dir::new("verify-lost", "verify(email_sent(to, subject))");
    let out = d.pay("R1", "A100", "__lost__");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(d.sent(), 1, "sent once, not repeated");
}

#[test]
fn verify_repeats_a_call_that_did_not_happen() {
    let d = Dir::new("verify-redo", "verify(email_sent(to, subject))");
    let out = d.pay("R1", "A100", "__flaky__");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(d.sent(), 1);
}

#[test]
fn verify_finds_what_a_lost_call_made_and_goes_on_with_it() {
    let d = Dir::new("verify-finds", "pause");
    // The server sends it, then reports a timeout of the service behind it:
    // the call may have happened, so `verify` looks for it.
    let out = d.calyx(&["run", "p.clyx", "--graph", "send", "--body", "__gateway__"]);
    let err = text(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("failed: Timeout: the mail gateway"), "{err}");
    assert!(err.contains("verify found what it made"), "{err}");
    assert_eq!(d.sent(), 1, "sent once, not repeated");
    assert_eq!(
        text(&out.stdout).trim(),
        "msg-1",
        "its answer is what was found"
    );
    // Replaying the run gives the same answer, from the journal.
    let out = d.calyx(&["replay", &run_id(&out), "--quiet"]);
    assert_eq!(text(&out.stdout).trim(), "msg-1", "{}", text(&out.stderr));
}

#[test]
fn a_person_gives_the_answer_of_a_call_that_has_one() {
    let d = Dir::new("done-answer", "pause");
    // Sent, then no answer: the run stops for a person.
    let out = d.calyx(&["run", "p.clyx", "--graph", "post_one", "--body", "__lost__"]);
    let err = text(&out.stderr);
    assert!(!out.status.success(), "{err}");
    let id = run_id(&out);
    // `done` alone is not enough: the program uses the answer.
    let out = d.calyx(&["resume", &id, "--quiet", "--uncertain", "done"]);
    assert!(
        text(&out.stderr).contains("--uncertain done=<answer>"),
        "{}",
        text(&out.stderr)
    );
    // The person found the message and gives its id.
    let out = d.calyx(&["resume", &id, "--quiet", "--uncertain", "done=msg-1"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout).trim(), "msg-1");
    assert_eq!(d.sent(), 1, "not sent again");
}

#[test]
fn a_batch_cut_in_half_sends_only_what_is_missing() {
    let d = Dir::new("batch-half", "pause");
    // The store sends 2 of the 4 e-mails and crashes.
    let out = d.calyx(&[
        "run",
        "p.clyx",
        "--graph",
        "notify",
        "--to",
        "metade@exemplo.org",
    ]);
    let err = text(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("repeated with the 2 of 4 items verify did not find"),
        "{err}"
    );
    let subjects: Vec<String> = d.store()["outbox"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["subject"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(subjects, ["a", "b", "c", "d"], "each once, in order");
}

#[test]
fn accept_loss_goes_on_without_knowing() {
    let d = Dir::new("accept", "accept_loss");
    let out = d.pay("R1", "A100", "__down__");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(d.sent(), 0, "the loss it accepted");
}

#[test]
fn pause_stops_and_a_person_decides_when_resuming() {
    let d = Dir::new("pause", "pause");
    let out = d.pay("R1", "A100", "__lost__");
    assert_eq!(out.status.code(), Some(3));
    let err = text(&out.stderr);
    assert!(
        err.contains("may or may not have happened (Timeout"),
        "{err}"
    );
    assert!(err.contains("--uncertain done"), "{err}");
    let id = run_id(&out);
    assert_eq!(d.sent(), 1);

    // Resuming without a decision stops again; nothing is sent.
    let out = d.calyx(&["resume", &id, "--quiet"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(text(&out.stderr).contains("the run stopped while it was in progress"));
    assert_eq!(d.sent(), 1);

    // The person checked: it was sent.
    let out = d.calyx(&["resume", &id, "--quiet", "--uncertain", "done"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "ok\n");
    assert_eq!(d.sent(), 1);
    assert_eq!(d.refunded("A100"), 100.0, "the refund was not repeated");

    // The decision is in the journal: replay needs no decision.
    let out = d.calyx(&["replay", &id, "--quiet"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn a_person_can_have_an_uncertain_call_retried() {
    let d = Dir::new("retry", "pause");
    let out = d.pay("R1", "A100", "__flaky__");
    assert_eq!(out.status.code(), Some(3));
    let id = run_id(&out);
    assert_eq!(d.sent(), 0);
    let out = d.calyx(&["resume", &id, "--quiet", "--uncertain", "retry"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(d.sent(), 1);
}

/// The fake store says what each tool does (MCP annotations): `email` is
/// neither read-only nor idempotent, `email_sent` is read-only.
const MISDECLARED: &str = r#"
tool email(to: Text, subject: Text, body: Text) -> Unit:
    effect write

tool email_sent(to: Text, subject: Text) -> Bool:
    effect read

tool get_order(id: Text) -> Text:
    effect read

graph g(to: Text) -> Text:
    sent = email(to, "oi", "corpo")
    return "ok"
"#;

#[test]
fn check_tools_compares_declarations_with_what_the_servers_say() {
    let d = Dir::with_program("tools-ok", &program("verify(email_sent(to, subject))"));
    let out = d.calyx(&["check", "p.clyx", "--tools"]);
    assert!(out.status.success(), "{}", text(&out.stdout));
    assert_eq!(
        text(&out.stdout),
        "",
        "the declarations agree with the server"
    );

    // `get_order` renamed to a tool no server has.
    let d = Dir::with_program(
        "tools-bad",
        &MISDECLARED.replace("get_order", "email_sent2"),
    );
    let out = d.calyx(&["check", "p.clyx", "--tools"]);
    let report = text(&out.stdout);
    assert!(
        report.contains("warning[W0702]: tool `email` is a `write` without a key"),
        "{report}"
    );
    assert!(
        report.contains("error[E0701]: no MCP server for tool `email_sent2`"),
        "{report}"
    );
    assert!(!report.contains("`email_sent`"), "{report}");
    assert_eq!(
        out.status.code(),
        Some(1),
        "a tool with no server is an error"
    );
}

#[test]
fn a_run_warns_once_when_a_server_contradicts_a_declaration() {
    // `email` declared `read`: the server says it is not read-only.
    let d = Dir::with_program(
        "tools-run",
        &MISDECLARED.replace("    effect write\n", "    effect read\n"),
    );
    let out = d.calyx(&["run", "p.clyx", "--to", "ana@exemplo.org"]);
    let err = text(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert_eq!(
        err.matches("[W0701]: tool `email` is declared `read`")
            .count(),
        1,
        "{err}"
    );
    assert_eq!(d.sent(), 1);
}
