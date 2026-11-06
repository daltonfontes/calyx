//! OAuth for remote MCP servers: `calyx login` once, then runs with no one
//! there, refreshing the token when the server refuses it.
//!
//! The server is tests/oauth_server.py: an MCP server, its authorization
//! server and token endpoint, checking PKCE, the registered redirect, the
//! `resource`, and rotating refresh tokens.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const PROGRAM: &str = r#"
tool whoami(who: Text) -> Text:
    effect read

graph main(who: Text) -> Text:
    return whoami(who)
"#;

struct Fixture {
    server: Child,
    port: u16,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture(name: &str) -> Fixture {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oauth_server.py");
    let mut server = Command::new("python3")
        .arg(&script)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(server.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line.trim().strip_prefix("port ").unwrap().parse().unwrap();
    let dir = std::env::temp_dir().join(format!("calyx-oauth-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("p.clyx"), PROGRAM).unwrap();
    std::fs::write(
        dir.join("calyx.toml"),
        format!("[tools.whoami]\nurl = \"http://127.0.0.1:{port}/mcp\"\noauth = true\n"),
    )
    .unwrap();
    Fixture { server, port, dir }
}

impl Fixture {
    fn calyx(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_calyx"));
        c.args(args)
            .current_dir(&self.dir)
            .env("CALYX_OAUTH_DIR", self.dir.join("oauth"));
        c
    }

    fn run(&self) -> std::process::Output {
        self.calyx(&["run", "p.clyx", "--quiet", "--no-journal", "--who", "ana"])
            .output()
            .unwrap()
    }

    /// A GET to the test server's admin paths; the body.
    fn admin(&self, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        write!(s, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out.split_once("\r\n\r\n").unwrap().1.to_owned()
    }

    fn stat(&self, key: &str) -> u64 {
        let v: serde_json::Value = serde_json::from_str(&self.admin("/admin/stats")).unwrap();
        v[key].as_u64().unwrap()
    }

    /// `calyx login`, with the test as the person: it opens the address
    /// `calyx login` prints, and follows the authorization server's redirect
    /// back to the listener on 127.0.0.1, as a browser would.
    fn login(&self) -> std::process::Output {
        let mut child = self
            .calyx(&["login", "whoami", "--no-browser"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut printed = String::new();
        let address = loop {
            let mut line = String::new();
            assert!(
                out.read_line(&mut line).unwrap() > 0,
                "login printed: {printed}"
            );
            printed.push_str(&line);
            if let Some(a) = line.trim().strip_prefix("http://") {
                break format!("http://{a}");
            }
        };
        let browser = Command::new("python3")
            .args([
                "-c",
                "import sys, urllib.request; urllib.request.urlopen(sys.argv[1]).read()",
            ])
            .arg(&address)
            .status()
            .unwrap();
        assert!(browser.success(), "the browser could not follow {address}");
        let mut rest = String::new();
        out.read_to_string(&mut rest).unwrap();
        let mut o = child.wait_with_output().unwrap();
        o.stdout = (printed + &rest).into_bytes();
        o
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn login_once_then_runs_refresh_on_their_own() {
    let f = fixture("flow");

    // Before the login: the run says what to do.
    let o = f.run();
    assert!(!o.status.success());
    assert!(
        text(&o.stderr).contains("calyx login"),
        "{}",
        text(&o.stderr)
    );

    let o = f.login();
    assert!(o.status.success(), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(
        text(&o.stdout).contains("logged in to"),
        "{}",
        text(&o.stdout)
    );
    assert_eq!(f.stat("codes"), 1);
    let kept: Vec<_> = std::fs::read_dir(f.dir.join("oauth"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .collect();
    assert_eq!(kept.len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = kept[0].metadata().unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the tokens must be readable only by their owner"
        );
    }

    // A run uses the token: no refresh needed.
    let o = f.run();
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "hello ana, from behind OAuth");
    assert_eq!(f.stat("refreshes"), 0);

    // The server stops taking the token: the run refreshes it, and goes on.
    f.admin("/admin/expire");
    let o = f.run();
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(f.stat("refreshes"), 1);
    // And the new refresh token was kept (the old one is refused now).
    f.admin("/admin/expire");
    let o = f.run();
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(f.stat("refreshes"), 2);

    // The person revoked the app: the run says to log in again.
    f.admin("/admin/revoke");
    let o = f.run();
    assert!(!o.status.success());
    assert!(
        text(&o.stderr).contains("run `calyx login <tool>` again"),
        "{}",
        text(&o.stderr)
    );

    // Logout forgets the tokens.
    let o = f.calyx(&["logout", "whoami"]).output().unwrap();
    assert!(
        text(&o.stdout).contains("logged out"),
        "{}",
        text(&o.stdout)
    );
    let o = f.run();
    assert!(
        text(&o.stderr).contains("needs a login"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn oauth_is_for_remote_servers_and_not_with_a_fixed_token() {
    let f = fixture("config");
    std::fs::write(
        f.dir.join("calyx.toml"),
        "[tools.whoami]\ncommand = [\"python3\", \"x.py\"]\noauth = true\n",
    )
    .unwrap();
    let o = f.calyx(&["login", "whoami"]).output().unwrap();
    assert!(
        text(&o.stderr).contains("`oauth` is for a remote server"),
        "{}",
        text(&o.stderr)
    );
    std::fs::write(
        f.dir.join("calyx.toml"),
        "[tools.whoami]\nurl = \"https://x.example/mcp\"\ntoken_env = \"T\"\noauth = true\n",
    )
    .unwrap();
    let o = f.calyx(&["login", "whoami"]).output().unwrap();
    assert!(text(&o.stderr).contains("use one"), "{}", text(&o.stderr));
}
