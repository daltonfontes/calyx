//! Compiles the C interpreter into this crate, so the Rust I/O layer and
//! the C runtime end up in one library.

fn main() {
    let sources = ["src/runtime.c", "src/json.c", "src/exec.c"];
    for f in sources.iter().chain(&[
        "src/json.h",
        "include/calyx_runtime.h",
        "include/calyx_io.h",
        "include/calyx_verify.h",
    ]) {
        println!("cargo:rerun-if-changed={f}");
    }
    cc::Build::new()
        .files(sources)
        .include("include")
        .std("c11")
        .warnings(true)
        .extra_warnings(true)
        .flag_if_supported("-Wpedantic")
        .warnings_into_errors(true)
        .compile("calyx_interp");
    println!("cargo:rustc-link-lib=m");
}
