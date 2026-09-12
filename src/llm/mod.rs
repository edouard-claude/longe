//! The LLM client: a registry of providers built from `harness.toml`, with retry.
//! Two wire protocols, one enum: static dispatch, no trait object.

pub mod anthropic;
pub mod ollama;
pub mod openai;
pub mod provider;

use std::collections::BTreeMap;
use std::time::Duration;

use anthropic::AnthropicClient;
use ollama::OllamaClient;
use openai::OpenAiClient;
pub use provider::{ChatMessage, ChatRequest, ChatResponse, LlmError, Role};

use crate::config::{Harness, ModelRef, ProviderCfg, ProviderKind};

#[derive(Debug, Clone)]
pub enum ProviderClient {
    Anthropic(AnthropicClient),
    OpenAi(OpenAiClient),
    Ollama(OllamaClient),
}

#[derive(Debug, Clone)]
struct Entry {
    client: ProviderClient,
    max_retries: u32,
}

/// All configured providers.
#[derive(Debug, Clone)]
pub struct LlmRegistry {
    http: reqwest::Client,
    providers: BTreeMap<String, Entry>,
}

impl LlmRegistry {
    pub fn from_harness(h: &Harness) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()?;
        let mut providers = BTreeMap::new();
        for (name, cfg) in &h.providers {
            providers.insert(name.clone(), Entry::from_cfg(cfg));
        }
        Ok(Self { http, providers })
    }

    pub fn has_provider(&self, name: &str) -> bool {
        self.providers.contains_key(name)
    }

    pub fn provider_names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// One completion with exponential-backoff retry on retryable errors.
    pub async fn complete(
        &self,
        model: &ModelRef,
        req: &ChatRequest,
    ) -> Result<ChatResponse, LlmError> {
        let entry = self
            .providers
            .get(&model.provider)
            .ok_or_else(|| LlmError::UnknownProvider(model.provider.clone()))?;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let res = match &entry.client {
                ProviderClient::Anthropic(c) => c.complete(&self.http, &model.provider, req).await,
                ProviderClient::OpenAi(c) => c.complete(&self.http, &model.provider, req).await,
                ProviderClient::Ollama(c) => c.complete(&self.http, req).await,
            };
            match res {
                Ok(r) => return Ok(r),
                Err(e) if e.is_retryable() && attempt <= entry.max_retries => {
                    let wait = Duration::from_millis(500 * (1u64 << (attempt - 1).min(6)));
                    tracing::warn!(provider = %model.provider, attempt, error = %e, "llm retry in {wait:?}");
                    tokio::time::sleep(wait).await;
                }
                Err(e) if e.is_retryable() && attempt > 1 => {
                    return Err(LlmError::Exhausted {
                        attempts: attempt,
                        last: Box::new(e),
                    })
                }
                // A non-retryable error keeps its own type even after retries, so the
                // caller can match on it (`Truncated` drives the max_tokens retry).
                Err(e) => return Err(e),
            }
        }
    }
}

/// Overlay provider-specific fields on a request body.
pub fn merge_extra(body: &mut serde_json::Value, extra: &serde_json::Value) {
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
}

impl Entry {
    fn from_cfg(cfg: &ProviderCfg) -> Self {
        let extra = serde_json::to_value(&cfg.extra).unwrap_or(serde_json::Value::Null);
        let client = match cfg.kind {
            ProviderKind::Anthropic => ProviderClient::Anthropic(AnthropicClient {
                base_url: cfg.base_url.clone(),
                api_key_env: cfg.api_key_env.clone(),
                timeout_seconds: cfg.timeout_seconds,
                extra,
            }),
            ProviderKind::Openai => ProviderClient::OpenAi(OpenAiClient {
                base_url: cfg.base_url.clone(),
                api_key_env: cfg.api_key_env.clone(),
                timeout_seconds: cfg.timeout_seconds,
                extra,
            }),
            ProviderKind::Ollama => {
                let think = extra.get("think").and_then(serde_json::Value::as_bool);
                ProviderClient::Ollama(OllamaClient {
                    base_url: cfg.base_url.clone(),
                    timeout_seconds: cfg.timeout_seconds,
                    think,
                    extra,
                })
            }
        };
        Self {
            client,
            max_retries: cfg.max_retries,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Fake servers for both wire protocols, in-process with axum.
    use super::provider::Usage;
    use super::*;
    use axum::{routing::post, Router};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn sse(events: &[(&str, &str)]) -> String {
        let mut s = String::new();
        for (ev, data) in events {
            if !ev.is_empty() {
                s.push_str(&format!("event: {ev}\n"));
            }
            s.push_str(&format!("data: {data}\n\n"));
        }
        s
    }

    async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    fn harness(name: &str, kind: ProviderKind, base_url: &str, key_env: Option<&str>) -> Harness {
        let mut h = Harness::default();
        h.providers.insert(
            name.into(),
            ProviderCfg {
                kind,
                base_url: base_url.into(),
                api_key_env: key_env.map(String::from),
                max_retries: 2,
                timeout_seconds: 10,
                extra: toml::Table::new(),
            },
        );
        h
    }

    fn req() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: "sys".into(),
            messages: vec![ChatMessage::user("hi")],
            temperature: 0.0,
            max_tokens: 100,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn anthropic_stream_is_accumulated_with_usage() {
        let body = sse(&[
            (
                "message_start",
                r#"{"type":"message_start","message":{"usage":{"input_tokens":12}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let app = Router::new().route(
            "/v1/messages",
            post(move |headers: axum::http::HeaderMap| {
                let body = body.clone();
                async move {
                    assert_eq!(headers.get("x-api-key").unwrap(), "k1");
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        );
        let url = serve(app).await;
        std::env::set_var("LONGE_TEST_KEY_A", "k1");
        let reg = LlmRegistry::from_harness(&harness(
            "a",
            ProviderKind::Anthropic,
            &url,
            Some("LONGE_TEST_KEY_A"),
        ))
        .unwrap();
        let m = ModelRef {
            provider: "a".into(),
            name: "m".into(),
        };
        let r = reg.complete(&m, &req()).await.unwrap();
        assert_eq!(r.text, "Hello");
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 12,
                output_tokens: 3,
                reasoning_tokens: 0
            }
        );
        assert_eq!(r.stop_reason.as_deref(), Some("end_turn"));
        assert!(!r.usage_estimated);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn openai_stream_with_usage_and_done() {
        let body = sse(&[
            (
                "",
                r#"{"choices":[{"delta":{"content":"a"},"finish_reason":null}]}"#,
            ),
            (
                "",
                r#"{"choices":[{"delta":{"content":"b"},"finish_reason":"stop"}]}"#,
            ),
            (
                "",
                r#"{"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":2}}"#,
            ),
            ("", "[DONE]"),
        ]);
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let body = body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );
        let url = serve(app).await;
        let reg = LlmRegistry::from_harness(&harness(
            "o",
            ProviderKind::Openai,
            &format!("{url}/v1"),
            None,
        ))
        .unwrap();
        let m = ModelRef {
            provider: "o".into(),
            name: "m".into(),
        };
        let r = reg.complete(&m, &req()).await.unwrap();
        assert_eq!(r.text, "ab");
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 7,
                output_tokens: 2,
                reasoning_tokens: 0
            }
        );
        assert_eq!(r.stop_reason.as_deref(), Some("stop"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn openai_without_usage_estimates() {
        let body = sse(&[
            ("", r#"{"choices":[{"delta":{"content":"abcdefgh"}}]}"#),
            ("", "[DONE]"),
        ]);
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let body = body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );
        let url = serve(app).await;
        let reg =
            LlmRegistry::from_harness(&harness("o", ProviderKind::Openai, &url, None)).unwrap();
        let m = ModelRef {
            provider: "o".into(),
            name: "m".into(),
        };
        let r = reg.complete(&m, &req()).await.unwrap();
        assert!(r.usage_estimated);
        assert_eq!(r.usage.output_tokens, 2);
        assert!(r.usage.input_tokens > 0);
    }

    /// A reasoning model that spends the whole budget thinking: no `content`, a
    /// stream of `reasoning_content`, then `finish_reason: "length"`.
    #[tokio::test(flavor = "multi_thread")]
    async fn openai_length_stop_is_truncated_not_empty() {
        let body = sse(&[
            (
                "",
                r#"{"choices":[{"delta":{"reasoning_content":"let me think"},"finish_reason":null}]}"#,
            ),
            (
                "",
                r#"{"choices":[{"delta":{"reasoning_content":" harder"},"finish_reason":"length"}]}"#,
            ),
            (
                "",
                r#"{"choices":[],"usage":{"prompt_tokens":40,"completion_tokens":100,"completion_tokens_details":{"reasoning_tokens":100}}}"#,
            ),
            ("", "[DONE]"),
        ]);
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let body = body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );
        let url = serve(app).await;
        let reg =
            LlmRegistry::from_harness(&harness("o", ProviderKind::Openai, &url, None)).unwrap();
        let m = ModelRef {
            provider: "o".into(),
            name: "m".into(),
        };
        let err = reg.complete(&m, &req()).await.unwrap_err();
        assert!(
            matches!(
                err,
                LlmError::Truncated {
                    input_tokens: 40,
                    output_tokens: 100,
                    reasoning_tokens: 100
                }
            ),
            "{err}"
        );
        assert!(!err.is_retryable());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn openai_reasoning_then_answer_counts_reasoning() {
        let body = sse(&[
            (
                "",
                r#"{"choices":[{"delta":{"reasoning_content":"hmm"},"finish_reason":null}]}"#,
            ),
            (
                "",
                r#"{"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            ),
            (
                "",
                r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":16,"completion_tokens_details":{"reasoning_tokens":14}}}"#,
            ),
            ("", "[DONE]"),
        ]);
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let body = body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );
        let url = serve(app).await;
        let reg =
            LlmRegistry::from_harness(&harness("o", ProviderKind::Openai, &url, None)).unwrap();
        let m = ModelRef {
            provider: "o".into(),
            name: "m".into(),
        };
        let r = reg.complete(&m, &req()).await.unwrap();
        assert_eq!(r.text, "ok");
        assert_eq!(r.reasoning_chars, 3);
        assert_eq!(r.usage.reasoning_tokens, 14);
        assert_eq!(r.usage.output_tokens, 16);
        assert_eq!(r.stop_reason.as_deref(), Some("stop"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn anthropic_max_tokens_stop_is_truncated_even_with_text() {
        let body = sse(&[
            (
                "message_start",
                r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"```lua\nfs.write('a', [[cut"}}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":8}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let app = Router::new().route(
            "/v1/messages",
            post(move || {
                let body = body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );
        let url = serve(app).await;
        let reg =
            LlmRegistry::from_harness(&harness("a", ProviderKind::Anthropic, &url, None)).unwrap();
        let m = ModelRef {
            provider: "a".into(),
            name: "m".into(),
        };
        let err = reg.complete(&m, &req()).await.unwrap_err();
        assert!(
            matches!(
                err,
                LlmError::Truncated {
                    output_tokens: 8,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retries_on_503_then_succeeds() {
        let hits = Arc::new(AtomicU32::new(0));
        let h2 = hits.clone();
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let hits = h2.clone();
                async move {
                    let n = hits.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            [("content-type", "text/plain")],
                            "busy".to_string(),
                        )
                    } else {
                        (
                            axum::http::StatusCode::OK,
                            [("content-type", "text/event-stream")],
                            sse(&[
                                ("", r#"{"choices":[{"delta":{"content":"ok"}}]}"#),
                                ("", "[DONE]"),
                            ]),
                        )
                    }
                }
            }),
        );
        let url = serve(app).await;
        let reg =
            LlmRegistry::from_harness(&harness("o", ProviderKind::Openai, &url, None)).unwrap();
        let m = ModelRef {
            provider: "o".into(),
            name: "m".into(),
        };
        let r = reg.complete(&m, &req()).await.unwrap();
        assert_eq!(r.text, "ok");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn non_retryable_status_fails_fast() {
        let hits = Arc::new(AtomicU32::new(0));
        let h2 = hits.clone();
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let hits = h2.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (axum::http::StatusCode::BAD_REQUEST, "nope".to_string())
                }
            }),
        );
        let url = serve(app).await;
        let reg =
            LlmRegistry::from_harness(&harness("o", ProviderKind::Openai, &url, None)).unwrap();
        let m = ModelRef {
            provider: "o".into(),
            name: "m".into(),
        };
        let err = reg.complete(&m, &req()).await.unwrap_err();
        assert!(matches!(err, LlmError::Status { status: 400, .. }), "{err}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_key_and_unknown_provider() {
        let reg = LlmRegistry::from_harness(&harness(
            "a",
            ProviderKind::Anthropic,
            "http://127.0.0.1:1",
            Some("LONGE_NO_SUCH_KEY"),
        ))
        .unwrap();
        let err = reg
            .complete(
                &ModelRef {
                    provider: "a".into(),
                    name: "m".into(),
                },
                &req(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::MissingApiKey { .. }));
        let err = reg
            .complete(
                &ModelRef {
                    provider: "zz".into(),
                    name: "m".into(),
                },
                &req(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::UnknownProvider(_)));
    }
}
