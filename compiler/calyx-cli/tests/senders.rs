//! Signed deliveries: only the senders calyx.toml allows can answer a
//! `receive` of a limited message type (`calyx keys new`, `deliver --as`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROGRAM: &str = r#"
message Approval = Approved | Denied(reason: Text)

tool refund(request: Text, order: Text, amount: Float) -> Unit:
    effect write
    idempotency_key request

graph approve(request: Text) -> Text:
    decision = receive Approval, timeout 1 days:
        on timeout: Denied(reason="timeout")
    return match decision:
        case Approved:
            paid = refund(request, "A100", 10)
            "paid"
        case Denied(reason): reason
"#;

fn calyx(d: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(args)
        .current_dir(d)
        .env("CALYX_KEYS_DIR", d.join("keys"))
        .env("CALYX_FAKE_STORE", d.join("store.json"))
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The `ed25519:...` line `calyx keys new` prints.
fn new_key(d: &Path, name: &str) -> String {
    let out = calyx(d, &["keys", "new", name]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    text(&out.stdout)
        .lines()
        .find(|l| l.starts_with(&format!("{name} = ")))
        .unwrap()
        .to_owned()
}

fn setup(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("calyx-senders-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("p.clyx"), PROGRAM).unwrap();
    let ana = new_key(&d, "ana");
    let eve = new_key(&d, "eve");
    let store =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/tools/fake_store.py");
    std::fs::write(
        d.join("calyx.toml"),
        format!(
            "[tools.refund]\ncommand = [\"python3\", {:?}]\n\n[senders]\n{ana}\n{eve}\n\n[messages.Approval]\nsenders = [\"ana\"]\n",
            store.to_str().unwrap()
        ),
    )
    .unwrap();
    d
}

fn start(d: &Path) -> String {
    let out = calyx(d, &["run", "p.clyx", "--request", "r1"]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    text(&out.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("calyx: run ").map(str::to_owned))
        .unwrap()
}

#[test]
fn only_an_allowed_sender_can_approve() {
    let d = setup("allowed");
    let id = start(&d);
    // Unsigned, or signed by someone not allowed: refused at once.
    let out = calyx(&d, &["deliver", &id, "Approval", "Approved"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("must be signed by one of: ana"),
        "{}",
        text(&out.stderr)
    );
    let out = calyx(&d, &["deliver", &id, "Approval", "Approved", "--as", "eve"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("`eve` may not send `Approval`"),
        "{}",
        text(&out.stderr)
    );
    // eve's key under ana's name.
    let eve_key = d.join("keys/eve.ed25519");
    let out = calyx(
        &d,
        &[
            "deliver",
            &id,
            "Approval",
            "Approved",
            "--as",
            "ana",
            "--key",
            eve_key.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("this is not `ana`'s key"),
        "{}",
        text(&out.stderr)
    );

    let out = calyx(&d, &["deliver", &id, "Approval", "Approved", "--as", "ana"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = calyx(&d, &["resume", &id]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout).trim(), "paid");
    assert!(
        text(&out.stderr).contains("delivered, signed by ana"),
        "{}",
        text(&out.stderr)
    );
    let journal =
        std::fs::read_to_string(d.join(format!(".calyx/runs/{id}/journal.jsonl"))).unwrap();
    assert!(journal.contains("\"sender\":\"ana\""), "{journal}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_approval_written_into_the_inbox_by_hand_is_not_taken() {
    let d = setup("forged");
    let id = start(&d);
    let waits = std::fs::read_to_string(d.join(format!(".calyx/runs/{id}/waits.jsonl"))).unwrap();
    let key = waits
        .split("\"key\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    // Unsigned, and signed as ana with a made-up signature.
    let inbox = d.join(format!(".calyx/runs/{id}/inbox.jsonl"));
    let forged = format!(
        "{{\"key\":\"{key}\",\"message\":\"Approval\",\"value\":\"Approved\",\"at\":1}}\n{{\"key\":\"{key}\",\"message\":\"Approval\",\"value\":\"Approved\",\"at\":1,\"sender\":\"ana\",\"sig\":\"ed25519:{}\"}}\n",
        "00".repeat(64)
    );
    std::fs::write(&inbox, forged).unwrap();
    let out = calyx(&d, &["resume", &id]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "still waiting: {}",
        text(&out.stderr)
    );
    let err = text(&out.stderr);
    assert!(
        err.contains("delivery refused: `Approval` must be signed"),
        "{err}"
    );
    assert!(
        err.contains("delivery refused: the signature is not `ana`'s"),
        "{err}"
    );
    let store = std::fs::read_to_string(d.join("store.json")).unwrap_or_default();
    assert!(
        !store.contains("\"request\": \"r1\""),
        "nothing was paid: {store}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_key_is_never_replaced_and_a_listed_name_needs_a_key() {
    let d = setup("config");
    let out = calyx(&d, &["keys", "new", "ana"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("a key is never replaced"),
        "{}",
        text(&out.stderr)
    );
    std::fs::write(
        d.join("calyx.toml"),
        "[messages.Approval]\nsenders = [\"bob\"]\n",
    )
    .unwrap();
    let out = calyx(&d, &["run", "p.clyx", "--request", "r9"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("`bob` is not in [senders]"),
        "{}",
        text(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&d);
}
