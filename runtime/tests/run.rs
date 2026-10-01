//! End to end: source → verifier → IR → C interpreter → I/O layer, with
//! fake models and the fake MCP search server. No network, no API key.

use std::path::Path;

use calyx_runtime::{Config, RunOptions, run};
use serde_json::{Value, json};

fn examples() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples")
}

fn compile(src: &str) -> String {
    let report = calyx_check::check("t.clyx", src);
    assert!(!report.has_errors(), "{}", report.render());
    report.ir.to_json()
}

fn opts() -> RunOptions {
    // SAFETY: tests that run programs hold LOCK; nothing else reads the environment concurrently.
    unsafe { std::env::set_var("CALYX_RETRY_BASE_MS", "1") };
    RunOptions {
        config: Config::load(&examples().join("calyx.toml")).unwrap(),
        fake_models: true,
        trace: false,
        ..RunOptions::default()
    }
}

const PRELUDE: &str = r#"
model m = "fake-model"

tool web_search(query: Text) -> Text:
    effect read
    max_output 50 tokens

type Plan:
    questions: List[Text] max 2
    title: Text

prompt split(topic: Text) -> Plan:
    """Divida: {topic}"""

prompt echo(x: Text) -> Text:
    """{x}"""
"#;

// Runs are serialized: the I/O layer has one global configuration.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run_src(body: &str, graph: &str, args: Value) -> Result<Value, String> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let ir = compile(&format!("{PRELUDE}\n{body}"));
    run(&ir, graph, &args, opts())
}

#[test]
fn runs_the_research_example() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let src = std::fs::read_to_string(examples().join("research.clyx")).unwrap();
    let ir = compile(&src);
    let out = run(&ir, "research", &json!({"topic": "energia solar"}), opts()).unwrap();
    let text = out.as_str().unwrap();
    assert!(
        text.starts_with("[resposta falsa para: Escreva um relatório sobre \"energia solar\""),
        "{text}"
    );
}

#[test]
fn records_fields_fan_out_and_interpolation() {
    let out = run_src(
        r#"
graph g(topic: Text) -> List[Text]:
    plan = m(split(topic))
    labels = for each q in plan.questions: "{plan.title} / {q}"
    return labels
"#,
        "g",
        json!({"topic": "t"}),
    )
    .unwrap();
    assert_eq!(
        out,
        json!([
            "item falso 1 (Divida: t) / item falso 1 (Divida: t)",
            "item falso 1 (Divida: t) / item falso 2 (Divida: t)"
        ])
    );
}

#[test]
fn tools_run_on_their_mcp_server_and_output_is_cut_at_max_output() {
    let out = run_src(
        r#"
graph g(q: Text) -> Text:
    return web_search(q)
"#,
        "g",
        json!({"q": "sol"}),
    )
    .unwrap();
    let text = out.as_str().unwrap();
    assert!(text.starts_with("[1] sol - fonte de exemplo 1"), "{text}");
    // max_output 50 tokens ≈ 200 bytes.
    assert!(text.len() <= 200, "{}", text.len());
}

#[test]
fn subgraphs_receive_their_arguments() {
    let out = run_src(
        r#"
graph inner(a: Text, b: Text) -> Text:
    return "{a}+{b}"

graph outer(x: Text) -> Text:
    return inner(b="dois", a=x)
"#,
        "outer",
        json!({"x": "um"}),
    )
    .unwrap();
    assert_eq!(out, json!("um+dois"));
}

#[test]
fn a_failing_tool_stops_the_run_with_the_node_and_the_reason() {
    let err = run_src(
        r#"
graph g(q: Text) -> Text:
    found = web_search(q)
    return m(echo(found))
"#,
        "g",
        json!({"q": "__fail__"}),
    )
    .unwrap_err();
    assert_eq!(
        err,
        "in graph `g`, node `found`: tool `web_search` failed: ToolError: falha simulada"
    );
}

#[test]
fn a_tool_without_a_server_is_a_configuration_error() {
    let err = run_src(
        r#"
tool other(x: Text) -> Text:
    effect read

graph g(q: Text) -> Text:
    return other(q)
"#,
        "g",
        json!({"q": "x"}),
    )
    .unwrap_err();
    assert!(err.contains("no MCP server for tool `other`"), "{err}");
}

#[test]
fn missing_arguments_are_reported() {
    let err = run_src(
        r#"
graph g(q: Text) -> Text:
    return q
"#,
        "g",
        json!({}),
    )
    .unwrap_err();
    assert_eq!(err, "missing argument `q` for graph `g`");
}

#[test]
fn temporary_model_errors_are_retried() {
    let out = run_src(
        r#"
model flaky = "fake-flaky"

graph g(x: Text) -> Text:
    a = flaky(echo(x))
    b = flaky(echo(a))
    return b
"#,
        "g",
        json!({"x": "oi"}),
    )
    .unwrap();
    assert!(
        out.as_str().unwrap().starts_with("[resposta falsa"),
        "{out}"
    );
}

#[test]
fn a_model_that_stays_unavailable_fails_after_four_attempts() {
    let err = run_src(
        r#"
model down = "fake-unavailable"

graph g(x: Text) -> Text:
    return down(echo(x))
"#,
        "g",
        json!({"x": "oi"}),
    )
    .unwrap_err();
    assert_eq!(
        err,
        "in graph `g`, node `return`: model `fake-unavailable` with prompt `echo` failed: Unavailable: HTTP 503: fake model is overloaded"
    );
}
