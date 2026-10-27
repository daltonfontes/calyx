//! A program saved by a Windows editor (`\r\n` line ends) is the same
//! program: it compiles to the same IR, so its runs resume on any system,
//! and no `\r` reaches a prompt.

use std::process::Command;

const PROGRAM: &str = "model m = \"fake-model\"\n\nprompt say(x: Text) -> Text:\n    \"\"\"\n    Diga {x}\n    em duas linhas.\n    \"\"\"\n\ngraph g(x: Text) -> Text:\n    a = m(say(x))\n    return a\n";

#[test]
fn a_windows_file_compiles_to_the_same_program() {
    let d = std::env::temp_dir().join(format!("calyx-crlf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("lf.clyx"), PROGRAM).unwrap();
    std::fs::write(d.join("crlf.clyx"), PROGRAM.replace('\n', "\r\n")).unwrap();
    let ir = |f: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_calyx"))
            .args(["check", "--ir-json", f])
            .current_dir(&d)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let (lf, crlf) = (ir("lf.clyx"), ir("crlf.clyx"));
    assert!(!crlf.contains("\\r"), "{crlf}");
    assert_eq!(lf, crlf);
    let _ = std::fs::remove_dir_all(&d);
}
