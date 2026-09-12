//! Anthropic Messages API, streaming.

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::provider::{ChatRequest, ChatResponse, LlmError, Role, Usage};

#[derive(Debug, Clone)]
pub struct AnthropicClient {
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub timeout_seconds: u64,
    pub extra: serde_json::Value,
}

impl AnthropicClient {
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
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                json!({
                    "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                    "content": m.content,
                })
            })
            .collect();
        let mut body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": messages,
            "temperature": req.temperature,
            "stream": true,
        });
        if !req.system.is_empty() {
            body["system"] = Value::String(req.system.clone());
        }
        super::merge_extra(&mut body, &self.extra);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut rb = http
            .post(&url)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .json(&body);
        if let Some(k) = key {
            rb = rb.header("x-api-key", k);
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
        let mut usage = Usage::default();
        let mut got_usage = false;
        let mut stop_reason = None;
        while let Some(ev) = stream.next().await {
            let ev = ev.map_err(|e| LlmError::Stream(e.to_string()))?;
            let data: Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match ev.event.as_str() {
                "message_start" => {
                    if let Some(n) = data["message"]["usage"]["input_tokens"].as_u64() {
                        usage.input_tokens = n;
                        got_usage = true;
                    }
                }
                "content_block_delta" => {
                    if let Some(t) = data["delta"]["text"].as_str() {
                        text.push_str(t);
                    }
                }
                "message_delta" => {
                    if let Some(n) = data["usage"]["output_tokens"].as_u64() {
                        usage.output_tokens = n;
                    }
                    if let Some(s) = data["delta"]["stop_reason"].as_str() {
                        stop_reason = Some(s.to_string());
                    }
                }
                "error" => {
                    return Err(LlmError::Stream(
                        data["error"]["message"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_string(),
                    ));
                }
                _ => {}
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
            reasoning_chars: 0,
        })
    }
}
