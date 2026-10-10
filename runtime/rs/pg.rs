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
//! - `calyx_files`: the lines of a run's other files, by name: the
//!   deadlines of its `receive`s (`waits.jsonl`) and the messages delivered
//!   to them (`inbox.jsonl`).
//! - `calyx_entities`: one row per entity (by type and the hash of its key)
//!   with the document `entity.json` holds on one machine: its state and
//!   the ids of the messages applied to it.
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
//! **One owner per entity.** A message to an entity is applied in one
//! transaction that locks the entity's row (`FOR UPDATE` for `send`, `FOR
//! SHARE` for `ask`), reads its document, runs the handler and writes the
//! new document: what `flock` on its directory does on one machine, for
//! every machine. The ids of the applied messages are in the same row, so
//! a message is applied exactly once, whichever machine resumes its run.
//!
//! **TLS.** `sslmode=require` (or `verify-ca`, `verify-full`) in the URL
//! encrypts the connection, checking the server's certificate against the
//! system's roots, and `sslrootcert=FILE` adds a CA (a self-signed server).
//! `require` encrypts without checking who answers, as libpq does;
//! `verify-full` also checks the host name. Without `sslmode`, the
//! connection tries TLS and falls back to plain text (`prefer`).

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Mutex;

use postgres::Client;

mod tls;

pub const URL_ENV: &str = "CALYX_DATABASE_URL";

static CLIENT: Mutex<Option<Client>> = Mutex::new(None);
/// Entities have their own connection: a message holds its entity's row
/// lock until its transaction ends, and the journal must not wait for it.
static ENTITY_CLIENT: Mutex<Option<Client>> = Mutex::new(None);

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
);
CREATE TABLE IF NOT EXISTS calyx_files (
    run text NOT NULL,
    name text NOT NULL,
    seq bigserial,
    line text NOT NULL,
    PRIMARY KEY (run, name, seq)
);
CREATE TABLE IF NOT EXISTS calyx_entities (
    entity text NOT NULL,
    khash text NOT NULL,
    key text NOT NULL,
    doc text NOT NULL DEFAULT '',
    PRIMARY KEY (entity, khash)
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
    on(&CLIENT, f)
}

fn on<T>(
    slot: &Mutex<Option<Client>>,
    f: impl FnOnce(&mut Client) -> Result<T, postgres::Error>,
) -> Result<T, String> {
    let mut guard = slot
        .lock()
        .map_err(|_| "the database connection is poisoned")?;
    if guard.is_none() {
        *guard = Some(connect()?);
    }
    let client = guard.as_mut().expect("connected above");
    let r = f(client).map_err(|e| e.to_string());
    // A connection that broke is opened again next time.
    if client.is_closed() {
        *guard = None;
    }
    r
}

fn connect() -> Result<Client, String> {
    let url = std::env::var(URL_ENV).map_err(|_| format!("{URL_ENV} is not set"))?;
    let mut client = tls::connect(&url).map_err(|e| format!("cannot connect to {URL_ENV}: {e}"))?;
    // Several machines may start at once: one creates the tables.
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    tx.execute("SELECT pg_advisory_xact_lock(7461929)", &[])
        .and_then(|_| tx.batch_execute(SCHEMA))
        .map_err(|e| format!("cannot create the journal tables: {e}"))?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(client)
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

/// The lines of one of a run's files (`waits.jsonl`, `inbox.jsonl`), in
/// order.
pub fn file_lines(run: &str, name: &str) -> Result<Vec<String>, String> {
    with(|c| {
        Ok(c.query(
            "SELECT line FROM calyx_files WHERE run = $1 AND name = $2 ORDER BY seq",
            &[&run, &name],
        )?
        .iter()
        .map(|r| r.get(0))
        .collect())
    })
}

/// Appends `line` to a run's `inbox.jsonl` unless a line there has the same
/// `key` already: two machines delivering to one `receive` at once, only
/// one message is taken. `false` if one was there.
pub fn deliver_once(run: &str, key: &str, line: &str) -> Result<bool, String> {
    with(|c| {
        let mut tx = c.transaction()?;
        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtextextended($1 || ' inbox', 0))",
            &[&run],
        )?;
        let taken: bool = tx
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM calyx_files WHERE run = $1 \
                 AND name = 'inbox.jsonl' AND (line::jsonb)->>'key' = $2)",
                &[&run, &key],
            )?
            .get(0);
        if !taken {
            tx.execute(
                "INSERT INTO calyx_files (run, name, line) VALUES ($1, 'inbox.jsonl', $2)",
                &[&run, &line],
            )?;
        }
        tx.commit()?;
        Ok(!taken)
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

/// Appends one line to one of a run's files, committed before it returns.
///
/// # Safety
/// `run` and `name` are NUL-terminated; `line` points to `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_pg_file_append(
    run: *const c_char,
    name: *const c_char,
    line: *const u8,
    len: usize,
) -> c_int {
    let (Some(run), Some(name)) = (text(run), text(name)) else {
        return 0;
    };
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
            "INSERT INTO calyx_files (run, name, line) VALUES ($1, $2, $3)",
            &[&run, &name, &line],
        )
    }))
}

/// One of a run's files as text, one line per entry, each ending in `\n`
/// (empty if there is none); NULL on error. Free it with
/// `calyx_string_free`.
///
/// # Safety
/// Both are NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_pg_file_read(
    run: *const c_char,
    name: *const c_char,
) -> *mut c_char {
    let (Some(run), Some(name)) = (text(run), text(name)) else {
        return std::ptr::null_mut();
    };
    match file_lines(&run, &name) {
        Ok(lines) => {
            let mut all = String::new();
            for l in lines {
                all.push_str(&l);
                all.push('\n');
            }
            CString::new(all).map_or(std::ptr::null_mut(), CString::into_raw)
        }
        Err(e) => {
            eprintln!("calyx: journal database: {e}");
            std::ptr::null_mut()
        }
    }
}

/// What a message does to an entity: given its document (NULL for a new
/// entity), the new document (allocated with `malloc`), or NULL to leave it
/// as it is.
pub type EntityStep = unsafe extern "C" fn(ud: *mut c_void, doc: *const c_char) -> *mut c_char;

unsafe extern "C" {
    fn free(p: *mut c_void);
}

/// Applies one message to an entity, in one transaction holding its row:
/// exclusively for a `send` (`exclusive` = 1), shared for an `ask`. `step`
/// runs inside it. 1 when the transaction committed, 0 on error (and then
/// nothing was written).
///
/// # Safety
/// The strings are NUL-terminated; `step` is safe to call with `ud`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calyx_pg_entity(
    entity: *const c_char,
    khash: *const c_char,
    key: *const c_char,
    exclusive: c_int,
    step: EntityStep,
    ud: *mut c_void,
) -> c_int {
    let (Some(entity), Some(khash), Some(key)) = (text(entity), text(khash), text(key)) else {
        return 0;
    };
    status_ok(on(&ENTITY_CLIENT, |c| {
        let mut tx = c.transaction()?;
        tx.execute(
            "INSERT INTO calyx_entities (entity, khash, key) VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING",
            &[&entity, &khash, &key],
        )?;
        let lock = if exclusive != 0 { "UPDATE" } else { "SHARE" };
        let doc: String = tx
            .query_one(
                &format!(
                    "SELECT doc FROM calyx_entities WHERE entity = $1 AND khash = $2 FOR {lock}"
                ),
                &[&entity, &khash],
            )?
            .get(0);
        let old = (!doc.is_empty()).then(|| CString::new(doc).unwrap_or_default());
        // SAFETY: the caller's contract; `old` lives until the call returns.
        let new = unsafe { step(ud, old.as_ref().map_or(std::ptr::null(), |d| d.as_ptr())) };
        if !new.is_null() {
            // SAFETY: `step` returns a NUL-terminated string from `malloc`.
            let doc = unsafe { CStr::from_ptr(new) }
                .to_string_lossy()
                .into_owned();
            unsafe { free(new.cast()) };
            tx.execute(
                "UPDATE calyx_entities SET doc = $3 WHERE entity = $1 AND khash = $2",
                &[&entity, &khash, &doc],
            )?;
        }
        tx.commit()
    }))
}
