//! Built programs (decision D35): `calyx build` copies the `calyx` binary
//! itself and appends the program to it, so the result runs on a machine
//! with neither Calyx nor a C compiler.
//!
//! Layout of a built binary:
//!
//! ```text
//! [the calyx executable][payload: JSON][payload length: u64 LE][MAGIC]
//! ```
//!
//! The payload carries the source, not the IR: the verifier is in the
//! binary anyway, checking takes about a millisecond, and the runtime
//! verifies whatever it loads (D24).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const MAGIC: &[u8; 8] = b"CALYXPK1";
const TRAILER: u64 = 16;

/// A program embedded in a built binary.
#[derive(Debug, Clone, PartialEq)]
pub struct Bundle {
    /// The source file's name, for messages (e.g. `research.clyx`).
    pub file: String,
    pub source: String,
    /// The graph the binary runs.
    pub graph: String,
    /// The `calyx.toml` found at build time, if any.
    pub config: Option<String>,
}

impl Bundle {
    fn to_json(&self) -> Value {
        json!({
            "version": 1,
            "file": self.file,
            "source": self.source,
            "graph": self.graph,
            "config": self.config,
        })
    }

    fn from_json(v: &Value) -> Option<Bundle> {
        if v["version"] != 1 {
            return None;
        }
        Some(Bundle {
            file: v["file"].as_str()?.to_owned(),
            source: v["source"].as_str()?.to_owned(),
            graph: v["graph"].as_str()?.to_owned(),
            config: v["config"].as_str().map(str::to_owned),
        })
    }
}

/// The bundle at the end of `path`, if it has one.
pub fn read(path: &Path) -> Option<Bundle> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len < TRAILER {
        return None;
    }
    f.seek(SeekFrom::Start(len - TRAILER)).ok()?;
    let mut trailer = [0u8; TRAILER as usize];
    f.read_exact(&mut trailer).ok()?;
    if &trailer[8..] != MAGIC {
        return None;
    }
    let size = u64::from_le_bytes(trailer[..8].try_into().ok()?);
    if size > len - TRAILER {
        return None;
    }
    f.seek(SeekFrom::Start(len - TRAILER - size)).ok()?;
    let mut payload = vec![0u8; usize::try_from(size).ok()?];
    f.read_exact(&mut payload).ok()?;
    Bundle::from_json(&serde_json::from_slice(&payload).ok()?)
}

/// The bundle inside the running executable, if any.
pub fn current() -> Option<(PathBuf, Bundle)> {
    let exe = std::env::current_exe().ok()?;
    let bundle = read(&exe)?;
    Some((exe, bundle))
}

/// Writes `runtime` (an executable without a bundle) followed by `bundle`
/// to `out`, executable.
pub fn write(runtime: &Path, bundle: &Bundle, out: &Path) -> Result<(), String> {
    let mut bytes =
        std::fs::read(runtime).map_err(|e| format!("cannot read `{}`: {e}", runtime.display()))?;
    if read(runtime).is_some() {
        return Err(format!(
            "`{}` already carries a program; build with the `calyx` binary",
            runtime.display()
        ));
    }
    let payload = bundle.to_json().to_string().into_bytes();
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(MAGIC);

    // Write next to the target and rename, so a failed build never leaves
    // half a binary behind (or replaces a running one in place).
    let tmp = out.with_extension("calyx-build-tmp");
    let result = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        drop(f);
        std::fs::rename(&tmp, out)
    })();
    result.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write `{}`: {e}", out.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Bundle {
        Bundle {
            file: "p.clyx".into(),
            source: "graph g() -> Text:\n    return \"oi\"\n".into(),
            graph: "g".into(),
            config: Some("[tools.x]\ncommand = [\"y\"]\n".into()),
        }
    }

    #[test]
    fn a_bundle_round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("calyx-bundle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let runtime = dir.join("runtime");
        std::fs::write(&runtime, b"\x7fELF not really an executable").unwrap();
        assert_eq!(read(&runtime), None);

        let out = dir.join("out");
        write(&runtime, &sample(), &out).unwrap();
        assert_eq!(read(&out), Some(sample()));
        // The runtime part is untouched.
        assert!(
            std::fs::read(&out)
                .unwrap()
                .starts_with(b"\x7fELF not really")
        );
        // A built binary cannot be the runtime of another build.
        assert!(write(&out, &sample(), &dir.join("again")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_without_a_valid_trailer_have_no_bundle() {
        let dir = std::env::temp_dir().join(format!("calyx-nobundle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("f");
        // Right magic, impossible length.
        let mut bytes = b"abc".to_vec();
        bytes.extend_from_slice(&1000u64.to_le_bytes());
        bytes.extend_from_slice(MAGIC);
        std::fs::write(&f, &bytes).unwrap();
        assert_eq!(read(&f), None);
        std::fs::write(&f, b"short").unwrap();
        assert_eq!(read(&f), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
