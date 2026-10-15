//! Applying a title: rename the folder and the media files, update `meta.json` (F-44, F-80…F-82).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Naming;
use crate::pipeline::meta::{CallStatus, Meta};
use crate::pipeline::naming::{NameParts, base_name, rename_with_retry, unique_path};

/// Rename the call folder and its media/subtitle files after `title` (or back to `{date}_{time}_{app}` for `None`).
/// Returns the new folder. `meta.json` is written only after every rename succeeded (F-82).
pub fn apply_title(
    call_dir: &Path,
    title: Option<&str>,
    source: &str,
    naming: &Naming,
) -> Result<PathBuf> {
    let mut meta = Meta::load(call_dir)?;
    let started = meta.started()?;
    let base = base_name(
        naming,
        &NameParts {
            started_at: started,
            app_tag: &meta.app.tag,
            title,
        },
    );
    let parent = call_dir.parent().context("у папки звонка нет родителя")?;
    let new_dir = unique_path(parent, &base, None, Some(call_dir));
    let final_base = new_dir.file_name().unwrap().to_string_lossy().to_string();
    let retry = Duration::from_secs(300);

    // Files first, inside the old folder.
    let old_video = meta.files.video.clone();
    for slot in [&mut meta.files.video, &mut meta.files.subtitles] {
        if let Some(name) = slot.clone() {
            let old = call_dir.join(&name);
            let ext = Path::new(&name)
                .extension()
                .map(|e| e.to_string_lossy().to_string());
            let new_name = match &ext {
                Some(e) => format!("{final_base}.{e}"),
                None => final_base.clone(),
            };
            if new_name != name && old.exists() {
                rename_with_retry(&old, &call_dir.join(&new_name), retry)?;
            }
            *slot = Some(new_name);
        }
    }
    // summary.md links the recording by file name.
    if let (Some(old), Some(new)) = (&old_video, &meta.files.video) {
        let summary = call_dir.join("summary.md");
        if old != new
            && let Ok(text) = std::fs::read_to_string(&summary)
        {
            let enc = |s: &str| s.replace(' ', "%20");
            let updated = text.replace(&format!("]({})", enc(old)), &format!("]({})", enc(new)));
            if updated != text {
                crate::pipeline::meta::write_text_atomic(&summary, &updated)?;
            }
        }
    }
    if new_dir != call_dir {
        rename_with_retry(call_dir, &new_dir, retry)?;
    }
    meta.title = title.map(str::to_string);
    meta.title_source = title.map(|_| source.to_string());
    meta.suggested_title = None;
    if matches!(meta.status, CallStatus::AwaitingName) {
        meta.status = CallStatus::Done;
    }
    meta.save(&new_dir)?;
    Ok(new_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::pipeline::meta::AppInfo;

    #[test]
    fn rename_folder_and_files() {
        let d = tempfile::tempdir().unwrap();
        let naming = Config::default().naming;
        let started = chrono::DateTime::parse_from_rfc3339("2026-10-09T14:30:12+03:00").unwrap();
        let dir = d.path().join("2026-10-09_14-30_zoom");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("2026-10-09_14-30_zoom.mp4"), b"v").unwrap();
        std::fs::write(dir.join("2026-10-09_14-30_zoom.srt"), b"s").unwrap();
        let mut m = Meta::new(
            "id".into(),
            AppInfo {
                rule: "Zoom".into(),
                tag: "zoom".into(),
                ..Default::default()
            },
            "auto",
            started,
        );
        m.status = CallStatus::AwaitingName;
        m.files.video = Some("2026-10-09_14-30_zoom.mp4".into());
        m.files.subtitles = Some("2026-10-09_14-30_zoom.srt".into());
        m.save(&dir).unwrap();

        let new = apply_title(&dir, Some("Обсуждение CI"), "llm", &naming).unwrap();
        assert_eq!(
            new.file_name().unwrap(),
            "2026-10-09_14-30_zoom_обсуждение-ci"
        );
        assert!(new.join("2026-10-09_14-30_zoom_обсуждение-ci.mp4").exists());
        assert!(new.join("2026-10-09_14-30_zoom_обсуждение-ci.srt").exists());
        let m = Meta::load(&new).unwrap();
        assert_eq!(m.status, CallStatus::Done);
        assert_eq!(m.title.as_deref(), Some("Обсуждение CI"));
    }
}
