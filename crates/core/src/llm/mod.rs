//! LLM providers (F-70, F-71).

mod anthropic;
mod ollama;
mod openai_compat;

use std::time::Duration;

use anyhow::{Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::{LlmEndpoint, LlmProviderKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(s: impl Into<String>) -> Self {
        Message {
            role: Role::System,
            content: s.into(),
        }
    }
    pub fn user(s: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: s.into(),
        }
    }
    pub fn assistant(s: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: s.into(),
        }
    }
}

/// Errors worth retrying later (`summary_pending`, F-75) vs. permanent ones.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM недоступна: {0}")]
    Unavailable(String),
    #[error("ошибка LLM: {0}")]
    Fatal(String),
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// `(provider, model)` for `meta.json`.
    fn describe(&self) -> (String, String);
    fn is_cloud(&self) -> bool;
    /// Usable context window in tokens.
    fn context_tokens(&self) -> u32;
    fn max_output_tokens(&self) -> u32;
    async fn complete(
        &self,
        messages: &[Message],
        schema: Option<&serde_json::Value>,
    ) -> Result<String>;
}

/// Resolves `keyring:<name>` references (F-101). Implemented by the apps.
pub trait SecretResolver: Send + Sync {
    fn get(&self, name: &str) -> Result<String>;
}

pub struct NoSecrets;
impl SecretResolver for NoSecrets {
    fn get(&self, name: &str) -> Result<String> {
        bail!("секрет {name} недоступен")
    }
}

pub fn resolve_secret(value: Option<&str>, secrets: &dyn SecretResolver) -> Result<Option<String>> {
    match value {
        None => Ok(None),
        Some(v) if v.trim().is_empty() => Ok(None),
        Some(v) => match v.strip_prefix("keyring:") {
            Some(name) => secrets.get(name).map(Some),
            None => Ok(Some(v.to_string())),
        },
    }
}

/// localhost or a private network address (F-71).
pub fn is_local_url(url: &str) -> bool {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['/', '?'])
        .next()
        .unwrap_or("");
    let host = if host.starts_with('[') {
        host.trim_start_matches('[').split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
    };
    let host = host.to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".lan")
    {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

pub fn is_cloud_endpoint(e: &LlmEndpoint) -> bool {
    match e.provider {
        LlmProviderKind::Anthropic => true,
        LlmProviderKind::None => false,
        _ => !is_local_url(&e.base_url),
    }
}

pub fn http_client(timeout_sec: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_sec.max(10)))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .expect("http client")
}

/// Map a reqwest/HTTP failure to retryable or not.
pub(crate) fn classify(status: Option<reqwest::StatusCode>, msg: String) -> anyhow::Error {
    match status {
        None => LlmError::Unavailable(msg).into(),
        Some(s) if s.is_server_error() || s == reqwest::StatusCode::TOO_MANY_REQUESTS => {
            LlmError::Unavailable(msg).into()
        }
        Some(s) if s == reqwest::StatusCode::REQUEST_TIMEOUT => LlmError::Unavailable(msg).into(),
        Some(_) => LlmError::Fatal(msg).into(),
    }
}

pub(crate) async fn post_json(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, String)],
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut req = client.post(url).json(body);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return Err(classify(None, format!("{url}: {e}"))),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let short: String = text.chars().take(500).collect();
        return Err(classify(
            Some(status),
            format!("{url}: HTTP {status}: {short}"),
        ));
    }
    serde_json::from_str(&text).map_err(|e| LlmError::Fatal(format!("{url}: не JSON: {e}")).into())
}

/// Build a provider for an endpoint. `Ok(None)` for `provider = "none"`.
pub fn build(
    e: &LlmEndpoint,
    secrets: &dyn SecretResolver,
) -> Result<Option<Box<dyn LlmProvider>>> {
    let key = resolve_secret(e.api_key.as_deref(), secrets)?;
    Ok(match e.provider {
        LlmProviderKind::None => None,
        LlmProviderKind::Ollama => Some(Box::new(ollama::Ollama::new(e.clone()))),
        LlmProviderKind::OpenaiCompatible => {
            Some(Box::new(openai_compat::OpenAiCompat::new(e.clone(), key)))
        }
        LlmProviderKind::Anthropic => {
            let Some(key) = key else {
                bail!("для anthropic нужен llm.api_key")
            };
            Some(Box::new(anthropic::Anthropic::new(e.clone(), key)))
        }
    })
}

/// Remove `<think>…</think>` blocks and Markdown fences, then cut out the outermost JSON object (§11, п. 12).
pub fn extract_json(text: &str) -> &str {
    let mut t = text;
    if let Some(end) = t.rfind("</think>") {
        t = &t[end + "</think>".len()..];
    }
    let t = t.trim();
    match (t.find('{'), t.rfind('}')) {
        (Some(a), Some(b)) if b > a => &t[a..=b],
        _ => t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_urls() {
        assert!(is_local_url("http://127.0.0.1:11434"));
        assert!(is_local_url("http://localhost:1234/v1"));
        assert!(is_local_url("http://192.168.1.20:8080"));
        assert!(is_local_url("http://[::1]:8080"));
        assert!(!is_local_url("https://api.deepseek.com"));
        assert!(!is_local_url("https://8.8.8.8/v1"));
    }

    #[test]
    fn json_extraction() {
        assert_eq!(
            extract_json("<think>hm {x}</think>\n```json\n{\"a\":1}\n```"),
            "{\"a\":1}"
        );
        assert_eq!(
            extract_json("  {\"a\": {\"b\": 2}} "),
            "{\"a\": {\"b\": 2}}"
        );
    }
}
