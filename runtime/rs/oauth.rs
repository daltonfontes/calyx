//! OAuth for remote MCP servers, as the MCP authorization spec asks of a
//! client: the authorization code flow with PKCE, for a person, once.
//!
//! `calyx login <tool>` does the part a person must do:
//!
//! 1. **Discovery.** The MCP server answers a request without a token with
//!    401 and `WWW-Authenticate: Bearer resource_metadata="..."`, the URL
//!    of its protected resource metadata (RFC 9728), which names its
//!    authorization server. When it does not, the well-known locations on
//!    its origin are tried. The authorization server's own metadata
//!    (RFC 8414, or OpenID Connect discovery) gives its endpoints.
//! 2. **Registration.** With `client_id` in calyx.toml, that one; else
//!    dynamic client registration (RFC 7591), as a public client.
//! 3. **Authorization.** The browser opens the authorization URL (with a
//!    PKCE challenge, a random `state` and the server as `resource`,
//!    RFC 8707); the person approves; the authorization server sends the
//!    browser back to a listener on 127.0.0.1 with a code, which is
//!    exchanged for tokens.
//!
//! The tokens go to `~/.calyx/oauth/` (or `CALYX_OAUTH_DIR`), one file per
//! server URL, readable only by its owner. Runs use the access token and,
//! when it expires or the server refuses it, the refresh token, with no
//! one there: a `worker` keeps working. Refreshing takes a lock on the
//! file, so processes that refresh at once do not spend a rotating
//! refresh token twice.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};

use crate::config::OAuthConfig;
use crate::io::IoError;

/// Refresh this long before the access token expires.
const EARLY: u64 = 60;
/// How long `calyx login` waits for the person to approve.
const LOGIN_WAIT: Duration = Duration::from_secs(300);

/// Where the tokens for a server are kept.
pub fn store_path(url: &str) -> PathBuf {
    let dir = std::env::var_os("CALYX_OAUTH_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::home_dir().map(|h| h.join(".calyx").join("oauth")))
        .unwrap_or_else(|| PathBuf::from(".calyx/oauth"));
    let hash = ring::digest::digest(&ring::digest::SHA256, url.as_bytes());
    let hex: String = hash.as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    dir.join(format!("{hex}.json"))
}

/// The authorization server's endpoints, and the resource to ask for.
struct Endpoints {
    authorization: String,
    token: String,
    registration: Option<String>,
    resource: String,
}

/// `calyx login`: the whole flow, with `open` showing the person the URL to
/// visit. Returns the file the tokens went to.
pub fn login(url: &str, cfg: &OAuthConfig, open: &dyn Fn(&str)) -> Result<PathBuf, String> {
    let agent = crate::llm::agent();
    let ep = discover(&agent, url)?;
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot listen on 127.0.0.1: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let (client_id, client_secret) = match &cfg.client_id {
        Some(id) => (id.clone(), client_secret(cfg)?),
        None => register(&agent, &ep, &redirect)?,
    };
    let verifier = random(32)?;
    let challenge =
        b64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref());
    let state = random(16)?;
    let mut query = vec![
        ("response_type", "code".to_owned()),
        ("client_id", client_id.clone()),
        ("redirect_uri", redirect.clone()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256".to_owned()),
        ("state", state.clone()),
        ("resource", ep.resource.clone()),
    ];
    if !cfg.scopes.is_empty() {
        query.push(("scope", cfg.scopes.join(" ")));
    }
    let sep = if ep.authorization.contains('?') {
        '&'
    } else {
        '?'
    };
    let auth_url = format!("{}{sep}{}", ep.authorization, form(&query));
    open(&auth_url);
    let code = wait_for_code(&listener, &state)?;
    let mut fields = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", code),
        ("redirect_uri", redirect),
        ("client_id", client_id.clone()),
        ("code_verifier", verifier),
        ("resource", ep.resource.clone()),
    ];
    if let Some(s) = &client_secret {
        fields.push(("client_secret", s.clone()));
    }
    let tokens = token_request(&agent, &ep.token, &fields).map_err(|e| e.message)?;
    let mut stored = json!({
        "server": url,
        "resource": ep.resource,
        "token_endpoint": ep.token,
        "client_id": client_id,
    });
    if let Some(s) = client_secret {
        stored["client_secret"] = json!(s);
    }
    merge(&mut stored, &tokens);
    let path = store_path(url);
    save(&path, &stored)?;
    Ok(path)
}

/// Removes the tokens kept for `url`; whether there were any.
pub fn logout(url: &str) -> Result<bool, String> {
    let path = store_path(url);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The access token to send to `url`: the one kept, or a fresh one from the
/// refresh token when it has expired (or is about to). `refused`: the
/// token the server just refused, so it is refreshed even if it has not
/// expired, unless another process already did.
pub fn access_token(url: &str, refused: Option<&str>) -> Result<String, IoError> {
    let path = store_path(url);
    let login = || {
        IoError::new(
            "Config",
            format!(
                "the MCP server at {url} needs a login: run `calyx login <tool>` for a tool with this url"
            ),
        )
    };
    let stored = load(&path).ok_or_else(login)?;
    let token = stored["access_token"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if !token.is_empty() && refused.is_none_or(|r| r != token) && !expired(&stored) {
        return Ok(token);
    }
    // Another process may be refreshing: one at a time, and the first one's
    // token for the others.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))
        .map_err(|e| IoError::new("Config", format!("{}: {e}", path.display())))?;
    let _ = lock.lock();
    let stored = load(&path).ok_or_else(login)?;
    let token = stored["access_token"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if !token.is_empty() && refused.is_none_or(|r| r != token) && !expired(&stored) {
        return Ok(token);
    }
    let Some(refresh) = stored["refresh_token"].as_str() else {
        return Err(IoError::new(
            "Auth",
            format!(
                "the login to the MCP server at {url} expired and it gave no refresh token: run `calyx login <tool>` again"
            ),
        ));
    };
    let mut fields = vec![
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", refresh.to_owned()),
        (
            "client_id",
            stored["client_id"].as_str().unwrap_or_default().to_owned(),
        ),
    ];
    if let Some(r) = stored["resource"].as_str() {
        fields.push(("resource", r.to_owned()));
    }
    if let Some(s) = stored["client_secret"].as_str() {
        fields.push(("client_secret", s.to_owned()));
    }
    let endpoint = stored["token_endpoint"].as_str().unwrap_or_default();
    let fresh = token_request(&crate::llm::agent(), endpoint, &fields).map_err(|e| {
        if e.kind == "Auth" {
            IoError::new(
                "Auth",
                format!(
                    "the login to the MCP server at {url} is no longer valid ({}): run `calyx login <tool>` again",
                    e.message
                ),
            )
        } else {
            e
        }
    })?;
    let mut updated = stored.clone();
    merge(&mut updated, &fresh);
    save(&path, &updated).map_err(|e| IoError::new("Config", e))?;
    Ok(updated["access_token"]
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

fn expired(stored: &Value) -> bool {
    stored["expires_at"]
        .as_u64()
        .is_some_and(|at| now() + EARLY >= at)
}

/// A token response into the kept tokens: a new refresh token replaces the
/// old one (rotation); none keeps the old one.
fn merge(stored: &mut Value, tokens: &Value) {
    stored["access_token"] = tokens["access_token"].clone();
    if let Some(r) = tokens.get("refresh_token").filter(|r| r.is_string()) {
        stored["refresh_token"] = r.clone();
    }
    match tokens["expires_in"].as_u64() {
        Some(s) => stored["expires_at"] = json!(now() + s),
        None => {
            if let Some(o) = stored.as_object_mut() {
                o.remove("expires_at");
            }
        }
    }
    if let Some(s) = tokens.get("scope").filter(|s| s.is_string()) {
        stored["scope"] = s.clone();
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn client_secret(cfg: &OAuthConfig) -> Result<Option<String>, String> {
    match &cfg.client_secret_env {
        None => Ok(None),
        Some(var) => std::env::var(var)
            .ok()
            .filter(|v| !v.is_empty())
            .map(Some)
            .ok_or_else(|| format!("oauth.client_secret_env names {var}, which is not set")),
    }
}

/// Finds the authorization server of the MCP server at `url`.
fn discover(agent: &ureq::Agent, url: &str) -> Result<Endpoints, String> {
    let (origin, path) = split(url)?;
    // A request without a token: the 401 names the metadata.
    let mut candidates = Vec::new();
    if let Ok(resp) = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .send(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "calyx", "version": env!("CARGO_PKG_VERSION")}}})
            .to_string(),
        )
        && let Some(m) = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| param(v, "resource_metadata"))
    {
        candidates.push(m);
    }
    if !path.is_empty() && path != "/" {
        candidates.push(format!(
            "{origin}/.well-known/oauth-protected-resource{path}"
        ));
    }
    candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
    let resource = candidates.iter().find_map(|c| get_json(agent, c));
    let (issuer, resource) = match resource {
        Some(r) => (
            r["authorization_servers"][0]
                .as_str()
                .ok_or("the MCP server's metadata names no authorization server")?
                .to_owned(),
            r["resource"].as_str().unwrap_or(url).to_owned(),
        ),
        // Servers from before RFC 9728 in MCP: the authorization server is
        // on the MCP server's own origin.
        None => (origin.clone(), url.to_owned()),
    };
    let (as_origin, as_path) = split(&issuer)?;
    let as_path = as_path.trim_end_matches('/');
    let mut metas = vec![
        format!("{as_origin}/.well-known/oauth-authorization-server{as_path}"),
        format!("{as_origin}/.well-known/openid-configuration{as_path}"),
    ];
    if !as_path.is_empty() {
        metas.push(format!(
            "{as_origin}{as_path}/.well-known/openid-configuration"
        ));
    }
    let meta = metas
        .iter()
        .find_map(|m| get_json(agent, m))
        .ok_or_else(|| format!("no authorization server metadata found for {issuer}"))?;
    if let Some(methods) = meta["code_challenge_methods_supported"].as_array()
        && !methods.iter().any(|m| m == "S256")
    {
        return Err(format!(
            "the authorization server {issuer} does not offer PKCE with S256, which Calyx requires"
        ));
    }
    let endpoint = |k: &str| -> Result<Option<String>, String> {
        match meta[k].as_str() {
            None => Ok(None),
            Some(e) => {
                secure(e)?;
                Ok(Some(e.to_owned()))
            }
        }
    };
    Ok(Endpoints {
        authorization: endpoint("authorization_endpoint")?
            .ok_or("the authorization server has no authorization_endpoint")?,
        token: endpoint("token_endpoint")?
            .ok_or("the authorization server has no token_endpoint")?,
        registration: endpoint("registration_endpoint")?,
        resource,
    })
}

/// Endpoints that receive codes and tokens must be HTTPS (localhost aside,
/// for development).
fn secure(url: &str) -> Result<(), String> {
    let local = ["http://localhost", "http://127.0.0.1", "http://[::1]"]
        .iter()
        .any(|p| url.starts_with(p));
    if url.starts_with("https://") || local {
        Ok(())
    } else {
        Err(format!(
            "`{url}` is not HTTPS: Calyx sends no codes or tokens to it"
        ))
    }
}

/// Dynamic client registration, as a public client.
fn register(
    agent: &ureq::Agent,
    ep: &Endpoints,
    redirect: &str,
) -> Result<(String, Option<String>), String> {
    let at = ep.registration.as_deref().ok_or(
        "the authorization server does not register clients: set `client_id` in the tool's `oauth` in calyx.toml",
    )?;
    let body = json!({
        "client_name": "Calyx",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let mut resp = agent
        .post(at)
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .map_err(|e| format!("registering at {at}: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("registering at {at}: HTTP {status}: {text}"));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("registering at {at}: {e}"))?;
    let id = v["client_id"]
        .as_str()
        .ok_or_else(|| format!("registering at {at}: no client_id in the answer"))?;
    Ok((
        id.to_owned(),
        v["client_secret"].as_str().map(str::to_owned),
    ))
}

/// Waits for the browser to come back with the code.
fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + LOGIN_WAIT;
    loop {
        if Instant::now() > deadline {
            return Err("no answer from the browser in 5 minutes".into());
        }
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let Some(query) = target.strip_prefix("/callback?") else {
            let _ = page(&mut stream, 404, "Not found.");
            continue;
        };
        let get = |k: &str| {
            query
                .split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(key, _)| *key == k)
                .map(|(_, v)| unescape(v))
        };
        if get("state").as_deref() != Some(state) {
            let _ = page(&mut stream, 400, "This answer is not from this login.");
            continue;
        }
        if let Some(e) = get("error") {
            let why = get("error_description").unwrap_or_default();
            let _ = page(
                &mut stream,
                400,
                "The login was not approved. You can close this tab.",
            );
            return Err(format!("the authorization server answered {e}: {why}"));
        }
        let Some(code) = get("code") else {
            let _ = page(&mut stream, 400, "No code in the answer.");
            return Err("the authorization server sent no code".into());
        };
        let _ = page(
            &mut stream,
            200,
            "Calyx is logged in. You can close this tab.",
        );
        return Ok(code);
    }
}

fn page(stream: &mut impl Write, status: u16, text: &str) -> std::io::Result<()> {
    let body = format!("<!doctype html><meta charset=utf-8><title>Calyx</title><p>{text}</p>");
    write!(
        stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A POST to the token endpoint; its JSON answer.
fn token_request(
    agent: &ureq::Agent,
    endpoint: &str,
    fields: &[(&str, String)],
) -> Result<Value, IoError> {
    let mut resp = agent
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(form(fields))
        .map_err(|e| IoError::new("Network", format!("{endpoint}: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        let why = v["error"].as_str().map_or_else(
            || text.chars().take(200).collect(),
            |e| {
                format!(
                    "{e} {}",
                    v["error_description"].as_str().unwrap_or_default()
                )
            },
        );
        let kind = match status {
            400 | 401 | 403 => "Auth",
            429 => "RateLimit",
            500..=599 => "Unavailable",
            _ => "ToolError",
        };
        return Err(IoError::new(kind, format!("HTTP {status}: {}", why.trim())));
    }
    if !v["access_token"].is_string() {
        return Err(IoError::new(
            "Auth",
            format!("{endpoint} answered without an access_token"),
        ));
    }
    Ok(v)
}

fn get_json(agent: &ureq::Agent, url: &str) -> Option<Value> {
    let mut resp = agent
        .get(url)
        .header("Accept", "application/json")
        .call()
        .ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let v: Value = serde_json::from_str(&resp.body_mut().read_to_string().ok()?).ok()?;
    v.is_object().then_some(v)
}

fn load(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Writes the tokens readable only by their owner, whole or not at all.
fn save(path: &Path, v: &Value) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(&tmp)
        .and_then(|mut f| {
            f.write_all(
                serde_json::to_string_pretty(v)
                    .unwrap_or_default()
                    .as_bytes(),
            )?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })
}

/// `https://host:port/a/b` into `https://host:port` and `/a/b`.
fn split(url: &str) -> Result<(String, String), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("`{url}` is not a URL"))?;
    let (host, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let path = path.split('?').next().unwrap_or_default();
    Ok((format!("{scheme}://{host}"), path.to_owned()))
}

/// The value of `name="..."` in a `WWW-Authenticate` header.
fn param(header: &str, name: &str) -> Option<String> {
    let at = header.find(&format!("{name}="))? + name.len() + 1;
    let rest = &header[at..];
    match rest.strip_prefix('"') {
        Some(q) => q.split('"').next().map(str::to_owned),
        None => rest
            .split([',', ' '])
            .next()
            .map(str::to_owned)
            .filter(|v| !v.is_empty()),
    }
}

fn random(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| "no random numbers available".to_owned())?;
    Ok(b64url(&buf))
}

fn b64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn form(fields: &[(&str, String)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", escape(k), escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_the_rfc_7636_example() {
        // RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge =
            b64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref());
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
    }

    #[test]
    fn urls_headers_and_forms() {
        assert_eq!(
            split("https://mcp.example.com:8443/v1/mcp?x=1").unwrap(),
            ("https://mcp.example.com:8443".into(), "/v1/mcp".into())
        );
        assert_eq!(
            param(
                r#"Bearer error="invalid_token", resource_metadata="https://a.example/.well-known/oauth-protected-resource""#,
                "resource_metadata"
            )
            .as_deref(),
            Some("https://a.example/.well-known/oauth-protected-resource")
        );
        assert_eq!(escape("a b/c=d&é"), "a%20b%2Fc%3Dd%26%C3%A9");
        assert_eq!(unescape("a%20b%2Fc+d%C3%A9"), "a b/c dé");
        assert_eq!(unescape("100%"), "100%");
        assert!(secure("http://evil.example/token").is_err());
        assert!(secure("http://127.0.0.1:9/token").is_ok());
    }
}
