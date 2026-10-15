//! A new call folder with `meta.json` for a recording (F-80, F-83). Media work happens later in the worker.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, Local, NaiveDateTime, TimeZone};

use crate::config::Config;
use crate::pipeline::meta::{AppInfo, CallStatus, Meta};
use crate::pipeline::naming::{NameParts, base_name, unique_path};

/// OBS default file name `2026-10-09 14-30-12.mkv` → start time.
pub fn started_from_obs_name(path: &Path) -> Option<DateTime<FixedOffset>> {
    let stem = path.file_stem()?.to_str()?;
    let s: String = stem.chars().take(19).collect();
    let naive = NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H-%M-%S")
        .or_else(|_| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d_%H-%M-%S"))
        .ok()?;
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|d| d.fixed_offset())
}

/// Start time of a recording: from the OBS file name, else file modification time minus `duration_sec`.
pub fn guess_started(path: &Path, duration_sec: f64) -> DateTime<FixedOffset> {
    if let Some(t) = started_from_obs_name(path) {
        return t;
    }
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let end: DateTime<Local> = modified
        .map(DateTime::<Local>::from)
        .unwrap_or_else(Local::now);
    (end - chrono::Duration::milliseconds((duration_sec * 1000.0) as i64)).fixed_offset()
}

pub struct NewCall<'a> {
    pub started_at: DateTime<FixedOffset>,
    pub app: AppInfo,
    pub trigger: &'a str,
    pub source_file: &'a Path,
    pub user_title: Option<String>,
    pub cloud_allowed: bool,
}

/// Create `<calls_dir>/{date}_{time}_{app}` with `meta.json` pointing at the source recording.
pub fn create_call(cfg: &Config, nc: NewCall) -> Result<(PathBuf, Meta)> {
    let calls = cfg.calls_dir();
    std::fs::create_dir_all(&calls)
        .with_context(|| format!("не удалось создать {}", calls.display()))?;
    let base = base_name(
        &cfg.naming,
        &NameParts {
            started_at: nc.started_at,
            app_tag: &nc.app.tag,
            title: None,
        },
    );
    let dir = unique_path(&calls, &base, None, None);
    std::fs::create_dir_all(&dir)?;
    let mut meta = Meta::new(
        uuid::Uuid::new_v4().to_string(),
        nc.app,
        nc.trigger,
        nc.started_at,
    );
    meta.status = CallStatus::Queued;
    meta.source_file = Some(nc.source_file.to_string_lossy().to_string());
    if let Some(t) = nc.user_title.filter(|t| !t.trim().is_empty()) {
        meta.title = Some(t);
        meta.title_source = Some("user".into());
    }
    meta.cloud_allowed = nc.cloud_allowed;
    meta.save(&dir)?;
    Ok((dir, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obs_names() {
        let t = started_from_obs_name(Path::new("C:/x/2026-10-09 14-30-12.mkv")).unwrap();
        assert_eq!(
            t.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-10-09 14:30:12"
        );
        assert!(started_from_obs_name(Path::new("call.mkv")).is_none());
    }
}
