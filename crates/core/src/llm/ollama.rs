//! Ollama native API: `/api/chat` with `options.num_ctx` (§11, п. 11).

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};

use super::{LlmError, LlmProvider, Message, http_client, post_json};
use crate::config::LlmEndpoint;

pub struct Ollama {
    e: LlmEndpoint,
    client: reqwest::Client,
}

impl Ollama {
    pub fn new(e: LlmEndpoint) -> Self {
        let client = http_client(e.timeout_sec.unwrap_or(600));
        Ollama { e, client }
    }
}

#[async_trait]
impl LlmProvider for Ollama {
    fn describe(&self) -> (String, String) {
        ("ollama".into(), self.e.model.clone())
    }
    fn is_cloud(&self) -> bool {
        !super::is_local_url(&self.e.base_url)
    }
    fn context_tokens(&self) -> u32 {
        self.e.context_tokens.unwrap_or(16384)
    }
    fn max_output_tokens(&self) -> u32 {
        self.e.max_output_tokens.unwrap_or(2048)
    }

    async fn complete(&self, messages: &[Message], schema: Option<&Value>) -> Result<String> {
        let mut body = json!({
            "model": self.e.model,
            "messages": messages,
            "stream": false,
            "options": {
                "num_ctx": self.context_tokens(),
                "num_predict": self.max_output_tokens(),
                "temperature": self.e.temperature.unwrap_or(0.2),
            },
            "keep_alive": self.e.keep_alive.unwrap_or(0),
        });
        if let Some(s) = schema {
            body["format"] = s.clone();
        }
        if let Some(extra) = &self.e.extra_body {
            for (k, v) in extra {
                body[k] = v.clone();
            }
        }
        let url = format!("{}/api/chat", self.e.base_url.trim_end_matches('/'));
        let v = post_json(&self.client, &url, &[], &body).await?;
        let content = v["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if content.is_empty() {
            return Err(LlmError::Fatal("пустой ответ".into()).into());
        }
        Ok(content)
    }
}
