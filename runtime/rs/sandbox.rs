//! Sandboxes (decisions D13, D26): a working copy of a directory, inside
//! the run's directory, that tools borrow to read or to edit.
//!
//! - **One owner at a time.** Calls that edit a sandbox hold its lock alone;
//!   calls that read it share the lock. The compiler already orders the
//!   steps; the lock also covers an agent's calls of one turn.
//! - **Snapshots by content.** Each file is stored once, by its SHA-256, in
//!   `<sandbox>.snapshots/blobs/`; a snapshot is a manifest (path → hash,
//!   whether executable) stored by its own hash in `manifests/`. Taking one
//!   costs reading the files; unchanged files are not stored again.
//! - **A failed call leaves no trace.** Before a call that edits, a
//!   snapshot; if the call fails, the sandbox goes back to it, so a retry
//!   starts from the same state.
//! - **Recovery.** After each call that edits, a snapshot whose hash goes
//!   into the call's answer, and so into the journal. Resuming a run puts
//!   the sandbox back to the last snapshot in the journal: whatever a call
//!   that never reached the journal did is undone, and that call runs again.
//!
//! Symbolic links are not followed or kept: a sandbox holds files.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::{Value, json};

unsafe extern "C" {
    fn cx_sha256_hex(data: *const u8, len: usize, out: *mut u8);
}

fn sha256(data: &[u8]) -> String {
    let mut out = [0u8; 65];
    // SAFETY: `out` has room for 64 hex digits and the NUL.
    unsafe { cx_sha256_hex(data.as_ptr(), data.len(), out.as_mut_ptr()) };
    String::from_utf8_lossy(&out[..64]).into_owned()
}

/// Where a sandbox keeps its snapshots.
pub fn store(sandbox: &Path) -> PathBuf {
    let mut s = sandbox.as_os_str().to_owned();
    s.push(".snapshots");
    PathBuf::from(s)
}

fn io_err(what: &str, path: &Path, e: std::io::Error) -> String {
    format!("{what} `{}`: {e}", path.display())
}

/// Files under `dir`, relative, with their content and executable bit.
fn walk(dir: &Path, rel: &Path, out: &mut Vec<(String, PathBuf, bool)>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(dir.join(rel))
        .map_err(|e| io_err("cannot read", &dir.join(rel), e))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = rel.join(e.file_name());
        let meta = fs::symlink_metadata(dir.join(&path))
            .map_err(|err| io_err("cannot read", &dir.join(&path), err))?;
        if meta.is_dir() {
            walk(dir, &path, out)?;
        } else if meta.is_file() {
            #[cfg(unix)]
            let exec = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let exec = false;
            let key = path.to_string_lossy().replace('\\', "/");
            out.push((key, path, exec));
        }
    }
    Ok(())
}

/// Takes a snapshot of `dir`; returns the manifest's hash.
pub fn snapshot(dir: &Path) -> Result<String, String> {
    let st = store(dir);
    let blobs = st.join("blobs");
    let manifests = st.join("manifests");
    fs::create_dir_all(&blobs).map_err(|e| io_err("cannot create", &blobs, e))?;
    fs::create_dir_all(&manifests).map_err(|e| io_err("cannot create", &manifests, e))?;
    let mut files = Vec::new();
    walk(dir, Path::new(""), &mut files)?;
    let mut manifest = BTreeMap::new();
    for (key, rel, exec) in files {
        let data =
            fs::read(dir.join(&rel)).map_err(|e| io_err("cannot read", &dir.join(&rel), e))?;
        let hash = sha256(&data);
        let blob = blobs.join(&hash);
        if !blob.exists() {
            write_atomic(&blob, &data)?;
        }
        manifest.insert(key, json!({"hash": hash, "exec": exec}));
    }
    let text = serde_json::to_string(&manifest).expect("a map of strings serializes");
    let hash = sha256(text.as_bytes());
    let path = manifests.join(format!("{hash}.json"));
    if !path.exists() {
        write_atomic(&path, text.as_bytes())?;
    }
    Ok(hash)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, data).map_err(|e| io_err("cannot write", &tmp, e))?;
    fs::rename(&tmp, path).map_err(|e| io_err("cannot write", path, e))
}

/// Puts `dir` back to the snapshot `hash`: files not in it are removed,
/// changed files rewritten, missing ones created.
pub fn restore(dir: &Path, hash: &str) -> Result<(), String> {
    let st = store(dir);
    let mpath = st.join("manifests").join(format!("{hash}.json"));
    let text = fs::read_to_string(&mpath).map_err(|e| io_err("cannot read snapshot", &mpath, e))?;
    let manifest: BTreeMap<String, Value> =
        serde_json::from_str(&text).map_err(|e| format!("snapshot `{hash}` is damaged: {e}"))?;
    let mut current = Vec::new();
    if dir.exists() {
        walk(dir, Path::new(""), &mut current)?;
    }
    for (key, rel, _) in &current {
        if !manifest.contains_key(key) {
            fs::remove_file(dir.join(rel))
                .map_err(|e| io_err("cannot remove", &dir.join(rel), e))?;
        }
    }
    for (key, entry) in &manifest {
        let want = entry["hash"].as_str().unwrap_or_default();
        let path = dir.join(key);
        let same = fs::read(&path).map(|d| sha256(&d) == want).unwrap_or(false);
        if !same {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| io_err("cannot create", parent, e))?;
            }
            let data = fs::read(st.join("blobs").join(want)).map_err(|e| {
                io_err(
                    "cannot read snapshot content",
                    &st.join("blobs").join(want),
                    e,
                )
            })?;
            fs::write(&path, data).map_err(|e| io_err("cannot write", &path, e))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let exec = entry["exec"].as_bool().unwrap_or(false);
            let mode = if exec { 0o755 } else { 0o644 };
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(mode));
        }
    }
    remove_empty_dirs(dir);
    Ok(())
}

fn remove_empty_dirs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        let p = e.path();
        if fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
            remove_empty_dirs(&p);
            let _ = fs::remove_dir(&p); // fails, as it should, unless empty
        }
    }
}

/// Copies `from` into a new sandbox at `to` and takes its first snapshot,
/// kept as `base` (what `diff`-like tools compare with).
pub fn create(from: &Path, to: &Path) -> Result<(), String> {
    if !from.is_dir() {
        return Err(format!("`{}` is not a directory", from.display()));
    }
    if to.exists() {
        fs::remove_dir_all(to).map_err(|e| io_err("cannot clear", to, e))?;
    }
    fs::create_dir_all(to).map_err(|e| io_err("cannot create", to, e))?;
    let mut files = Vec::new();
    walk(from, Path::new(""), &mut files)?;
    for (_, rel, exec) in files {
        let dest = to.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err("cannot create", parent, e))?;
        }
        fs::copy(from.join(&rel), &dest).map_err(|e| io_err("cannot copy", &from.join(&rel), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(
                &dest,
                fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }),
            );
        }
        let _ = exec;
    }
    let base = snapshot(to)?;
    write_atomic(&store(to).join("base"), base.as_bytes())
}

/// The snapshot a run's journal ends with for each sandbox (the call that
/// edited it last), from the `sandbox` field of call answers.
pub fn journaled(journal_dir: &Path) -> HashMap<String, String> {
    let mut last: HashMap<String, (u64, String)> = HashMap::new();
    let Ok(text) = fs::read_to_string(journal_dir.join("journal.jsonl")) else {
        return HashMap::new();
    };
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry["type"] != "call" {
            continue;
        }
        let ok = match entry["blob"].as_str() {
            Some(h) => fs::read_to_string(journal_dir.join("blobs").join(h))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .unwrap_or(Value::Null),
            None => entry["ok"].clone(),
        };
        let Some(snaps) = ok["sandbox"].as_array() else {
            continue;
        };
        for s in snaps {
            let (Some(path), Some(hash), Some(seq)) = (
                s["path"].as_str(),
                s["snapshot"].as_str(),
                s["seq"].as_u64(),
            ) else {
                continue;
            };
            let newer = last.get(path).is_none_or(|(n, _)| seq > *n);
            if newer {
                last.insert(path.to_owned(), (seq, hash.to_owned()));
            }
        }
    }
    last.into_iter().map(|(p, (_, h))| (p, h)).collect()
}

/// Puts a sandbox back to where the journal left it: its last journaled
/// snapshot, or its base if no call that edits it finished.
pub fn recover(sandbox: &Path, journaled: &HashMap<String, String>) -> Result<(), String> {
    let key = sandbox.display().to_string();
    let hash = match journaled.get(&key) {
        Some(h) => h.clone(),
        None => fs::read_to_string(store(sandbox).join("base"))
            .map_err(|e| io_err("cannot read the first snapshot of", sandbox, e))?,
    };
    restore(sandbox, hash.trim())
}

// ----- locks ---------------------------------------------------------------

type Lock = Arc<RwLock<u64>>;

static LOCKS: Mutex<Option<HashMap<String, Lock>>> = Mutex::new(None);

fn lock_for(path: &str) -> Lock {
    let mut guard = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(HashMap::new)
        .entry(path.to_owned())
        .or_default()
        .clone()
}

/// A sandbox lent to a call.
pub struct Borrow {
    pub path: String,
    pub edits: bool,
}

/// Runs `call` holding the locks of the sandboxes it borrows. For each one
/// it edits: a snapshot before, restored if the call fails, and one after,
/// returned as `{"path", "snapshot", "seq"}` for the journal.
/// `wrap` turns a sandbox error (a snapshot that cannot be taken) into `E`.
pub fn with_borrows<T, E>(
    borrows: &[Borrow],
    wrap: impl Fn(String) -> E,
    call: impl FnOnce() -> Result<T, E>,
) -> Result<(T, Vec<Value>), E> {
    // One lock per sandbox, taken in path order (no deadlock between calls
    // that borrow several); exclusive if any borrow edits it.
    let mut paths: Vec<&str> = borrows.iter().map(|b| b.path.as_str()).collect();
    paths.sort_unstable();
    paths.dedup();
    let locks: Vec<(Lock, bool, &str)> = paths
        .into_iter()
        .map(|p| {
            let edits = borrows.iter().any(|b| b.path == p && b.edits);
            (lock_for(p), edits, p)
        })
        .collect();
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    for (lock, edits, path) in &locks {
        if *edits {
            writes.push((lock.write().unwrap_or_else(|e| e.into_inner()), *path));
        } else {
            reads.push(lock.read().unwrap_or_else(|e| e.into_inner()));
        }
    }
    let mut before = Vec::new();
    for (_, path) in &writes {
        before.push(snapshot(Path::new(path)).map_err(&wrap)?);
    }
    match call() {
        Ok(v) => {
            let mut snaps = Vec::new();
            for (guard, path) in writes.iter_mut() {
                let hash = snapshot(Path::new(path)).map_err(&wrap)?;
                **guard += 1;
                snaps.push(json!({"path": path, "snapshot": hash, "seq": **guard}));
            }
            Ok((v, snaps))
        }
        Err(e) => {
            for ((_, path), hash) in writes.iter().zip(&before) {
                restore(Path::new(path), hash).map_err(&wrap)?;
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("calyx-sandbox-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn snapshots_restore_files_and_remove_new_ones() {
        let src = tmp("src");
        fs::create_dir_all(src.join("pkg")).unwrap();
        fs::write(src.join("a.txt"), "um").unwrap();
        fs::write(src.join("pkg/b.txt"), "dois").unwrap();
        let sb = tmp("box");
        create(&src, &sb).unwrap();
        let base = fs::read_to_string(store(&sb).join("base")).unwrap();

        fs::write(sb.join("a.txt"), "mudou").unwrap();
        fs::write(sb.join("novo.txt"), "x").unwrap();
        fs::remove_file(sb.join("pkg/b.txt")).unwrap();
        fs::create_dir_all(sb.join("vazio/dentro")).unwrap();
        let changed = snapshot(&sb).unwrap();
        assert_ne!(changed, base);

        restore(&sb, &base).unwrap();
        assert_eq!(fs::read_to_string(sb.join("a.txt")).unwrap(), "um");
        assert_eq!(fs::read_to_string(sb.join("pkg/b.txt")).unwrap(), "dois");
        assert!(!sb.join("novo.txt").exists());
        assert!(!sb.join("vazio").exists());
        assert_eq!(snapshot(&sb).unwrap(), base, "same content, same hash");

        restore(&sb, &changed).unwrap();
        assert_eq!(fs::read_to_string(sb.join("a.txt")).unwrap(), "mudou");
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&sb);
        let _ = fs::remove_dir_all(store(&sb));
    }

    #[test]
    fn a_failed_call_that_edits_leaves_no_trace() {
        let src = tmp("src2");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), "um").unwrap();
        let sb = tmp("box2");
        create(&src, &sb).unwrap();
        let path = sb.display().to_string();
        let b = [Borrow {
            path: path.clone(),
            edits: true,
        }];
        let r: Result<((), Vec<Value>), String> = with_borrows(
            &b,
            |e| e,
            || {
                fs::write(sb.join("a.txt"), "meio feito").unwrap();
                Err("falhou".into())
            },
        );
        assert!(r.is_err());
        assert_eq!(fs::read_to_string(sb.join("a.txt")).unwrap(), "um");

        let (_, snaps) = with_borrows(
            &b,
            |e| e,
            || {
                fs::write(sb.join("a.txt"), "feito").unwrap();
                Ok::<_, String>(())
            },
        )
        .unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0]["path"], path.as_str());
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&sb);
        let _ = fs::remove_dir_all(store(&sb));
    }
}
