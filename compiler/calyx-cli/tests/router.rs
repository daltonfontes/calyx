//! Routers through the CLI (milestone M10, decision D30): the models are
//! tried in order until an answer passes the check, the choice goes to the
//! journal, and with no answer passing the call fails where `try` can
//! catch it. A missing setup (an API key) stops the run instead.
//!
//! Fake models: `fake-unavailable` always fails, `fake-unsure` answers
//! `false` to every yes/no, `fake-model` answers `true`.

use std::path::PathBuf;
use std::process::{Command, Output};

const PROGRAM: &str = r#"
model down = "fake-unavailable"
model unsure = "fake-unsure"
model strong = "fake-model"
model remote = "gemini-3.5-flash-lite"

type Answer:
    text: Text
    confident: Bool

def sure(a: Answer) -> Bool:
    return a.confident

router smart = route [down, unsure, strong]:
    policy cheapest_that_passes(sure)

router weak = route [down, unsure]:
    policy cheapest_that_passes(sure)

router keyless = route [remote, strong]:
    policy cheapest_that_passes(sure)

prompt ask(q: Text) -> Answer:
    """{q}"""

graph escalate(q: Text) -> Text:
    a = smart(ask(q))
    return a.text

graph nobody(q: Text) -> Text:
    r = try weak(ask(q))
    return match r:
        case Ok(a): a.text
        case Failed(error): "falhou: {error}"

graph setup(q: Text) -> Text:
    r = try keyless(ask(q))
    return match r:
        case Ok(a): a.text
        case Failed(error): "falhou"
"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("calyx-router-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.clyx"), PROGRAM).unwrap();
        Dir(dir)
    }

    fn calyx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(args)
            .current_dir(&self.0)
            .env("CALYX_RETRY_BASE_MS", "1")
            .env_remove("GEMINI_API_KEY")
            .output()
            .unwrap()
    }

    fn run(&self, graph: &str) -> Output {
        self.calyx(&["run", "p.clyx", "--graph", graph, "--q", "oi"])
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
    text(&out.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("calyx: run "))
        .unwrap()
        .to_owned()
}

#[test]
fn models_are_tried_in_order_until_an_answer_passes() {
    let d = Dir::new("escalate");
    let out = d.run("escalate");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    // The first fails, the second is not sure, the third passes.
    let order: Vec<usize> = ["fake-unavailable", "fake-unsure(ask)", "fake-model(ask)"]
        .iter()
        .map(|m| err.find(m).unwrap_or_else(|| panic!("{m} not in {err}")))
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{err}");
    assert!(err.contains("route smart  -> fake-model"), "{err}");

    // The choice is in the journal: replay takes it from there.
    let id = run_id(&out);
    let journal =
        std::fs::read_to_string(d.0.join(".calyx/runs").join(&id).join("journal.jsonl")).unwrap();
    assert!(journal.contains("\"model\":\"fake-model\""), "{journal}");
    let replay = d.calyx(&["replay", &id]);
    assert!(replay.status.success(), "{}", text(&replay.stderr));
    assert_eq!(text(&replay.stdout), text(&out.stdout));
    assert!(text(&replay.stderr).contains("route smart  -> fake-model  from the journal"));
}

#[test]
fn with_no_answer_passing_the_call_fails_and_try_catches_it() {
    let d = Dir::new("nobody");
    let out = d.run("nobody");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("router `weak`: no model's answer passed `sure`"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn a_missing_api_key_stops_the_run_even_inside_try() {
    let d = Dir::new("setup");
    let out = d.run("setup");
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stdout));
    assert!(text(&out.stderr).contains("GEMINI_API_KEY"));
}
