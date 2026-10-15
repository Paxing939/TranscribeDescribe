//! Application configuration (`config.toml`, spec §6, F-100).
//!
//! Defaults live in one place — `default_config.toml`. A user file is deep-merged
//! over it, so missing keys get their default values, and only then deserialized.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;

pub const DEFAULT_CONFIG_TOML: &str = include_str!("default_config.toml");

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub detection: Detection,
    pub obs: Obs,
    pub pipeline: Pipeline,
    pub worker: Worker,
    pub transcription: Transcription,
    pub llm: Llm,
    pub naming: Naming,
    pub overlay: Overlay,
    pub hotkeys: Hotkeys,
    pub storage: Storage,
    #[serde(default)]
    pub hooks: Hooks,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct General {
    pub calls_dir: String,
    pub user_name: String,
    pub language: String,
    pub autostart: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleMode {
    Auto,
    Ask,
    RecordThenAsk,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTimeoutAction {
    Ignore,
    Record,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecordMode {
    #[default]
    Audio,
    Screen,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Detection {
    pub enabled: bool,
    pub poll_interval_sec: f64,
    pub start_debounce_sec: f64,
    pub end_debounce_sec: f64,
    pub silence_timeout_sec: f64,
    pub silence_threshold_db: f64,
    pub auto_stop_countdown_sec: f64,
    pub prompt_timeout_sec: f64,
    pub prompt_timeout_action: PromptTimeoutAction,
    pub max_duration_min: f64,
    pub default_mode: RuleMode,
    pub ignore_processes: Vec<String>,
    pub sources: Sources,
    #[serde(default)]
    pub process_sets: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    pub window: bool,
    pub mic: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RuleConfig {
    pub name: String,
    #[serde(default)]
    pub process: Vec<String>,
    #[serde(default)]
    pub meeting_process: Vec<String>,
    pub meeting_window_regex: Option<String>,
    pub meeting_window_ignore_regex: Option<String>,
    pub window_title_regex: Option<String>,
    pub mode: Option<RuleMode>,
    pub start_debounce_sec: Option<f64>,
    pub record_mode: Option<RecordMode>,
    pub screen: Option<u32>,
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Obs {
    pub host: String,
    pub port: u16,
    pub password: String,
    pub exe_path: String,
    pub launch_if_not_running: bool,
    pub launch_args: Vec<String>,
    pub screen_scenes: Vec<String>,
    pub audio_profile: String,
    pub default_record_mode: RecordMode,
    pub default_screen: u32,
    pub override_record_dir: bool,
    pub mic_input: String,
    pub desktop_input: String,
    pub tracks: Tracks,
    pub remux_to_mp4: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tracks {
    pub mix: u32,
    pub mic: u32,
    pub desktop: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineMode {
    Local,
    RecordOnly,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteShort {
    Ask,
    Auto,
    Never,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pipeline {
    pub mode: PipelineMode,
    pub defer_during_recording: bool,
    pub keep_extracted_audio: bool,
    pub min_duration_sec: f64,
    pub delete_short_recordings: DeleteShort,
    pub ffmpeg_dir: String,
    pub remote: Remote,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Remote {
    pub outbox_dir: Option<String>,
    pub audio_format: String,
    pub include_video: bool,
    pub cleanup_after_import: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    pub inbox_dir: Option<String>,
    pub poll_interval_sec: f64,
    pub claim_timeout_min: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Device {
    Auto,
    Gpu,
    Cpu,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Transcription {
    pub engine: String,
    pub model_gpu: String,
    pub model_cpu: String,
    pub models_dir: String,
    pub device: Device,
    pub language: String,
    pub beam_size: u32,
    pub vad: bool,
    /// Diarization of the «Собеседник» track (F-60), needs the `sherpa` build.
    pub diarization: bool,
    pub glossary: Vec<String>,
    pub hallucination_blacklist: Vec<String>,
    pub merge_gap_sec: f64,
    pub echo_dedupe: EchoDedupe,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EchoDedupe {
    pub enabled: bool,
    pub similarity: f64,
    pub max_offset_sec: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderKind {
    Ollama,
    OpenaiCompatible,
    Anthropic,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonMode {
    Auto,
    JsonSchema,
    JsonObject,
    None,
}

/// One LLM endpoint. Used both for `[llm]` and `llm.local_fallback`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmEndpoint {
    pub provider: LlmProviderKind,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    pub api_key: Option<String>,
    pub json_mode: Option<JsonMode>,
    pub extra_body: Option<serde_json::Map<String, serde_json::Value>>,
    pub context_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub timeout_sec: Option<u64>,
    pub keep_alive: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Llm {
    pub provider: LlmProviderKind,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub allow_cloud: bool,
    pub json_mode: JsonMode,
    pub extra_body: serde_json::Map<String, serde_json::Value>,
    pub context_tokens: u32,
    pub max_output_tokens: u32,
    pub temperature: f64,
    pub timeout_sec: u64,
    pub keep_alive: i64,
    pub prompts_dir: Option<String>,
    pub local_fallback: Option<LlmEndpoint>,
}

impl Llm {
    /// The primary endpoint with every optional field filled from `[llm]`.
    pub fn primary(&self) -> LlmEndpoint {
        LlmEndpoint {
            provider: self.provider,
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            json_mode: Some(self.json_mode),
            extra_body: Some(self.extra_body.clone()),
            context_tokens: Some(self.context_tokens),
            max_output_tokens: Some(self.max_output_tokens),
            temperature: Some(self.temperature),
            timeout_sec: Some(self.timeout_sec),
            keep_alive: Some(self.keep_alive),
        }
    }

    /// The fallback endpoint; missing optional fields inherit from `[llm]`.
    pub fn fallback(&self) -> Option<LlmEndpoint> {
        let f = self.local_fallback.as_ref()?;
        let p = self.primary();
        Some(LlmEndpoint {
            provider: f.provider,
            base_url: f.base_url.clone(),
            model: f.model.clone(),
            api_key: f.api_key.clone(),
            json_mode: f.json_mode.or(Some(JsonMode::Auto)),
            extra_body: f.extra_body.clone().or(Some(Default::default())),
            context_tokens: f.context_tokens.or(p.context_tokens),
            max_output_tokens: f.max_output_tokens.or(p.max_output_tokens),
            temperature: f.temperature.or(p.temperature),
            timeout_sec: f.timeout_sec.or(p.timeout_sec),
            keep_alive: f.keep_alive.or(p.keep_alive),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Naming {
    pub template: String,
    pub date_format: String,
    pub time_format: String,
    pub slugify: bool,
    pub transliterate: bool,
    pub max_title_len: usize,
    pub auto_apply_after_min: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    pub position: String,
    pub recording_pill: String,
    pub exclude_from_capture: bool,
    pub remind_consent: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hotkeys {
    pub toggle_recording: String,
    pub bookmark: String,
    pub pause: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    pub min_free_gb: f64,
    pub delete_video_after_days: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    pub on_complete_webhook: Option<String>,
    pub on_complete_command: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config::from_toml_str("").expect("embedded default config is valid")
    }
}

impl Config {
    /// Parse a user config, filling missing keys from the defaults.
    pub fn from_toml_str(user: &str) -> Result<Config> {
        let mut base: toml::Table =
            toml::from_str(DEFAULT_CONFIG_TOML).context("embedded default config")?;
        let user: toml::Table = toml::from_str(user).map_err(|e| anyhow::anyhow!("{e}"))?;
        merge_tables(&mut base, user);
        let cfg: Config = toml::Value::Table(base)
            .try_into()
            .map_err(|e: toml::de::Error| anyhow::anyhow!("{e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load `config.toml`; create it from the commented defaults if missing.
    pub fn load_or_create(path: &Path) -> Result<Config> {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, DEFAULT_CONFIG_TOML)
                .with_context(|| format!("не удалось создать {}", path.display()))?;
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        // Notepad and PowerShell may save UTF-8 with a BOM.
        let text = text.trim_start_matches('\u{feff}');
        Config::from_toml_str(text).with_context(|| format!("ошибка в {}", path.display()))
    }

    pub fn load_default_location(paths: &AppPaths) -> Result<Config> {
        Config::load_or_create(&paths.config_file())
    }

    fn validate(&self) -> Result<()> {
        // Compile every regex up front: a bad pattern is a config error, not a panic in detection (§11, п. 32).
        for rule in &self.detection.rules {
            for (key, re) in [
                ("meeting_window_regex", &rule.meeting_window_regex),
                (
                    "meeting_window_ignore_regex",
                    &rule.meeting_window_ignore_regex,
                ),
                ("window_title_regex", &rule.window_title_regex),
            ] {
                if let Some(re) = re {
                    regex::Regex::new(re)
                        .with_context(|| format!("правило «{}»: неверный {key}", rule.name))?;
                }
            }
            for p in &rule.process {
                if let Some(set) = p.strip_prefix('@')
                    && !self.detection.process_sets.contains_key(set)
                {
                    anyhow::bail!("правило «{}»: нет набора процессов @{set}", rule.name);
                }
            }
        }
        for re in &self.transcription.hallucination_blacklist {
            regex::Regex::new(re)
                .with_context(|| format!("hallucination_blacklist: неверный regex {re}"))?;
        }
        Ok(())
    }

    /// `general.calls_dir` with `~` expanded.
    pub fn calls_dir(&self) -> PathBuf {
        expand_home(&self.general.calls_dir)
    }

    pub fn models_dir(&self, paths: &AppPaths) -> PathBuf {
        if self.transcription.models_dir == "auto" {
            paths.models_dir()
        } else {
            expand_home(&self.transcription.models_dir)
        }
    }

    pub fn prompts_dir(&self) -> Option<PathBuf> {
        self.llm.prompts_dir.as_deref().map(expand_home)
    }
}

pub fn expand_home(p: &str) -> PathBuf {
    // One separator style, so paths compare equal in the job queue.
    let p = &if cfg!(windows) {
        p.replace('/', "\\")
    } else {
        p.to_string()
    };
    if let Some(rest) = p.strip_prefix("~\\").or_else(|| p.strip_prefix("~/"))
        && let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
    {
        return home.join(rest);
    }
    if p == "~"
        && let Some(b) = directories::BaseDirs::new()
    {
        return b.home_dir().to_path_buf();
    }
    PathBuf::from(p)
}

/// Deep-merge `over` into `base`: tables merge key by key, everything else (arrays too) replaces.
fn merge_tables(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge_tables(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        let c = Config::default();
        assert_eq!(c.detection.rules.len(), 10);
        assert_eq!(c.detection.rules[0].name, "Zoom");
        assert_eq!(c.llm.provider, LlmProviderKind::Ollama);
        assert!(c.llm.local_fallback.is_none());
        assert_eq!(c.obs.tracks.mic, 2);
    }

    #[test]
    fn user_keys_override_defaults() {
        let c = Config::from_toml_str(
            r#"
            [llm]
            provider = "openai_compatible"
            base_url = "https://api.deepseek.com"
            model = "deepseek-flash"
            allow_cloud = true
            json_mode = "json_object"
            extra_body = { thinking = { type = "disabled" } }
            local_fallback = { provider = "ollama", base_url = "http://127.0.0.1:11434", model = "qwen2.5:7b" }
            "#,
        )
        .unwrap();
        assert_eq!(c.llm.provider, LlmProviderKind::OpenaiCompatible);
        assert_eq!(c.llm.temperature, 0.2, "untouched keys keep defaults");
        let fb = c.llm.fallback().unwrap();
        assert_eq!(fb.provider, LlmProviderKind::Ollama);
        assert_eq!(fb.context_tokens, Some(16384));
        assert_eq!(
            c.detection.rules.len(),
            10,
            "rules array kept when not overridden"
        );
    }

    #[test]
    fn bad_regex_is_reported() {
        let err = Config::from_toml_str(
            r#"
            [[detection.rules]]
            name = "X"
            window_title_regex = "("
            "#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("правило «X»"));
    }

    #[test]
    fn unknown_key_is_an_error() {
        assert!(Config::from_toml_str("[general]\ncalls_dirr = \"x\"").is_err());
    }
}
