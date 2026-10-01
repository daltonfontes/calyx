//! Golden tests: every `tests/programs/*.clyx` is checked and its rendered
//! diagnostics compared with the sibling `.expected` file (empty or missing
//! means "no diagnostics"). Run with `UPDATE_EXPECT=1` to rewrite them.
//!
//! Programs without errors also have their compiled template compared with
//! the sibling `.ir` file. Also checks the programs in `examples/`.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn clyx_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            clyx_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "clyx") {
            out.push(path);
        }
    }
}

fn display_name(path: &Path) -> String {
    path.strip_prefix(repo_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Examples that use only supported constructs and must pass the full check.
/// The others use constructs from later milestones and are only lexed.
const FULLY_CHECKED: &[&str] = &[
    "examples/research.clyx",
    "examples/agent.clyx",
    "examples/refund.clyx",
    "examples/fix.clyx",
    "examples/memory.clyx",
    "examples/approval.clyx",
    "examples/debate.clyx",
    "examples/race.clyx",
];

#[test]
fn programs_match_expected_diagnostics_and_ir() {
    let update = std::env::var_os("UPDATE_EXPECT").is_some();
    let mut files = Vec::new();
    clyx_files(&repo_root().join("tests/programs"), &mut files);
    assert!(!files.is_empty(), "no programs found in tests/programs");

    let mut failures = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let report = calyx_check::check(&name, &text);
        let mut outputs = vec![(path.with_extension("expected"), report.render())];
        if !report.has_errors() {
            outputs.push((path.with_extension("ir"), report.ir.to_string()));
        }
        for (expected_path, actual) in outputs {
            if update {
                if actual.is_empty() {
                    let _ = fs::remove_file(&expected_path);
                } else {
                    fs::write(&expected_path, &actual).unwrap();
                }
                continue;
            }
            let expected = fs::read_to_string(&expected_path).unwrap_or_default();
            if actual != expected {
                failures.push(format!(
                    "{}\n--- expected\n{expected}--- actual\n{actual}",
                    display_name(&expected_path)
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn examples_are_valid() {
    let mut files = Vec::new();
    clyx_files(&repo_root().join("examples"), &mut files);
    assert!(!files.is_empty(), "no examples found");
    for path in files {
        let name = display_name(&path);
        let text = fs::read_to_string(&path).unwrap();
        let report = if FULLY_CHECKED.contains(&name.as_str()) {
            calyx_check::check(&name, &text)
        } else {
            calyx_check::lex_only(&name, &text)
        };
        assert!(report.diagnostics.is_empty(), "{}", report.render());
    }
}
