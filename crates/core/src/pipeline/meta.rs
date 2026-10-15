//! `meta.json` — the source of truth for a call (§7.3, F-83).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::stt::EngineInfo;

pub const META_FILE: &str = "meta.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Recording,
    Recorded,
    Queued,
    Transcribing,
    Summarizing,
    SummaryPending,
    AwaitingName,
    Exported,
    Done,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub rule: String,
    pub tag: String,
    #[serde(default)]
    pub process: Option<String>,
    #[serde(default)]
    pub window_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pause {
    pub at: String,
    pub duration_sec: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub at_sec: f64,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Files {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_md: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_md: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioStreams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmInfo {
    pub provider: String,
    pub model: String,
    pub chunks: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PipelineInfo {
    pub mode: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stt: Option<EngineInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub schema_version: u32,
    pub id: String,
    pub status: CallStatus,
    #[serde(default)]
    pub title: Option<String>,
    /// `user` | `llm` | `auto`.
    #[serde(default)]
    pub title_source: Option<String>,
    /// Title proposed by the LLM, waiting for confirmation (F-44).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_title: Option<String>,
    pub app: AppInfo,
    pub trigger: String,
    #[serde(default)]
    pub detected_by: Vec<String>,
    #[serde(default)]
    pub record_mode: Option<String>,
    #[serde(default)]
    pub screen: Option<u32>,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub duration_sec: Option<f64>,
    #[serde(default)]
    pub pauses: Vec<Pause>,
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
    #[serde(default)]
    pub files: Files,
    #[serde(default)]
    pub audio_streams: AudioStreams,
    #[serde(default)]
    pub pipeline: PipelineInfo,
    pub cloud_allowed: bool,
    #[serde(default)]
    pub webcam_used: bool,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Recording not yet moved into the folder (cleared after ingest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
}

impl Meta {
    pub fn new(
        id: String,
        app: AppInfo,
        trigger: &str,
        started_at: chrono::DateTime<chrono::FixedOffset>,
    ) -> Meta {
        Meta {
            schema_version: 1,
            id,
            status: CallStatus::Queued,
            title: None,
            title_source: None,
            suggested_title: None,
            app,
            trigger: trigger.to_string(),
            detected_by: vec![],
            record_mode: None,
            screen: None,
            started_at: started_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
            ended_at: None,
            duration_sec: None,
            pauses: vec![],
            bookmarks: vec![],
            files: Files::default(),
            audio_streams: AudioStreams::default(),
            pipeline: PipelineInfo::default(),
            cloud_allowed: true,
            webcam_used: false,
            errors: vec![],
            warnings: vec![],
            source_file: None,
        }
    }

    pub fn started(&self) -> Result<chrono::DateTime<chrono::FixedOffset>> {
        chrono::DateTime::parse_from_rfc3339(&self.started_at).context("meta.started_at")
    }

    pub fn path(call_dir: &Path) -> PathBuf {
        call_dir.join(META_FILE)
    }

    pub fn load(call_dir: &Path) -> Result<Meta> {
        let p = Meta::path(call_dir);
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("не удалось прочитать {}", p.display()))?;
        serde_json::from_str(&text).with_context(|| format!("ошибка в {}", p.display()))
    }

    /// Atomic write: temp file + rename.
    pub fn save(&self, call_dir: &Path) -> Result<()> {
        write_json_atomic(&Meta::path(call_dir), self)
    }
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(&tmp, text).with_context(|| format!("не удалось записать {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("не удалось записать {}", path.display()))?;
    Ok(())
}

pub fn write_text_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).with_context(|| format!("не удалось записать {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("не удалось записать {}", path.display()))?;
    Ok(())
}
