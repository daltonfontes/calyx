//! Golden tests: every `tests/programs/*.clyx` is checked and its rendered
//! diagnostics compared with the sibling `.expected` file (empty or missing
//! means "no diagnostics"). Run with `UPDATE_EXPECT=1` to rewrite them.
//!
//! Also checks that every program in `examples/` is accepted.

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

#[test]
fn programs_match_expected_diagnostics() {
    let update = std::env::var_os("UPDATE_EXPECT").is_some();
    let mut files = Vec::new();
    clyx_files(&repo_root().join("tests/programs"), &mut files);
    assert!(!files.is_empty(), "no programs found in tests/programs");

    let mut failures = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let actual = calyx_check::check(&name, &text).render();
        let expected_path = path.with_extension("expected");
        if update {
            fs::write(&expected_path, &actual).unwrap();
            continue;
        }
        let expected = fs::read_to_string(&expected_path).unwrap_or_default();
        if actual != expected {
            failures.push(format!(
                "{}\n--- expected\n{expected}--- actual\n{actual}",
                display_name(&path)
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn examples_are_accepted() {
    let mut files = Vec::new();
    clyx_files(&repo_root().join("examples"), &mut files);
    assert!(!files.is_empty(), "no examples found");
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        let report = calyx_check::check(&display_name(&path), &text);
        assert!(!report.has_errors(), "{}", report.render());
    }
}
