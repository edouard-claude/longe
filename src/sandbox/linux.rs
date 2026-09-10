//! Linux backend: Landlock (filesystem allow-list plus TCP denial on ABI V4+), applied
//! by a helper invocation of our own binary which then `exec`s the command. No
//! `pre_exec`, hence no `unsafe`.

use std::path::PathBuf;
use std::process::Command;

use landlock::{
    Access, AccessFs, AccessNet, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI,
};
use serde::{Deserialize, Serialize};

use super::{run_prepared, Output, Policy, Sandbox, SandboxError};
use crate::config::SandboxMode;

#[derive(Debug, Default)]
pub struct LandlockSeccomp;

/// What the helper needs, serialized on its command line.
#[derive(Debug, Serialize, Deserialize)]
pub struct HelperSpec {
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    pub network: bool,
}

pub fn helper_spec(p: &Policy) -> HelperSpec {
    let mut read_roots: Vec<PathBuf> = [
        "/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc", "/proc", "/dev", "/opt", "/var/lib",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    read_roots.push(p.workspace.clone());
    read_roots.extend(p.read_extra.iter().cloned());
    let mut write_roots = vec![PathBuf::from("/tmp"), PathBuf::from("/dev/null")];
    if p.mode != SandboxMode::ReadOnly {
        write_roots.push(p.workspace.clone());
        for extra in &p.read_extra {
            if extra.ends_with(".cargo") {
                write_roots.push(extra.clone());
            }
        }
    } else {
        write_roots.clear();
        write_roots.push(PathBuf::from("/dev/null"));
    }
    // Hidden paths are simply never listed; Landlock is an allow-list.
    read_roots.retain(|r| !p.is_hidden(r));
    write_roots.retain(|r| !p.is_hidden(r));
    HelperSpec {
        read_roots,
        write_roots,
        network: p.network,
    }
}

/// Apply the restrictions to the current process (called by `longe __sandbox-exec`).
pub fn apply(spec: &HelperSpec) -> Result<(), SandboxError> {
    let abi = ABI::V4;
    let fs_all = AccessFs::from_all(abi);
    let fs_read = AccessFs::from_read(abi);
    let mut ruleset = Ruleset::default()
        .handle_access(fs_all)
        .map_err(|e| SandboxError::Policy(e.to_string()))?;
    if !spec.network {
        ruleset = ruleset
            .handle_access(AccessNet::from_all(abi))
            .map_err(|e| SandboxError::Policy(e.to_string()))?;
    }
    let mut created = ruleset
        .create()
        .map_err(|e| SandboxError::Policy(e.to_string()))?;
    for r in &spec.read_roots {
        if let Ok(fd) = PathFd::new(r) {
            created = created
                .add_rule(PathBeneath::new(fd, fs_read))
                .map_err(|e| SandboxError::Policy(e.to_string()))?;
        }
    }
    for w in &spec.write_roots {
        if let Ok(fd) = PathFd::new(w) {
            created = created
                .add_rule(PathBeneath::new(fd, fs_all))
                .map_err(|e| SandboxError::Policy(e.to_string()))?;
        }
    }
    created
        .restrict_self()
        .map_err(|e| SandboxError::Policy(e.to_string()))?;
    Ok(())
}

impl Sandbox for LandlockSeccomp {
    fn run(&self, p: &Policy, cmd: &str) -> Result<Output, SandboxError> {
        let exe = std::env::current_exe()?;
        let spec = serde_json::to_string(&helper_spec(p))
            .map_err(|e| SandboxError::Policy(e.to_string()))?;
        let mut c = Command::new(exe);
        c.arg("__sandbox-exec")
            .arg(spec)
            .arg("/bin/sh")
            .arg("-c")
            .arg(cmd);
        run_prepared(c, p, cmd)
    }
    fn name(&self) -> &'static str {
        "landlock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SandboxCfg;
    use std::path::Path;

    #[test]
    fn spec_never_lists_hidden_roots() {
        let cfg = SandboxCfg::default();
        let p = Policy::for_session(&cfg, Path::new("/srv/ws"), Path::new("/srv/store"));
        let s = helper_spec(&p);
        assert!(s.read_roots.iter().any(|r| r == Path::new("/srv/ws")));
        assert!(!s.read_roots.iter().any(|r| r.starts_with("/srv/store")));
        assert!(s.write_roots.iter().any(|r| r == Path::new("/srv/ws")));
    }
}
