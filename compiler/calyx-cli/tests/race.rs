//! `rounds` and `race` through the CLI (milestone M9): the items of a round
//! run at once and a resumed run continues in the round where it stopped
//! (decision D18); the first branch that passes wins a race, the others
//! stop, and the winner is decided once, in the journal (decision D12).
//!
//! The tool is examples/tools/fake_search.py: `__slow__` makes a search
//! take 1.5 s, `__fail__` makes it fail.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROGRAM: &str = r#"
model m = "fake-model"

tool web_search(query: Text) -> Text:
    effect read

prompt say(role: Text, answers: List[Text]) -> Text:
    """{role}: {answers}"""

prompt solve(q: Text) -> Text:
    """resolva {q}"""

graph debate(q: Text) -> Text:
    roles = ["a", "b", "c"]
    start = for each r in roles:
        m(say(r, [q]))
    final = rounds 2, carry answers = start:
        turn = for each r in roles:
            m(say(r, answers))
        next turn
    return m(say("juiz", final))

graph slow(q: Text) -> Text:
    first = web_search("__slow__ {q}")
    second = web_search("depois de {first}")
    return second

graph pick(q: Text) -> Text:
    best = race first where len(it) > 0:
        slow: slow(q)
        quick: m(solve(q))
        on none: fail "nada"
    return best

graph nobody(q: Text) -> Text:
    best = race first:
        one: web_search("__fail__ {q}")
        two: web_search("__fail__ {q}?")
        on none: fail "nenhuma busca respondeu"
    return best

graph fallback(q: Text) -> Text:
    best = race first where len(it) > 1000:
        one: web_search(q)
        two: m(solve(q))
        on none: "nenhuma resposta longa"
    return best
"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("calyx-race-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.clyx"), PROGRAM).unwrap();
        let tool = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/tools/fake_search.py")
            .canonicalize()
            .unwrap();
        std::fs::write(
            dir.join("calyx.toml"),
            format!(
                "[tools.web_search]\ncommand = [\"python3\", \"{}\"]\n",
                tool.display()
            ),
        )
        .unwrap();
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

    fn run(&self, graph: &str, crash_after: Option<u32>) -> Output {
        self.calyx(
            &[
                "run",
                "p.clyx",
                "--graph",
                graph,
                "--fake-models",
                "--q",
                "x",
            ],
            crash_after,
        )
    }

    fn journal(&self, out: &Output) -> String {
        let id = run_id(out);
        std::fs::read_to_string(self.0.join(".calyx/runs").join(id).join("journal.jsonl")).unwrap()
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
fn each_round_and_each_item_has_its_own_calls() {
    let d = Dir::new("rounds");
    let out = d.run("debate", None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    // How many run at once depends on the machine; how many run does not.
    assert!(err.contains("10 model call(s)"), "{err}");
    // One key per round and per item.
    let journal = d.journal(&out);
    for round in 0..2 {
        for item in 0..3 {
            let key = format!("debate/final#2.{round}#1[{item}]#0");
            assert!(journal.contains(&key), "{key} not in {journal}");
        }
    }
}

#[test]
fn a_resumed_run_continues_in_the_round_where_it_stopped() {
    let d = Dir::new("rounds-resume");
    // 3 opening calls and 2 of the first round reach the journal.
    let out = d.run("debate", Some(5));
    assert!(!out.status.success());
    let id = run_id(&out);
    let out = d.calyx(&["resume", &id, "--fake-models"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("5 model call(s)"), "{err}");
    assert!(err.contains("5 taken from the journal"), "{err}");
}

#[test]
fn the_first_branch_that_passes_wins_and_the_others_stop() {
    let d = Dir::new("pick");
    let out = d.run("pick", None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("resolva x"));
    let err = text(&out.stderr);
    assert!(err.contains("race  won by `quick`"), "{err}");
    // The losing branch stopped after its first call: the second never ran.
    let journal = d.journal(&out);
    assert!(journal.contains("\"winner\":\"quick\""), "{journal}");
    assert!(!journal.contains("/slow/second#"), "{journal}");
}

#[test]
fn resume_and_replay_take_the_winner_from_the_journal() {
    let d = Dir::new("replay");
    let out = d.run("pick", None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let first = text(&out.stdout);
    let id = run_id(&out);
    let out = d.calyx(&["replay", &id], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), first);
    let err = text(&out.stderr);
    assert!(err.contains("won by `quick`  from the journal"), "{err}");
    assert!(err.contains("0 model call(s)"), "{err}");
}

#[test]
fn failed_branches_lose_and_on_none_decides() {
    let d = Dir::new("none");
    let out = d.run("nobody", None);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("nenhuma busca respondeu"));

    let out = d.run("fallback", None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "nenhuma resposta longa\n");
    assert!(text(&out.stderr).contains("race  no branch won"));
}
