//! Saga (D12): a race branch that loses has its compensated writes undone,
//! once, also across a crash, and only after they finish.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("calyx-saga-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        std::fs::copy(examples.join("saga.clyx"), dir.join("saga.clyx")).unwrap();
        let store = examples.join("tools/fake_store.py").canonicalize().unwrap();
        let toml: String = ["charge", "uncharge"]
            .iter()
            .map(|t| {
                format!(
                    "[tools.{t}]\ncommand = [\"python3\", \"{}\"]\n",
                    store.display()
                )
            })
            .collect();
        std::fs::write(dir.join("calyx.toml"), toml).unwrap();
        Dir(dir)
    }

    fn calyx(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(args)
            .current_dir(&self.0)
            .env("CALYX_FAKE_STORE", self.0.join("store.json"))
            .envs(env.iter().copied())
            .output()
            .unwrap()
    }

    fn count(&self, list: &str) -> usize {
        let text = std::fs::read_to_string(self.0.join("store.json")).unwrap_or_default();
        let db: Value = serde_json::from_str(&text).unwrap_or_default();
        db[list].as_array().map_or(0, Vec::len)
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

fn run_id(err: &str) -> String {
    err.lines()
        .find_map(|l| l.strip_prefix("calyx: run "))
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn the_losing_branch_that_paid_is_undone_once() {
    let d = Dir::new("once");
    let out = d.calyx(&["run", "saga.clyx", "--request", "R1"], &[]);
    let err = text(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert_eq!(
        text(&out.stdout).trim_start().get(..14),
        Some("paid by wallet")
    );
    assert!(
        err.contains("undo  uncharge  (branch `card` lost)"),
        "{err}"
    );
    assert_eq!((d.count("charges"), d.count("uncharged")), (1, 1));
}

#[test]
fn a_crash_after_the_race_still_undoes_the_loser_on_resume() {
    let d = Dir::new("crash");
    // The third entry is the race's winner: the run dies before the undo.
    let first = d.calyx(
        &["run", "saga.clyx", "--request", "R1"],
        &[("CALYX_CRASH_AFTER", "3")],
    );
    let err = text(&first.stderr);
    assert!(!first.status.success(), "{err}");
    assert!(err.contains("CALYX_CRASH_AFTER=3 reached"), "{err}");
    assert_eq!((d.count("charges"), d.count("uncharged")), (1, 0));

    let resumed = d.calyx(&["resume", &run_id(&err)], &[]);
    let rerr = text(&resumed.stderr);
    assert!(resumed.status.success(), "{rerr}");
    assert!(rerr.contains("from the journal"), "{rerr}");
    assert!(rerr.contains("undo  uncharge"), "{rerr}");
    assert_eq!((d.count("charges"), d.count("uncharged")), (1, 1));

    // Resumed again (or replayed), the undo comes from the journal.
    let again = d.calyx(&["replay", &run_id(&err)], &[]);
    assert!(again.status.success(), "{}", text(&again.stderr));
    assert_eq!(d.count("uncharged"), 1);
}

#[test]
fn a_charge_still_in_flight_is_undone_after_it_lands() {
    let d = Dir::new("inflight");
    // The card's charge answers 1.5 s after charging; the wallet wins at
    // 0.5 s, while the charge is still on its way.
    let out = d.calyx(&["run", "saga.clyx", "--request", "R1__slowcharge__"], &[]);
    let err = text(&out.stderr);
    assert!(out.status.success(), "{err}");
    let charged = err.find("write charge").expect(&err);
    let undone = err.find("write uncharge").expect(&err);
    assert!(charged < undone, "the undo waited for the charge:\n{err}");
    assert_eq!((d.count("charges"), d.count("uncharged")), (1, 1));
}
