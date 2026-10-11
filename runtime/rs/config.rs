//! Project configuration (`calyx.toml`): which API serves each model and
//! which MCP server implements each tool (decision D34).
//!
//! ```toml
//! [providers.nvidia]
//! url = "https://integrate.api.nvidia.com/v1"
//! key_env = "NVIDIA_API_KEY"
//! models = ["meta/", "nvidia/"]      # model ids starting with these
//!
//! [tools.web_search]
//! command = ["python3", "tools/fake_search.py"]   # relative to this file
//!
//! [tools.list_commits]                       # a remote MCP server (Streamable HTTP)
//! url = "https://api.githubcopilot.com/mcp/"
//! token_env = "GITHUB_PERSONAL_ACCESS_TOKEN" # sent as `Authorization: Bearer ...`
//! headers_env = { "X-MCP-Toolsets" = "GITHUB_TOOLSETS" }   # values from the environment
//!
//! [tools.refund.probe]           # for `calyx check --tools --probe` only
//! setup = "probe_order"           # optional: a tool called first, no arguments
//! args = { order = "{setup}", amount = 1.0 }       # the key's parameter is set by Calyx
//! count = "refunds_with"          # a read tool that counts the effects
//! count_args = { order = "{setup}", request = "{key}" }
//!
//! [prices."gemini-3.5-flash-lite"]   # USD per million tokens (illustrative)
//! input = 0.10
//! output = 0.40
//! ```
//!
//! The program pins the model id (decision D23); the configuration only
//! says where to send it. Keys are read from environment variables, never
//! from the file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "calyx.toml";

/// An OpenAI-compatible chat API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub name: String,
    /// Base URL; requests go to `{url}/chat/completions`.
    pub url: String,
    /// Environment variable with the API key.
    pub key_env: String,
    /// Model id prefixes this provider serves.
    pub prefixes: Vec<String>,
}

/// Where a tool's MCP server is: a command that starts it and speaks over
/// stdio, or the URL of a remote one (Streamable HTTP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolServer {
    /// Empty for a remote server.
    pub command: Vec<String>,
    /// A remote server's endpoint.
    pub url: Option<String>,
    /// The environment variable with the token sent as
    /// `Authorization: Bearer <token>` to a remote server.
    pub token_env: Option<String>,
    /// More headers for a remote server: name to the environment variable
    /// with its value (never the value itself: the file is not a secret).
    pub headers_env: Vec<(String, String)>,
    /// The tool's name on the server, when it differs from the Calyx name.
    pub remote_name: Option<String>,
    /// Directory the command runs in (the directory of `calyx.toml`).
    pub dir: PathBuf,
    /// How `calyx check --tools --probe` tests that the tool honours keys.
    pub probe: Option<Probe>,
}

/// A test that a keyed write applies a repeated key once: call it twice
/// with the same key, then count its effects with a read tool. It makes
/// real writes, so it is only for a service's test environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// A tool (by its name in `[tools]`) called first with no arguments;
    /// its answer replaces `{setup}` in the arguments.
    pub setup: Option<String>,
    /// The write's arguments; `{key}` and `{setup}` are replaced.
    pub args: serde_json::Map<String, serde_json::Value>,
    /// A read tool (by its name in `[tools]`) answering how many effects.
    pub count: String,
    pub count_args: serde_json::Map<String, serde_json::Value>,
}

impl ToolServer {
    /// One server per distinct command or URL, shared by the tools on it.
    pub fn key(&self) -> Vec<String> {
        match &self.url {
            Some(url) => vec!["url".into(), url.clone()],
            None => self.command.clone(),
        }
    }

    /// How the server is named in messages.
    pub fn describe(&self) -> String {
        match &self.url {
            Some(url) => url.clone(),
            None => self.command.join(" "),
        }
    }
}

fn probe(v: &toml::Value, owner: &str) -> Result<Option<Probe>, String> {
    let Some(p) = v.get("probe") else {
        return Ok(None);
    };
    let owner = format!("{owner}.probe");
    let map = |key: &str| -> Result<serde_json::Map<String, serde_json::Value>, String> {
        match p.get(key) {
            None => Ok(Default::default()),
            Some(t) => match serde_json::to_value(t) {
                Ok(serde_json::Value::Object(m)) => Ok(m),
                _ => Err(format!("`{owner}.{key}` must be a table")),
            },
        }
    };
    Ok(Some(Probe {
        setup: p
            .get("setup")
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
        args: map("args")?,
        count: string(p, "count", &owner)?,
        count_args: map("count_args")?,
    }))
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub providers: Vec<Provider>,
    pub tools: HashMap<String, ToolServer>,
    /// Price per model id, in USD per million tokens: (input, output).
    pub prices: HashMap<String, (f64, f64)>,
    /// Where the configuration came from, for messages.
    pub path: Option<PathBuf>,
    /// `[telemetry]`: where traces and metrics go (see `telemetry`).
    pub telemetry: crate::telemetry::TelemetryConfig,
    /// `[senders]` and `[messages]`: who may deliver which message.
    pub senders: crate::senders::Senders,
}

/// Providers known without configuration. A `calyx.toml` entry with the
/// same name replaces them.
fn builtin() -> Vec<Provider> {
    let p = |name: &str, url: &str, key: &str, prefixes: &[&str]| Provider {
        name: name.into(),
        url: url.into(),
        key_env: key.into(),
        prefixes: prefixes.iter().map(|s| (*s).to_owned()).collect(),
    };
    vec![
        p(
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "GEMINI_API_KEY",
            &["gemini-", "gemma-"],
        ),
        p(
            "openai",
            "https://api.openai.com/v1",
            "OPENAI_API_KEY",
            &["gpt-", "o1", "o3", "o4"],
        ),
        p(
            "nvidia",
            "https://integrate.api.nvidia.com/v1",
            "NVIDIA_API_KEY",
            &["nvidia/"],
        ),
    ]
}

impl Config {
    /// Built-in providers only.
    pub fn builtin() -> Config {
        Config {
            providers: builtin(),
            ..Config::default()
        }
    }

    /// Looks for `calyx.toml` in `dir` and its parents. Without one, the
    /// built-in providers are used and no tool has a server.
    pub fn discover(dir: &Path) -> Result<Config, String> {
        match Config::find(dir) {
            Some(path) => Config::load(&path),
            None => Ok(Config::builtin()),
        }
    }

    /// The `calyx.toml` in `dir` or the nearest of its parents.
    pub fn find(dir: &Path) -> Option<PathBuf> {
        dir.ancestors()
            .map(|d| d.join(FILE_NAME))
            .find(|candidate| candidate.is_file())
    }

    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut cfg = Config::parse(&text, &dir).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.path = Some(path.to_path_buf());
        Ok(cfg)
    }

    pub fn parse(text: &str, dir: &Path) -> Result<Config, String> {
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let mut cfg = Config::builtin();
        for (key, value) in &table {
            match key.as_str() {
                "providers" => {
                    for (name, v) in section(value, "providers")? {
                        let p = Provider {
                            name: name.clone(),
                            url: string(v, "url", name)?.trim_end_matches('/').to_owned(),
                            key_env: string(v, "key_env", name)?,
                            prefixes: strings(v, "models", name)?,
                        };
                        cfg.providers.retain(|q| q.name != p.name);
                        // Configured providers win over built-in ones.
                        cfg.providers.insert(0, p);
                    }
                }
                "tools" => {
                    for (name, v) in section(value, "tools")? {
                        let opt = |k: &str| -> Result<Option<String>, String> {
                            match v.get(k) {
                                None => Ok(None),
                                Some(x) => x
                                    .as_str()
                                    .map(|s| Some(s.to_owned()))
                                    .ok_or(format!("tools.{name}.{k} must be text")),
                            }
                        };
                        let url = opt("url")?;
                        let command = match (&url, v.get("command")) {
                            (Some(_), Some(_)) => {
                                return Err(format!(
                                    "tools.{name}: `command` or `url`, not both"
                                ));
                            }
                            (Some(u), None) => {
                                if !u.starts_with("http://") && !u.starts_with("https://") {
                                    return Err(format!(
                                        "tools.{name}.url must start with https:// (or http://)"
                                    ));
                                }
                                Vec::new()
                            }
                            (None, _) => strings(v, "command", name).map_err(|_| {
                                format!(
                                    "`{name}` needs `command = [\"...\"]` (a local MCP server) or `url = \"https://...\"` (a remote one)"
                                )
                            })?,
                        };
                        if url.is_none() && command.is_empty() {
                            return Err(format!("tools.{name}.command is empty"));
                        }
                        let mut headers_env = Vec::new();
                        if let Some(h) = v.get("headers_env") {
                            let h = h.as_table().ok_or(format!(
                                "tools.{name}.headers_env must be a table of header = \"VARIABLE\""
                            ))?;
                            for (header, var) in h {
                                let var = var.as_str().ok_or(format!(
                                    "tools.{name}.headers_env.{header} must name an environment variable"
                                ))?;
                                headers_env.push((header.clone(), var.to_owned()));
                            }
                        }
                        let token_env = opt("token_env")?;
                        if url.is_none() && (token_env.is_some() || !headers_env.is_empty()) {
                            return Err(format!(
                                "tools.{name}: `token_env` and `headers_env` are for a remote server (`url`)"
                            ));
                        }
                        let remote_name = match v.get("name") {
                            Some(n) => Some(
                                n.as_str()
                                    .ok_or(format!("tools.{name}.name must be text"))?
                                    .to_owned(),
                            ),
                            None => None,
                        };
                        cfg.tools.insert(
                            name.clone(),
                            ToolServer {
                                command,
                                url,
                                token_env,
                                headers_env,
                                remote_name,
                                dir: dir.to_path_buf(),
                                probe: probe(v, &format!("tools.{name}"))?,
                            },
                        );
                    }
                }
                "prices" => {
                    for (model, v) in section(value, "prices")? {
                        let price = |k: &str| {
                            v.get(k)
                                .and_then(|x| x.as_float().or(x.as_integer().map(|i| i as f64)))
                                .ok_or_else(|| {
                                    format!(
                                        "prices.\"{model}\" needs `{k} = <USD per million tokens>`"
                                    )
                                })
                        };
                        cfg.prices
                            .insert(model.clone(), (price("input")?, price("output")?));
                    }
                }
                "telemetry" => {
                    let t = section(value, "telemetry")?;
                    let text = |k: &str| -> Result<Option<String>, String> {
                        match t.get(k) {
                            None => Ok(None),
                            Some(v) => v
                                .as_str()
                                .map(|s| Some(s.to_owned()))
                                .ok_or(format!("telemetry.{k} must be text")),
                        }
                    };
                    let mut headers_env = Vec::new();
                    if let Some(h) = t.get("headers_env") {
                        let h = h.as_table().ok_or(
                            "telemetry.headers_env must be a table of header = \"VARIABLE\"",
                        )?;
                        for (name, var) in h {
                            let var = var.as_str().ok_or(format!(
                                "telemetry.headers_env.{name} must name an environment variable"
                            ))?;
                            headers_env.push((name.clone(), var.to_owned()));
                        }
                    }
                    for k in t.keys() {
                        if !["endpoint", "service_name", "headers_env", "file"]
                            .contains(&k.as_str())
                        {
                            return Err(format!("unknown key `telemetry.{k}`"));
                        }
                    }
                    cfg.telemetry = crate::telemetry::TelemetryConfig {
                        endpoint: text("endpoint")?,
                        service_name: text("service_name")?,
                        headers_env,
                        file: text("file")?.map(|f| dir.join(f)),
                    };
                }
                "senders" => {
                    for (name, v) in section(value, "senders")? {
                        let text = v
                            .as_str()
                            .ok_or(format!("senders.{name} must be `\"ed25519:<hex>\"`"))?;
                        let key = crate::senders::parse_public(text)
                            .map_err(|e| format!("senders.{name}: {e}"))?;
                        cfg.senders.keys.insert(name.clone(), key);
                    }
                }
                "messages" => {
                    for (message, v) in section(value, "messages")? {
                        let names = strings(v, "senders", &format!("messages.{message}"))?;
                        cfg.senders.allowed.insert(message.clone(), names);
                    }
                }
                other => return Err(format!("unknown section `{other}`")),
            }
        }
        for (message, names) in &cfg.senders.allowed {
            if let Some(n) = names.iter().find(|n| !cfg.senders.keys.contains_key(*n)) {
                return Err(format!(
                    "messages.{message}: `{n}` is not in [senders] (add its public key from `calyx keys new {n}`)"
                ));
            }
        }
        Ok(cfg)
    }

    /// The provider for a model id: the first whose prefix matches.
    pub fn provider_for(&self, model_id: &str) -> Option<&Provider> {
        self.providers
            .iter()
            .find(|p| p.prefixes.iter().any(|pre| model_id.starts_with(pre)))
    }
}

fn section<'a>(v: &'a toml::Value, name: &str) -> Result<&'a toml::Table, String> {
    v.as_table()
        .ok_or_else(|| format!("`{name}` must be a section"))
}

fn string(v: &toml::Value, key: &str, owner: &str) -> Result<String, String> {
    v.get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("`{owner}` needs `{key} = \"...\"`"))
}

fn strings(v: &toml::Value, key: &str, owner: &str) -> Result<Vec<String>, String> {
    let list = v
        .get(key)
        .and_then(toml::Value::as_array)
        .ok_or_else(|| format!("`{owner}` needs `{key} = [\"...\"]`"))?;
    list.iter()
        .map(|x| {
            x.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("`{owner}.{key}` must be a list of texts"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_providers_match_by_prefix() {
        let cfg = Config::builtin();
        assert_eq!(
            cfg.provider_for("gemini-3.5-flash-lite").unwrap().name,
            "gemini"
        );
        assert!(cfg.provider_for("claude-sonnet-5-5").is_none());
    }

    #[test]
    fn parses_providers_and_tools() {
        let cfg = Config::parse(
            r#"
            [providers.local]
            url = "http://localhost:11434/v1/"
            key_env = "NONE"
            models = ["llama"]

            [tools.web_search]
            command = ["python3", "search.py"]
            name = "search"
            "#,
            Path::new("/p"),
        )
        .unwrap();
        let p = cfg.provider_for("llama3").unwrap();
        assert_eq!(p.url, "http://localhost:11434/v1");
        let t = &cfg.tools["web_search"];
        assert_eq!(t.command, ["python3", "search.py"]);
        assert_eq!(t.remote_name.as_deref(), Some("search"));
        assert_eq!(t.dir, Path::new("/p"));
    }

    #[test]
    fn parses_prices() {
        let cfg = Config::parse(
            "[prices.\"gemini-x\"]\ninput = 0.1\noutput = 2\n",
            Path::new("."),
        )
        .unwrap();
        assert_eq!(cfg.prices["gemini-x"], (0.1, 2.0));
    }

    #[test]
    fn rejects_unknown_sections() {
        assert!(Config::parse("[model]\n", Path::new(".")).is_err());
    }
}
