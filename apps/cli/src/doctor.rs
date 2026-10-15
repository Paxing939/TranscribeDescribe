//! `callscribe doctor` (F-130): system check.

use std::time::Duration;

use anyhow::Result;
use callscribe_core::config::LlmProviderKind;
use callscribe_core::llm::{self, Message};
use callscribe_core::{AppPaths, Config, models};
use serde::Serialize;

use crate::secrets::Keyring;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: String,
    pub level: Level,
    pub detail: String,
}

fn ok(name: &str, d: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        level: Level::Ok,
        detail: d.into(),
    }
}
fn warn(name: &str, d: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        level: Level::Warn,
        detail: d.into(),
    }
}
fn err(name: &str, d: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        level: Level::Error,
        detail: d.into(),
    }
}

pub fn run(paths: &AppPaths, cfg: Result<Config>, load_model: bool) -> Vec<Check> {
    let mut v = Vec::new();
    let cfg = match cfg {
        Ok(c) => {
            v.push(ok("конфиг", paths.config_file().display().to_string()));
            c
        }
        Err(e) => {
            v.push(err("конфиг", format!("{e:#}")));
            return v;
        }
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    // Calls folder and free space (F-85).
    let calls = cfg.calls_dir();
    match std::fs::create_dir_all(&calls) {
        Ok(()) => match callscribe_platform::free_space_bytes(&calls) {
            Ok(b) => {
                let gb = b as f64 / 1e9;
                if gb < cfg.storage.min_free_gb {
                    v.push(warn(
                        "место на диске",
                        format!(
                            "{gb:.1} ГБ свободно в {} (< {} ГБ)",
                            calls.display(),
                            cfg.storage.min_free_gb
                        ),
                    ));
                } else {
                    v.push(ok(
                        "место на диске",
                        format!("{gb:.1} ГБ свободно в {}", calls.display()),
                    ));
                }
            }
            Err(e) => v.push(warn("место на диске", format!("{e:#}"))),
        },
        Err(e) => v.push(err("папка звонков", format!("{}: {e}", calls.display()))),
    }

    // ffmpeg.
    match callscribe_media::Ffmpeg::locate(&cfg.pipeline.ffmpeg_dir) {
        Ok(f) => v.push(ok(
            "ffmpeg",
            format!(
                "{} ({})",
                f.version().unwrap_or_default(),
                f.ffmpeg.display()
            ),
        )),
        Err(e) => v.push(err("ffmpeg", format!("{e:#}"))),
    }

    // STT backend and models.
    v.push(ok(
        "бэкенд распознавания",
        format!(
            "whisper.cpp, сборка: {}",
            callscribe_stt_whisper::backend_name()
        ),
    ));
    let mdir = cfg.models_dir(paths);
    for (label, m) in [
        ("модель (GPU)", &cfg.transcription.model_gpu),
        ("модель (CPU)", &cfg.transcription.model_cpu),
    ] {
        let p = mdir.join(models::whisper_file(m));
        if p.is_file() {
            v.push(ok(label, p.display().to_string()));
        } else {
            v.push(warn(
                label,
                format!(
                    "{} не скачана — скачается при первой обработке",
                    models::whisper_file(m)
                ),
            ));
        }
    }
    if load_model {
        let p = mdir.join(models::whisper_file(&cfg.transcription.model_gpu));
        if p.is_file() {
            callscribe_stt_whisper::quiet_native_logs();
            let t = std::time::Instant::now();
            match callscribe_stt_whisper::WhisperTranscriber::load(
                &p,
                &cfg.transcription.model_gpu,
                true,
                None,
            ) {
                Ok(_) => v.push(ok(
                    "загрузка модели",
                    format!("{:.1} с", t.elapsed().as_secs_f64()),
                )),
                Err(e) => v.push(err("загрузка модели", format!("{e:#}"))),
            }
        }
    }

    // Secrets store (§11, п. 38).
    match keyring::Entry::new(crate::secrets::SERVICE, "__doctor_probe") {
        Ok(e) => match e.set_password("ok").and_then(|_| e.get_password()) {
            Ok(_) => {
                let _ = e.delete_credential();
                v.push(ok("хранилище секретов", "Windows Credential Manager"));
            }
            Err(x) => v.push(err("хранилище секретов", format!("{x}"))),
        },
        Err(x) => v.push(err("хранилище секретов", format!("{x}"))),
    }

    // OBS.
    v.extend(rt.block_on(crate::doctor::obs_checks(&cfg)));

    // LLM.
    v.extend(rt.block_on(llm_checks(&cfg)));
    v
}

async fn llm_checks(cfg: &Config) -> Vec<Check> {
    let mut v = Vec::new();
    if cfg.llm.provider == LlmProviderKind::None {
        v.push(ok("LLM", "отключена (provider = none)"));
        return v;
    }
    let mut endpoints = vec![("LLM", cfg.llm.primary())];
    if let Some(f) = cfg.llm.fallback() {
        endpoints.push(("LLM (local_fallback)", f));
    }
    for (name, e) in endpoints {
        let cloud = llm::is_cloud_endpoint(&e);
        if cloud && !cfg.llm.allow_cloud {
            v.push(warn(
                name,
                format!(
                    "{} — облачный провайдер, но llm.allow_cloud = false",
                    e.base_url
                ),
            ));
            continue;
        }
        let p = match llm::build(&e, &Keyring) {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(x) => {
                v.push(err(name, format!("{x:#}")));
                continue;
            }
        };
        let schema = serde_json::json!({"type": "object", "required": ["ok"], "properties": {"ok": {"type": "boolean"}}});
        let msg = [Message::user("Ответь JSON: {\"ok\": true}")];
        let r =
            tokio::time::timeout(Duration::from_secs(90), p.complete(&msg, Some(&schema))).await;
        let (prov, model) = p.describe();
        match r {
            Ok(Ok(text))
                if serde_json::from_str::<serde_json::Value>(llm::extract_json(&text)).is_ok() =>
            {
                v.push(ok(
                    name,
                    format!(
                        "{prov} / {model}: отвечает JSON{}",
                        if cloud { " (облако)" } else { "" }
                    ),
                ))
            }
            Ok(Ok(text)) => v.push(warn(
                name,
                format!(
                    "{prov} / {model}: ответ не JSON: {}",
                    text.chars().take(80).collect::<String>()
                ),
            )),
            Ok(Err(x)) => v.push(err(name, format!("{prov} / {model}: {x:#}"))),
            Err(_) => v.push(err(name, format!("{prov} / {model}: нет ответа за 90 с"))),
        }
    }
    v
}

pub async fn obs_checks(cfg: &Config) -> Vec<Check> {
    let mut v = Vec::new();
    let secret = match cfg.obs.password.strip_prefix("keyring:") {
        Some(name) => {
            use callscribe_core::llm::SecretResolver;
            Keyring.get(name).ok()
        }
        None => None,
    };
    let client = tokio::time::timeout(
        Duration::from_secs(5),
        callscribe_obs::ObsClient::connect(&cfg.obs, secret),
    )
    .await;
    match client {
        Ok(Ok(c)) => {
            v.push(ok(
                "OBS",
                format!("websocket {}:{} — подключено", cfg.obs.host, cfg.obs.port),
            ));
            let problems = c.check_settings(&cfg.obs).await;
            if problems.is_empty() {
                v.push(ok(
                    "настройки OBS",
                    "дорожки, контейнер, сцены и профиль в порядке",
                ));
            } else {
                for p in problems {
                    v.push(warn("настройки OBS", p));
                }
            }
        }
        Ok(Err(e)) => v.push(warn(
            "OBS",
            format!("не подключиться: {e:#} (OBS не запущен или websocket выключен, §14)"),
        )),
        Err(_) => v.push(warn("OBS", "нет ответа за 5 с")),
    }
    v
}

pub fn print(checks: &[Check], json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(checks).unwrap());
        return;
    }
    for c in checks {
        let mark = match c.level {
            Level::Ok => "[ OK ]",
            Level::Warn => "[ !! ]",
            Level::Error => "[FAIL]",
        };
        println!("{mark} {}: {}", c.name, c.detail);
    }
}
