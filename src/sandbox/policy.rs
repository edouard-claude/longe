//! One policy, three modes, fixed rules that no mode can lift.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{SandboxCfg, SandboxMode};

/// What a confined command may do.
#[derive(Debug, Clone)]
pub struct Policy {
    pub mode: SandboxMode,
    pub workspace: PathBuf,
    /// Extra read-only roots (toolchains).
    pub read_extra: Vec<PathBuf>,
    pub network: bool,
    pub timeout: Duration,
    /// Paths that must stay invisible whatever the mode: the store, `~/.ssh`, the
    /// runtime binary, credential files.
    pub hidden: Vec<PathBuf>,
    /// When the daemon runs as root, `sh()` runs as this Unix user.
    pub agent_user: Option<String>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

impl Policy {
    /// Build the policy for a session from `[sandbox]`.
    pub fn for_session(cfg: &SandboxCfg, workspace: &Path, store_root: &Path) -> Self {
        let mut hidden = vec![store_root.to_path_buf()];
        if let Some(h) = home() {
            for p in [
                ".ssh",
                ".aws",
                ".gnupg",
                ".netrc",
                ".config/gh",
                ".docker/config.json",
            ] {
                hidden.push(h.join(p));
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            hidden.push(exe);
        }
        let mut read_extra = cfg.read_extra.clone();
        if let Some(h) = home() {
            read_extra.push(h.join(".cargo"));
            read_extra.push(h.join(".rustup"));
        }
        Self {
            mode: cfg.mode,
            workspace: workspace.to_path_buf(),
            read_extra,
            network: cfg.network,
            timeout: Duration::from_secs(cfg.timeout_seconds),
            hidden,
            agent_user: cfg.agent_user.clone(),
        }
    }

    /// The verifier runs a human-authored command: same confinement, but the evals
    /// directory is visible (it needs the expected outputs) and the timeout is its own.
    pub fn for_verify(mut self, evals_dir: &Path, timeout: Duration) -> Self {
        self.hidden.retain(|h| !evals_dir.starts_with(h));
        self.read_extra.push(evals_dir.to_path_buf());
        self.timeout = timeout;
        if self.mode == SandboxMode::ReadOnly {
            // `cargo test` writes to target/: the verifier needs the workspace.
            self.mode = SandboxMode::WorkspaceWrite;
        }
        self
    }

    /// Environment variables that must never reach a confined command.
    pub fn env_is_secret(name: &str) -> bool {
        let n = name.to_ascii_uppercase();
        n.ends_with("_API_KEY")
            || n.ends_with("_TOKEN")
            || n.ends_with("_SECRET")
            || n.ends_with("_PASSWORD")
            || n == "ANTHROPIC_AUTH_TOKEN"
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // the Linux allow-list builder uses it
    pub fn is_hidden(&self, path: &Path) -> bool {
        self.hidden.iter().any(|h| path.starts_with(h))
    }

    /// A workspace nested in the store (or the reverse) cannot be confined by an
    /// allow-list backend.
    pub fn validate(&self, store_root: &Path) -> Result<(), String> {
        if self.mode == SandboxMode::FullAccess {
            return Ok(());
        }
        if self.workspace.starts_with(store_root) {
            return Err(format!(
                "workspace {} is inside the store {}: it would expose the store to sh()",
                self.workspace.display(),
                store_root.display()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_recognised() {
        assert!(Policy::env_is_secret("ANTHROPIC_API_KEY"));
        assert!(Policy::env_is_secret("github_token"));
        assert!(!Policy::env_is_secret("PATH"));
    }

    #[test]
    fn verify_policy_unhides_evals_and_writes() {
        let cfg = SandboxCfg {
            mode: SandboxMode::ReadOnly,
            ..SandboxCfg::default()
        };
        let store = Path::new("/srv/store");
        let p = Policy::for_session(&cfg, Path::new("/srv/ws"), store);
        assert!(p.is_hidden(Path::new("/srv/store/evals/cases/001/expected")));
        let v = p.for_verify(&store.join("evals"), Duration::from_secs(5));
        assert!(!v.is_hidden(Path::new("/srv/store/evals/run.sh")));
        assert_eq!(v.mode, SandboxMode::WorkspaceWrite);
    }

    #[test]
    fn workspace_inside_store_is_rejected() {
        let cfg = SandboxCfg::default();
        let p = Policy::for_session(&cfg, Path::new("/srv/store/ws"), Path::new("/srv/store"));
        assert!(p.validate(Path::new("/srv/store")).is_err());
        let ok = Policy::for_session(&cfg, Path::new("/srv/ws"), Path::new("/srv/store"));
        assert!(ok.validate(Path::new("/srv/store")).is_ok());
    }
}
