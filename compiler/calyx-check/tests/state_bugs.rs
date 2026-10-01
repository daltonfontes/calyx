//! Question Q2 of the hypothesis: how many state bugs does the compiler
//! catch before running? Each `tests/state_bugs/*.clyx` is a small workflow
//! with one known bug, and says in its second line who catches it:
//!
//! - `# Pego por: E0304` (or a warning, `W0602`): the compiler, with that code;
//! - `# Pego por: runtime`: not the compiler, but the runtime when the bug
//!   would bite (tested in `compiler/calyx-cli/tests/writes.rs`);
//! - `# Pego por: none`: nobody. These stay in the suite on purpose.
//!
//! For `runtime` and `none` the compiler must say nothing, so the count is
//! honest. Run with `--nocapture` to see the summary.

use std::fs;
use std::path::Path;

#[test]
fn state_bugs_are_caught_where_their_header_says() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/state_bugs");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "clyx"))
        .collect();
    files.sort();
    assert!(files.len() >= 20, "the suite has {} programs", files.len());

    let (mut compiler, mut runtime, mut none) = (0, 0, 0);
    let mut failures = Vec::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = fs::read_to_string(path).unwrap();
        let expect = text
            .lines()
            .find_map(|l| l.strip_prefix("# Pego por: "))
            .unwrap_or_else(|| panic!("{name}: no `# Pego por:` line"))
            .trim()
            .to_owned();
        let report = calyx_check::check(&name, &text);
        let codes: Vec<&str> = report.diagnostics.iter().map(|d| d.code).collect();
        match expect.as_str() {
            "runtime" | "none" => {
                if !codes.is_empty() {
                    failures.push(format!("{name}: expected no diagnostics, got {codes:?}"));
                }
                if expect == "runtime" {
                    runtime += 1;
                } else {
                    none += 1;
                }
            }
            code => {
                if !codes.contains(&code) {
                    failures.push(format!("{name}: expected {code}, got {codes:?}"));
                }
                compiler += 1;
            }
        }
    }
    eprintln!(
        "Q2: {} bugs; compiler {compiler}, runtime {runtime}, not caught {none}",
        files.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
