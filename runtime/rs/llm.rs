//! Model calls over the OpenAI-compatible chat API, which Gemini, NVIDIA,
//! OpenAI, OpenRouter, Groq and Ollama all accept, plus a fake model for
//! tests and offline development.
//!
//! This layer only talks to the network. Retries, timeouts per effect and
//! decoding the answer into the prompt's type are the C runtime's job.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::Provider;
use crate::io::IoError;

pub struct ModelRequest {
    pub model: String,
    pub prompt: String,
    /// JSON Schema of the answer; `None` for plain text.
    pub schema: Option<Value>,
    pub max_output: Option<u64>,
    pub timeout_ms: u64,
}

pub struct Answer {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub ms: u64,
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // Errors are classified from the status code, not raised by ureq.
        .http_status_as_error(false)
        // The platform's roots, so proxies with their own CA work.
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

pub fn call(
    agent: &ureq::Agent,
    provider: &Provider,
    req: &ModelRequest,
) -> Result<Answer, IoError> {
    let key = std::env::var(&provider.key_env).map_err(|_| {
        IoError::new(
            "Config",
            format!(
                "model `{}` uses provider `{}`, which needs the environment variable {}",
                req.model, provider.name, provider.key_env
            ),
        )
    })?;
    let mut body = json!({
        "model": req.model,
        "messages": [{"role": "user", "content": req.prompt}],
    });
    if let Some(n) = req.max_output {
        body["max_tokens"] = json!(n);
    }
    if let Some(schema) = &req.schema {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {"name": "answer", "schema": schema},
        });
    }
    let started = Instant::now();
    let url = format!("{}/chat/completions", provider.url);
    let result = agent
        .post(&url)
        .config()
        .timeout_global(Some(Duration::from_millis(req.timeout_ms)))
        .build()
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .send(body.to_string());
    let mut resp = result.map_err(transport_error)?;
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.trim().parse::<f64>().ok());
    let text = resp.body_mut().read_to_string().map_err(transport_error)?;
    let ms = started.elapsed().as_millis() as u64;
    if status != 200 {
        let mut e = status_error(status, &text);
        if let Some(secs) = retry_after {
            e.retry_after_ms = Some((secs * 1000.0) as u64);
        }
        return Err(e);
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| IoError::new("Decode", format!("invalid JSON from the provider: {e}")))?;
    let choice = &v["choices"][0];
    let content = choice["message"]["content"].as_str().unwrap_or("");
    if choice["finish_reason"] == "length" && req.schema.is_some() {
        return Err(IoError::new(
            "Decode",
            format!(
                "the answer was cut at max_output ({} tokens) before the JSON ended",
                req.max_output.unwrap_or(0)
            ),
        ));
    }
    Ok(Answer {
        text: content.to_owned(),
        input_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        ms,
    })
}

fn transport_error(e: ureq::Error) -> IoError {
    match e {
        ureq::Error::Timeout(_) => IoError::new("Timeout", e.to_string()),
        other => IoError::new("Network", other.to_string()),
    }
}

/// Error kinds the runtime understands; it retries the temporary ones.
fn status_error(status: u16, body: &str) -> IoError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            // Gemini wraps the error object in a list.
            let e = if v.is_array() {
                &v[0]["error"]
            } else {
                &v["error"]
            };
            e["message"].as_str().map(str::to_owned)
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    let kind = match status {
        408 => "Timeout",
        429 => "RateLimit",
        401 | 403 => "Auth",
        500..=599 => "Unavailable",
        _ => "BadRequest",
    };
    let mut e = IoError::new(kind, format!("HTTP {status}: {message}"));
    e.retry_after_ms = retry_hint(&message);
    e
}

/// "Please retry in 21.66s" (Gemini puts the wait in the message).
fn retry_hint(message: &str) -> Option<u64> {
    let lower = message.to_ascii_lowercase();
    let rest = &lower[lower.find("retry in ")? + "retry in ".len()..];
    let number: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let secs: f64 = number.parse().ok()?;
    let unit = &rest[number.len()..];
    Some(if unit.starts_with("ms") {
        secs as u64
    } else {
        (secs * 1000.0) as u64
    })
}

/// Fake models for tests of the retry policy: `fake-unavailable` always
/// fails with a temporary error; `fake-flaky` fails every other call.
pub fn fake_failure(model: &str) -> Option<IoError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let unavailable = || IoError::new("Unavailable", "HTTP 503: fake model is overloaded");
    match model {
        "fake-unavailable" => Some(unavailable()),
        "fake-flaky" if CALLS.fetch_add(1, Ordering::Relaxed).is_multiple_of(2) => {
            Some(unavailable())
        }
        _ => None,
    }
}

/// A deterministic answer shaped by the schema. No network. A model named
/// `fake-slow-<ms>` waits that long first, to measure parallelism.
pub fn fake(req: &ModelRequest) -> Answer {
    let started = Instant::now();
    if let Some(ms) = req
        .model
        .strip_prefix("fake-slow-")
        .and_then(|n| n.parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_millis(ms));
    }
    let first_line = req
        .prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let summary: String = first_line.chars().take(60).collect();
    let text = match &req.schema {
        Some(schema) => fake_value(schema, &summary, 0).to_string(),
        None => format!("[resposta falsa para: {summary}]"),
    };
    Answer {
        input_tokens: (req.prompt.len() / 4) as u64,
        output_tokens: (text.len() / 4) as u64,
        text,
        ms: started.elapsed().as_millis() as u64,
    }
}

fn fake_value(schema: &Value, summary: &str, n: usize) -> Value {
    if let Some(options) = schema["enum"].as_array() {
        return options.first().cloned().unwrap_or(Value::Null);
    }
    match schema["type"].as_str() {
        Some("string") => json!(format!("item falso {} ({summary})", n + 1)),
        Some("integer") => json!(n + 1),
        Some("number") => json!(n as f64 + 1.5),
        Some("boolean") => json!(true),
        Some("array") => {
            let count = schema["maxItems"].as_u64().unwrap_or(3).min(3) as usize;
            Value::Array(
                (0..count)
                    .map(|i| fake_value(&schema["items"], summary, i))
                    .collect(),
            )
        }
        _ => {
            let mut obj = serde_json::Map::new();
            if let Some(props) = schema["properties"].as_object() {
                for (k, s) in props {
                    obj.insert(k.clone(), fake_value(s, summary, n));
                }
            }
            Value::Object(obj)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_answers_follow_the_schema() {
        let req = ModelRequest {
            model: "fake".into(),
            prompt: "\nDivida o tema\n".into(),
            schema: Some(json!({
                "type": "object",
                "properties": {"questions": {"type": "array", "items": {"type": "string"}, "maxItems": 2}},
                "required": ["questions"]
            })),
            max_output: None,
            timeout_ms: 1000,
        };
        let v: Value = serde_json::from_str(&fake(&req).text).unwrap();
        assert_eq!(v["questions"].as_array().unwrap().len(), 2);
        assert_eq!(v["questions"][0], "item falso 1 (Divida o tema)");
    }

    #[test]
    fn classifies_http_errors() {
        let e = status_error(503, r#"[{"error":{"code":503,"message":"high demand"}}]"#);
        assert_eq!(e.kind, "Unavailable");
        assert_eq!(e.message, "HTTP 503: high demand");
        assert_eq!(status_error(429, "").kind, "RateLimit");
        let quota = status_error(
            429,
            r#"[{"error":{"message":"Quota exceeded. Please retry in 21.66s."}}]"#,
        );
        assert_eq!(quota.retry_after_ms, Some(21_660));
        assert_eq!(status_error(400, "{}").kind, "BadRequest");
    }
}
