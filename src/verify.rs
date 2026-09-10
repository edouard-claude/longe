//! The external verifier. A human-authored command, run confined in the workspace,
//! whose report goes back to the model. `done()` is refused while it fails.

use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::VerifyCfg;
use crate::sandbox::{Policy, Sandbox, SandboxError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyOutcome {
    pub ok: bool,
    pub code: i32,
    pub report: String,
    pub seconds: u64,
    pub timed_out: bool,
}

impl VerifyOutcome {
    pub fn summary(&self) -> String {
        let status = if self.ok { "OK" } else { "FAILED" };
        format!(
            "verify {status} (exit {}, {}s)\n{}",
            self.code, self.seconds, self.report
        )
    }
}

/// Keep the last `max` bytes of a report, on a char boundary.
pub fn tail_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("[... {} bytes elided ...]\n{}", start, &s[start..])
}

/// Run the verifier. `None` command means "nothing configured": vacuously OK.
pub fn run(
    cfg: &VerifyCfg,
    sandbox: &dyn Sandbox,
    session_policy: &Policy,
    evals_dir: &Path,
) -> Result<VerifyOutcome, SandboxError> {
    let Some(cmd) = cfg.command.as_deref().filter(|c| !c.trim().is_empty()) else {
        return Ok(VerifyOutcome {
            ok: true,
            code: 0,
            report: "no verify command configured".into(),
            seconds: 0,
            timed_out: false,
        });
    };
    let policy = session_policy
        .clone()
        .for_verify(evals_dir, Duration::from_secs(cfg.timeout_seconds));
    let full = format!(
        "export LONGE_EVALS={}; {cmd}",
        crate::sandbox::shell_quote(evals_dir)
    );
    let start = Instant::now();
    let out = sandbox.run(&policy, &full)?;
    let mut text = String::new();
    if !out.stdout.trim().is_empty() {
        text.push_str(&out.stdout);
    }
    if !out.stderr.trim().is_empty() {
        text.push_str("\n--- stderr ---\n");
        text.push_str(&out.stderr);
    }
    if out.timed_out {
        text.push_str("\n--- verifier timed out ---\n");
    }
    Ok(VerifyOutcome {
        ok: out.code == 0 && !out.timed_out,
        code: out.code,
        report: tail_bytes(text.trim(), cfg.report_bytes),
        seconds: start.elapsed().as_secs(),
        timed_out: out.timed_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SandboxCfg;
    use crate::sandbox::Unconfined;

    fn policy(ws: &Path, store: &Path) -> Policy {
        Policy::for_session(&SandboxCfg::default(), ws, store)
    }

    #[test]
    fn no_command_is_ok() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path(), Path::new("/x/store"));
        let o = run(
            &VerifyCfg::default(),
            &Unconfined,
            &p,
            Path::new("/x/store/evals"),
        )
        .unwrap();
        assert!(o.ok);
    }

    #[test]
    fn failing_command_reports_failure_with_output() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path(), Path::new("/x/store"));
        let cfg = VerifyCfg {
            command: Some("echo 3/5 OK; echo case 2 failed 1>&2; exit 1".into()),
            ..VerifyCfg::default()
        };
        let o = run(&cfg, &Unconfined, &p, Path::new("/x/store/evals")).unwrap();
        assert!(!o.ok);
        assert_eq!(o.code, 1);
        assert!(o.report.contains("3/5 OK"));
        assert!(o.report.contains("case 2 failed"));
        assert!(o.summary().starts_with("verify FAILED"));
    }

    #[test]
    fn evals_dir_is_exported() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path(), Path::new("/x/store"));
        let cfg = VerifyCfg {
            command: Some("echo $LONGE_EVALS".into()),
            ..VerifyCfg::default()
        };
        let o = run(&cfg, &Unconfined, &p, Path::new("/x/store/evals")).unwrap();
        assert!(o.ok);
        assert_eq!(o.report.trim(), "/x/store/evals");
    }

    #[test]
    fn timeout_is_a_failure() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path(), Path::new("/x/store"));
        let cfg = VerifyCfg {
            command: Some("sleep 5".into()),
            timeout_seconds: 1,
            ..VerifyCfg::default()
        };
        let o = run(&cfg, &Unconfined, &p, Path::new("/x/store/evals")).unwrap();
        assert!(!o.ok);
        assert!(o.timed_out);
    }

    #[test]
    fn tail_keeps_last_bytes() {
        let t = tail_bytes("abcdef", 3);
        assert!(t.ends_with("def"));
        assert!(t.contains("elided"));
        assert_eq!(tail_bytes("abc", 10), "abc");
        let t = tail_bytes("ééé", 3);
        assert!(t.ends_with('é'));
    }
}
