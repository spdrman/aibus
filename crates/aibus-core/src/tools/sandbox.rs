//! The optional OS sandbox for `bash` (09 section 2, `[tools] sandbox`). Off unless the user
//! asks. `bwrap` (Linux) mounts the system read-only and gives the command the project, its
//! working directory and `$TMPDIR` writable, and nothing else of the user's tree. `sandbox-exec`
//! (macOS) does the same with a profile. `network = false` cuts the network in both.
//!
//! Only `bash` goes through this; hooks and MCP servers are the user's own configuration.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::SandboxKind;

/// System directories a command needs to run at all. Mounted read-only, and only when present.
const SYSTEM_DIRS: [&str; 7] = ["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/etc"];

/// The `sandbox-exec` profile. Later rules win, so the allows that follow a deny re-open the
/// project, the working directory and `$TMPDIR`, which may sit inside the home directory.
const PROFILE: &str = r#"(version 1)
(allow default)
(deny file-write*)
(allow file-write*
  (subpath (param "PROJECT")) (subpath (param "CWD")) (subpath (param "TMP"))
  (literal "/dev/null") (literal "/dev/tty") (literal "/dev/dtracehelper") (regex #"^/dev/fd/"))
(deny file-read* (subpath "/Users") (subpath "/Volumes") (subpath (param "HOME")))
(allow file-read* (subpath (param "PROJECT")) (subpath (param "CWD")) (subpath (param "TMP")))
"#;

/// What a `bash` call runs inside, with the directories it may write.
#[derive(Clone, Debug)]
pub struct Sandbox {
    pub kind: SandboxKind,
    /// `false` cuts the network inside the sandbox.
    pub network: bool,
}

impl Sandbox {
    /// The command that runs `script` with `sh -c` for the sandbox. `project` and `cwd` (a
    /// worktree, say) stay writable, so does `tmp`; `home` is hidden even when it is the parent
    /// of the project.
    pub fn command(&self, script: &str, project: &Path, cwd: &Path, tmp: &Path, home: Option<&Path>) -> Command {
        let (project, cwd, tmp) = (real(project), real(cwd), real(tmp));
        let mut cmd = Command::new("sh");
        match self.kind {
            SandboxKind::None => {}
            SandboxKind::Bwrap => {
                cmd = Command::new("bwrap");
                cmd.args(["--die-with-parent", "--new-session", "--unshare-user-try", "--unshare-pid", "--unshare-ipc"]);
                if !self.network {
                    cmd.arg("--unshare-net");
                }
                for dir in SYSTEM_DIRS {
                    cmd.args(["--ro-bind-try", dir, dir]);
                }
                cmd.args(["--dev", "/dev", "--proc", "/proc"]);
                for dir in [&tmp, &project, &cwd] {
                    cmd.arg("--bind").arg(dir).arg(dir);
                }
                cmd.arg("--chdir").arg(&cwd).arg("--").arg("sh");
            }
            SandboxKind::SandboxExec => {
                cmd = Command::new("sandbox-exec");
                let param = |name: &str, p: &Path| {
                    let mut v = OsString::from(format!("{name}="));
                    v.push(p);
                    v
                };
                // A path that matches nothing, so the profile always has a HOME to name.
                let home = home.map_or_else(|| PathBuf::from("/var/empty"), real);
                let cut = if self.network { "" } else { "(deny network*)\n" };
                cmd.arg("-p").arg(format!("{PROFILE}{cut}"));
                for (name, p) in [("PROJECT", &project), ("CWD", &cwd), ("TMP", &tmp), ("HOME", &home)] {
                    cmd.arg("-D").arg(param(name, p));
                }
                cmd.arg("sh");
            }
        }
        cmd.arg("-c").arg(script);
        cmd
    }

    /// Whether the program for this kind is on `PATH` (it is not run). `None` is always there.
    pub fn available(&self, path: &str) -> bool {
        let program = match self.kind {
            SandboxKind::None => return true,
            SandboxKind::Bwrap => "bwrap",
            SandboxKind::SandboxExec => "sandbox-exec",
        };
        std::env::split_paths(path).any(|d| d.join(program).is_file())
    }
}

/// The canonical form of `p` (the sandbox matches resolved paths), or `p` when it does not exist.
fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(c: &Command) -> Vec<String> {
        c.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn bwrap_binds_the_project_and_tmp_and_not_the_home() {
        let s = Sandbox { kind: SandboxKind::Bwrap, network: true };
        let c = s.command("echo hi", Path::new("/proj"), Path::new("/proj"), Path::new("/tmp/t"), Some(Path::new("/home/u")));
        let a = args(&c);
        assert_eq!(c.get_program(), "bwrap");
        assert!(!a.contains(&"--unshare-net".to_owned()));
        assert!(a.windows(3).any(|w| w == ["--bind", "/proj", "/proj"]));
        assert!(a.windows(3).any(|w| w == ["--bind", "/tmp/t", "/tmp/t"]));
        assert!(!a.iter().any(|x| x.contains("/home/u")));
        assert_eq!(&a[a.len() - 3..], ["sh", "-c", "echo hi"]);
    }

    #[test]
    fn the_network_switch_adds_the_unshare() {
        let s = Sandbox { kind: SandboxKind::Bwrap, network: false };
        let c = s.command("true", Path::new("/p"), Path::new("/p"), Path::new("/t"), None);
        assert!(args(&c).contains(&"--unshare-net".to_owned()));
        let s = Sandbox { kind: SandboxKind::SandboxExec, network: false };
        let c = s.command("true", Path::new("/p"), Path::new("/p"), Path::new("/t"), Some(Path::new("/Users/u")));
        let a = args(&c);
        assert_eq!(c.get_program(), "sandbox-exec");
        assert!(a[a.iter().position(|x| x == "-p").unwrap() + 1].ends_with("(deny network*)\n"));
        assert!(a.contains(&"HOME=/Users/u".to_owned()));
        assert_eq!(&a[a.len() - 3..], ["sh", "-c", "true"]);
    }

    #[test]
    fn none_is_a_plain_shell_and_always_available() {
        let s = Sandbox { kind: SandboxKind::None, network: true };
        let c = s.command("x", Path::new("/p"), Path::new("/p"), Path::new("/t"), None);
        assert_eq!(c.get_program(), "sh");
        assert!(s.available(""));
        assert!(!Sandbox { kind: SandboxKind::Bwrap, network: true }.available(""));
    }
}
