//! Ollama native chat API (`/api/chat`). The OpenAI-compatible endpoint leaks a
//! model's reasoning into a separate `reasoning` field and leaves `content` empty on
//! thinking models; the native API exposes `think: false` and a clean `content`.

use futures_util::StreamExt;
use serde_json::{json, Value};

use super::provider::{
    estimate_request, estimate_tokens, ChatRequest, ChatResponse, LlmError, Role, Usage,
};

#[derive(Debug, Clone)]
pub struct OllamaClient {
    pub base_url: String,
    pub timeout_seconds: u64,
    /// `Some(false)` disables thinking; `None` leaves the model's default.
    pub think: Option<bool>,
    pub extra: Value,
}

/// `base_url` here is the OpenAI-style `.../v1`; the native API is its parent `/api`.
fn native_base(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_string()
}

impl OllamaClient {
    pub async fn complete(
        &self,
        http: &reqwest::Client,
        req: &ChatRequest,
    ) -> Result<ChatResponse, LlmError> {
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
            "stream": true,
            "options": {"temperature": req.temperature, "num_predict": req.max_tokens},
        });
        if let Some(t) = self.think {
            body["think"] = Value::Bool(t);
        }
        super::merge_extra(&mut body, &self.extra);
        let url = format!("{}/api/chat", native_base(&self.base_url));
        let resp = http
            .post(&url)
            .header("content-type", "application/json")
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: crate::verify::tail_bytes(&body, 2000),
            });
        }
        // The native API streams newline-delimited JSON, not SSE; eventsource-stream
        // would swallow it, so read raw chunks and split on newlines.
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut text = String::new();
        let mut usage = Usage::default();
        let mut got_usage = false;
        let mut stop_reason = None;
        'outer: while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim().to_string();
                buf.drain(..=nl);
                if line.is_empty() {
                    continue;
                }
                let data: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(err) = data.get("error").and_then(Value::as_str) {
                    return Err(LlmError::Stream(err.to_string()));
                }
                if let Some(t) = data["message"]["content"].as_str() {
                    text.push_str(t);
                }
                if data["done"].as_bool() == Some(true) {
                    stop_reason = data["done_reason"].as_str().map(String::from);
                    if let Some(n) = data["prompt_eval_count"].as_u64() {
                        usage.input_tokens = n;
                        got_usage = true;
                    }
                    if let Some(n) = data["eval_count"].as_u64() {
                        usage.output_tokens = n;
                    }
                    break 'outer;
                }
            }
        }
        if text.is_empty() {
            return Err(LlmError::Empty);
        }
        if !got_usage {
            usage = Usage {
                input_tokens: estimate_request(req),
                output_tokens: estimate_tokens(&text),
            };
        }
        Ok(ChatResponse {
            text,
            usage,
            stop_reason,
            usage_estimated: !got_usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_base_strips_v1() {
        assert_eq!(
            native_base("http://127.0.0.1:11434/v1"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            native_base("http://127.0.0.1:11434/v1/"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(native_base("http://host/api"), "http://host/api");
    }
}
