//! Windows backend placeholder: `CreateRestrictedToken` + Job Object, next delivery.

use super::{Output, Policy, Sandbox, SandboxError};

#[derive(Debug, Default)]
pub struct RestrictedToken;

impl Sandbox for RestrictedToken {
    fn run(&self, _p: &Policy, _cmd: &str) -> Result<Output, SandboxError> {
        Err(SandboxError::Unsupported("windows restricted token"))
    }
    fn name(&self) -> &'static str {
        "restricted-token"
    }
}
