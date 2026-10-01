//! Milestone M5: loops, `match`, `if`, operators, records, `try` and
//! agents, run with fake models (no network).

use std::path::{Path, PathBuf};

use calyx_runtime::{Config, Mode, RunOptions, run};
use serde_json::{Value, json};

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples")
}

const PRELUDE: &str = r#"
model m = "fake-model"
model stuck = "fake-stuck"
model busy = "fake-busy"

tool web_search(query: Text) -> Text:
    effect read
    max_output 500 tokens
    description "Busca na web."

type Review = Approved | Rejected(feedback: Text)

type Point:
    x: Int
    y: Int

prompt echo(x: Text) -> Text:
    """{x}"""

prompt review(x: Text) -> Review:
    """{x}"""

prompt points(x: Text) -> Point:
    """{x}"""
"#;

fn go(body: &str, args: Value) -> Result<Value, String> {
    go_with(body, args, RunOptions::default())
}

fn go_with(body: &str, args: Value, extra: RunOptions) -> Result<Value, String> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let report = calyx_check::check("t.clyx", &format!("{PRELUDE}\n{body}"));
    assert!(!report.has_errors(), "{}", report.render());
    let opts = RunOptions {
        config: Config::load(&examples().join("calyx.toml")).unwrap(),
        ..extra
    };
    // SAFETY: tests hold LOCK; nothing else reads the environment concurrently.
    unsafe { std::env::set_var("CALYX_RETRY_BASE_MS", "1") };
    run(&report.ir.to_json(), "g", &args, opts)
}

#[test]
fn loops_carry_a_value_until_done() {
    let out = go(
        r#"
graph g(n: Int) -> Int:
    counted = loop i = 0, max 10:
        if i < n:
            next i + 1
        else:
            done i * 100
        on limit: last
    return counted
"#,
        json!({"n": 3}),
    );
    assert_eq!(out.unwrap(), json!(300));
}

#[test]
fn a_loop_at_its_limit_keeps_the_last_value_or_fails() {
    let body = r#"
graph g(n: Int) -> Int:
    counted = loop i = 0, max 3:
        next i + n
        on limit: last
    return counted
"#;
    assert_eq!(go(body, json!({"n": 2})).unwrap(), json!(6));

    let failing = body.replace("on limit: last", "on limit: fail \"too many turns\"");
    let err = go(&failing, json!({"n": 2})).unwrap_err();
    assert_eq!(err, "in graph `g`, node `counted`: too many turns");
}

#[test]
fn match_takes_the_variant_apart() {
    let body = r#"
graph g(x: Text) -> Text:
    r = m(review(x))
    said = match r:
        case Approved: "aprovado"
        case Rejected(feedback): "rejeitado: " + feedback
    return said
"#;
    assert_eq!(go(body, json!({"x": "ok"})).unwrap(), json!("aprovado"));
    // The fake model picks the variant named in brackets.
    let out = go(body, json!({"x": "[Rejected] por favor"})).unwrap();
    assert_eq!(out, json!("rejeitado: item falso 1 ([Rejected] por favor)"));
}

#[test]
fn a_review_loop_rewrites_until_approved() {
    let body = r#"
graph g(x: Text) -> Text:
    final = loop text = x, max 3:
        match m(review(text)):
            case Approved: done text
            case Rejected(feedback): next "ok"
        on limit: fail "never approved"
    return final
"#;
    // First turn rejected (the text names it), second approved.
    assert_eq!(go(body, json!({"x": "[Rejected]"})).unwrap(), json!("ok"));
}

#[test]
fn operators_and_records() {
    let out = go(
        r#"
graph g(a: Int, b: Int) -> Point:
    p = Point(x=a * 2 + b, y=-a)
    ok = p.x > 10 and not (p.y == 0) or a == b
    names = ["a"] + ["b", "c"]
    shown = if ok: "{p.x}/{names}"
    else: "no"
    return Point(x=p.x, y=p.y)
"#,
        json!({"a": 5, "b": 3}),
    );
    assert_eq!(out.unwrap(), json!({"x": 13, "y": -5}));
}

#[test]
fn try_turns_failures_into_values() {
    let out = go(
        r#"
graph g(q: Text) -> Text:
    r = try web_search(q)
    said = match r:
        case Ok(value): "ok"
        case Failed(error): "falhou: " + error
    return said
"#,
        json!({"q": "__fail__"}),
    );
    assert_eq!(
        out.unwrap(),
        json!("falhou: tool `web_search` failed: ToolError: falha simulada")
    );
}

#[test]
fn try_catches_a_failed_loop_and_a_failed_subgraph() {
    let out = go(
        r#"
graph inner(q: Text) -> Text:
    return web_search(q)

graph g(q: Text) -> Text:
    a = try inner(q)
    b = try loop i = 0, max 2:
        next i + 1
        on limit: fail "limite"
    said = match a:
        case Ok(value): "?"
        case Failed(first):
            match b:
                case Ok(_): "?"
                case Failed(second): "ambos falharam"
    return said
"#,
        json!({"q": "__fail__"}),
    );
    assert_eq!(out.unwrap(), json!("ambos falharam"));
}

#[test]
fn an_agent_calls_tools_and_answers() {
    let out = go(
        r#"
graph g(q: Text) -> Text:
    found = agent m:
        tools [web_search]
        max_turns 5
        task echo(q)
        on turn_limit: final_answer
        on stuck: fail "stuck"
    return found
"#,
        json!({"q": "energia solar"}),
    );
    assert_eq!(
        out.unwrap(),
        json!("[resposta falsa para: energia solar] (com 1 observação(ões))")
    );
}

#[test]
fn an_agent_answers_in_the_task_type() {
    let out = go(
        r#"
graph g(q: Text) -> Point:
    p = agent m:
        tools [web_search]
        max_turns 3
        task points(q)
        on turn_limit: final_answer
        on stuck: final_answer
    return p
"#,
        json!({"q": "x"}),
    );
    assert_eq!(out.unwrap(), json!({"x": 1, "y": 1}));
}

#[test]
fn a_stuck_agent_fails_or_answers() {
    let body = r#"
graph g(q: Text) -> Text:
    found = agent stuck:
        tools [web_search]
        max_turns 10
        task echo(q)
        on turn_limit: final_answer
        on stuck: fail "repetindo a mesma busca"
    return found
"#;
    let err = go(body, json!({"q": "x"})).unwrap_err();
    assert_eq!(err, "in graph `g`, node `found`: repetindo a mesma busca");
    let answers = body.replace(
        "on stuck: fail \"repetindo a mesma busca\"",
        "on stuck: final_answer",
    );
    let out = go(&answers, json!({"q": "x"})).unwrap();
    // Asked for an answer after three identical turns (two were run).
    assert_eq!(
        out,
        json!("[resposta falsa para: x] (com 2 observação(ões))")
    );
}

#[test]
fn an_agent_at_its_turn_limit_gives_a_final_answer() {
    let dir = std::env::temp_dir().join(format!("calyx-m5-busy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let body = r#"
graph g(q: Text) -> Text:
    found = agent busy:
        tools [web_search]
        max_turns 3
        task echo(q)
        on turn_limit: final_answer
        on stuck: fail "stuck"
    return found
"#;
    let out = go_with(
        body,
        json!({"q": "x"}),
        RunOptions {
            journal: Some(dir.clone()),
            ..RunOptions::default()
        },
    );
    assert_eq!(
        out.unwrap(),
        json!("[resposta falsa para: x] (com 3 observação(ões))")
    );
    let journal = std::fs::read_to_string(dir.join("journal.jsonl")).unwrap();
    let keys: Vec<String> = journal
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["type"] == "call")
        .map(|e| e["key"].as_str().unwrap().to_owned())
        .collect();
    // 3 turns, each with a tool call, then the final answer.
    assert_eq!(
        keys,
        [
            "g/found#0.t0",
            "g/found#0.t0.c0",
            "g/found#0.t1",
            "g/found#0.t1.c0",
            "g/found#0.t2",
            "g/found#0.t2.c0",
            "g/found#0.final"
        ]
    );
    // The agent replays from its journal without calling anything.
    let replay = go_with(
        body,
        json!({"q": "x"}),
        RunOptions {
            journal: Some(dir.clone()),
            mode: Mode::Replay,
            ..RunOptions::default()
        },
    );
    assert_eq!(
        replay.unwrap(),
        json!("[resposta falsa para: x] (com 3 observação(ões))")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_tool_call_is_an_observation_for_the_agent() {
    let out = go(
        r#"
graph g(q: Text) -> Text:
    found = agent m:
        tools [web_search]
        max_turns 3
        task echo(q)
        on turn_limit: final_answer
        on stuck: final_answer
    return found
"#,
        json!({"q": "__fail__"}),
    );
    // The search failed, the model saw the error and still answered.
    assert_eq!(
        out.unwrap(),
        json!("[resposta falsa para: __fail__] (com 1 observação(ões))")
    );
}
