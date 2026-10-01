//! Concurrency derived from the graph (milestone M4), with fake models that
//! take a fixed time (`fake-slow-<ms>`). Timing bounds are generous: they
//! tell parallel from sequential, not exact durations.

use std::path::{Path, PathBuf};
use std::time::Instant;

use calyx_runtime::{Config, Mode, RunOptions, run};
use serde_json::{Value, json};

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples")
}

fn compile(src: &str) -> String {
    let report = calyx_check::check("t.clyx", src);
    assert!(!report.has_errors(), "{}", report.render());
    report.ir.to_json()
}

fn opts() -> RunOptions {
    RunOptions {
        config: Config::load(&examples().join("calyx.toml")).unwrap(),
        ..RunOptions::default()
    }
}

const PRELUDE: &str = r#"
model slow = "fake-slow-200"
model quick = "fake-model"

prompt echo(x: Text) -> Text:
    """{x}"""

prompt join(a: Text, b: List[Text]) -> Text:
    """{a} {b}"""
"#;

fn timed(body: &str, args: Value, o: RunOptions) -> (Result<Value, String>, f64) {
    let ir = compile(&format!("{PRELUDE}\n{body}"));
    let started = Instant::now();
    let out = run(&ir, "g", &args, o);
    (out, started.elapsed().as_secs_f64())
}

const FAN_OUT: &str = r#"
graph g(xs: List[Text]) -> List[Text]:
    answers = for each x in xs: slow(echo(x))
    return answers
"#;

fn eight() -> Value {
    json!({"xs": ["a", "b", "c", "d", "e", "f", "g", "h"]})
}

#[test]
fn independent_calls_run_at_the_same_time() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // 8 calls of 200 ms: about 1.6 s one after the other.
    let (out, secs) = timed(FAN_OUT, eight(), opts());
    let out = out.unwrap();
    assert!(secs < 0.8, "took {secs:.2} s");
    // Results keep the order of the list, whatever finished first (D7).
    let first = out[0].as_str().unwrap();
    let last = out[7].as_str().unwrap();
    assert!(first.ends_with(": a]"), "{first}");
    assert!(last.ends_with(": h]"), "{last}");
}

#[test]
fn deterministic_mode_runs_one_call_at_a_time() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let parallel = timed(FAN_OUT, eight(), opts()).0.unwrap();
    let (out, secs) = timed(
        FAN_OUT,
        eight(),
        RunOptions {
            deterministic: true,
            ..opts()
        },
    );
    assert!(secs >= 1.5, "took {secs:.2} s");
    // Same result either way.
    assert_eq!(out.unwrap(), parallel);
}

#[test]
fn threads_limits_the_calls_in_flight() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let body = FAN_OUT.replace("    answers =", "    limits threads 2\n    answers =");
    // 8 calls of 200 ms, 2 at a time: about 0.8 s.
    let (out, secs) = timed(&body, eight(), opts());
    out.unwrap();
    assert!((0.75..1.5).contains(&secs), "took {secs:.2} s");
}

#[test]
fn rate_spaces_the_calls() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let body = r#"
graph g(xs: List[Text]) -> List[Text]:
    limits rate 10/s
    answers = for each x in xs: quick(echo(x))
    return answers
"#;
    // 8 instant calls at 10 per second: the last starts after 0.7 s.
    let (out, secs) = timed(body, eight(), opts());
    out.unwrap();
    assert!(secs >= 0.65, "took {secs:.2} s");
}

#[test]
fn chains_and_independent_branches_overlap() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // `first -> second` (400 ms) and `side` (200 ms) are independent.
    let body = r#"
graph g(x: Text) -> Text:
    first = slow(echo(x))
    second = slow(echo(first))
    side = for each y in [x, x, x]: slow(echo(y))
    return quick(join(second, side))
"#;
    let (out, secs) = timed(body, json!({"x": "oi"}), opts());
    out.unwrap();
    assert!(secs < 0.6, "took {secs:.2} s");
}

#[test]
fn the_critical_path_goes_first() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // With one call at a time, the long chain (rank 12) starts before the
    // independent items (rank 6), though they come first in the source.
    let body = r#"
graph g(x: Text) -> Text:
    side = for each y in [x, x]: quick(echo(y))
    first = quick(echo(x))
    second = quick(echo(first))
    third = quick(echo(second))
    return quick(join(third, side))
"#;
    let dir = std::env::temp_dir().join(format!("calyx-critical-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (out, _) = timed(
        body,
        json!({"x": "oi"}),
        RunOptions {
            deterministic: true,
            journal: Some(dir.clone()),
            ..opts()
        },
    );
    out.unwrap();
    let journal = std::fs::read_to_string(dir.join("journal.jsonl")).unwrap();
    let first_call = journal
        .lines()
        .find(|l| l.contains("\"type\":\"call\""))
        .unwrap();
    assert!(first_call.contains("\"key\":\"g/first#0\""), "{first_call}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_used_up_budget_stops_the_run_and_a_larger_one_continues_it() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let body = r#"
graph g(x: Text) -> Text:
    limits budget 1 USD
    a = quick(echo(x))
    b = quick(echo(a))
    return quick(echo(b))
"#;
    let mut config = Config::load(&examples().join("calyx.toml")).unwrap();
    // A price high enough that one call spends the budget.
    config
        .prices
        .insert("fake-model".into(), (1_000_000.0, 1_000_000.0));
    let dir = std::env::temp_dir().join(format!("calyx-budget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let with = |mode: Mode, budget_usd: Option<f64>| RunOptions {
        config: config.clone(),
        journal: Some(dir.clone()),
        mode,
        budget_usd,
        ..RunOptions::default()
    };
    let (out, _) = timed(body, json!({"x": "oi"}), with(Mode::New, None));
    let err = out.unwrap_err();
    assert!(err.contains("the budget of 1.00 USD is used up"), "{err}");

    let (out, _) = timed(body, json!({"x": "oi"}), with(Mode::Resume, Some(1e9)));
    assert!(out.is_ok(), "{out:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deterministic_runs_make_the_calls_in_the_same_order() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let body = r#"
graph g(xs: List[Text]) -> Text:
    side = for each x in xs: quick(echo(x))
    first = quick(echo("a"))
    second = quick(echo(first))
    return quick(join(second, side))
"#;
    let order = |n: u32| {
        let dir = std::env::temp_dir().join(format!("calyx-order-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (out, _) = timed(
            body,
            eight(),
            RunOptions {
                deterministic: true,
                journal: Some(dir.clone()),
                ..opts()
            },
        );
        out.unwrap();
        let journal = std::fs::read_to_string(dir.join("journal.jsonl")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        journal
            .lines()
            .filter(|l| l.contains("\"type\":\"call\""))
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                v["key"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>()
    };
    let first = order(1);
    for n in 2..5 {
        assert_eq!(order(n), first);
    }
    // The longest path first; `second` and the items tie (two calls from
    // the end), and ties keep creation order: the items, in list order.
    assert_eq!(first[0], "g/first#0");
    assert_eq!(first[1], "g/side[0]#0");
    assert_eq!(first[8], "g/side[7]#0");
    assert_eq!(first[9], "g/second#0");
}
