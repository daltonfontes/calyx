//! Isolated tools (`isolate = true` in calyx.toml): the tool's MCP server
//! runs under Landlock, the Linux kernel's sandbox for unprivileged
//! processes, so what it runs (tests, scripts, code an agent wrote) cannot
//! reach the rest of the machine:
//!
//! - it **writes** only in the run's sandboxes and a temporary directory of
//!   its own (`TMPDIR`), and `/dev/null`;
//! - it **reads** only the system (`/usr`, `/etc`, `/lib`...), the
//!   directories of its own command (its script, not the whole project,
//!   where a `.env` may be), the sandboxes, and what `read` adds;
//! - it opens **no TCP connections** and listens on none, unless
//!   `network = true`.
//!
//! Where Landlock is not available (another system, an old kernel, or one
//! built without it), the server does not start: a tool asked to be
//! isolated never runs without it. UDP and Unix sockets are not covered by
//! Landlock's network rules.
//!
//! The rules are built before the server starts; the child applies them
//! between `fork` and `exec` with two system calls and no allocation.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::IsolateConfig;

/// The directories a tool may write in, for this run: its sandboxes.
static SANDBOXES: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// The run's sandboxes, which isolated tools may write in.
pub fn set_sandboxes(paths: Vec<PathBuf>) {
    *SANDBOXES.lock().unwrap_or_else(|e| e.into_inner()) = paths;
}

/// Makes `command` (an MCP server's, run in `dir`) start isolated.
pub fn apply(
    command: &mut Command,
    cmd: &[String],
    dir: &Path,
    cfg: &IsolateConfig,
) -> Result<(), String> {
    let sandboxes = SANDBOXES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    imp::apply(command, cmd, dir, cfg, &sandboxes)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// The directories a command reads its own code from: its program's, if a
/// path, and those of its arguments that are files (a script).
fn own_dirs(cmd: &[String], dir: &Path) -> Vec<PathBuf> {
    cmd.iter()
        .map(|a| {
            let p = Path::new(a);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                dir.join(p)
            }
        })
        .filter(|p| p.is_file())
        .filter_map(|p| p.canonicalize().ok()?.parent().map(Path::to_path_buf))
        .collect()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use crate::config::IsolateConfig;

    const CREATE_RULESET: libc::c_long = 444;
    const ADD_RULE: libc::c_long = 445;
    const RESTRICT_SELF: libc::c_long = 446;
    const RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: u32 = 1;

    // Filesystem rights (`LANDLOCK_ACCESS_FS_*`).
    const EXECUTE: u64 = 1;
    const WRITE_FILE: u64 = 1 << 1;
    const READ_FILE: u64 = 1 << 2;
    const READ_DIR: u64 = 1 << 3;
    const REFER: u64 = 1 << 13; // ABI 2
    const TRUNCATE: u64 = 1 << 14; // ABI 3
    const IOCTL_DEV: u64 = 1 << 15; // ABI 5
    /// The rights that apply to a file (the others are for directories).
    const FILE_RIGHTS: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE | IOCTL_DEV;
    // Network rights (`LANDLOCK_ACCESS_NET_*`), ABI 4.
    const BIND_TCP: u64 = 1;
    const CONNECT_TCP: u64 = 1 << 1;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
        handled_access_net: u64,
    }

    #[repr(C, packed)]
    struct PathBeneath {
        allowed_access: u64,
        parent_fd: i32,
    }

    /// The system's own files: programs, libraries, configuration.
    /// (Not `/run`, where sockets such as Docker's are, nor all of `/dev`.)
    const SYSTEM: &[&str] = &[
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib32",
        "/lib64",
        "/libx32",
        "/etc",
        "/opt",
        "/nix",
        "/proc",
        "/dev/zero",
        "/dev/random",
        "/dev/urandom",
    ];

    fn abi() -> i64 {
        // SAFETY: the documented way to ask for the version: no attributes.
        unsafe {
            libc::syscall(
                CREATE_RULESET,
                std::ptr::null::<u8>(),
                0usize,
                RULESET_VERSION,
            )
        }
    }

    pub fn apply(
        command: &mut Command,
        cmd: &[String],
        dir: &Path,
        cfg: &IsolateConfig,
        sandboxes: &[PathBuf],
    ) -> Result<(), String> {
        let abi = abi();
        if abi < 1 {
            return Err("`isolate` needs Landlock, and this kernel has none (Linux 5.13 or later, built with it)".into());
        }
        if !cfg.network && abi < 4 {
            return Err(format!(
                "`isolate` without network needs Landlock 4 (Linux 6.7); this kernel has Landlock {abi}: upgrade it, or set `network = true` for this tool"
            ));
        }
        let mut handled_fs = (1u64 << 13) - 1;
        if abi >= 2 {
            handled_fs |= REFER;
        }
        if abi >= 3 {
            handled_fs |= TRUNCATE;
        }
        if abi >= 5 {
            handled_fs |= IOCTL_DEV;
        }
        let attr = RulesetAttr {
            handled_access_fs: handled_fs,
            handled_access_net: if cfg.network {
                0
            } else {
                BIND_TCP | CONNECT_TCP
            },
        };
        let size = if cfg.network {
            std::mem::size_of::<u64>()
        } else {
            std::mem::size_of::<RulesetAttr>()
        };
        // SAFETY: `attr` is a valid ruleset attribute of `size` bytes.
        let fd = unsafe { libc::syscall(CREATE_RULESET, &attr as *const RulesetAttr, size, 0u32) };
        if fd < 0 {
            return Err(format!("Landlock: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: a new file descriptor, owned from here on.
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        let read = EXECUTE | READ_FILE | READ_DIR;
        let tmp = std::env::temp_dir().join(format!(
            "calyx-tool-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let mut rules: Vec<(PathBuf, u64)> =
            SYSTEM.iter().map(|p| (PathBuf::from(p), read)).collect();
        rules.extend(super::own_dirs(cmd, dir).into_iter().map(|p| (p, read)));
        rules.extend(cfg.read.iter().map(|p| (p.clone(), read)));
        let write = handled_fs;
        rules.extend(sandboxes.iter().map(|p| (p.clone(), write)));
        rules.extend(cfg.write.iter().map(|p| (p.clone(), write)));
        rules.push((tmp.clone(), write));
        rules.push((
            PathBuf::from("/dev/null"),
            READ_FILE | WRITE_FILE | TRUNCATE,
        ));
        for (path, access) in rules {
            let Ok(file) = std::fs::File::open(&path) else {
                continue; // not on this system
            };
            let is_file = file.metadata().is_ok_and(|m| !m.is_dir());
            let access = access & handled_fs & if is_file { FILE_RIGHTS } else { u64::MAX };
            let rule = PathBeneath {
                allowed_access: access,
                parent_fd: file.as_raw_fd(),
            };
            // SAFETY: a valid ruleset and rule; `file` is open during the call.
            let r = unsafe {
                libc::syscall(
                    ADD_RULE,
                    ruleset.as_raw_fd(),
                    RULE_PATH_BENEATH,
                    &rule as *const PathBeneath,
                    0u32,
                )
            };
            if r < 0 {
                return Err(format!(
                    "Landlock: {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
        }
        command.env("TMPDIR", &tmp);
        // The closure holds the ruleset (closed on exec) until the command
        // is dropped; the child restricts itself with it before `exec`.
        // SAFETY: only system calls between fork and exec, no allocation.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                    || libc::syscall(RESTRICT_SELF, ruleset.as_raw_fd(), 0u32) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use crate::config::IsolateConfig;

    pub fn apply(
        _command: &mut Command,
        _cmd: &[String],
        _dir: &Path,
        _cfg: &IsolateConfig,
        _sandboxes: &[PathBuf],
    ) -> Result<(), String> {
        Err("`isolate` needs Landlock, which only Linux has: on this system an isolated tool does not run (run it on Linux, or in a container)".into())
    }
}
