//! `isolate = true`: a tool's MCP server under Landlock. The server is
//! tests/isolate_server.py, whose one tool tries to read a secret outside
//! its sandbox, write outside it, open a TCP connection, write inside it
//! and use its temporary directory, and says what worked.
//!
//! Where Landlock is not available (Windows, macOS, an old kernel), the
//! run must refuse to start the tool: an isolated tool never runs without it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROGRAM: &str = r#"
tool probe(box: edits Sandbox, secret: Text, outside: Text, port: Text) -> Text:
    effect sandbox

graph main(repo: Sandbox, secret: Text, outside: Text, port: Text) -> Text:
    return probe(edits repo, secret, outside, port)
"#;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup(name: &str, isolate: bool) -> Dir {
    let d = std::env::temp_dir().join(format!("calyx-isolate-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("repo")).unwrap();
    std::fs::create_dir_all(d.join("private")).unwrap();
    std::fs::write(d.join("repo/README"), "hi").unwrap();
    std::fs::write(d.join("private/secret.txt"), "the secret").unwrap();
    std::fs::write(d.join("p.clyx"), PROGRAM).unwrap();
    let server = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/isolate_server.py");
    std::fs::write(
        d.join("calyx.toml"),
        format!(
            "[tools.probe]\ncommand = [\"python3\", '{}']\nisolate = {isolate}\n",
            server.display()
        ),
    )
    .unwrap();
    Dir(d)
}

fn run(d: &Dir, port: u16) -> Output {
    let p = |s: &str| d.0.join(s).display().to_string();
    Command::new(env!("CARGO_BIN_EXE_calyx"))
        .args(["run", "p.clyx", "--quiet", "--repo", &p("repo"), "--secret"])
        .arg(p("private/secret.txt"))
        .arg("--outside")
        .arg(p("private/written.txt"))
        .args(["--port", &port.to_string()])
        .current_dir(&d.0)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Whether this kernel has Landlock with network rules (version 4).
#[cfg(target_os = "linux")]
fn landlock() -> bool {
    // SAFETY: asks the version, with no attributes.
    unsafe { libc::syscall(444, std::ptr::null::<u8>(), 0usize, 1u32) >= 4 }
}

#[cfg(not(target_os = "linux"))]
fn landlock() -> bool {
    false
}

#[test]
fn an_isolated_tool_reaches_only_its_sandbox() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    // Not isolated: the tool reaches everything its user can.
    let open = setup("open", false);
    let o = run(&open, port);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let got: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(got["read_secret"], "yes", "{got}");
    assert_eq!(got["connect"], "yes", "{got}");

    let shut = setup("shut", true);
    let o = run(&shut, port);
    if !landlock() {
        // Where the confinement itself must be tested (CI on Linux), a
        // kernel without Landlock fails the test instead of passing it by
        // the refusal alone.
        assert!(
            std::env::var_os("CALYX_REQUIRE_LANDLOCK").is_none(),
            "CALYX_REQUIRE_LANDLOCK is set, and this kernel has no Landlock with network rules"
        );
        // Refused before the tool runs; nothing written outside.
        assert!(!o.status.success());
        assert!(
            text(&o.stderr).contains("`isolate` needs Landlock"),
            "{}",
            text(&o.stderr)
        );
        assert!(!shut.0.join("private/written.txt").exists());
        return;
    }
    assert!(o.status.success(), "{}", text(&o.stderr));
    let got: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        got["read_secret"].as_str().unwrap().starts_with("no"),
        "{got}"
    );
    assert!(
        got["write_outside"].as_str().unwrap().starts_with("no"),
        "{got}"
    );
    assert!(got["connect"].as_str().unwrap().starts_with("no"), "{got}");
    assert_eq!(got["write_inside"], "yes", "{got}");
    assert_eq!(got["temp"], "yes", "{got}");
    assert!(!shut.0.join("private/written.txt").exists());
}

#[test]
fn isolate_is_for_local_servers() {
    let d = setup("remote", true);
    std::fs::write(
        d.0.join("calyx.toml"),
        "[tools.probe]\nurl = \"https://x.example/mcp\"\nisolate = true\n",
    )
    .unwrap();
    let o = run(&d, 1);
    assert!(
        text(&o.stderr).contains("`isolate` is for a local server"),
        "{}",
        text(&o.stderr)
    );
}
