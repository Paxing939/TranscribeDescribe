//! Settings window (F-102) and first-run wizard (F-103): a form over the most used keys of `config.toml`.
//! Edits go through `toml_edit`, so the user's comments survive; secrets go to the keyring (F-101).

use std::process::Command;

use callscribe_core::config::{Config, LlmProviderKind};
use callscribe_core::config_edit::{Edits, apply_edits_in_file};
use callscribe_core::AppPaths;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use toml_edit::{InlineTable, Value};

const KEY_LLM: &str = "llm";
const KEY_OBS: &str = "obs";

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Form {
    calls_dir: String,
    user_name: String,
    autostart: bool,
    detection_enabled: bool,
    source_window: bool,
    source_mic: bool,
    default_mode: String,
    /// `auto` — from the OBS websocket config; `keyring` — a saved password.
    obs_password_mode: String,
    obs_password: Option<String>,
    obs_exe: String,
    default_record_mode: String,
    model_gpu: String,
    model_cpu: String,
    device: String,
    language: String,
    glossary: String,
    /// ollama | deepseek | openai_compatible | anthropic | none
    llm_preset: String,
    llm_base_url: String,
    llm_model: String,
    allow_cloud: bool,
    llm_api_key: Option<String>,
    has_llm_key: bool,
    use_local_fallback: bool,
    auto_apply_min: f64,
    transliterate: bool,
    overlay_position: String,
    recording_pill: String,
    exclude_from_capture: bool,
    remind_consent: bool,
    min_free_gb: f64,
    delete_video_after_days: u32,
    pipeline_mode: String,
    first_run: bool,
}

fn enum_str<T: Serialize>(v: T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn has_secret(name: &str) -> bool {
    keyring::Entry::new("CallScribe", name).and_then(|e| e.get_password()).is_ok()
}

pub struct FirstRun(pub bool);

#[tauri::command]
pub fn settings_get(first: tauri::State<'_, FirstRun>) -> Result<Form, String> {
    let cfg = Config::load_default_location(&AppPaths::new()).map_err(|e| format!("{e:#}"))?;
    let preset = match cfg.llm.provider {
        LlmProviderKind::OpenaiCompatible if cfg.llm.base_url.contains("deepseek.com") => "deepseek".to_string(),
        p => enum_str(p),
    };
    Ok(Form {
        calls_dir: cfg.general.calls_dir.clone(),
        user_name: cfg.general.user_name.clone(),
        autostart: cfg.general.autostart,
        detection_enabled: cfg.detection.enabled,
        source_window: cfg.detection.sources.window,
        source_mic: cfg.detection.sources.mic,
        default_mode: enum_str(cfg.detection.default_mode),
        obs_password_mode: if cfg.obs.password.starts_with("keyring:") { "keyring".into() } else { "auto".into() },
        obs_password: None,
        obs_exe: cfg.obs.exe_path.clone(),
        default_record_mode: enum_str(cfg.obs.default_record_mode),
        model_gpu: cfg.transcription.model_gpu.clone(),
        model_cpu: cfg.transcription.model_cpu.clone(),
        device: enum_str(cfg.transcription.device),
        language: cfg.transcription.language.clone(),
        glossary: cfg.transcription.glossary.join(", "),
        llm_preset: preset,
        llm_base_url: cfg.llm.base_url.clone(),
        llm_model: cfg.llm.model.clone(),
        allow_cloud: cfg.llm.allow_cloud,
        llm_api_key: None,
        has_llm_key: cfg.llm.api_key.as_deref().and_then(|k| k.strip_prefix("keyring:")).is_some_and(has_secret),
        use_local_fallback: cfg.llm.local_fallback.is_some(),
        auto_apply_min: cfg.naming.auto_apply_after_min,
        transliterate: cfg.naming.transliterate,
        overlay_position: cfg.overlay.position.clone(),
        recording_pill: cfg.overlay.recording_pill.clone(),
        exclude_from_capture: cfg.overlay.exclude_from_capture,
        remind_consent: cfg.overlay.remind_consent,
        min_free_gb: cfg.storage.min_free_gb,
        delete_video_after_days: cfg.storage.delete_video_after_days.unwrap_or(0),
        pipeline_mode: enum_str(cfg.pipeline.mode),
        first_run: first.0,
    })
}

fn p(path: &str) -> Vec<String> {
    path.split('.').map(str::to_string).collect()
}

#[tauri::command]
pub async fn settings_save(form: Form, state: tauri::State<'_, crate::Tx>) -> Result<(), String> {
    let mut e: Edits = vec![
        (p("general.calls_dir"), Some(form.calls_dir.trim().into())),
        (p("general.user_name"), Some(form.user_name.trim().into())),
        (p("general.autostart"), Some(form.autostart.into())),
        (p("detection.enabled"), Some(form.detection_enabled.into())),
        (p("detection.sources.window"), Some(form.source_window.into())),
        (p("detection.sources.mic"), Some(form.source_mic.into())),
        (p("detection.default_mode"), Some(form.default_mode.as_str().into())),
        (p("obs.exe_path"), Some(form.obs_exe.trim().into())),
        (p("obs.default_record_mode"), Some(form.default_record_mode.as_str().into())),
        (p("transcription.model_gpu"), Some(form.model_gpu.trim().into())),
        (p("transcription.model_cpu"), Some(form.model_cpu.trim().into())),
        (p("transcription.device"), Some(form.device.as_str().into())),
        (p("transcription.language"), Some(form.language.as_str().into())),
        (p("naming.auto_apply_after_min"), Some(form.auto_apply_min.into())),
        (p("naming.transliterate"), Some(form.transliterate.into())),
        (p("overlay.position"), Some(form.overlay_position.as_str().into())),
        (p("overlay.recording_pill"), Some(form.recording_pill.as_str().into())),
        (p("overlay.exclude_from_capture"), Some(form.exclude_from_capture.into())),
        (p("overlay.remind_consent"), Some(form.remind_consent.into())),
        (p("storage.min_free_gb"), Some(form.min_free_gb.into())),
        (p("pipeline.mode"), Some(form.pipeline_mode.as_str().into())),
    ];
    let glossary: toml_edit::Array = form.glossary.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    e.push((p("transcription.glossary"), Some(Value::Array(glossary))));
    e.push((
        p("storage.delete_video_after_days"),
        (form.delete_video_after_days > 0).then(|| (form.delete_video_after_days as i64).into()),
    ));

    // OBS password (F-20).
    if form.obs_password_mode == "keyring" {
        if let Some(pw) = form.obs_password.as_deref().filter(|s| !s.is_empty()) {
            crate::secrets::set(KEY_OBS, pw).map_err(|e| format!("{e:#}"))?;
        }
        e.push((p("obs.password"), Some(format!("keyring:{KEY_OBS}").into())));
    } else {
        e.push((p("obs.password"), Some("auto".into())));
    }

    // LLM preset (F-70, F-71).
    if let Some(k) = form.llm_api_key.as_deref().filter(|s| !s.is_empty()) {
        crate::secrets::set(KEY_LLM, k).map_err(|e| format!("{e:#}"))?;
    }
    let key_ref: Option<Value> = Some(format!("keyring:{KEY_LLM}").into());
    match form.llm_preset.as_str() {
        "deepseek" => {
            let mut extra = InlineTable::new();
            let mut thinking = InlineTable::new();
            thinking.insert("type", "disabled".into());
            extra.insert("thinking", Value::InlineTable(thinking));
            e.extend([
                (p("llm.provider"), Some("openai_compatible".into())),
                (p("llm.base_url"), Some("https://api.deepseek.com".into())),
                (p("llm.model"), Some(if form.llm_model.trim().is_empty() { "deepseek-flash" } else { form.llm_model.trim() }.into())),
                (p("llm.api_key"), key_ref.clone()),
                (p("llm.json_mode"), Some("json_object".into())),
                (p("llm.extra_body"), Some(Value::InlineTable(extra))),
                (p("llm.context_tokens"), Some(131072i64.into())),
            ]);
        }
        "ollama" => e.extend([
            (p("llm.provider"), Some("ollama".into())),
            (p("llm.base_url"), Some(if form.llm_base_url.trim().is_empty() { "http://127.0.0.1:11434" } else { form.llm_base_url.trim() }.into())),
            (p("llm.model"), Some(form.llm_model.trim().into())),
            (p("llm.api_key"), None),
            (p("llm.json_mode"), Some("auto".into())),
            (p("llm.extra_body"), Some(Value::InlineTable(InlineTable::new()))),
            (p("llm.context_tokens"), Some(16384i64.into())),
        ]),
        "openai_compatible" | "anthropic" => e.extend([
            (p("llm.provider"), Some(form.llm_preset.as_str().into())),
            (p("llm.base_url"), Some(form.llm_base_url.trim().into())),
            (p("llm.model"), Some(form.llm_model.trim().into())),
            (p("llm.api_key"), if form.has_llm_key || form.llm_api_key.is_some() { key_ref.clone() } else { None }),
        ]),
        _ => e.push((p("llm.provider"), Some("none".into()))),
    }
    e.push((p("llm.allow_cloud"), Some(form.allow_cloud.into())));
    e.push((
        p("llm.local_fallback"),
        form.use_local_fallback.then(|| {
            let mut t = InlineTable::new();
            t.insert("provider", "ollama".into());
            t.insert("base_url", "http://127.0.0.1:11434".into());
            t.insert("model", "qwen2.5:7b".into());
            Value::InlineTable(t)
        }),
    ));

    apply_edits_in_file(&AppPaths::new().config_file(), &e).map_err(|e| format!("{e:#}"))?;
    let _ = state.0.send(crate::controller::Cmd::Menu("reload".into())).await;
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct Check {
    name: String,
    level: String,
    detail: String,
}

/// Runs `callscribe doctor --json` (the tray does not link STT engines, §4.2).
#[tauri::command]
pub async fn settings_doctor() -> Result<Vec<Check>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("callscribe.exe");
        let mut cmd = Command::new(exe);
        cmd.args(["doctor", "--json"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let out = cmd.output().map_err(|e| format!("doctor: {e}"))?;
        serde_json::from_slice::<Vec<Check>>(&out.stdout).map_err(|e| format!("doctor: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

pub fn open_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let r = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html#settings".into()))
        .title("CallScribe — настройки")
        .inner_size(760.0, 760.0)
        .min_inner_size(560.0, 480.0)
        .center()
        .build();
    if let Err(e) = r {
        tracing::error!("окно настроек: {e}");
    }
}
