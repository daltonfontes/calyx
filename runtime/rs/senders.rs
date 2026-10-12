//! Who may send a message to a run (`calyx deliver`): signed deliveries.
//!
//! A message type can be limited to some senders in `calyx.toml`:
//!
//! ```toml
//! [senders]                        # each person's public key
//! ana = "ed25519:9f0c...e1"        # from `calyx keys new ana`
//! bruno = "ed25519:4b7a...20"
//!
//! [messages.Approval]
//! senders = ["ana", "bruno"]       # only they may deliver an Approval
//! ```
//!
//! `calyx deliver <run> Approval Approved --as ana` signs the delivery with
//! ana's private key (Ed25519; `calyx keys new` writes it, readable only by
//! its owner, to ~/.calyx/keys/ or $CALYX_KEYS_DIR). What is signed is the
//! run, the `receive` it goes to, the message type, the value and the time,
//! so a signature cannot be moved to another run, another wait or another
//! answer. The runtime checks it when the run takes the message: a delivery
//! to a limited type that is unsigned, signed by someone not listed, or
//! whose signature does not match is not taken (the run keeps waiting), so
//! writing the inbox by hand, or into the database, gets nowhere. Who sent
//! it goes to the journal with the value.
//!
//! Types not listed in `[messages]` take any delivery, as before; a
//! delivery signed anyway records its sender.

use std::collections::HashMap;
use std::path::PathBuf;

use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde_json::Value;

/// `[senders]` and `[messages.X] senders = [...]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Senders {
    /// Name to Ed25519 public key.
    pub keys: HashMap<String, Vec<u8>>,
    /// Message type to the names that may send it.
    pub allowed: HashMap<String, Vec<String>>,
}

impl Senders {
    /// Who may send `message`, if the type is limited.
    pub fn limited(&self, message: &str) -> Option<&[String]> {
        self.allowed.get(message).map(Vec::as_slice)
    }
}

/// `ed25519:<hex>` -> the key's bytes.
pub fn parse_public(text: &str) -> Result<Vec<u8>, String> {
    let hex = text
        .strip_prefix("ed25519:")
        .ok_or("a public key is written `ed25519:<hex>` (from `calyx keys new`)")?;
    let bytes = from_hex(hex).ok_or("the key is not hexadecimal")?;
    if bytes.len() != 32 {
        return Err(format!(
            "an Ed25519 public key has 32 bytes, not {}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// What a signature covers. The value is written as serde_json writes it
/// (object keys in order), so signer and checker agree byte for byte.
pub fn payload(run: &str, key: &str, message: &str, value: &Value, at: f64) -> String {
    format!("calyx-delivery-v1\n{run}\n{key}\n{message}\n{value}\n{at}")
}

/// Where `calyx keys new` keeps private keys.
pub fn keys_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CALYX_KEYS_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".calyx").join("keys")
}

/// Makes a key pair for `name`: the private key goes to `keys_dir()`
/// (readable only by its owner); returns its path and the public key as
/// `ed25519:<hex>`, for `[senders]`.
pub fn new_key(name: &str) -> Result<(PathBuf, String), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || "-_.".contains(c))
    {
        return Err(format!(
            "`{name}`: a key's name is letters, digits, `-`, `_` or `.`"
        ));
    }
    let dir = keys_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.ed25519"));
    if path.exists() {
        return Err(format!(
            "{} exists: a key is never replaced",
            path.display()
        ));
    }
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| "cannot generate a key".to_owned())?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|e| e.to_string())?;
    write_private(&path, pkcs8.as_ref())?;
    Ok((
        path,
        format!("ed25519:{}", to_hex(pair.public_key().as_ref())),
    ))
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// The private key of `name`, from `file` or from `keys_dir()`.
fn private_key(name: &str, file: Option<&std::path::Path>) -> Result<Ed25519KeyPair, String> {
    let path = file.map_or_else(|| keys_dir().join(format!("{name}.ed25519")), PathBuf::from);
    let bytes = std::fs::read(&path).map_err(|e| {
        format!(
            "no private key for `{name}` at {} ({e}); make one with `calyx keys new {name}`",
            path.display()
        )
    })?;
    Ed25519KeyPair::from_pkcs8(&bytes)
        .map_err(|_| format!("{} is not a key made by `calyx keys new`", path.display()))
}

/// Signs a delivery as `name`; the signature as `ed25519:<hex>`. The key
/// must be the one `[senders]` lists for `name`, so a wrong key is said
/// now, not when the run refuses the message.
pub fn sign(
    senders: &Senders,
    name: &str,
    file: Option<&std::path::Path>,
    payload: &str,
) -> Result<String, String> {
    let pair = private_key(name, file)?;
    match senders.keys.get(name) {
        Some(k) if k.as_slice() == pair.public_key().as_ref() => {}
        Some(_) => {
            return Err(format!(
                "this is not `{name}`'s key: its public key is not the one in [senders]"
            ));
        }
        None => return Err(format!("`{name}` is not in [senders] in calyx.toml")),
    }
    Ok(format!(
        "ed25519:{}",
        to_hex(pair.sign(payload.as_bytes()).as_ref())
    ))
}

/// Whether the run may take a delivery (a line of its inbox): its sender
/// if signed, or why not.
pub fn check(senders: &Senders, run: &str, line: &Value) -> Result<Option<String>, String> {
    let message = line["message"].as_str().unwrap_or("");
    let sender = line["sender"].as_str();
    let allowed = senders.limited(message);
    let Some(name) = sender else {
        return match allowed {
            None => Ok(None),
            Some(names) => Err(format!(
                "`{message}` must be signed by one of: {}",
                names.join(", ")
            )),
        };
    };
    if let Some(names) = allowed
        && !names.iter().any(|n| n == name)
    {
        return Err(format!("`{name}` may not send `{message}`"));
    }
    let key = senders
        .keys
        .get(name)
        .ok_or_else(|| format!("`{name}` is not in [senders]"))?;
    let sig = line["sig"]
        .as_str()
        .and_then(|s| s.strip_prefix("ed25519:"))
        .and_then(from_hex)
        .ok_or("the delivery has no signature")?;
    let text = payload(
        run,
        line["key"].as_str().unwrap_or(""),
        message,
        &line["value"],
        line["at"].as_f64().unwrap_or(0.0),
    );
    UnparsedPublicKey::new(&ED25519, key)
        .verify(text.as_bytes(), &sig)
        .map_err(|_| format!("the signature is not `{name}`'s for this delivery"))?;
    Ok(Some(name.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_signature_holds_for_its_delivery_only() {
        let dir = std::env::temp_dir().join(format!("calyx-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY (test): no other thread of this test reads the variable.
        unsafe { std::env::set_var("CALYX_KEYS_DIR", &dir) };
        let (_, public) = new_key("ana").unwrap();
        let (_, other) = new_key("eve").unwrap();
        assert!(new_key("ana").is_err(), "a key is never replaced");
        let mut s = Senders::default();
        s.keys.insert("ana".into(), parse_public(&public).unwrap());
        s.keys.insert("eve".into(), parse_public(&other).unwrap());
        s.allowed.insert("Approval".into(), vec!["ana".into()]);

        let value = json!({"kind": "Approved"});
        let p = payload("run1", "g/approval#0", "Approval", &value, 100.0);
        let sig = sign(&s, "ana", None, &p).unwrap();
        let line = json!({"key": "g/approval#0", "message": "Approval", "value": value,
                          "at": 100.0, "sender": "ana", "sig": sig});
        assert_eq!(check(&s, "run1", &line).unwrap().as_deref(), Some("ana"));
        // Another run, another answer, another time: not this signature.
        assert!(check(&s, "run2", &line).is_err());
        let mut denied = line.clone();
        denied["value"] = json!({"kind": "Denied", "reason": "x"});
        assert!(check(&s, "run1", &denied).is_err());
        let mut later = line.clone();
        later["at"] = json!(101.0);
        assert!(check(&s, "run1", &later).is_err());
        // Unsigned, or by someone not allowed.
        let mut unsigned = line.clone();
        unsigned.as_object_mut().unwrap().remove("sender");
        assert!(
            check(&s, "run1", &unsigned)
                .unwrap_err()
                .contains("must be signed")
        );
        let eve_sig = sign(&s, "eve", None, &p).unwrap();
        let mut eve = line.clone();
        eve["sender"] = json!("eve");
        eve["sig"] = json!(eve_sig);
        assert!(
            check(&s, "run1", &eve)
                .unwrap_err()
                .contains("may not send")
        );
        // ana's name with eve's key.
        let eve_key = keys_dir().join("eve.ed25519");
        assert!(sign(&s, "ana", Some(&eve_key), &p).is_err());
        // The delivery goes through the inbox as JSON text: every time,
        // read back, must be the one signed (`float_roundtrip`; without it
        // about 1 in 8 times did not).
        for i in 0..2000u32 {
            let at = 1_760_278_415.0 + f64::from(i) * 0.000_123_457;
            let p = payload("run1", "g/approval#0", "Approval", &value, at);
            let sig = sign(&s, "ana", None, &p).unwrap();
            let text = json!({"key": "g/approval#0", "message": "Approval", "value": value,
                              "at": at, "sender": "ana", "sig": sig})
            .to_string();
            let line: Value = serde_json::from_str(&text).unwrap();
            assert!(check(&s, "run1", &line).is_ok(), "at = {at}");
        }
        // A type nobody limited takes any delivery.
        let free = json!({"key": "k", "message": "Note", "value": "x", "at": 1.0});
        assert_eq!(check(&s, "run1", &free).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
