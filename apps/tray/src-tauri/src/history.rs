//! History window (F-90): list, full-text search, actions.

use std::path::{Path, PathBuf};

use callscribe_core::pipeline::finalize::apply_title;
use callscribe_core::pipeline::meta::Meta;
use callscribe_core::storage::{Db, JobKind};
use callscribe_core::{AppPaths, Config};
use serde::Serialize;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    id: String,
    call_dir: String,
    status: String,
    title: Option<String>,
    app: String,
    started_at: String,
    duration_sec: Option<f64>,
    has_video: bool,
    has_summary: bool,
}

fn db() -> Result<Db, String> {
    Db::open(&AppPaths::new().db_file()).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn history_list(query: String) -> Result<Vec<Row>, String> {
    let db = db()?;
    let mut rows = db.recent_calls(1000).map_err(|e| format!("{e:#}"))?;
    if !query.is_empty() {
        // FTS5 query: every word as a prefix term.
        let fts: String = query
            .split_whitespace()
            .map(|w| format!("\"{}\"*", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");
        let ids = db.search(&fts, 1000).map_err(|e| format!("{e:#}"))?;
        rows.retain(|r| ids.contains(&r.id));
    }
    Ok(rows
        .into_iter()
        .map(|r| {
            let dir = PathBuf::from(&r.call_dir);
            let meta = Meta::load(&dir).ok();
            let has_video = meta.as_ref().and_then(|m| m.files.video.as_ref()).is_some_and(|v| dir.join(v).is_file());
            Row {
                has_summary: dir.join("summary.md").is_file(),
                has_video,
                id: r.id,
                call_dir: r.call_dir,
                status: meta.as_ref().map(|m| serde_json::to_value(m.status).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()).unwrap_or(r.status),
                title: meta.as_ref().and_then(|m| m.title.clone()).or(r.title),
                app: r.app,
                started_at: r.started_at,
                duration_sec: r.duration_sec,
            }
        })
        .collect())
}

fn open(p: &Path) -> Result<(), String> {
    tauri_plugin_opener::open_path(p, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn history_action(id: String, action: String, value: Option<String>) -> Result<(), String> {
    let db = db()?;
    let row = db
        .recent_calls(1000)
        .map_err(|e| format!("{e:#}"))?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or("звонок не найден")?;
    let dir = PathBuf::from(&row.call_dir);
    let meta = Meta::load(&dir).map_err(|e| format!("{e:#}"))?;
    match action.as_str() {
        "open_video" => open(&dir.join(meta.files.video.as_deref().ok_or("нет записи")?)),
        "open_summary" => open(&dir.join("summary.md")),
        "open_folder" => open(&dir),
        "rename" => {
            let cfg = Config::load_default_location(&AppPaths::new()).map_err(|e| format!("{e:#}"))?;
            let t = value.as_deref().map(str::trim).filter(|t| !t.is_empty());
            let new = apply_title(&dir, t, "user", &cfg.naming).map_err(|e| format!("{e:#}"))?;
            let mut row = row;
            row.call_dir = new.to_string_lossy().to_string();
            row.title = t.map(str::to_string);
            db.upsert_call(&row, std::fs::read_to_string(new.join("transcript.md")).ok().as_deref(), std::fs::read_to_string(new.join("summary.md")).ok().as_deref())
                .map_err(|e| format!("{e:#}"))
        }
        "delete" => {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            db.delete_call(&id).map_err(|e| format!("{e:#}"))
        }
        "retranscribe" => {
            for f in ["transcript.json", "transcript.md", "summary.md"] {
                let _ = std::fs::remove_file(dir.join(f));
            }
            db.enqueue(&dir, JobKind::Process, &serde_json::json!({"auto_name": true})).map(|_| ()).map_err(|e| format!("{e:#}"))
        }
        "resummarize" => {
            let _ = std::fs::remove_file(dir.join("summary.md"));
            db.enqueue(&dir, JobKind::Summarize, &serde_json::json!({"auto_name": true})).map(|_| ()).map_err(|e| format!("{e:#}"))?;
            db.retry_now().map_err(|e| format!("{e:#}"))
        }
        other => Err(format!("неизвестное действие {other}")),
    }
}

/// Open (or focus) the history window.
pub fn open_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("history") {
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let r = WebviewWindowBuilder::new(app, "history", WebviewUrl::App("index.html#history".into()))
        .title("CallScribe — история звонков")
        .inner_size(980.0, 620.0)
        .min_inner_size(640.0, 360.0)
        .center()
        .build();
    if let Err(e) = r {
        tracing::error!("окно истории: {e}");
    }
}
