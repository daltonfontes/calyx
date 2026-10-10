//! The journal in PostgreSQL, for runs shared by several machines (D6).
//!
//! With `CALYX_DATABASE_URL` set (`postgres://user@host:port/db`), a run's
//! journal lines and blobs go to PostgreSQL instead of `.calyx/runs/<id>/`:
//! the same lines, in the same order, so the interpreter reads and writes
//! them the same way (`journal.c` calls these functions). Any machine that
//! reaches the database can list the runs and resume one.
//!
//! - `calyx_runs`: one row per run, with its status (`running`,
//!   `finished`, `failed`, `waiting`) and the machine that last ran it.
//! - `calyx_journal`: the lines of every run, in order.
//! - `calyx_blobs`: large answers, by the SHA-256 of their content.
//!
//! Each line is its own committed transaction (`synchronous_commit` as the
//! server is configured, `on` by default): an entry that was appended is on
//! the server's disk, which is what the file journal gets from `fsync`.
//!
//! **One machine per run.** A run is executed under a session-level
//! advisory lock on its id, held by this process's connection. If the
//! process or its machine dies, PostgreSQL drops the session and the lock
//! with it, and another machine may take the run over (`calyx worker`).
//!
//! The connection has no TLS: use a database on a private network or
//! behind a tunnel.

use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::Mutex;

use postgres::{Client, NoTls};

pub const URL_ENV: &str = "CALYX_DATABASE_URL";

static CLIENT: Mutex<Option<Client>> = Mutex::new(None);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS calyx_runs (
    id text PRIMARY KEY,
    status text NOT NULL,
    host text NOT NULL DEFAULT '',
    started timestamptz NOT NULL DEFAULT now(),
    updated timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS calyx_journal (
    run text NOT NULL,
    seq bigserial,
    line text NOT NULL,
    PRIMARY KEY (run, seq)
);
CREATE TABLE IF NOT EXISTS calyx_blobs (
    hash text PRIMARY KEY,
    data bytea NOT NULL
);";

/// The journal is in PostgreSQL (the URL is set).
pub fn enabled() -> bool {
    std::env::var(URL_ENV).is_ok_and(|u| !u.is_empty())
}

fn host() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_owned())
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
}

/// Runs `f` on this process's connection, opening it (and creating the
/// tables) the first time.
fn with<T>(f: impl FnOnce(&mut Client) -> Result<T, postgres::Error>) -> Result<T, String> {
    let mut guard = CLIENT
        .lock()
        .map_err(|_| "the database connection is poisoned")?;
    if guard.is_none() {
        let url = std::env::var(URL_ENV).map_err(|_| format!("{URL_ENV} is not set"))?;
        let mut client = Client::connect(&url, NoTls)
            .map_err(|e| format!("cannot connect to {URL_ENV}: {e}"))?;
        // Several machines may start at once: one creates the tables.
        let mut tx = client.transaction().map_err(|e| e.to_string())?;
        tx.execute("SELECT pg_advisory_xact_lock(7461929)", &[])
            .and_then(|_| tx.batch_execute(SCHEMA))
            .map_err(|e| format!("cannot create the journal tables: {e}"))?;
        tx.commit().map_err(|e| e.to_string())?;
        *guard = Some(client);
    }
    let client = guard.as_mut().expect("connected above");
    f(client).map_err(|e| e.to_string())
}

/// Takes the run for this process (until it exits). `false`: another
/// process, on this machine or another, is running it.
pub fn lock_run(id: &str) -> Result<bool, String> {
    with(|c| {
        c.query_one(
            "SELECT pg_try_advisory_lock(hashtextextended($1, 0))",
            &[&id],
        )
        .map(|r| r.get(0))
    })
}

/// Whether some process holds the run now (a probe: it does not keep it).
pub fn run_is_held(id: &str) -> Result<bool, String> {
    with(|c| {
        let got: bool = c
            .query_one(
                "SELECT pg_try_advisory_lock(hashtextextended($1, 0))",
                &[&id],
            )?
            .get(0);
        if got {
            c.execute("SELECT pg_advisory_unlock(hashtextextended($1, 0))", &[&id])?;
        }
        Ok(!got)
    })
}

/// The lines of a run's journal, in order; `None` if there is no such run.
pub fn lines(id: &str) -> Result<Option<Vec<String>>, String> {
    with(|c| {
        let rows = c.query(
            "SELECT line FROM calyx_journal WHERE run = $1 ORDER BY seq",
            &[&id],
        )?;
        Ok((!rows.is_empty()).then(|| rows.iter().map(|r| r.get(0)).collect()))
    })
}

/// Every run with its status, oldest first.
pub fn runs() -> Result<Vec<(String, String)>, String> {
    with(|c| {
        Ok(
            c.query("SELECT id, status FROM calyx_runs ORDER BY id", &[])?
                .iter()
                .map(|r| (r.get(0), r.get(1)))
                .collect(),
        )
    })
}

fn text(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the C side passes NUL-terminated strings.
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

fn status_ok(r: Result<impl Sized, String>) -> c_int {
    match r {
        Ok(_) => 1,
        Err(e) => {
            eprintln!("calyx: journal database: {e}");
            0
        }
    }
}

/// Is the journal in PostgreSQL?
#[unsafe(no_mangle)]
pub extern "C" fn calyx_pg_enabled() -> c_int {
    c_int::from(enabled())
}

/// A new run: its row, `running`. 0 if it exists already, or on error.
///
/// # Safety
/// `run` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_pg_create(run: *const c_char) -> c_int {
    let Some(run) = text(run) else { return 0 };
    status_ok(with(|c| {
        c.execute(
            "INSERT INTO calyx_runs (id, status, host) VALUES ($1, 'running', $2)",
            &[&run, &host()],
        )
    }))
}

/// Sets a run's status (`running`, `finished`, `failed`, `waiting`).
///
/// # Safety
/// Both are NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_pg_status(run: *const c_char, status: *const c_char) -> c_int {
    let (Some(run), Some(status)) = (text(run), text(status)) else {
        return 0;
    };
    status_ok(with(|c| {
        c.execute(
            "UPDATE calyx_runs SET status = $2, host = $3, updated = now() WHERE id = $1",
            &[&run, &status, &host()],
        )
    }))
}

/// Appends one line to a run's journal, committed before it returns.
///
/// # Safety
/// `run` is NUL-terminated; `line` points to `len` bytes of UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_pg_append(run: *const c_char, line: *const u8, len: usize) -> c_int {
    let Some(run) = text(run) else { return 0 };
    if line.is_null() {
        return 0;
    }
    // SAFETY: the caller passes `len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(line, len) };
    let Ok(line) = std::str::from_utf8(bytes) else {
        return 0;
    };
    status_ok(with(|c| {
        c.execute(
            "INSERT INTO calyx_journal (run, line) VALUES ($1, $2)",
            &[&run, &line],
        )
    }))
}

/// A run's journal as text, one line per entry, each ending in `\n`; NULL
/// if there is no such run or on error. Free it with `calyx_string_free`.
///
/// # Safety
/// `run` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_pg_read(run: *const c_char) -> *mut c_char {
    let Some(run) = text(run) else {
        return std::ptr::null_mut();
    };
    match lines(&run) {
        Ok(Some(lines)) => {
            let mut all = String::new();
            for l in lines {
                all.push_str(&l);
                all.push('\n');
            }
            CString::new(all).map_or(std::ptr::null_mut(), CString::into_raw)
        }
        Ok(None) => std::ptr::null_mut(),
        Err(e) => {
            eprintln!("calyx: journal database: {e}");
            std::ptr::null_mut()
        }
    }
}

/// Stores a blob under its hash (once: the same hash is the same content).
///
/// # Safety
/// `hash` is NUL-terminated; `data` points to `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_pg_blob_put(
    hash: *const c_char,
    data: *const u8,
    len: usize,
) -> c_int {
    let Some(hash) = text(hash) else { return 0 };
    if data.is_null() {
        return 0;
    }
    // SAFETY: the caller passes `len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
    status_ok(with(|c| {
        c.execute(
            "INSERT INTO calyx_blobs (hash, data) VALUES ($1, $2) ON CONFLICT (hash) DO NOTHING",
            &[&hash, &bytes],
        )
    }))
}

/// A blob's content as a string, or NULL. Free it with `calyx_string_free`.
///
/// # Safety
/// `hash` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_pg_blob_get(hash: *const c_char) -> *mut c_char {
    let Some(hash) = text(hash) else {
        return std::ptr::null_mut();
    };
    let found = with(|c| {
        c.query_opt("SELECT data FROM calyx_blobs WHERE hash = $1", &[&hash])
            .map(|r| r.map(|r| r.get::<_, Vec<u8>>(0)))
    });
    match found {
        Ok(Some(data)) => CString::new(data).map_or(std::ptr::null_mut(), CString::into_raw),
        _ => std::ptr::null_mut(),
    }
}
