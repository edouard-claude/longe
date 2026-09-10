//! macOS backend: a generated Seatbelt profile, applied with `sandbox-exec -p`.
//!
//! Reads are allowed everywhere except the hidden paths (the store, `~/.ssh`,
//! credential files, the binary, any `.env`); writes are limited by mode; network
//! is denied unless the policy allows it.

use std::process::Command;

use super::{run_prepared, Output, Policy, Sandbox, SandboxError};
use crate::config::SandboxMode;

#[derive(Debug, Default)]
pub struct Seatbelt;

/// Both the path as given and its canonical form: the kernel matches on resolved paths
/// (`/var` is a symlink to `/private/var`), the profile author thinks in logical ones.
fn path_forms(p: &std::path::Path) -> Vec<String> {
    let mut v = vec![p.to_string_lossy().into_owned()];
    if let Ok(c) = std::fs::canonicalize(p) {
        let c = c.to_string_lossy().into_owned();
        if !v.contains(&c) {
            v.push(c);
        }
    }
    v
}

fn subpaths(p: &std::path::Path) -> String {
    path_forms(p)
        .iter()
        .map(|s| format!("(subpath {})", sb_string(s)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn sb_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Render the profile for a policy.
pub fn profile(p: &Policy) -> String {
    let mut s = String::from(
        "(version 1)\n(deny default)\n\
         (allow process-exec)\n(allow process-fork)\n(allow process-info*)\n\
         (allow signal)\n(allow sysctl-read)\n(allow mach-lookup)\n(allow mach-priv-task-port)\n\
         (allow ipc-posix*)\n(allow system-socket)\n(allow file-ioctl)\n\
         (allow file-read*)\n(allow file-read-metadata)\n\
         (allow network-outbound (literal \"/private/var/run/syslog\"))\n",
    );
    match p.mode {
        SandboxMode::ReadOnly => {
            s.push_str("(allow file-write* (literal \"/dev/null\") (regex #\"^/dev/tty\"))\n");
        }
        SandboxMode::WorkspaceWrite | SandboxMode::FullAccess => {
            let ws = subpaths(&p.workspace);
            s.push_str(&format!(
                "(allow file-write* {ws} (subpath \"/private/tmp\") (subpath \"/tmp\") \
                 (subpath \"/private/var/folders\") (subpath \"/var/folders\") \
                 (literal \"/dev/null\") (regex #\"^/dev/tty\") (regex #\"^/dev/fd/\"))\n",
                ws = ws
            ));
            if let Some(t) = std::env::var_os("TMPDIR") {
                s.push_str(&format!(
                    "(allow file-write* {})\n",
                    subpaths(std::path::Path::new(&t))
                ));
            }
            // Cargo needs its registry/target caches.
            for extra in &p.read_extra {
                let e = extra.to_string_lossy();
                if e.ends_with("/.cargo") {
                    s.push_str(&format!("(allow file-write* {})\n", subpaths(extra)));
                }
            }
        }
    }
    if p.network {
        s.push_str("(allow network*)\n");
    } else {
        // Local Unix sockets stay usable (e.g. the shell's own needs), TCP/UDP do not.
        s.push_str("(deny network*)\n(allow network* (local unix-socket))\n");
    }
    // Fixed rules, emitted LAST: in SBPL the last matching rule wins, so these beat
    // every allow above (including the temp-area write allowance).
    for h in &p.hidden {
        s.push_str(&format!("(deny file-read* file-write* {})\n", subpaths(h)));
    }
    s.push_str("(deny file-read* file-write* (regex #\"/\\.env(\\..*)?$\"))\n");
    s.push_str(
        "(deny file-read* file-write* (regex #\"/id_(rsa|ed25519|ecdsa|dsa)(\\.pub)?$\"))\n",
    );
    s
}

impl Sandbox for Seatbelt {
    fn run(&self, p: &Policy, cmd: &str) -> Result<Output, SandboxError> {
        let mut c = Command::new("/usr/bin/sandbox-exec");
        c.arg("-p")
            .arg(profile(p))
            .arg("/bin/sh")
            .arg("-c")
            .arg(cmd);
        run_prepared(c, p, cmd)
    }
    fn name(&self) -> &'static str {
        "seatbelt"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SandboxCfg;
    use std::path::Path;
    use std::time::Duration;

    fn setup() -> (tempfile::TempDir, Policy) {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path().join("ws");
        let store = d.path().join("store");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(store.join("evals")).unwrap();
        std::fs::write(store.join("prompt.md"), "secret prompt").unwrap();
        std::fs::write(store.join("evals").join("expected"), "42").unwrap();
        let cfg = SandboxCfg {
            timeout_seconds: 20,
            ..SandboxCfg::default()
        };
        let mut p = Policy::for_session(&cfg, &ws, &store);
        p.timeout = Duration::from_secs(20);
        (d, p)
    }

    #[test]
    fn workspace_write_allows_workspace_and_denies_store() {
        let (d, p) = setup();
        let sb = Seatbelt;
        let out = sb.run(&p, "echo ok > out.txt && cat out.txt").unwrap();
        assert_eq!(out.stdout.trim(), "ok", "{out:?}");
        let store = d.path().join("store");
        let out = sb
            .run(&p, &format!("cat {}/prompt.md", store.display()))
            .unwrap();
        assert_ne!(out.code, 0, "store must be unreadable: {out:?}");
        assert!(!out.stdout.contains("secret prompt"));
        let out = sb
            .run(&p, &format!("cat {}/evals/expected", store.display()))
            .unwrap();
        assert_ne!(out.code, 0, "evals must be unreadable: {out:?}");
        let out = sb
            .run(&p, &format!("echo x > {}/prompt.md", store.display()))
            .unwrap();
        assert_ne!(out.code, 0, "store must be unwritable: {out:?}");
        assert_eq!(
            std::fs::read_to_string(store.join("prompt.md")).unwrap(),
            "secret prompt"
        );
    }

    #[test]
    fn writes_outside_workspace_are_denied() {
        let (d, p) = setup();
        // The temp area itself is writable (TMPDIR), so aim at the home directory.
        let home = std::env::var("HOME").unwrap();
        let target =
            std::path::PathBuf::from(home).join(format!(".longe-sb-test-{}", std::process::id()));
        let out = Seatbelt
            .run(&p, &format!("echo x > {}", target.display()))
            .unwrap();
        let leaked = target.exists();
        let _ = std::fs::remove_file(&target);
        assert_ne!(out.code, 0, "{out:?}");
        assert!(!leaked);
        let _ = d;
    }

    #[test]
    fn ssh_keys_and_env_files_are_invisible() {
        let (_d, p) = setup();
        let sb = Seatbelt;
        // Even if ~/.ssh exists, listing/reading must fail. Never print the output:
        // on failure it would contain the key.
        let out = sb.run(&p, "ls ~/.ssh >/dev/null 2>&1 && exit 7; cat ~/.ssh/id_rsa >/dev/null 2>&1 && exit 8; exit 0").unwrap();
        assert_eq!(out.code, 0, "ssh must be invisible (code {})", out.code);
        std::fs::write(p.workspace.join(".env"), "TOKEN=1").unwrap();
        let out = sb.run(&p, "cat .env").unwrap();
        assert_ne!(out.code, 0, "code {}", out.code);
        assert!(!out.stdout.contains("TOKEN=1"));
    }

    #[test]
    fn network_is_denied_by_default_and_allowed_on_request() {
        let (_d, mut p) = setup();
        let out = Seatbelt
            .run(
                &p,
                "curl -sS --max-time 3 http://example.com >/dev/null 2>&1 || nc -z -w 2 1.1.1.1 53",
            )
            .unwrap();
        assert_ne!(out.code, 0, "network must be denied: {out:?}");
        p.network = true;
        // Only checks the profile allows it; the machine may be offline.
        assert!(profile(&p).contains("(allow network*)"));
    }

    #[test]
    fn read_only_denies_workspace_writes() {
        let (_d, mut p) = setup();
        p.mode = SandboxMode::ReadOnly;
        let out = Seatbelt.run(&p, "echo x > f.txt").unwrap();
        assert_ne!(out.code, 0, "{out:?}");
        assert!(!p.workspace.join("f.txt").exists());
        let out = Seatbelt
            .run(&p, "ls / >/dev/null && echo readable")
            .unwrap();
        assert_eq!(out.stdout.trim(), "readable", "{out:?}");
    }

    #[test]
    fn profile_quotes_paths() {
        let cfg = SandboxCfg::default();
        let p = Policy::for_session(&cfg, Path::new("/tmp/a b"), Path::new("/tmp/s"));
        assert!(profile(&p).contains("(subpath \"/tmp/a b\")"));
    }
}
