//! OpenAI Chat Completions–compatible servers: DeepSeek, LM Studio, llama.cpp server, vLLM, OpenRouter…

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};

use super::{LlmError, LlmProvider, Message, http_client, post_json};
use crate::config::{JsonMode, LlmEndpoint};

pub struct OpenAiCompat {
    e: LlmEndpoint,
    key: Option<String>,
    client: reqwest::Client,
    /// For `json_mode = "auto"`: the mode the server accepted last time.
    accepted: Mutex<Option<JsonMode>>,
}

impl OpenAiCompat {
    pub fn new(e: LlmEndpoint, key: Option<String>) -> Self {
        let client = http_client(e.timeout_sec.unwrap_or(600));
        OpenAiCompat {
            e,
            key,
            client,
            accepted: Mutex::new(None),
        }
    }

    fn url(&self) -> String {
        format!("{}/chat/completions", self.e.base_url.trim_end_matches('/'))
    }

    fn body(&self, messages: &[Message], schema: Option<&Value>, mode: JsonMode) -> Value {
        let mut body = json!({
            "model": self.e.model,
            "messages": messages,
            "temperature": self.e.temperature.unwrap_or(0.2),
            "max_tokens": self.e.max_output_tokens.unwrap_or(2048),
            "stream": false,
        });
        match (mode, schema) {
            (JsonMode::JsonSchema, Some(s)) => {
                body["response_format"] = json!({
                    "type": "json_schema",
                    "json_schema": {"name": "summary", "schema": s, "strict": false}
                });
            }
            (JsonMode::JsonObject, Some(_)) | (JsonMode::JsonSchema, None) => {
                body["response_format"] = json!({"type": "json_object"});
            }
            _ => {}
        }
        if let Some(extra) = &self.e.extra_body {
            for (k, v) in extra {
                body[k] = v.clone();
            }
        }
        body
    }

    async fn send(&self, body: &Value) -> Result<String> {
        let mut headers = Vec::new();
        if let Some(k) = &self.key {
            headers.push(("Authorization", format!("Bearer {k}")));
        }
        let v = post_json(&self.client, &self.url(), &headers, body).await?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if content.is_empty() {
            // DeepSeek occasionally returns an empty content (§11, п. 21).
            return Err(LlmError::Fatal("пустой ответ".into()).into());
        }
        Ok(content)
    }
}

fn is_bad_request(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("HTTP 400") || s.contains("HTTP 422")
}

#[async_trait]
impl LlmProvider for OpenAiCompat {
    fn describe(&self) -> (String, String) {
        ("openai_compatible".into(), self.e.model.clone())
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
        let configured = self.e.json_mode.unwrap_or(JsonMode::Auto);
        if configured != JsonMode::Auto || schema.is_none() {
            return self.send(&self.body(messages, schema, configured)).await;
        }
        let remembered = *self.accepted.lock().unwrap();
        let order: Vec<JsonMode> = match remembered {
            Some(m) => vec![m],
            None => vec![JsonMode::JsonSchema, JsonMode::JsonObject, JsonMode::None],
        };
        let mut last = None;
        for mode in order {
            match self.send(&self.body(messages, schema, mode)).await {
                Ok(c) => {
                    *self.accepted.lock().unwrap() = Some(mode);
                    return Ok(c);
                }
                Err(e) if is_bad_request(&e) => {
                    tracing::info!("json_mode {mode:?} не принят сервером, пробую следующий");
                    last = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("json_mode auto: все режимы отклонены")))
    }
}
