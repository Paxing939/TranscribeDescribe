//! Anthropic Messages API (always cloud).

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};

use super::{LlmError, LlmProvider, Message, Role, http_client, post_json};
use crate::config::LlmEndpoint;

pub struct Anthropic {
    e: LlmEndpoint,
    key: String,
    client: reqwest::Client,
}

impl Anthropic {
    pub fn new(e: LlmEndpoint, key: String) -> Self {
        let client = http_client(e.timeout_sec.unwrap_or(600));
        Anthropic { e, key, client }
    }
}

#[async_trait]
impl LlmProvider for Anthropic {
    fn describe(&self) -> (String, String) {
        ("anthropic".into(), self.e.model.clone())
    }
    fn is_cloud(&self) -> bool {
        true
    }
    fn context_tokens(&self) -> u32 {
        self.e.context_tokens.unwrap_or(16384)
    }
    fn max_output_tokens(&self) -> u32 {
        self.e.max_output_tokens.unwrap_or(2048)
    }

    async fn complete(&self, messages: &[Message], _schema: Option<&Value>) -> Result<String> {
        let system: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content.as_str())
            .collect();
        let msgs: Vec<&Message> = messages.iter().filter(|m| m.role != Role::System).collect();
        let mut body = json!({
            "model": self.e.model,
            "max_tokens": self.max_output_tokens(),
            "temperature": self.e.temperature.unwrap_or(0.2),
            "messages": msgs,
        });
        if !system.is_empty() {
            body["system"] = json!(system.join("\n\n"));
        }
        let base = if self.e.base_url.trim().is_empty() {
            "https://api.anthropic.com"
        } else {
            self.e.base_url.trim_end_matches('/')
        };
        let url = format!("{base}/v1/messages");
        let headers = [
            ("x-api-key", self.key.clone()),
            ("anthropic-version", "2023-06-01".to_string()),
        ];
        let v = post_json(&self.client, &url, &headers, &body).await?;
        let text: String = v["content"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Err(LlmError::Fatal("пустой ответ".into()).into());
        }
        Ok(text)
    }
}
