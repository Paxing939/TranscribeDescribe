//! Чтение конфига obs-websocket (F-20, пароль `auto`).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Нужные поля `plugin_config/obs-websocket/config.json`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct WsConfig {
    #[serde(default)]
    pub server_enabled: bool,
    #[serde(default = "default_port")]
    pub server_port: u16,
    #[serde(default)]
    pub server_password: String,
    #[serde(default = "default_true")]
    pub auth_required: bool,
}

fn default_port() -> u16 {
    4455
}

fn default_true() -> bool {
    true
}

/// Разобрать JSON конфига obs-websocket.
pub fn parse_ws_config(json: &str) -> Result<WsConfig> {
    serde_json::from_str(json).context("некорректный config.json obs-websocket")
}

/// Путь к конфигу obs-websocket для текущей ОС.
pub fn ws_config_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = PathBuf::from(std::env::var_os("APPDATA")?).join("obs-studio");
    #[cfg(not(windows))]
    let base = PathBuf::from(std::env::var_os("HOME")?)
        .join(".config")
        .join("obs-studio");
    Some(
        base.join("plugin_config")
            .join("obs-websocket")
            .join("config.json"),
    )
}

/// Прочитать конфиг obs-websocket; `None`, если файла нет или он не разбирается.
pub fn read_websocket_config() -> Option<WsConfig> {
    let path = ws_config_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    match parse_ws_config(&text) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("{}: {e:#}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_obs_config() {
        let c = parse_ws_config(
            r#"{"alerts_enabled": false, "auth_required": true, "first_load": false,
                "server_enabled": true, "server_password": "pw", "server_port": 4466}"#,
        )
        .unwrap();
        assert_eq!(c.server_port, 4466);
        assert_eq!(c.server_password, "pw");
        assert!(c.auth_required && c.server_enabled);
    }

    #[test]
    fn defaults_for_missing_fields() {
        let c = parse_ws_config("{}").unwrap();
        assert_eq!(c.server_port, 4455);
        assert!(c.auth_required);
        assert!(!c.server_enabled);
    }

    #[test]
    fn broken_json_is_error() {
        assert!(parse_ws_config("{").is_err());
    }
}
