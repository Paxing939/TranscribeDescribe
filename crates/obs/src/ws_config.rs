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

#[cfg(test)]
mod ini_tests {
    #[test]
    fn ini_section() {
        let c = super::parse_ini_section("[General]\nX=1\n[OBSWebSocket]\nServerEnabled=true\nServerPort=4456\nAuthRequired=true\nServerPassword=abc\n").unwrap();
        assert!(c.server_enabled && c.auth_required);
        assert_eq!((c.server_port, c.server_password.as_str()), (4456, "abc"));
        assert!(super::parse_ini_section("[General]\nX=1\n").is_none());
    }
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

/// Секция `[OBSWebSocket]` в `user.ini` / `global.ini` (в новых OBS настройки могут храниться там).
pub fn parse_ini_section(ini: &str) -> Option<WsConfig> {
    let mut in_sec = false;
    let mut found = false;
    let mut c = WsConfig { server_enabled: false, server_port: 4455, server_password: String::new(), auth_required: true };
    for line in ini.lines().map(str::trim) {
        if line.starts_with('[') {
            in_sec = line.eq_ignore_ascii_case("[OBSWebSocket]");
            continue;
        }
        if !in_sec {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let truthy = |v: &str| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1");
        match k.trim() {
            "ServerEnabled" => c.server_enabled = truthy(v),
            "ServerPort" => c.server_port = v.trim().parse().unwrap_or(4455),
            "AuthRequired" => c.auth_required = truthy(v),
            "ServerPassword" => {
                c.server_password = v.trim().to_string();
                found = true;
            }
            _ => {}
        }
    }
    found.then_some(c)
}

/// Прочитать конфиг obs-websocket; `None`, если файла нет или он не разбирается.
/// Сначала ini-файлы OBS (секция `[OBSWebSocket]`), затем `plugin_config/obs-websocket/config.json`.
pub fn read_websocket_config() -> Option<WsConfig> {
    let path = ws_config_path()?;
    if let Some(base) = path.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
        for ini in ["user.ini", "global.ini"] {
            if let Some(c) = std::fs::read_to_string(base.join(ini)).ok().as_deref().and_then(parse_ini_section) {
                return Some(c);
            }
        }
    }
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
