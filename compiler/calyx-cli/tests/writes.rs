//! External writes through the CLI (milestone M6): idempotency keys,
//! preconditions checked by the tool (D29) and the `on_uncertain` policies
//! of `write once` (D2), live and when resuming.
//!
//! The tools run on the fake store (examples/tools/fake_store.py), whose
//! state is a JSON file per test. Words in an e-mail's body simulate
//! failures: `__lost__` (sent, then no answer), `__down__` (crash before
//! sending), `__flaky__` (crash before sending, the first time only).

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
        let dir = std::env::temp_dir().join(format!("calyx-writes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/tools/fake_store.py")
            .canonicalize()
            .unwrap();
        let cmd = format!("command = [\"python3\", \"{}\"]\n", store.display());
        let toml: String = ["refund", "email", "email_sent"]
            .iter()
            .map(|t| format!("[tools.{t}]\n{cmd}\n"))
            .collect();
        std::fs::write(dir.join("calyx.toml"), toml).unwrap();
        std::fs::write(dir.join("p.clyx"), program(policy)).unwrap();
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
