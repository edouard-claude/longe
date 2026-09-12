//! OpenAI-compatible chat completions (Ollama, DeepSeek, OpenRouter), streaming.

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::provider::{ChatRequest, ChatResponse, LlmError, Role, Usage};

#[derive(Debug, Clone)]
pub struct OpenAiClient {
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub timeout_seconds: u64,
    pub extra: serde_json::Value,
}

impl OpenAiClient {
    fn api_key(&self, provider: &str) -> Result<Option<String>, LlmError> {
        match &self.api_key_env {
            None => Ok(None),
            Some(var) => std::env::var(var)
                .ok()
                .filter(|k| !k.is_empty())
                .map(Some)
                .ok_or_else(|| LlmError::MissingApiKey {
                    provider: provider.to_string(),
                    var: var.clone(),
                }),
        }
    }

    pub async fn complete(
        &self,
        http: &reqwest::Client,
        provider: &str,
        req: &ChatRequest,
    ) -> Result<ChatResponse, LlmError> {
        let key = self.api_key(provider)?;
        let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
        if !req.system.is_empty() {
            messages.push(json!({"role": "system", "content": req.system}));
        }
        for m in &req.messages {
            messages.push(json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                "content": m.content,
            }));
        }
        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        super::merge_extra(&mut body, &self.extra);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut rb = http
            .post(&url)
            .header("content-type", "application/json")
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .json(&body);
        if let Some(k) = key {
            rb = rb.bearer_auth(k);
        }
        let resp = rb.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: crate::verify::tail_bytes(&body, 2000),
            });
        }
        let mut stream = resp.bytes_stream().eventsource();
        let mut text = String::new();
        let mut reasoning_chars = 0usize;
        let mut usage = Usage::default();
        let mut got_usage = false;
        let mut stop_reason = None;
        while let Some(ev) = stream.next().await {
            let ev = ev.map_err(|e| LlmError::Stream(e.to_string()))?;
            if ev.data.trim() == "[DONE]" {
                break;
            }
            let data: Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(err) = data.get("error") {
                return Err(LlmError::Stream(
                    err["message"].as_str().unwrap_or("unknown").to_string(),
                ));
            }
            if let Some(choice) = data["choices"].as_array().and_then(|c| c.first()) {
                if let Some(t) = choice["delta"]["content"].as_str() {
                    text.push_str(t);
                }
                // DeepSeek and friends stream the chain of thought as
                // `reasoning_content`; only its size matters here.
                if let Some(t) = choice["delta"]["reasoning_content"].as_str() {
                    reasoning_chars += t.chars().count();
                }
                if let Some(s) = choice["finish_reason"].as_str() {
                    stop_reason = Some(s.to_string());
                }
            }
            if let Some(u) = data.get("usage").filter(|u| !u.is_null()) {
                if let Some(n) = u["prompt_tokens"].as_u64() {
                    usage.input_tokens = n;
                    got_usage = true;
                }
                if let Some(n) = u["completion_tokens"].as_u64() {
                    usage.output_tokens = n;
                }
                if let Some(n) = u["completion_tokens_details"]["reasoning_tokens"].as_u64() {
                    usage.reasoning_tokens = n;
                }
            }
        }
        let usage_estimated = !got_usage;
        if usage_estimated {
            usage = Usage::estimated(req, &text);
        }
        if stop_reason.as_deref().is_some_and(LlmError::is_length_stop) {
            return Err(LlmError::Truncated {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                reasoning_tokens: usage.reasoning_tokens,
            });
        }
        if text.is_empty() {
            return Err(LlmError::Empty);
        }
        Ok(ChatResponse {
            text,
            usage,
            stop_reason,
            usage_estimated,
            reasoning_chars,
        })
    }
}
