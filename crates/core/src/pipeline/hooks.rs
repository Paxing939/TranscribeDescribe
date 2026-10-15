//! After-processing hooks (F-120) and video retention (F-86).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::pipeline::meta::Meta;

/// POST `{meta, summary_md}` to `on_complete_webhook` and/or run `on_complete_command`.
/// Failures are logged, never fatal for the call.
pub async fn on_complete(cfg: &Config, call_dir: &Path) {
    let meta = match Meta::load(call_dir) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("хук: {e:#}");
            return;
        }
    };
    let summary_md = call_dir.join("summary.md");
    if let Some(url) = cfg
        .hooks
        .on_complete_webhook
        .as_deref()
        .filter(|u| !u.is_empty())
    {
        let body = serde_json::json!({
            "call_dir": call_dir.to_string_lossy(),
            "meta": meta,
            "summary_md": std::fs::read_to_string(&summary_md).ok(),
        });
        let r = reqwest::Client::new()
            .post(url)
            .timeout(Duration::from_secs(30))
            .json(&body)
            .send()
            .await;
        match r {
            Ok(resp) if resp.status().is_success() => tracing::info!("webhook: {}", resp.status()),
            Ok(resp) => tracing::warn!("webhook {url}: HTTP {}", resp.status()),
            Err(e) => tracing::warn!("webhook {url}: {e}"),
        }
    }
    if let Some(cmd) = cfg
        .hooks
        .on_complete_command
        .as_deref()
        .filter(|c| !c.is_empty())
    {
        let line = cmd
            .replace("{call_dir}", &call_dir.to_string_lossy())
            .replace("{summary_md}", &summary_md.to_string_lossy())
            .replace("{meta_json}", &call_dir.join("meta.json").to_string_lossy());
        if let Err(e) = run_command(&line).await {
            tracing::warn!("on_complete_command: {e:#}");
        }
    }
}

async fn run_command(line: &str) -> Result<()> {
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(line);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(line);
        c
    };
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = tokio::time::timeout(Duration::from_secs(300), cmd.output())
        .await
        .context("таймаут 5 мин")??;
    if !out.status.success() {
        anyhow::bail!(
            "код {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    tracing::info!("on_complete_command выполнен");
    Ok(())
}

/// Delete recordings older than `storage.delete_video_after_days`; transcripts and summaries stay (F-86).
pub fn apply_retention(cfg: &Config) -> Result<usize> {
    let Some(days) = cfg.storage.delete_video_after_days else {
        return Ok(0);
    };
    let cutoff = chrono::Local::now().fixed_offset() - chrono::Duration::days(days as i64);
    let mut n = 0;
    let Ok(entries) = std::fs::read_dir(cfg.calls_dir()) else {
        return Ok(0);
    };
    for e in entries.flatten() {
        let dir = e.path();
        let Ok(mut meta) = Meta::load(&dir) else {
            continue;
        };
        let Ok(started) = meta.started() else {
            continue;
        };
        if started >= cutoff {
            continue;
        }
        if let Some(v) = meta.files.video.take() {
            let p = dir.join(&v);
            if p.is_file() {
                std::fs::remove_file(&p).with_context(|| format!("удаление {}", p.display()))?;
                meta.warnings
                    .push(format!("запись удалена по сроку хранения ({days} дн.)"));
                meta.save(&dir)?;
                n += 1;
            } else {
                meta.files.video = Some(v);
            }
        }
    }
    Ok(n)
}
