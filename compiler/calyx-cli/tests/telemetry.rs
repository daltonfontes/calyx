//! Telemetry: traces and metrics sent over OTLP/HTTP (JSON) to a collector,
//! here a fake one in the test that keeps every request.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// What the collector received: path, headers (lowercase names), body.
type Received = Arc<Mutex<Vec<(String, Vec<(String, String)>, Value)>>>;

/// A collector on a free port, answering 200 to every request.
fn collector() -> (String, Received) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let got: Received = Arc::default();
    let keep = got.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
            let mut headers = Vec::new();
            let mut len = 0;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    let (k, v) = (k.trim().to_lowercase(), v.trim().to_owned());
                    if k == "content-length" {
                        len = v.parse().unwrap();
                    }
                    headers.push((k, v));
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
            keep.lock().unwrap().push((path, headers, body));
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}");
        }
    });
    (url, got)
}

const PROGRAM: &str = r#"
model m = "fake-model"

type Order:
    id: Text
    email: Text
    total: Float

message Approval = Approved | Denied(reason: Text)

tool get_order(id: Text) -> Order:
    effect read

tool refund(request: Text, order: Text, amount: Float) -> Unit:
    effect write
    idempotency_key request

policy refund:
    require amount <= 100 else "acima de 100, com aprovação"

prompt explain(o: Order) -> Text:
    """Explique o pedido {o}."""

graph pay(request: Text, order: Text, amount: Float) -> Text:
    o = get_order(order)
    text = m(explain(o))
    paid = try refund(request, o.id, amount)
    return match paid:
        case Ok(value): text
        case Failed(error): error

graph approve(request: Text, order: Text) -> Text:
    o = get_order(order)
    decision = receive Approval about o, timeout 1 days:
        on timeout: Denied(reason="timeout")
    return match decision:
        case Approved:
            paid = refund(request, o.id, 10)
            "paid"
        case Denied(reason): reason
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("calyx-telemetry-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("p.clyx"), PROGRAM).unwrap();
    let store =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/tools/fake_store.py");
    let mut toml = String::new();
    for t in ["get_order", "refund"] {
        toml += &format!(
            "[tools.{t}]\ncommand = [\"python3\", {:?}]\n",
            store.to_str().unwrap()
        );
    }
    std::fs::write(d.join("calyx.toml"), toml).unwrap();
    d
}

fn calyx(d: &Path, endpoint: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(args)
        .current_dir(d)
        .env("CALYX_FAKE_STORE", d.join("store.json"))
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint)
        .env("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer%20t0ken")
        .env("OTEL_SERVICE_NAME", "teste")
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn attrs(v: &Value) -> serde_json::Map<String, Value> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            let value = a["value"]
                .as_object()
                .and_then(|o| o.values().next().cloned());
            (
                a["key"].as_str().unwrap().to_owned(),
                value.unwrap_or(Value::Null),
            )
        })
        .collect()
}

fn spans(got: &Received) -> Vec<Value> {
    got.lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| p == "/v1/traces")
        .flat_map(|(_, _, b)| {
            b["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

/// metric name -> (attributes, value) of every point received.
fn points(got: &Received, name: &str) -> Vec<(serde_json::Map<String, Value>, String)> {
    let mut out = Vec::new();
    for (p, _, b) in got.lock().unwrap().iter() {
        if p != "/v1/metrics" {
            continue;
        }
        for m in b["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if m["name"] != name {
                continue;
            }
            let data = if m["sum"].is_null() {
                &m["histogram"]
            } else {
                &m["sum"]
            };
            for dp in data["dataPoints"].as_array().into_iter().flatten() {
                let v = dp
                    .get("asInt")
                    .or(dp.get("asDouble"))
                    .or(dp.get("count"))
                    .map(ToString::to_string)
                    .unwrap_or_default();
                out.push((attrs(&dp["attributes"]), v.trim_matches('"').to_owned()));
            }
        }
    }
    out
}

#[test]
fn a_run_sends_its_spans_and_metrics() {
    let (url, got) = collector();
    let d = dir("run");
    let args = [
        "run",
        "p.clyx",
        "--graph",
        "pay",
        "--request",
        "r1",
        "--order",
        "A100",
    ];
    let out = calyx(&d, &url, &[&args[..], &["--amount", "500"]].concat());
    assert!(out.status.success(), "{}", text(&out.stderr));

    let requests = got.lock().unwrap().clone();
    assert!(
        requests.iter().all(|(_, h, _)| h
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer t0ken")),
        "the headers go with every request"
    );
    let resource = attrs(&requests[0].2["resourceSpans"][0]["resource"]["attributes"]);
    assert_eq!(resource["service.name"], "teste");

    let spans = spans(&got);
    let names: Vec<&str> = spans.iter().filter_map(|s| s["name"].as_str()).collect();
    for want in ["run pay", "execute_tool get_order", "chat fake-model"] {
        assert!(names.contains(&want), "{names:?}");
    }
    // The refund was refused by the policy: never made, an event of the run.
    assert!(!names.contains(&"execute_tool refund"), "{names:?}");
    let root = spans.iter().find(|s| s["name"] == "run pay").unwrap();
    assert!(spans.iter().all(|s| s["traceId"] == root["traceId"]));
    for s in spans.iter().filter(|s| s["name"] != "run pay") {
        assert_eq!(s["parentSpanId"], root["spanId"]);
    }
    let a = attrs(&root["attributes"]);
    assert_eq!(a["calyx.run.status"], "finished");
    let denied = &root["events"][0];
    assert_eq!(denied["name"], "policy.denied");
    assert_eq!(
        attrs(&denied["attributes"])["calyx.policy.rule"],
        "require amount <= 100"
    );
    let chat = spans
        .iter()
        .find(|s| s["name"] == "chat fake-model")
        .unwrap();
    let a = attrs(&chat["attributes"]);
    assert_eq!(a["gen_ai.request.model"], "fake-model");
    assert!(a.contains_key("gen_ai.usage.input_tokens"));
    assert_eq!(a["calyx.call.key"], "pay/text#0");
    // Nothing the program reads or writes: no prompt, no answer.
    assert!(
        !requests
            .iter()
            .any(|(_, _, b)| b.to_string().contains("Explique"))
    );

    let calls = points(&got, "calyx.calls");
    assert!(
        calls
            .iter()
            .any(|(a, v)| a["calyx.call.name"] == "get_order"
                && a["calyx.call.outcome"] == "ok"
                && v == "1")
    );
    let denials = points(&got, "calyx.policy.denials");
    assert_eq!(denials.len(), 1);
    assert_eq!(denials[0].0["gen_ai.tool.name"], "refund");
    assert!(!points(&got, "calyx.call.duration").is_empty());
    assert!(
        points(&got, "calyx.runs")
            .iter()
            .any(|(a, _)| a["calyx.run.status"] == "finished")
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_resumed_run_continues_its_trace() {
    let (url, got) = collector();
    let d = dir("resume");
    let out = calyx(
        &d,
        &url,
        &[
            "run",
            "p.clyx",
            "--graph",
            "approve",
            "--request",
            "r2",
            "--order",
            "A100",
        ],
    );
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    let id = text(&out.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("calyx: run ").map(str::to_owned))
        .unwrap();
    let delivered = calyx(&d, &url, &["deliver", &id, "Approval", "Approved"]);
    assert!(delivered.status.success(), "{}", text(&delivered.stderr));
    let out = calyx(&d, &url, &["resume", &id]);
    assert!(out.status.success(), "{}", text(&out.stderr));

    let spans = spans(&got);
    let roots: Vec<&Value> = spans
        .iter()
        .filter(|s| s["name"] == "run approve")
        .collect();
    assert_eq!(roots.len(), 2, "one root span per process");
    assert_eq!(roots[0]["traceId"], roots[1]["traceId"]);
    let status: Vec<Value> = roots
        .iter()
        .map(|r| attrs(&r["attributes"])["calyx.run.status"].clone())
        .collect();
    assert_eq!(status, ["waiting", "finished"]);
    let modes: Vec<Value> = roots
        .iter()
        .map(|r| attrs(&r["attributes"])["calyx.run.mode"].clone())
        .collect();
    assert_eq!(modes, ["new", "resume"]);
    // get_order is not made again: counted as taken from the journal.
    assert_eq!(
        spans
            .iter()
            .filter(|s| s["name"] == "execute_tool get_order")
            .count(),
        1
    );
    assert!(points(&got, "calyx.calls").iter().any(|(a, _)| {
        a["calyx.call.name"] == "get_order" && a["calyx.call.outcome"] == "journal"
    }));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_collector_that_is_down_does_not_stop_the_run() {
    // A port nobody listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let d = dir("down");
    let out = calyx(
        &d,
        &format!("http://127.0.0.1:{port}"),
        &[
            "run",
            "p.clyx",
            "--graph",
            "pay",
            "--request",
            "r3",
            "--order",
            "A100",
            "--amount",
            "5",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert_eq!(err.matches("telemetry not sent").count(), 1, "{err}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_file_gets_the_same_requests() {
    let d = dir("file");
    let out = Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args([
            "run",
            "p.clyx",
            "--graph",
            "pay",
            "--request",
            "r4",
            "--order",
            "A100",
            "--amount",
            "5",
        ])
        .current_dir(&d)
        .env("CALYX_FAKE_STORE", d.join("store.json"))
        .env("CALYX_TELEMETRY_FILE", d.join("t.jsonl"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let lines: Vec<Value> = std::fs::read_to_string(d.join("t.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(lines.iter().any(|l| l.get("resourceSpans").is_some()));
    assert!(lines.iter().any(|l| l.get("resourceMetrics").is_some()));
    assert!(
        lines
            .iter()
            .any(|l| l.to_string().contains("execute_tool refund"))
    );
    let _ = std::fs::remove_dir_all(&d);
}
