//! The Calyx runtime.
//!
//! The interpreter is written in C (decision D10) and lives in
//! `runtime/src`. This crate builds it and adds what C is poorly suited
//! for: the shared verifier ([`verify`]) and the I/O layer the interpreter
//! calls for effects (models over HTTPS, tools over MCP; see [`io`]).
//! It is also a static library, so a C program links one archive.

pub mod config;
pub mod io;
mod llm;
mod mcp;
pub mod verify;

use std::ffi::{CStr, CString, c_char};
use std::path::PathBuf;

pub use config::Config;

unsafe extern "C" {
    fn calyx_run(
        ir_json: *const c_char,
        graph: *const c_char,
        args_json: *const c_char,
        options_json: *const c_char,
    ) -> *mut c_char;
    fn calyx_run_free(s: *mut c_char);
}

/// What to do with the run's journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// A new run.
    #[default]
    New,
    /// Continue an interrupted run: calls already in its journal are not
    /// made again.
    Resume,
    /// Run again from the journal only, without calling models or tools.
    Replay,
}

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub config: Config,
    /// Models answer with fake values shaped by the prompt's type.
    pub fake_models: bool,
    /// One line per node and call on stderr.
    pub trace: bool,
    /// The run's directory (journal and blobs). `None`: no journal.
    pub journal: Option<PathBuf>,
    pub mode: Mode,
    /// The source file, recorded in the journal to find it again.
    pub program: Option<PathBuf>,
    /// One worker and one call at a time: the same order on every run.
    pub deterministic: bool,
    /// Replaces the program's `budget` (in USD), e.g. to continue a run
    /// that used it up.
    pub budget_usd: Option<f64>,
}

/// Runs `graph` of a compiled program (the IR in JSON) with `args` (a JSON
/// object of parameter name to value). Returns the result as JSON.
pub fn run(
    ir_json: &str,
    graph: &str,
    args: &serde_json::Value,
    opts: RunOptions,
) -> Result<serde_json::Value, String> {
    let options = serde_json::json!({
        "trace": opts.trace,
        "journal": opts.journal.as_ref().map(|p| p.display().to_string()),
        "mode": match opts.mode {
            Mode::New => "new",
            Mode::Resume => "resume",
            Mode::Replay => "replay",
        },
        "program": opts.program.as_ref().map(|p| p.display().to_string()),
        "deterministic": opts.deterministic,
        "budget_usd": opts.budget_usd,
    });
    io::configure(opts.config, opts.fake_models);
    let c = |s: &str| CString::new(s).map_err(|_| "text contains NUL".to_owned());
    let (ir, g, a, o) = (
        c(ir_json)?,
        c(graph)?,
        c(&args.to_string())?,
        c(&options.to_string())?,
    );
    // SAFETY: valid NUL-terminated strings; the result is released below.
    let out = unsafe { calyx_run(ir.as_ptr(), g.as_ptr(), a.as_ptr(), o.as_ptr()) };
    io::shutdown();
    if out.is_null() {
        return Err("the runtime returned nothing (out of memory?)".into());
    }
    // SAFETY: `out` is a NUL-terminated string from `calyx_run`.
    let text = unsafe { CStr::from_ptr(out) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: released once, with the matching function.
    unsafe { calyx_run_free(out) };
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("invalid runtime output: {e}"))?;
    match v.get("ok") {
        Some(value) => Ok(value.clone()),
        None => Err(v["error"].as_str().unwrap_or("unknown error").to_owned()),
    }
}
