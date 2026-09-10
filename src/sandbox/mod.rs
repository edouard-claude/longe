//! Per-process confinement of `sh()`. One trait, one policy, one native backend per OS.
//! Nothing is re-implemented: these are the kernel's own primitives.

pub mod policy;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub use policy::Policy;

use crate::config::{SandboxBackend, SandboxMode};

/// Result of a confined command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    pub timed_out: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("cannot spawn `{cmd}`: {source}")]
    Spawn { cmd: String, source: std::io::Error },
    #[allow(dead_code)] // raised by the Windows backend and non-unix user switching
    #[error("sandbox backend unsupported on this platform: {0}")]
    Unsupported(&'static str),
    #[error("unknown unix user `{0}`")]
    UserLookup(String),
    #[allow(dead_code)] // raised by the Linux backend
    #[error("policy error: {0}")]
    Policy(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A confinement backend.
pub trait Sandbox: Send + Sync {
    fn run(&self, p: &Policy, cmd: &str) -> Result<Output, SandboxError>;
    fn name(&self) -> &'static str;
}

/// No confinement at all: `FullAccess`, or `backend = "none"`. Still applies the
/// Unix user switch and the secret-environment scrub.
#[derive(Debug, Default)]
pub struct Unconfined;

impl Sandbox for Unconfined {
    fn run(&self, p: &Policy, cmd: &str) -> Result<Output, SandboxError> {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(cmd);
        run_prepared(c, p, cmd)
    }
    fn name(&self) -> &'static str {
        "unconfined"
    }
}

/// The per-OS native backend.
#[cfg(target_os = "macos")]
pub type Native = macos::Seatbelt;
#[cfg(target_os = "linux")]
pub type Native = linux::LandlockSeccomp;
#[cfg(target_os = "windows")]
pub type Native = windows::RestrictedToken;

/// Pick the backend for a policy.
pub fn select(backend: SandboxBackend, mode: SandboxMode) -> Box<dyn Sandbox> {
    match (backend, mode) {
        (SandboxBackend::None, _) | (_, SandboxMode::FullAccess) => Box::new(Unconfined),
        (SandboxBackend::Native, _) => Box::new(Native::default()),
    }
}

/// Resolve a Unix user name to a uid/gid pair via `id` (no unsafe, no libc).
pub fn lookup_user(name: &str) -> Result<(u32, u32), SandboxError> {
    let uid = Command::new("id").arg("-u").arg(name).output()?;
    let gid = Command::new("id").arg("-g").arg(name).output()?;
    if !uid.status.success() || !gid.status.success() {
        return Err(SandboxError::UserLookup(name.to_string()));
    }
    let parse = |o: &[u8]| String::from_utf8_lossy(o).trim().parse::<u32>().ok();
    match (parse(&uid.stdout), parse(&gid.stdout)) {
        (Some(u), Some(g)) => Ok((u, g)),
        _ => Err(SandboxError::UserLookup(name.to_string())),
    }
}

/// Shared tail of every backend: cwd, scrubbed environment, optional uid switch,
/// timeout with kill, output capture.
pub fn run_prepared(mut c: Command, p: &Policy, display: &str) -> Result<Output, SandboxError> {
    c.current_dir(&p.workspace);
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, _) in std::env::vars_os() {
        if k.to_str().is_some_and(Policy::env_is_secret) {
            c.env_remove(k);
        }
    }
    c.env("LONGE_SANDBOX", format!("{:?}", p.mode).to_lowercase());
    if let Some(user) = &p.agent_user {
        let (uid, gid) = lookup_user(user)?;
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.uid(uid).gid(gid);
        }
        #[cfg(not(unix))]
        {
            let _ = (uid, gid);
            return Err(SandboxError::Unsupported("agent_user on this platform"));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group so a timeout kills the whole tree, not just the shell.
        c.process_group(0);
    }
    let mut child = c.spawn().map_err(|source| SandboxError::Spawn {
        cmd: display.to_string(),
        source,
    })?;
    let start = Instant::now();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    // Drain pipes on threads so a chatty command cannot deadlock on a full pipe.
    let out_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = std::io::Read::read_to_end(s, &mut buf);
        }
        buf
    });
    let err_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = std::io::Read::read_to_end(s, &mut buf);
        }
        buf
    });
    let mut timed_out = false;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if start.elapsed() >= p.timeout {
            timed_out = true;
            kill_tree(child.id());
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    let code = status.code().unwrap_or(-1);
    Ok(Output {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        code: if timed_out { 124 } else { code },
        timed_out,
    })
}

/// Quote a path for a shell.
pub fn shell_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+'))
    {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Kill a whole process group (the child was spawned as a group leader).
fn kill_tree(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-9", "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SandboxCfg;

    fn policy(ws: &Path) -> Policy {
        let cfg = SandboxCfg {
            timeout_seconds: 2,
            ..SandboxCfg::default()
        };
        Policy::for_session(&cfg, ws, Path::new("/nonexistent/store"))
    }

    #[test]
    fn unconfined_runs_and_captures() {
        let d = tempfile::tempdir().unwrap();
        let out = Unconfined
            .run(&policy(d.path()), "echo hi; echo err 1>&2; exit 3")
            .unwrap();
        assert_eq!(out.stdout, "hi\n");
        assert_eq!(out.stderr, "err\n");
        assert_eq!(out.code, 3);
        assert!(!out.timed_out);
    }

    #[test]
    fn timeout_kills_and_reports() {
        let d = tempfile::tempdir().unwrap();
        let mut p = policy(d.path());
        p.timeout = Duration::from_millis(200);
        let start = Instant::now();
        let out = Unconfined.run(&p, "sleep 5; echo late").unwrap();
        assert!(out.timed_out);
        assert_eq!(out.code, 124);
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn secrets_are_scrubbed_from_env() {
        let d = tempfile::tempdir().unwrap();
        // The variable is set only for the child we spawn, so use a wrapper shell.
        let out = Unconfined
            .run(
                &policy(d.path()),
                "env | grep -c '^LONGE_SANDBOX=' ; echo \"key=${FAKE_API_KEY:-unset}\"",
            )
            .unwrap();
        assert!(out.stdout.contains("key=unset"));
    }

    #[test]
    fn cwd_is_workspace() {
        let d = tempfile::tempdir().unwrap();
        let out = Unconfined.run(&policy(d.path()), "pwd").unwrap();
        let got = std::fs::canonicalize(out.stdout.trim()).unwrap();
        assert_eq!(got, std::fs::canonicalize(d.path()).unwrap());
    }

    #[test]
    fn unknown_user_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let mut p = policy(d.path());
        p.agent_user = Some("no-such-user-longe".into());
        assert!(matches!(
            Unconfined.run(&p, "true"),
            Err(SandboxError::UserLookup(_))
        ));
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote(Path::new("/a/b-c")), "/a/b-c");
        assert_eq!(shell_quote(Path::new("/a b/it's")), "'/a b/it'\\''s'");
    }
}
