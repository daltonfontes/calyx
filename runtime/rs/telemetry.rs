//! Telemetry: a run's traces and metrics in the OpenTelemetry format
//! (OTLP over HTTP, JSON encoding), for any collector or backend that
//! takes it (the OpenTelemetry Collector, Jaeger, Grafana Tempo,
//! Honeycomb, Datadog, ...).
//!
//! Off unless configured, by the standard variables or by `calyx.toml`:
//!
//! ```toml
//! [telemetry]
//! endpoint = "http://localhost:4318"        # or OTEL_EXPORTER_OTLP_ENDPOINT
//! service_name = "atendimento"              # or OTEL_SERVICE_NAME
//! headers_env = { "x-honeycomb-team" = "HONEYCOMB_API_KEY" }   # values from the environment
//! file = "telemetry.jsonl"                  # or CALYX_TELEMETRY_FILE
//! ```
//!
//! `file` writes the same OTLP requests, one per line (what the
//! Collector's `otlpjsonfile` receiver reads), instead of or besides
//! sending them. OTEL_EXPORTER_OTLP_HEADERS (`k=v,k2=v2`) and the
//! per-signal OTEL_EXPORTER_OTLP_TRACES_ENDPOINT / _METRICS_ENDPOINT are
//! honoured; OTEL_SDK_DISABLED=true turns it all off.
//!
//! **Traces.** One trace per run: its id comes from the run's id, so a
//! resumed run (another process, maybe another machine) continues the same
//! trace. Each process adds a root span `run <graph>` and, under it, one
//! span per attempt of each call: `chat <model>`, `execute_tool <tool>`,
//! `entity <Entity>.<Handler>`, with the GenAI semantic conventions where
//! they apply (`gen_ai.request.model`, `gen_ai.usage.input_tokens`, ...)
//! and the call's key, node, effect, attempt, cost and error. Calls refused
//! by a policy and writes of unknown outcome are events of the root span.
//!
//! **Metrics** (delta sums, so short processes add up): `calyx.calls`
//! (outcome ok, error, or journal: taken from the journal), `calyx.call.
//! duration` (ms), `calyx.tokens`, `calyx.cost` (USD), `calyx.policy.
//! denials`, `calyx.writes.uncertain` and `calyx.runs` (by status).
//!
//! Nothing the program reads or writes goes out: no prompts, arguments or
//! answers, only names, keys, times, sizes and errors. Sending is done by a
//! thread, every second and at the end; a collector that fails never stops
//! a run (one warning on stderr).

use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_int};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

/// What `calyx.toml` says, before the environment overrides it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TelemetryConfig {
    pub endpoint: Option<String>,
    pub service_name: Option<String>,
    /// Header name to the environment variable with its value.
    pub headers_env: Vec<(String, String)>,
    /// Absolute, or relative to where the run starts (the file's is made
    /// relative to `calyx.toml` when it is read).
    pub file: Option<PathBuf>,
}

/// Where telemetry goes, once the environment is read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Target {
    traces_url: Option<String>,
    metrics_url: Option<String>,
    headers: Vec<(String, String)>,
    file: Option<PathBuf>,
    service: String,
    resource: Vec<(String, String)>,
}

fn target(cfg: &TelemetryConfig, env: &dyn Fn(&str) -> Option<String>) -> Option<Target> {
    if env("OTEL_SDK_DISABLED").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        return None;
    }
    let base = env("OTEL_EXPORTER_OTLP_ENDPOINT").or_else(|| cfg.endpoint.clone());
    let base = base.map(|b| b.trim_end_matches('/').to_owned());
    let traces_url = env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .or_else(|| base.as_ref().map(|b| format!("{b}/v1/traces")));
    let metrics_url = env("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
        .or_else(|| base.as_ref().map(|b| format!("{b}/v1/metrics")));
    let file = env("CALYX_TELEMETRY_FILE")
        .map(PathBuf::from)
        .or_else(|| cfg.file.clone());
    if traces_url.is_none() && metrics_url.is_none() && file.is_none() {
        return None;
    }
    let mut headers: Vec<(String, String)> = cfg
        .headers_env
        .iter()
        .filter_map(|(h, var)| env(var).map(|v| (h.clone(), v)))
        .collect();
    if let Some(list) = env("OTEL_EXPORTER_OTLP_HEADERS") {
        headers.extend(pairs(&list));
    }
    let service = env("OTEL_SERVICE_NAME")
        .or_else(|| cfg.service_name.clone())
        .unwrap_or_else(|| "calyx".into());
    let resource = env("OTEL_RESOURCE_ATTRIBUTES")
        .map(|l| pairs(&l))
        .unwrap_or_default();
    Some(Target {
        traces_url,
        metrics_url,
        headers,
        file,
        service,
        resource,
    })
}

/// `k=v,k2=v2`, values percent-decoded (the OTEL_* list format).
fn pairs(list: &str) -> Vec<(String, String)> {
    list.split(',')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), decode(v.trim())))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

fn decode(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ----- state ----------------------------------------------------------------

/// Duration buckets of `calyx.call.duration`, in ms.
const BOUNDS: &[f64] = &[
    5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0, 30000.0, 60000.0,
];

type Attrs = Vec<(&'static str, String)>;

#[derive(Default)]
struct Histogram {
    count: u64,
    sum: f64,
    buckets: Vec<u64>,
}

#[derive(Default)]
struct Metrics {
    since_us: u64,
    /// (metric, attributes) -> value.
    sums: BTreeMap<(&'static str, Attrs), f64>,
    durations: BTreeMap<Attrs, Histogram>,
}

impl Metrics {
    fn add(&mut self, name: &'static str, attrs: Attrs, v: f64) {
        *self.sums.entry((name, attrs)).or_insert(0.0) += v;
    }

    fn duration(&mut self, attrs: Attrs, ms: f64) {
        let h = self.durations.entry(attrs).or_default();
        if h.buckets.is_empty() {
            h.buckets = vec![0; BOUNDS.len() + 1];
        }
        h.count += 1;
        h.sum += ms;
        let i = BOUNDS.iter().position(|b| ms <= *b).unwrap_or(BOUNDS.len());
        h.buckets[i] += 1;
    }
}

struct Run {
    trace_id: String,
    span_id: String,
    id: String,
    graph: String,
    mode: String,
    start_us: u64,
    events: Vec<Value>,
}

struct State {
    target: Target,
    run: Option<Run>,
    /// Finished spans, already as JSON text: the work is done by the
    /// threads that end them, not all at once when sending.
    spans: Vec<String>,
    metrics: Metrics,
    warned: bool,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<State>> = Mutex::new(None);
static EXPORTER: Mutex<Option<(Sender<()>, JoinHandle<()>)>> = Mutex::new(None);
static SEQ: AtomicU64 = AtomicU64::new(0);

fn state() -> MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

/// FNV-1a, 64 bits, from `seed`.
fn fnv(seed: u64, data: &[u8]) -> u64 {
    let mut h = seed;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// The trace of a run: the same for every process that works on it.
fn trace_id(run: &str) -> String {
    let a = fnv(0xcbf2_9ce4_8422_2325, format!("calyx-run:{run}").as_bytes());
    let b = fnv(0x6c62_272e_07bb_0142, format!("calyx-run:{run}").as_bytes());
    format!("{a:016x}{b:016x}")
}

/// A span id no other span of the run has.
fn span_id() -> String {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let seed = format!("{}:{}:{n}", std::process::id(), now_us());
    let id = fnv(0xcbf2_9ce4_8422_2325, seed.as_bytes()).max(1);
    format!("{id:016x}")
}

/// Starts telemetry for this process, if anything asks for it. Called
/// before a run.
pub fn configure(cfg: &TelemetryConfig) {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let Some(t) = target(cfg, &env) else {
        ENABLED.store(false, Ordering::Relaxed);
        return;
    };
    let sends = t.traces_url.is_some() || t.metrics_url.is_some() || t.file.is_some();
    *state() = Some(State {
        target: t,
        run: None,
        spans: Vec::new(),
        metrics: Metrics {
            since_us: now_us(),
            ..Metrics::default()
        },
        warned: false,
    });
    ENABLED.store(true, Ordering::Relaxed);
    if sends {
        let (tx, rx) = channel();
        let handle = std::thread::spawn(move || exporter(rx));
        *EXPORTER.lock().unwrap_or_else(|e| e.into_inner()) = Some((tx, handle));
    }
}

/// Sends what is left and stops the exporter. Called after a run.
pub fn shutdown() {
    let exporter = EXPORTER.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some((tx, handle)) = exporter {
        drop(tx);
        let _ = handle.join();
    }
    ENABLED.store(false, Ordering::Relaxed);
    *state() = None;
}

/// Every second, and once more when the channel closes.
fn exporter(rx: Receiver<()>) {
    loop {
        let last = matches!(
            rx.recv_timeout(Duration::from_secs(1)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        );
        export();
        if last {
            return;
        }
    }
}

fn export() {
    let (target, traces, metrics) = {
        let mut guard = state();
        let Some(st) = guard.as_mut() else { return };
        let spans = std::mem::take(&mut st.spans);
        let traces = (!spans.is_empty()).then(|| traces_request(&st.target, spans));
        let now = now_us();
        let m = std::mem::take(&mut st.metrics);
        st.metrics.since_us = now;
        let metrics = (!m.sums.is_empty() || !m.durations.is_empty())
            .then(|| metrics_request(&st.target, &m, now).to_string());
        (st.target.clone(), traces, metrics)
    };
    let mut errors = Vec::new();
    for (body, url) in [(traces, &target.traces_url), (metrics, &target.metrics_url)] {
        let Some(body) = body else { continue };
        if let Some(path) = &target.file
            && let Err(e) = append(path, &body)
        {
            errors.push(format!("{}: {e}", path.display()));
        }
        if let Some(url) = url
            && let Err(e) = post(url, &target.headers, &body)
        {
            errors.push(format!("{url}: {e}"));
        }
    }
    if !errors.is_empty() {
        let mut guard = state();
        if let Some(st) = guard.as_mut()
            && !st.warned
        {
            st.warned = true;
            eprintln!(
                "calyx: warning: telemetry not sent ({}); the run goes on",
                errors.join("; ")
            );
        }
    }
}

fn append(path: &Path, body: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // One write per request: lines of concurrent writers do not mix.
    let mut line = body.to_owned();
    line.push('\n');
    f.write_all(line.as_bytes())
}

fn post(url: &str, headers: &[(String, String)], body: &str) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(5)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into();
    let mut req = agent.post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.send(body).map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!("HTTP {status}"))
    }
}

// ----- OTLP/JSON --------------------------------------------------------------

fn attr(k: &str, v: &Value) -> Option<Value> {
    let value = match v {
        Value::String(s) => json!({"stringValue": s}),
        Value::Bool(b) => json!({"boolValue": b}),
        Value::Number(n) if n.is_i64() || n.is_u64() => json!({"intValue": n.to_string()}),
        Value::Number(n) => json!({"doubleValue": n.as_f64()}),
        _ => return None,
    };
    Some(json!({"key": k, "value": value}))
}

fn attrs(m: &Map<String, Value>) -> Vec<Value> {
    m.iter().filter_map(|(k, v)| attr(k, v)).collect()
}

fn resource(t: &Target) -> Value {
    let mut m = Map::new();
    m.insert("service.name".into(), json!(t.service));
    m.insert("telemetry.sdk.name".into(), json!("calyx"));
    m.insert("telemetry.sdk.language".into(), json!("rust"));
    for (k, v) in &t.resource {
        m.insert(k.clone(), json!(v));
    }
    json!({"attributes": attrs(&m)})
}

fn scope() -> Value {
    json!({"name": "calyx", "version": env!("CARGO_PKG_VERSION")})
}

fn traces_request(t: &Target, spans: Vec<String>) -> String {
    format!(
        "{{\"resourceSpans\":[{{\"resource\":{},\"scopeSpans\":[{{\"scope\":{},\"spans\":[{}]}}]}}]}}",
        resource(t),
        scope(),
        spans.join(",")
    )
}

fn point_attrs(a: &Attrs) -> Vec<Value> {
    a.iter().filter_map(|(k, v)| attr(k, &json!(v))).collect()
}

fn metrics_request(t: &Target, m: &Metrics, now: u64) -> Value {
    let (start, end) = ((m.since_us * 1000).to_string(), (now * 1000).to_string());
    let mut by_name: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for ((name, a), v) in &m.sums {
        let mut p =
            json!({"attributes": point_attrs(a), "startTimeUnixNano": start, "timeUnixNano": end});
        if v.fract() == 0.0 && *name != "calyx.cost" {
            p["asInt"] = json!((*v as i64).to_string());
        } else {
            p["asDouble"] = json!(v);
        }
        by_name.entry(name).or_default().push(p);
    }
    let unit = |name: &str| match name {
        "calyx.calls" => "{call}",
        "calyx.tokens" => "{token}",
        "calyx.cost" => "USD",
        "calyx.runs" => "{run}",
        _ => "{event}",
    };
    let mut metrics: Vec<Value> = by_name
        .into_iter()
        .map(|(name, points)| {
            json!({"name": name, "unit": unit(name), "sum": {
                "aggregationTemporality": 1, "isMonotonic": true, "dataPoints": points}})
        })
        .collect();
    if !m.durations.is_empty() {
        let points: Vec<Value> = m
            .durations
            .iter()
            .map(|(a, h)| {
                json!({"attributes": point_attrs(a), "startTimeUnixNano": start,
                       "timeUnixNano": end, "count": h.count.to_string(), "sum": h.sum,
                       "bucketCounts": h.buckets.iter().map(u64::to_string).collect::<Vec<_>>(),
                       "explicitBounds": BOUNDS})
            })
            .collect();
        metrics.push(
            json!({"name": "calyx.call.duration", "unit": "ms", "histogram": {
            "aggregationTemporality": 1, "dataPoints": points}}),
        );
    }
    json!({"resourceMetrics": [{
        "resource": resource(t),
        "scopeMetrics": [{"scope": scope(), "metrics": metrics}],
    }]})
}

// ----- events from the runtime ----------------------------------------------

fn text(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the runtime passes NUL-terminated strings, valid for the call.
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// Whether anything is collected: the runtime builds no event otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn calyx_telemetry_enabled() -> c_int {
    c_int::from(ENABLED.load(Ordering::Relaxed))
}

/// A run starts in this process (new, resumed or replayed).
#[unsafe(no_mangle)]
pub extern "C" fn calyx_telemetry_begin(
    run_id: *const c_char,
    graph: *const c_char,
    mode: *const c_char,
) {
    let mut guard = state();
    let Some(st) = guard.as_mut() else { return };
    let id = text(run_id).unwrap_or_default();
    st.run = Some(Run {
        trace_id: trace_id(&id),
        span_id: span_id(),
        id,
        graph: text(graph).unwrap_or_default(),
        mode: text(mode).unwrap_or_else(|| "new".into()),
        start_us: now_us(),
        events: Vec::new(),
    });
}

/// One event of the run, as JSON (see exec.c, `tel_*`).
#[unsafe(no_mangle)]
pub extern "C" fn calyx_telemetry_event(event_json: *const c_char) {
    let Some(s) = text(event_json) else { return };
    let Ok(Value::Object(e)) = serde_json::from_str::<Value>(&s) else {
        return;
    };
    let mut guard = state();
    if let Some(st) = guard.as_mut() {
        event(st, &e);
    }
}

/// The run ends in this process: `finished`, `waiting` or `failed`.
#[unsafe(no_mangle)]
pub extern "C" fn calyx_telemetry_end(status: *const c_char, error: *const c_char) {
    let mut guard = state();
    let Some(st) = guard.as_mut() else { return };
    let Some(run) = st.run.take() else { return };
    let status = text(status).unwrap_or_else(|| "failed".into());
    let mut a = Map::new();
    a.insert("calyx.run.id".into(), json!(run.id));
    a.insert("calyx.graph".into(), json!(run.graph));
    a.insert("calyx.run.mode".into(), json!(run.mode));
    a.insert("calyx.run.status".into(), json!(status));
    let mut span = json!({
        "traceId": run.trace_id, "spanId": run.span_id,
        "name": format!("run {}", run.graph), "kind": 1,
        "startTimeUnixNano": (run.start_us * 1000).to_string(),
        "endTimeUnixNano": (now_us() * 1000).to_string(),
        "attributes": attrs(&a), "events": run.events,
    });
    span["status"] = match (status.as_str(), text(error)) {
        ("failed", e) => json!({"code": 2, "message": e.unwrap_or_default()}),
        _ => json!({"code": 1}),
    };
    st.spans.push(span.to_string());
    st.metrics.add(
        "calyx.runs",
        vec![
            ("calyx.graph", run.graph),
            ("calyx.run.mode", run.mode),
            ("calyx.run.status", status),
        ],
        1.0,
    );
}

fn get<'a>(e: &'a Map<String, Value>, k: &str) -> &'a str {
    e.get(k).and_then(Value::as_str).unwrap_or("")
}

fn num(e: &Map<String, Value>, k: &str) -> Option<f64> {
    e.get(k).and_then(Value::as_f64)
}

fn event(st: &mut State, e: &Map<String, Value>) {
    let Some(run) = &mut st.run else { return };
    let kind = get(e, "kind").to_owned();
    let name = get(e, "name").to_owned();
    match get(e, "type") {
        "call" => {
            let (t0, t1) = (num(e, "t0").unwrap_or(0.0), num(e, "t1").unwrap_or(0.0));
            let ok = e.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let mut a = Map::new();
            let span_name = match kind.as_str() {
                "model" => {
                    a.insert("gen_ai.operation.name".into(), json!("chat"));
                    a.insert("gen_ai.request.model".into(), json!(name));
                    a.insert("calyx.prompt".into(), json!(get(e, "prompt")));
                    format!("chat {name}")
                }
                "tool" => {
                    a.insert("gen_ai.operation.name".into(), json!("execute_tool"));
                    a.insert("gen_ai.tool.name".into(), json!(name));
                    format!("execute_tool {name}")
                }
                _ => format!("entity {name}"),
            };
            for k in ["key", "node", "effect"] {
                if !get(e, k).is_empty() {
                    a.insert(format!("calyx.call.{k}"), json!(get(e, k)));
                }
            }
            if let Some(n) = num(e, "attempt") {
                a.insert("calyx.call.attempt".into(), json!(n as i64));
            }
            for (k, otel) in [
                ("input_tokens", "gen_ai.usage.input_tokens"),
                ("output_tokens", "gen_ai.usage.output_tokens"),
            ] {
                if let Some(n) = num(e, k) {
                    a.insert(otel.into(), json!(n as i64));
                    st.metrics.add(
                        "calyx.tokens",
                        vec![
                            ("gen_ai.request.model", name.clone()),
                            (
                                "gen_ai.token.type",
                                k.trim_end_matches("_tokens").to_owned(),
                            ),
                        ],
                        n,
                    );
                }
            }
            if let Some(c) = num(e, "cost_usd") {
                a.insert("calyx.cost_usd".into(), json!(c));
                st.metrics.add(
                    "calyx.cost",
                    vec![("gen_ai.request.model", name.clone())],
                    c,
                );
            }
            if e.get("truncated").and_then(Value::as_bool) == Some(true) {
                a.insert("calyx.output.truncated".into(), json!(true));
            }
            let status = if ok {
                json!({"code": 1})
            } else {
                a.insert("error.type".into(), json!(get(e, "error_kind")));
                json!({"code": 2, "message": get(e, "error")})
            };
            st.spans.push(
                json!({
                    "traceId": run.trace_id, "spanId": span_id(), "parentSpanId": run.span_id,
                    "name": span_name, "kind": 3,
                    "startTimeUnixNano": ((t0 as u64) * 1000).to_string(),
                    "endTimeUnixNano": ((t1 as u64) * 1000).to_string(),
                    "attributes": attrs(&a), "status": status,
                })
                .to_string(),
            );
            let call = vec![("calyx.call.kind", kind.clone()), ("calyx.call.name", name)];
            let mut counted = call.clone();
            counted.push(("calyx.call.outcome", if ok { "ok" } else { "error" }.into()));
            st.metrics.add("calyx.calls", counted, 1.0);
            st.metrics.duration(call, (t1 - t0).max(0.0) / 1000.0);
        }
        "journal" => {
            st.metrics.add(
                "calyx.calls",
                vec![
                    ("calyx.call.kind", kind),
                    ("calyx.call.name", name),
                    ("calyx.call.outcome", "journal".into()),
                ],
                1.0,
            );
        }
        "deny" => {
            let agent = e.get("agent").and_then(Value::as_bool).unwrap_or(false);
            let mut a = Map::new();
            a.insert("gen_ai.tool.name".into(), json!(get(e, "tool")));
            a.insert("calyx.policy.rule".into(), json!(get(e, "rule")));
            a.insert("calyx.policy.by_agent".into(), json!(agent));
            a.insert("calyx.call.key".into(), json!(get(e, "key")));
            run.events.push(json!({
                "name": "policy.denied",
                "timeUnixNano": ((num(e, "t").unwrap_or(0.0) as u64) * 1000).to_string(),
                "attributes": attrs(&a),
            }));
            st.metrics.add(
                "calyx.policy.denials",
                vec![
                    ("gen_ai.tool.name", get(e, "tool").to_owned()),
                    ("calyx.policy.by_agent", agent.to_string()),
                ],
                1.0,
            );
        }
        "uncertain" => {
            let mut a = Map::new();
            a.insert("gen_ai.tool.name".into(), json!(get(e, "tool")));
            a.insert("calyx.write.resolution".into(), json!(get(e, "how")));
            a.insert("calyx.call.key".into(), json!(get(e, "key")));
            run.events.push(json!({
                "name": "write.uncertain",
                "timeUnixNano": ((num(e, "t").unwrap_or(0.0) as u64) * 1000).to_string(),
                "attributes": attrs(&a),
            }));
            st.metrics.add(
                "calyx.writes.uncertain",
                vec![("gen_ai.tool.name", get(e, "tool").to_owned())],
                1.0,
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_standard_variables_over_the_file() {
        let cfg = TelemetryConfig {
            endpoint: Some("http://file:4318".into()),
            service_name: Some("from-file".into()),
            headers_env: vec![("x-key".into(), "MY_KEY".into())],
            file: None,
        };
        let env = |k: &str| match k {
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some("http://env:4318/".into()),
            "OTEL_EXPORTER_OTLP_HEADERS" => Some("a=1,b=x%20y".into()),
            "MY_KEY" => Some("secret".into()),
            _ => None,
        };
        let t = target(&cfg, &env).unwrap();
        assert_eq!(t.traces_url.as_deref(), Some("http://env:4318/v1/traces"));
        assert_eq!(t.metrics_url.as_deref(), Some("http://env:4318/v1/metrics"));
        assert_eq!(t.service, "from-file");
        assert_eq!(
            t.headers,
            vec![
                ("x-key".into(), "secret".into()),
                ("a".into(), "1".into()),
                ("b".into(), "x y".into())
            ]
        );
        assert!(target(&TelemetryConfig::default(), &|_| None).is_none());
        let off = |k: &str| (k == "OTEL_SDK_DISABLED").then(|| "true".to_owned());
        assert!(target(&cfg, &off).is_none());
    }

    #[test]
    fn a_run_keeps_its_trace_across_processes() {
        assert_eq!(trace_id("20261010-1"), trace_id("20261010-1"));
        assert_ne!(trace_id("20261010-1"), trace_id("20261010-2"));
        assert_eq!(trace_id("x").len(), 32);
        assert_ne!(span_id(), span_id());
    }
}
