//! Provider-neutral request/response types and the error taxonomy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: f32,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Reasoning tokens, when the provider reports them. They are part of
    /// `output_tokens` and count against `max_tokens`.
    #[serde(default)]
    pub reasoning_tokens: u64,
}

impl Usage {
    pub fn total(self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Estimate when a provider sends no usage.
    pub fn estimated(req: &ChatRequest, text: &str) -> Self {
        Self {
            input_tokens: estimate_request(req),
            output_tokens: estimate_tokens(text),
            reasoning_tokens: 0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub text: String,
    pub usage: Usage,
    pub stop_reason: Option<String>,
    /// True when the provider sent no usage and the numbers are estimates.
    pub usage_estimated: bool,
    /// Characters of reasoning streamed before the answer (the text is not kept).
    pub reasoning_chars: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provider returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("stream: {0}")]
    Stream(String),
    #[error("provider `{provider}` needs the environment variable {var}")]
    MissingApiKey { provider: String, var: String },
    #[error("unknown provider `{0}` (declare it under [providers] in harness.toml)")]
    UnknownProvider(String),
    #[error("empty response")]
    Empty,
    /// The provider stopped at `max_tokens`. Replaying the same request is useless:
    /// the caller must raise the limit or ask for a shorter answer.
    #[error("response truncated at {output_tokens} output tokens ({reasoning_tokens} of reasoning): raise max_tokens or ask for less")]
    Truncated {
        input_tokens: u64,
        output_tokens: u64,
        reasoning_tokens: u64,
    },
    #[error("retries exhausted after {attempts} attempts: {last}")]
    Exhausted { attempts: u32, last: Box<LlmError> },
}

impl LlmError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http(e) => e.is_connect() || e.is_timeout() || e.is_request() || e.is_body(),
            Self::Status { status, .. } => *status == 429 || *status == 408 || *status >= 500,
            Self::Stream(_) | Self::Empty => true,
            Self::MissingApiKey { .. }
            | Self::UnknownProvider(_)
            | Self::Truncated { .. }
            | Self::Exhausted { .. } => false,
        }
    }

    /// The stop reasons the three protocols use for "hit max_tokens".
    pub fn is_length_stop(reason: &str) -> bool {
        matches!(reason, "length" | "max_tokens")
    }
}

/// Cheap token estimate when a provider sends no usage: 4 chars per token, rounded up.
pub fn estimate_tokens(s: &str) -> u64 {
    (s.chars().count() as u64).div_ceil(4)
}

pub fn estimate_request(req: &ChatRequest) -> u64 {
    estimate_tokens(&req.system)
        + req
            .messages
            .iter()
            .map(|m| estimate_tokens(&m.content) + 4)
            .sum::<u64>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn retryable_classification() {
        assert!(LlmError::Status {
            status: 429,
            body: String::new()
        }
        .is_retryable());
        assert!(LlmError::Status {
            status: 503,
            body: String::new()
        }
        .is_retryable());
        assert!(!LlmError::Status {
            status: 400,
            body: String::new()
        }
        .is_retryable());
        assert!(!LlmError::MissingApiKey {
            provider: "a".into(),
            var: "K".into()
        }
        .is_retryable());
        assert!(
            !LlmError::Truncated {
                input_tokens: 1,
                output_tokens: 2,
                reasoning_tokens: 1
            }
            .is_retryable(),
            "replaying a truncated request verbatim cannot help"
        );
        assert!(LlmError::is_length_stop("length"));
        assert!(LlmError::is_length_stop("max_tokens"));
        assert!(!LlmError::is_length_stop("stop"));
    }
}
