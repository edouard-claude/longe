//! `harness.toml`: the single configuration file of a store.
//!
//! Everything the runtime needs to know that is not code lives here: model, providers,
//! budget, verifier command, sandbox policy, reflection settings, crons.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A model reference: which provider, which model name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    pub provider: String,
    pub name: String,
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider, self.name)
    }
}

/// Default model plus sampling and context settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCfg {
    pub provider: String,
    pub name: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Context window in tokens, used for the compaction threshold.
    #[serde(default = "default_context_window")]
    pub context_window: u32,
    #[serde(default = "default_max_output")]
    pub max_output_tokens: u32,
}

fn default_temperature() -> f32 {
    0.2
}
fn default_context_window() -> u32 {
    32_768
}
fn default_max_output() -> u32 {
    8_192
}

impl ModelCfg {
    pub fn model_ref(&self) -> ModelRef {
        ModelRef {
            provider: self.provider.clone(),
            name: self.name.clone(),
        }
    }
}

impl Default for ModelCfg {
    fn default() -> Self {
        Self {
            provider: "ollama".into(),
            name: "qwen3-vl:8b".into(),
            temperature: default_temperature(),
            context_window: default_context_window(),
            max_output_tokens: default_max_output(),
        }
    }
}

/// Wire protocol spoken by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    Anthropic,
    Openai,
    /// Ollama native `/api/chat`, which can disable a model's thinking output.
    Ollama,
}

/// One provider entry under `[providers.<name>]`. Unknown keys are captured in
/// `extra` and merged into every request body (e.g. `think = false` for Ollama).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCfg {
    pub kind: ProviderKind,
    pub base_url: String,
    /// Name of the environment variable holding the API key. Never the key itself.
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default = "default_retries")]
    pub max_retries: u32,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    /// Extra request-body fields, captured from any keys not named above.
    #[serde(flatten, default)]
    pub extra: toml::Table,
}

fn default_retries() -> u32 {
    3
}
fn default_timeout() -> u64 {
    600
}

/// `[budget]`: the grind. `done()` is refused before the minimum, the session is
/// force-finished at the maximum.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetCfg {
    #[serde(default = "default_min_seconds")]
    pub min_seconds: u64,
    #[serde(default = "default_min_turns")]
    pub min_turns: u32,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
}

fn default_min_seconds() -> u64 {
    1800
}
fn default_min_turns() -> u32 {
    20
}
fn default_max_turns() -> u32 {
    400
}
fn default_max_tokens() -> u64 {
    4_000_000
}

impl Default for BudgetCfg {
    fn default() -> Self {
        Self {
            min_seconds: default_min_seconds(),
            min_turns: default_min_turns(),
            max_turns: default_max_turns(),
            max_tokens: default_max_tokens(),
        }
    }
}

/// `[verify]`: the external verifier, run in the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyCfg {
    /// Shell command. `done()` is refused while its last run failed.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default = "default_verify_timeout")]
    pub timeout_seconds: u64,
    /// Bytes of report kept (tail).
    #[serde(default = "default_report_bytes")]
    pub report_bytes: usize,
}

fn default_verify_timeout() -> u64 {
    1800
}
fn default_report_bytes() -> usize {
    16 * 1024
}

/// Sandbox modes, Codex-style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}

/// `[sandbox]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxCfg {
    #[serde(default = "default_sandbox_mode")]
    pub mode: SandboxMode,
    #[serde(default)]
    pub network: bool,
    #[serde(default = "default_sh_timeout")]
    pub timeout_seconds: u64,
    /// Extra read-only roots (toolchains).
    #[serde(default)]
    pub read_extra: Vec<PathBuf>,
    /// Unix user that `sh()` runs as when the daemon runs as root (container setup).
    #[serde(default)]
    pub agent_user: Option<String>,
    /// Backend selection: `native` (per-OS kernel primitive) or `none` (no confinement).
    #[serde(default)]
    pub backend: SandboxBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxBackend {
    #[default]
    Native,
    None,
}

fn default_sandbox_mode() -> SandboxMode {
    SandboxMode::WorkspaceWrite
}
fn default_sh_timeout() -> u64 {
    300
}

impl Default for SandboxCfg {
    fn default() -> Self {
        Self {
            mode: default_sandbox_mode(),
            network: false,
            timeout_seconds: default_sh_timeout(),
            read_extra: Vec::new(),
            agent_user: None,
            backend: SandboxBackend::Native,
        }
    }
}

/// `[reflect]`: post-run reflection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReflectCfg {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Model used for reflection; defaults to the session's model.
    #[serde(default)]
    pub model: Option<ModelRef>,
    /// Fitness command run in the store root with the candidate branch checked out.
    /// Its last stdout line must be a number; higher is better.
    #[serde(default)]
    pub fitness: Option<String>,
    #[serde(default = "default_fitness_timeout")]
    pub fitness_timeout_seconds: u64,
}

fn default_true() -> bool {
    true
}
fn default_fitness_timeout() -> u64 {
    3600
}

impl Default for ReflectCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            fitness: None,
            fitness_timeout_seconds: default_fitness_timeout(),
        }
    }
}

/// `[compact]`: L1 compaction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactCfg {
    /// Fraction of the context window that triggers compaction.
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    /// Turns kept verbatim after the summary.
    #[serde(default = "default_keep_last")]
    pub keep_last: usize,
}

fn default_threshold() -> f32 {
    0.7
}
fn default_keep_last() -> usize {
    5
}

impl Default for CompactCfg {
    fn default() -> Self {
        Self {
            threshold: default_threshold(),
            keep_last: default_keep_last(),
        }
    }
}

/// `[repl]`: the exec surface.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplCfg {
    /// Bytes of exec output returned to the model (the head); the rest stays in `_last`.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

fn default_max_output_bytes() -> usize {
    8 * 1024
}

impl Default for ReplCfg {
    fn default() -> Self {
        Self {
            max_output_bytes: default_max_output_bytes(),
        }
    }
}

/// `[daemon]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonCfg {
    /// Unix socket path. Default: `<store>/longed.sock`.
    #[serde(default)]
    pub socket: Option<PathBuf>,
    /// HTTP JSON bind address. Default `127.0.0.1:7878`; empty string disables.
    #[serde(default)]
    pub http: Option<String>,
    /// Minutes of idleness after which an idle session is offloaded to disk.
    #[serde(default = "default_offload_minutes")]
    pub offload_after_minutes: u64,
}

fn default_offload_minutes() -> u64 {
    30
}

/// What a cron entry does when it fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CronAction {
    /// Reflect over the most recent finished trajectory not yet reflected.
    Reflect,
    /// Spawn a root session with this task.
    Task { task: String, workspace: PathBuf },
    /// Offload idle sessions and delete trajectories older than `max_age_days`.
    Cleanup { max_age_days: u32 },
}

/// `[[cron]]`. No `deny_unknown_fields`: serde cannot combine it with `flatten`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub name: String,
    /// Standard 5-field cron expression, local time.
    pub schedule: String,
    #[serde(flatten)]
    pub action: CronAction,
}

/// The whole `harness.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Harness {
    #[serde(default)]
    pub daemon: DaemonCfg,
    #[serde(default)]
    pub model: ModelCfg,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderCfg>,
    #[serde(default)]
    pub budget: BudgetCfg,
    #[serde(default)]
    pub verify: VerifyCfg,
    #[serde(default)]
    pub sandbox: SandboxCfg,
    #[serde(default)]
    pub reflect: ReflectCfg,
    #[serde(default)]
    pub compact: CompactCfg,
    #[serde(default)]
    pub repl: ReplCfg,
    #[serde(default)]
    pub cron: Vec<CronJob>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid harness.toml at {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("cannot serialize harness.toml: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl Harness {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    #[cfg(test)]
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Providers known out of the box when the file declares none.
    pub fn with_default_providers(mut self) -> Self {
        if self.providers.is_empty() {
            self.providers.insert(
                "ollama".into(),
                ProviderCfg {
                    kind: ProviderKind::Ollama,
                    base_url: "http://127.0.0.1:11434".into(),
                    api_key_env: None,
                    max_retries: 3,
                    timeout_seconds: 600,
                    // No `think` key: some models (qwen3-vl) fail on think=false. The
                    // client reads only `content`, so reasoning is dropped anyway.
                    extra: toml::Table::new(),
                },
            );
            self.providers.insert(
                "anthropic".into(),
                ProviderCfg {
                    kind: ProviderKind::Anthropic,
                    base_url: std::env::var("ANTHROPIC_BASE_URL")
                        .unwrap_or_else(|_| "https://api.anthropic.com".into()),
                    api_key_env: Some("ANTHROPIC_API_KEY".into()),
                    max_retries: 3,
                    timeout_seconds: 600,
                    extra: toml::Table::new(),
                },
            );
            self.providers.insert(
                "deepseek".into(),
                ProviderCfg {
                    kind: ProviderKind::Openai,
                    base_url: "https://api.deepseek.com/v1".into(),
                    api_key_env: Some("DEEPSEEK_API_KEY".into()),
                    max_retries: 3,
                    timeout_seconds: 600,
                    extra: toml::Table::new(),
                },
            );
            self.providers.insert(
                "openrouter".into(),
                ProviderCfg {
                    kind: ProviderKind::Openai,
                    base_url: "https://openrouter.ai/api/v1".into(),
                    api_key_env: Some("OPENROUTER_API_KEY".into()),
                    max_retries: 3,
                    timeout_seconds: 600,
                    extra: toml::Table::new(),
                },
            );
        }
        self
    }
}

impl Default for VerifyCfg {
    fn default() -> Self {
        Self {
            command: None,
            timeout_seconds: default_verify_timeout(),
            report_bytes: default_report_bytes(),
        }
    }
}

impl Default for DaemonCfg {
    fn default() -> Self {
        Self {
            socket: None,
            http: None,
            offload_after_minutes: default_offload_minutes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        let h = Harness::parse("").unwrap();
        assert_eq!(h.budget.min_turns, 20);
        assert_eq!(h.sandbox.mode, SandboxMode::WorkspaceWrite);
        assert!(!h.sandbox.network);
        assert!((h.compact.threshold - 0.7).abs() < f32::EPSILON);
        assert_eq!(h.repl.max_output_bytes, 8192);
        let h = Harness::parse("[repl]\nmax_output_bytes = 16384\n").unwrap();
        assert_eq!(h.repl.max_output_bytes, 16384);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = Harness::parse("[budget]\nmin_secs = 3\n").unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn cron_actions_round_trip() {
        let text = r#"
[[cron]]
name = "nightly"
schedule = "0 3 * * *"
action = "reflect"

[[cron]]
name = "veille"
schedule = "*/30 * * * *"
action = "task"
task = "check the news"
workspace = "/tmp/w"
"#;
        let h = Harness::parse(text).unwrap();
        assert_eq!(h.cron.len(), 2);
        assert!(matches!(h.cron[0].action, CronAction::Reflect));
        assert!(matches!(h.cron[1].action, CronAction::Task { .. }));
        let back = h.to_toml().unwrap();
        let again = Harness::parse(&back).unwrap();
        assert_eq!(again.cron.len(), 2);
    }

    #[test]
    fn default_providers_are_added_only_when_missing() {
        let h = Harness::parse("").unwrap().with_default_providers();
        assert!(h.providers.contains_key("ollama"));
        let custom =
            Harness::parse("[providers.mine]\nkind = \"openai\"\nbase_url = \"http://x\"\n")
                .unwrap()
                .with_default_providers();
        assert_eq!(custom.providers.len(), 1);
    }
}
