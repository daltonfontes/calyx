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

/// A command that starts an MCP server speaking over stdio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolServer {
    pub command: Vec<String>,
    /// The tool's name on the server, when it differs from the Calyx name.
    pub remote_name: Option<String>,
    /// Directory the command runs in (the directory of `calyx.toml`).
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub providers: Vec<Provider>,
    pub tools: HashMap<String, ToolServer>,
    /// Where the configuration came from, for messages.
    pub path: Option<PathBuf>,
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
        let mut cur = Some(dir);
        while let Some(d) = cur {
            let candidate = d.join(FILE_NAME);
            if candidate.is_file() {
                return Config::load(&candidate);
            }
            cur = d.parent();
        }
        Ok(Config::builtin())
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
                        let command = strings(v, "command", name)?;
                        if command.is_empty() {
                            return Err(format!("tools.{name}.command is empty"));
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
                                remote_name,
                                dir: dir.to_path_buf(),
                            },
                        );
                    }
                }
                other => return Err(format!("unknown section `{other}`")),
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
    fn rejects_unknown_sections() {
        assert!(Config::parse("[model]\n", Path::new(".")).is_err());
    }
}
