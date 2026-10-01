//! `calyx build` (decision D35): a standalone executable that runs one
//! graph, with the program and its calyx.toml inside.
//!
//! Each test builds into its own directory and runs the result from a
//! different one, so nothing is found next to the source by accident.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROGRAM: &str = r#"
model m = "fake-model"

tool web_search(query: Text) -> Text:
    effect read
    max_output 50 tokens

prompt summarize(q: Text, found: Text) -> Text:
    """{q}: {found}"""

graph helper(x: Text) -> Text:
    return "helper {x}"

graph main(topic: Text, rounds: Nat) -> Text:
    found = web_search(topic)
    return m(summarize("{topic} x{rounds}", found))
"#;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("calyx-build-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
        let search = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/tools/fake_search.py")
            .canonicalize()
            .unwrap();
        std::fs::write(
            dir.join("src/calyx.toml"),
            format!(
                "[tools.web_search]\ncommand = [\"python3\", \"{}\"]\n",
                search.display()
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src/p.clyx"), PROGRAM).unwrap();
        Dir(dir)
    }

    fn calyx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }

    /// Runs the built program from a directory with no calyx.toml.
    fn program(&self, name: &str, args: &[&str]) -> Output {
        Command::new(self.0.join(name))
            .args(args)
            .current_dir(self.0.join("elsewhere"))
            .output()
            .unwrap()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn a_built_program_runs_its_graph_with_the_built_in_config() {
    let d = Dir::new("runs");
    let out = d.calyx(&["build", "src/p.clyx", "-o", "research"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("runs graph `main`"));

    let out = d.program(
        "research",
        &[
            "--topic",
            "sol",
            "--rounds",
            "2",
            "--fake-models",
            "--quiet",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    // The tool ran on the MCP server named in the built-in calyx.toml.
    let stdout = text(&out.stdout);
    assert!(
        stdout.starts_with("[resposta falsa para: sol x2: [1] sol - fonte"),
        "{stdout}"
    );
    // The run is journaled where the program runs.
    assert!(d.0.join("elsewhere/.calyx/runs").is_dir());
}

#[test]
fn help_lists_the_graph_parameters_and_errors_name_the_program() {
    let d = Dir::new("help");
    assert!(
        d.calyx(&["build", "src/p.clyx", "-o", "r"])
            .status
            .success()
    );
    let help = text(&d.program("r", &["--help"]).stdout);
    assert!(
        help.contains("usage: r --topic <Text> --rounds <Nat> [options]"),
        "{help}"
    );
    assert!(help.contains("Runs graph `main` of p.clyx"), "{help}");

    let out = d.program("r", &["--topic", "sol", "--rounds", "-1", "--fake-models"]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(
        err.starts_with("r: --rounds: expected a `Nat`, got `-1`"),
        "{err}"
    );

    let out = d.program("r", &["extra"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).starts_with("r: unexpected `extra`"));
}

#[test]
fn build_chooses_the_graph_and_can_leave_the_config_out() {
    let d = Dir::new("graph");
    let out = d.calyx(&["build", "src/p.clyx", "--graph", "helper", "--no-config"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    // Default name: the file's, without `.clyx`.
    let out = d.program("p", &["--x", "oi", "--quiet", "--no-journal"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "helper oi\n");

    let out = d.calyx(&["build", "src/p.clyx", "--graph", "nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("the program has no graph `nope`"));
}

#[test]
fn a_program_with_errors_is_not_built() {
    let d = Dir::new("errors");
    std::fs::write(
        d.0.join("src/bad.clyx"),
        "graph g() -> Text:\n    return x\n",
    )
    .unwrap();
    let out = d.calyx(&["build", "src/bad.clyx"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!d.0.join("bad").exists());
}

#[test]
fn an_interrupted_built_run_resumes_with_the_program_or_with_calyx() {
    let d = Dir::new("resume");
    assert!(
        d.calyx(&["build", "src/p.clyx", "-o", "r"])
            .status
            .success()
    );
    let out = Command::new(d.0.join("r"))
        .args([
            "--topic",
            "sol",
            "--rounds",
            "1",
            "--fake-models",
            "--quiet",
        ])
        .env("CALYX_CRASH_AFTER", "1")
        .current_dir(d.0.join("elsewhere"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = text(&out.stderr);
    let id = err
        .lines()
        .find_map(|l| l.strip_prefix("r: run "))
        .unwrap_or_else(|| panic!("{err}"))
        .to_owned();

    let out = d.program("r", &["resume", &id, "--fake-models", "--quiet"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let resumed = text(&out.stdout);

    // The journal names the binary; `calyx replay` finds the program in it.
    let out = Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(["replay", &id, "--quiet"])
        .current_dir(d.0.join("elsewhere"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), resumed);
}
