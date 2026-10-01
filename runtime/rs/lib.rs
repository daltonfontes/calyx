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

use std::ffi::{CStr, CString, c_char, c_int};

pub use config::Config;

unsafe extern "C" {
    fn calyx_run(
        ir_json: *const c_char,
        graph: *const c_char,
        args_json: *const c_char,
        flags: c_int,
    ) -> *mut c_char;
    fn calyx_run_free(s: *mut c_char);
}

/// Flag for `calyx_run`: print one line per node and effect to stderr.
pub const RUN_TRACE: c_int = 1;

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub config: Config,
    /// Models answer with fake values shaped by the prompt's type.
    pub fake_models: bool,
    pub trace: bool,
}

/// Runs `graph` of a compiled program (the IR in JSON) with `args` (a JSON
/// object of parameter name to value). Returns the result as JSON.
pub fn run(
    ir_json: &str,
    graph: &str,
    args: &serde_json::Value,
    opts: RunOptions,
) -> Result<serde_json::Value, String> {
    io::configure(opts.config, opts.fake_models);
    let c = |s: &str| CString::new(s).map_err(|_| "text contains NUL".to_owned());
    let (ir, g, a) = (c(ir_json)?, c(graph)?, c(&args.to_string())?);
    let flags = if opts.trace { RUN_TRACE } else { 0 };
    // SAFETY: valid NUL-terminated strings; the result is released below.
    let out = unsafe { calyx_run(ir.as_ptr(), g.as_ptr(), a.as_ptr(), flags) };
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
