//! Обработка на другой машине: пакет на обработку, сторона обработчика и импорт результатов
//! (§5.13, §7.6, F-140…F-144).
//!
//! Транспорт — любая синхронизация папки, поэтому: `meta.json` пакета пишется последним
//! (временный файл + rename), временные и конфликтные файлы синхронизации игнорируются,
//! в `meta.json` только относительные пути через `/`, а после выгрузки записывающая машина
//! пакет не меняет (только читает и удаляет после импорта).

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{Config, expand_home};
use crate::pipeline::meta::{
    Bookmark, CallStatus, META_FILE, Meta, PipelineInfo, write_json_atomic,
};

pub const PACKAGE_KIND: &str = "callscribe.package";
/// Подпапка `calls_dir`, куда собираются пакеты, пока `outbox_dir` недоступна (F-142).
pub const LOCAL_OUTBOX: &str = ".outbox";
/// Файлы результата, которые обработчик кладёт в пакет (§7.6).
pub const RESULT_TRANSCRIPT_JSON: &str = "transcript.json";
pub const RESULT_TRANSCRIPT_MD: &str = "transcript.md";
pub const RESULT_SUBTITLES: &str = "subtitles.srt";
pub const RESULT_SUMMARY: &str = "summary.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageStatus {
    Exported,
    Claimed,
    Processed,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PackageApp {
    pub rule: String,
    pub tag: String,
    #[serde(default)]
    pub window_title: Option<String>,
}

/// Параметры звонка для обработчика (F-141).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageCall {
    pub app: PackageApp,
    pub started_at: String,
    #[serde(default)]
    pub duration_sec: Option<f64>,
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
    #[serde(default)]
    pub title_hint: Option<String>,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub glossary: Vec<String>,
    pub cloud_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageFile {
    /// Относительный путь через `/`.
    pub path: String,
    /// `mic` | `desktop` | `mix`.
    pub role: String,
    pub size: u64,
    pub sha256: String,
}

/// Результат обработчика: название, список файлов и параметры STT/LLM (как `pipeline` в §7.3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PackageResult {
    #[serde(default)]
    pub suggested_title: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub pipeline: PipelineInfo,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageMeta {
    pub schema_version: u32,
    pub kind: String,
    pub id: String,
    pub status: PackageStatus,
    pub source_host: String,
    pub call: PackageCall,
    pub files: Vec<PackageFile>,
    #[serde(default)]
    pub claimed_by: Option<String>,
    #[serde(default)]
    pub claimed_at: Option<String>,
    #[serde(default)]
    pub result: Option<PackageResult>,
    #[serde(default)]
    pub errors: Vec<String>,
}

/// Пакет на диске вместе с прочитанным `meta.json`.
#[derive(Debug, Clone)]
pub struct PackageRef {
    pub dir: PathBuf,
    pub meta: PackageMeta,
}

impl PackageRef {
    /// Абсолютный путь к файлу пакета.
    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn save(&self) -> Result<()> {
        write_json_atomic(&self.dir.join(META_FILE), &self.meta)
    }
}

fn now_str() -> String {
    chrono::Local::now()
        .fixed_offset()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Временные и конфликтные файлы синхронизации (§11, п. 22).
pub fn is_sync_junk(name: &str) -> bool {
    name.contains(".sync-conflict-")
        || name.starts_with(".syncthing.")
        || name.starts_with("~syncthing~")
        || name.starts_with('.')
        || name.ends_with(".tmp")
        || name.ends_with(".partial")
        || name.contains(".partial.")
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)
        .with_context(|| format!("не удалось открыть {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

pub fn load_package(dir: &Path) -> Result<PackageMeta> {
    let p = dir.join(META_FILE);
    let text = std::fs::read_to_string(&p)
        .with_context(|| format!("не удалось прочитать {}", p.display()))?;
    let m: PackageMeta =
        serde_json::from_str(&text).with_context(|| format!("ошибка в {}", p.display()))?;
    if m.kind != PACKAGE_KIND {
        bail!("{}: это не пакет CallScribe (kind = {})", p.display(), m.kind);
    }
    Ok(m)
}

/// Все корректные пакеты папки; мусор синхронизации и папки без `meta.json` пропускаются.
fn list_packages(root: &Path) -> Vec<PackageRef> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let dir = e.path();
        if is_sync_junk(&name) || !dir.is_dir() {
            continue;
        }
        if let Ok(meta) = load_package(&dir) {
            out.push(PackageRef { dir, meta });
        }
    }
    out.sort_by(|a, b| {
        a.meta
            .call
            .started_at
            .cmp(&b.meta.call.started_at)
            .then_with(|| a.meta.id.cmp(&b.meta.id))
    });
    out
}

/// Размеры и SHA-256 файлов совпадают с `meta.json` (пакет докачан).
pub fn verify_files(dir: &Path, meta: &PackageMeta) -> Result<()> {
    for f in &meta.files {
        let p = dir.join(&f.path);
        let size = std::fs::metadata(&p)
            .with_context(|| format!("нет файла {}", f.path))?
            .len();
        if size != f.size {
            bail!("{}: размер {size} вместо {}", f.path, f.size);
        }
        if sha256_file(&p)? != f.sha256 {
            bail!("{}: SHA-256 не совпал", f.path);
        }
    }
    Ok(())
}

// ---------------- записывающая машина: выгрузка (F-141, F-142) ----------------

fn ensure_dir(p: &Path) -> bool {
    p.is_dir() || std::fs::create_dir_all(p).is_ok()
}

/// Папка для выгрузки: `outbox_dir`, а если она недоступна или не задана — `<calls_dir>/.outbox`.
/// Второй элемент — `true`, когда использована локальная.
pub fn resolve_outbox(cfg: &Config) -> (PathBuf, bool) {
    if let Some(o) = cfg.pipeline.remote.outbox_dir.as_deref().filter(|s| !s.trim().is_empty()) {
        let p = expand_home(o);
        if ensure_dir(&p) {
            return (p, false);
        }
        tracing::warn!("outbox_dir недоступна ({}) — пакет пойдёт в локальную очередь", p.display());
    }
    (cfg.calls_dir().join(LOCAL_OUTBOX), true)
}

/// Собрать пакет `<outbox>/<id>/` из папки звонка. `extract(видео, индекс_аудиопотока, файл)`
/// должен вырезать дорожку в 16 кГц моно в формате по расширению файла (flac/wav);
/// ffmpeg подставляет CLI, чтобы core не зависел от media.
/// `meta.json` пакета пишется последним; статус звонка → `exported`.
pub fn export_package<F>(cfg: &Config, call_dir: &Path, extract: F) -> Result<PathBuf>
where
    F: Fn(&Path, u32, &Path) -> Result<()>,
{
    let mut meta = Meta::load(call_dir)?;
    let video = call_dir.join(
        meta.files
            .video
            .clone()
            .context("в папке звонка нет файла записи")?,
    );
    if !video.is_file() {
        bail!("файл записи не найден: {}", video.display());
    }
    let ext = match cfg.pipeline.remote.audio_format.as_str() {
        "wav" => "wav",
        _ => "flac",
    };
    let s = &meta.audio_streams;
    let tracks: Vec<(&str, &str, u32)> = match (s.mic, s.desktop) {
        (Some(m), Some(d)) => vec![("me", "mic", m), ("remote", "desktop", d)],
        _ => vec![("mix", "mix", s.mix.unwrap_or(0))],
    };

    let (outbox, local) = resolve_outbox(cfg);
    std::fs::create_dir_all(&outbox)
        .with_context(|| format!("не удалось создать {}", outbox.display()))?;
    let pkg_dir = outbox.join(&meta.id);
    if pkg_dir.exists() {
        // Повторная выгрузка (например, после ошибки обработчика): старый пакет не нужен.
        std::fs::remove_dir_all(&pkg_dir)
            .with_context(|| format!("не удалось заменить {}", pkg_dir.display()))?;
    }
    std::fs::create_dir_all(&pkg_dir)?;

    let build = || -> Result<Vec<PackageFile>> {
        let mut files = Vec::new();
        for (stem, role, idx) in &tracks {
            let name = format!("{stem}.{ext}");
            let tmp = pkg_dir.join(format!("{stem}.partial.{ext}"));
            extract(&video, *idx, &tmp)?;
            let dest = pkg_dir.join(&name);
            std::fs::rename(&tmp, &dest)?;
            files.push(PackageFile {
                path: name,
                role: role.to_string(),
                size: std::fs::metadata(&dest)?.len(),
                sha256: sha256_file(&dest)?,
            });
        }
        Ok(files)
    };
    let files = match build() {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&pkg_dir);
            return Err(e);
        }
    };

    let pkg = PackageMeta {
        schema_version: 1,
        kind: PACKAGE_KIND.into(),
        id: meta.id.clone(),
        status: PackageStatus::Exported,
        source_host: meta.pipeline.host.clone(),
        call: PackageCall {
            app: PackageApp {
                rule: meta.app.rule.clone(),
                tag: meta.app.tag.clone(),
                window_title: meta.app.window_title.clone(),
            },
            started_at: meta.started_at.clone(),
            duration_sec: meta.duration_sec,
            bookmarks: meta.bookmarks.clone(),
            title_hint: (meta.title_source.as_deref() == Some("user"))
                .then(|| meta.title.clone())
                .flatten(),
            user_name: cfg.general.user_name.clone(),
            language: cfg.transcription.language.clone(),
            glossary: cfg.transcription.glossary.clone(),
            cloud_allowed: meta.cloud_allowed,
        },
        files,
        claimed_by: None,
        claimed_at: None,
        result: None,
        errors: vec![],
    };
    // Последним: по нему обработчик понимает, что пакет собран.
    write_json_atomic(&pkg_dir.join(META_FILE), &pkg)?;

    meta.status = CallStatus::Exported;
    meta.pipeline.mode = "remote".into();
    meta.save(call_dir)?;
    if local {
        tracing::info!("пакет собран в локальной очереди: {}", pkg_dir.display());
    }
    Ok(pkg_dir)
}

/// Перенести пакеты из `<calls_dir>/.outbox` в `outbox_dir`, когда она снова доступна (F-142).
/// Возвращает новые папки пакетов.
pub fn flush_local_outbox(cfg: &Config) -> Result<Vec<PathBuf>> {
    let local = cfg.calls_dir().join(LOCAL_OUTBOX);
    if !local.is_dir() {
        return Ok(vec![]);
    }
    let Some(target) = cfg
        .pipeline
        .remote
        .outbox_dir
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(expand_home)
    else {
        return Ok(vec![]);
    };
    if !ensure_dir(&target) {
        return Ok(vec![]);
    }
    let mut moved = Vec::new();
    for pkg in list_packages(&local) {
        // Недособранные и повреждённые пакеты не переносим.
        if verify_files(&pkg.dir, &pkg.meta).is_err() {
            tracing::warn!("пакет {} повреждён, не переношу", pkg.dir.display());
            continue;
        }
        let dest = target.join(&pkg.meta.id);
        match copy_package(&pkg, &dest) {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&pkg.dir);
                moved.push(dest);
            }
            Err(e) => {
                tracing::warn!("перенос пакета {}: {e:#}", pkg.meta.id);
                let _ = std::fs::remove_dir_all(&dest);
            }
        }
    }
    Ok(moved)
}

/// Копия пакета с проверкой; `meta.json` — последним.
fn copy_package(src: &PackageRef, dest: &Path) -> Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(&src.dir)?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == META_FILE || is_sync_junk(&name) || !e.path().is_file() {
            continue;
        }
        std::fs::copy(e.path(), dest.join(&name))?;
    }
    verify_files(dest, &src.meta)?;
    write_json_atomic(&dest.join(META_FILE), &src.meta)
}

// ---------------- машина-обработчик (F-143) ----------------

fn claimable(meta: &PackageMeta, claim_timeout_min: f64, now: DateTime<FixedOffset>) -> bool {
    match meta.status {
        PackageStatus::Exported => true,
        PackageStatus::Claimed => match meta
            .claimed_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        {
            Some(at) => (now - at).num_milliseconds() as f64 > claim_timeout_min * 60_000.0,
            None => true,
        },
        _ => false,
    }
}

/// Пакеты, которые можно брать в работу: `exported` или `claimed` дольше `claim_timeout_min`
/// и с совпадающими размерами и SHA-256 файлов (недокачанные пропускаются).
pub fn scan_inbox(
    dir: &Path,
    claim_timeout_min: f64,
    now: DateTime<FixedOffset>,
) -> Vec<PackageRef> {
    list_packages(dir)
        .into_iter()
        .filter(|p| claimable(&p.meta, claim_timeout_min, now))
        .filter(|p| match verify_files(&p.dir, &p.meta) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!("пакет {} ещё не готов: {e:#}", p.meta.id);
                false
            }
        })
        .collect()
}

/// Пометить пакет: `claimed_by` (хост), `claimed_at`.
pub fn claim(pkg: &mut PackageRef, host: &str) -> Result<()> {
    pkg.meta.status = PackageStatus::Claimed;
    pkg.meta.claimed_by = Some(host.to_string());
    pkg.meta.claimed_at = Some(now_str());
    pkg.save()
}

/// Результаты уже лежат в папке пакета: фиксируем их и ставим `processed`.
pub fn complete(pkg: &mut PackageRef, result: PackageResult) -> Result<()> {
    for f in &result.files {
        if !pkg.file(f).is_file() {
            bail!("в пакете нет файла результата {f}");
        }
    }
    pkg.meta.status = PackageStatus::Processed;
    pkg.meta.result = Some(result);
    pkg.save()
}

/// Обработка не удалась: `failed` и текст ошибки.
pub fn fail(pkg: &mut PackageRef, error: &str) -> Result<()> {
    pkg.meta.status = PackageStatus::Failed;
    pkg.meta.errors.push(error.to_string());
    pkg.save()
}

// ---------------- записывающая машина: импорт (F-144) ----------------

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportReport {
    /// Папки звонков, куда пришли результаты (статус `awaiting_name`) — для окна названия.
    pub imported: Vec<PathBuf>,
    /// Папки звонков, обработка которых на обработчике не удалась (статус `failed`).
    pub failed: Vec<PathBuf>,
}

/// Найти папку звонка по `id` в `calls_dir`.
pub fn find_call_dir(calls_dir: &Path, id: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(calls_dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let dir = e.path();
        if name.starts_with('.') || !dir.is_dir() {
            continue;
        }
        if Meta::load(&dir).is_ok_and(|m| m.id == id) {
            return Some(dir);
        }
    }
    None
}

fn copy_atomic(src: &Path, dest: &Path) -> Result<()> {
    let tmp = dest.with_extension("tmp");
    std::fs::copy(src, &tmp).with_context(|| format!("копирование {}", src.display()))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("не удалось записать {}", dest.display()))
}

/// Забрать результаты пакетов `processed` и ошибки пакетов `failed` из `outbox`.
/// Идемпотентно: звонок, который уже не в статусе `exported`, не трогается.
pub fn import_results(cfg: &Config, outbox: &Path) -> Result<ImportReport> {
    let calls = cfg.calls_dir();
    let mut report = ImportReport::default();
    for pkg in list_packages(outbox) {
        if !matches!(
            pkg.meta.status,
            PackageStatus::Processed | PackageStatus::Failed
        ) {
            continue;
        }
        let Some(call_dir) = find_call_dir(&calls, &pkg.meta.id) else {
            tracing::warn!("для пакета {} нет папки звонка в {}", pkg.meta.id, calls.display());
            continue;
        };
        let res = match pkg.meta.status {
            PackageStatus::Processed => import_processed(cfg, &pkg, &call_dir)
                .map(|imported| imported.then(|| report.imported.push(call_dir.clone()))),
            _ => import_failed(&pkg, &call_dir)
                .map(|changed| changed.then(|| report.failed.push(call_dir.clone()))),
        };
        if let Err(e) = res {
            tracing::warn!("импорт пакета {}: {e:#}", pkg.meta.id);
        }
    }
    Ok(report)
}

/// `true`, если результаты импортированы сейчас (а не раньше).
fn import_processed(cfg: &Config, pkg: &PackageRef, call_dir: &Path) -> Result<bool> {
    let mut meta = Meta::load(call_dir)?;
    let cleanup = cfg.pipeline.remote.cleanup_after_import;
    if meta.status != CallStatus::Exported {
        // Уже импортирован раньше (пакет остался из-за cleanup_after_import = false).
        if cleanup {
            let _ = std::fs::remove_dir_all(&pkg.dir);
        }
        return Ok(false);
    }
    if !pkg.file(RESULT_TRANSCRIPT_JSON).is_file() {
        bail!("в пакете нет {RESULT_TRANSCRIPT_JSON}");
    }
    for name in [RESULT_TRANSCRIPT_JSON, RESULT_TRANSCRIPT_MD, RESULT_SUMMARY] {
        if pkg.file(name).is_file() {
            copy_atomic(&pkg.file(name), &call_dir.join(name))?;
        }
    }
    let stem = meta
        .files
        .video
        .as_deref()
        .and_then(|v| Path::new(v).file_stem())
        .or_else(|| call_dir.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "subtitles".into());
    let srt_name = format!("{stem}.srt");
    if pkg.file(RESULT_SUBTITLES).is_file() {
        copy_atomic(&pkg.file(RESULT_SUBTITLES), &call_dir.join(&srt_name))?;
        meta.files.subtitles = Some(srt_name);
    }
    let has = |n: &str| call_dir.join(n).is_file();
    if has(RESULT_TRANSCRIPT_MD) {
        meta.files.transcript_md = Some(RESULT_TRANSCRIPT_MD.into());
    }
    meta.files.transcript_json = Some(RESULT_TRANSCRIPT_JSON.into());
    if has(RESULT_SUMMARY) {
        meta.files.summary_md = Some(RESULT_SUMMARY.into());
    }

    let result = pkg.meta.result.clone().unwrap_or_default();
    let user_title = (meta.title_source.as_deref() == Some("user"))
        .then(|| meta.title.clone())
        .flatten();
    meta.suggested_title = user_title.or(result.suggested_title.filter(|t| !t.trim().is_empty()));
    meta.pipeline = result.pipeline;
    meta.pipeline.mode = "remote".into();
    for w in result.warnings {
        if !meta.warnings.contains(&w) {
            meta.warnings.push(w);
        }
    }
    meta.status = CallStatus::AwaitingName;
    meta.save(call_dir)?;
    if cleanup {
        std::fs::remove_dir_all(&pkg.dir)
            .with_context(|| format!("не удалось удалить пакет {}", pkg.dir.display()))?;
    }
    Ok(true)
}

/// `true`, если ошибка записана в звонок сейчас (а не раньше).
fn import_failed(pkg: &PackageRef, call_dir: &Path) -> Result<bool> {
    let mut meta = Meta::load(call_dir)?;
    if meta.status != CallStatus::Exported {
        return Ok(false);
    }
    let host = pkg.meta.claimed_by.as_deref().unwrap_or("?");
    if pkg.meta.errors.is_empty() {
        meta.errors.push(format!("ошибка обработки на {host}"));
    }
    for e in &pkg.meta.errors {
        meta.errors.push(format!("ошибка на {host}: {e}"));
    }
    meta.status = CallStatus::Failed;
    meta.save(call_dir)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::meta::AppInfo;

    struct Env {
        _d: tempfile::TempDir,
        cfg: Config,
        outbox: PathBuf,
        call: PathBuf,
    }

    fn env(outbox_set: bool) -> Env {
        let d = tempfile::tempdir().unwrap();
        let calls = d.path().join("calls");
        let outbox = d.path().join("outbox");
        let mut cfg = Config::default();
        cfg.general.calls_dir = calls.to_string_lossy().to_string();
        if outbox_set {
            cfg.pipeline.remote.outbox_dir = Some(outbox.to_string_lossy().to_string());
        }
        let call = calls.join("2026-10-09_14-30_zoom");
        std::fs::create_dir_all(&call).unwrap();
        std::fs::write(call.join("2026-10-09_14-30_zoom.mp4"), b"video").unwrap();
        let started = DateTime::parse_from_rfc3339("2026-10-09T14:30:12+03:00").unwrap();
        let mut m = Meta::new(
            "id-1".into(),
            AppInfo {
                rule: "Zoom".into(),
                tag: "zoom".into(),
                ..Default::default()
            },
            "auto",
            started,
        );
        m.status = CallStatus::Queued;
        m.files.video = Some("2026-10-09_14-30_zoom.mp4".into());
        m.audio_streams.mic = Some(1);
        m.audio_streams.desktop = Some(2);
        m.audio_streams.mix = Some(0);
        m.duration_sec = Some(12.0);
        m.save(&call).unwrap();
        Env {
            _d: d,
            cfg,
            outbox,
            call,
        }
    }

    fn fake_extract(_v: &Path, idx: u32, out: &Path) -> Result<()> {
        std::fs::write(out, format!("audio-{idx}").repeat(100))?;
        Ok(())
    }

    #[test]
    fn export_roundtrip() {
        let e = env(true);
        let pkg_dir = export_package(&e.cfg, &e.call, fake_extract).unwrap();
        assert_eq!(pkg_dir, e.outbox.join("id-1"));
        let m = load_package(&pkg_dir).unwrap();
        assert_eq!(m.status, PackageStatus::Exported);
        assert_eq!(m.kind, PACKAGE_KIND);
        let names: Vec<_> = m.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, ["me.flac", "remote.flac"]);
        assert_eq!(m.files[0].role, "mic");
        assert!(!pkg_dir.join("meta.json.tmp").exists());
        assert!(!pkg_dir.join("me.partial.flac").exists());
        assert_eq!(Meta::load(&e.call).unwrap().status, CallStatus::Exported);

        let now = chrono::Local::now().fixed_offset();
        let mut found = scan_inbox(&e.outbox, 120.0, now);
        assert_eq!(found.len(), 1);
        claim(&mut found[0], "gpu-box").unwrap();
        assert_eq!(
            load_package(&pkg_dir).unwrap().claimed_by.as_deref(),
            Some("gpu-box")
        );
        // Захваченный свежий пакет повторно не берётся.
        assert!(scan_inbox(&e.outbox, 120.0, now).is_empty());

        std::fs::write(pkg_dir.join("transcript.json"), "{}").unwrap();
        std::fs::write(pkg_dir.join("transcript.md"), "# t").unwrap();
        std::fs::write(pkg_dir.join("subtitles.srt"), "1").unwrap();
        std::fs::write(pkg_dir.join("summary.md"), "# s").unwrap();
        complete(
            &mut found[0],
            PackageResult {
                suggested_title: Some("Обсуждение CI".into()),
                files: vec![
                    "transcript.json".into(),
                    "transcript.md".into(),
                    "subtitles.srt".into(),
                    "summary.md".into(),
                ],
                pipeline: PipelineInfo {
                    mode: "remote".into(),
                    host: "gpu-box".into(),
                    ..Default::default()
                },
                warnings: vec![],
            },
        )
        .unwrap();

        let rep = import_results(&e.cfg, &e.outbox).unwrap();
        assert_eq!(rep.imported, vec![e.call.clone()]);
        assert!(rep.failed.is_empty());
        for f in [
            "transcript.json",
            "transcript.md",
            "summary.md",
            "2026-10-09_14-30_zoom.srt",
        ] {
            assert!(e.call.join(f).is_file(), "{f}");
        }
        let m = Meta::load(&e.call).unwrap();
        assert_eq!(m.status, CallStatus::AwaitingName);
        assert_eq!(m.suggested_title.as_deref(), Some("Обсуждение CI"));
        assert_eq!(m.pipeline.host, "gpu-box");
        assert_eq!(m.files.subtitles.as_deref(), Some("2026-10-09_14-30_zoom.srt"));
        assert!(!pkg_dir.exists(), "cleanup_after_import");
    }

    #[test]
    fn import_keeps_package_and_is_idempotent() {
        let mut e = env(true);
        e.cfg.pipeline.remote.cleanup_after_import = false;
        let pkg_dir = export_package(&e.cfg, &e.call, fake_extract).unwrap();
        let mut p = scan_inbox(&e.outbox, 120.0, chrono::Local::now().fixed_offset()).remove(0);
        claim(&mut p, "h").unwrap();
        std::fs::write(pkg_dir.join("transcript.json"), "{}").unwrap();
        complete(&mut p, PackageResult::default()).unwrap();
        assert_eq!(import_results(&e.cfg, &e.outbox).unwrap().imported.len(), 1);
        assert!(pkg_dir.exists());
        assert!(import_results(&e.cfg, &e.outbox).unwrap().imported.is_empty());
    }

    #[test]
    fn failed_package_marks_call() {
        let e = env(true);
        let pkg_dir = export_package(&e.cfg, &e.call, fake_extract).unwrap();
        let mut p = scan_inbox(&e.outbox, 120.0, chrono::Local::now().fixed_offset()).remove(0);
        claim(&mut p, "gpu-box").unwrap();
        fail(&mut p, "нет модели").unwrap();
        let rep = import_results(&e.cfg, &e.outbox).unwrap();
        assert_eq!(rep.failed, vec![e.call.clone()]);
        assert!(rep.imported.is_empty());
        let m = Meta::load(&e.call).unwrap();
        assert_eq!(m.status, CallStatus::Failed);
        assert!(m.errors[0].contains("gpu-box") && m.errors[0].contains("нет модели"));
        assert!(pkg_dir.exists());
        // Повторный импорт ошибку не дублирует.
        assert!(import_results(&e.cfg, &e.outbox).unwrap().failed.is_empty());
        assert_eq!(Meta::load(&e.call).unwrap().errors.len(), 1);
        // Повторная выгрузка заменяет пакет.
        export_package(&e.cfg, &e.call, fake_extract).unwrap();
        assert_eq!(load_package(&pkg_dir).unwrap().status, PackageStatus::Exported);
    }

    #[test]
    fn sha_and_size_mismatch_skipped() {
        let e = env(true);
        let pkg_dir = export_package(&e.cfg, &e.call, fake_extract).unwrap();
        let now = chrono::Local::now().fixed_offset();
        assert_eq!(scan_inbox(&e.outbox, 120.0, now).len(), 1);
        // Тот же размер, другое содержимое.
        let p = pkg_dir.join("me.flac");
        let mut data = std::fs::read(&p).unwrap();
        data[0] ^= 0xff;
        std::fs::write(&p, &data).unwrap();
        assert!(scan_inbox(&e.outbox, 120.0, now).is_empty());
        // Недокачан.
        data.truncate(10);
        std::fs::write(&p, &data).unwrap();
        assert!(scan_inbox(&e.outbox, 120.0, now).is_empty());
    }

    #[test]
    fn claim_timeout() {
        let e = env(true);
        export_package(&e.cfg, &e.call, fake_extract).unwrap();
        let now = chrono::Local::now().fixed_offset();
        let mut p = scan_inbox(&e.outbox, 120.0, now).remove(0);
        claim(&mut p, "h").unwrap();
        assert!(scan_inbox(&e.outbox, 120.0, now + chrono::Duration::minutes(60)).is_empty());
        assert_eq!(
            scan_inbox(&e.outbox, 120.0, now + chrono::Duration::minutes(121)).len(),
            1
        );
    }

    #[test]
    fn junk_and_conflicts_ignored() {
        let e = env(true);
        let pkg_dir = export_package(&e.cfg, &e.call, fake_extract).unwrap();
        let conflict = e.outbox.join("id-1.sync-conflict-20261009-ABC");
        std::fs::create_dir_all(&conflict).unwrap();
        std::fs::copy(pkg_dir.join("meta.json"), conflict.join("meta.json")).unwrap();
        std::fs::create_dir_all(e.outbox.join(".stfolder")).unwrap();
        std::fs::write(pkg_dir.join("meta.sync-conflict-1.json"), "{broken").unwrap();
        std::fs::write(pkg_dir.join("me.flac.tmp"), "x").unwrap();
        let found = scan_inbox(&e.outbox, 120.0, chrono::Local::now().fixed_offset());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir, pkg_dir);
    }

    #[test]
    fn local_outbox_fallback_and_flush() {
        let mut e = env(false);
        // outbox_dir не задана → локальная очередь.
        let pkg = export_package(&e.cfg, &e.call, fake_extract).unwrap();
        assert_eq!(pkg, e.cfg.calls_dir().join(LOCAL_OUTBOX).join("id-1"));
        assert_eq!(Meta::load(&e.call).unwrap().status, CallStatus::Exported);
        // Папка так и не появилась.
        assert!(flush_local_outbox(&e.cfg).unwrap().is_empty());
        // Появилась.
        e.cfg.pipeline.remote.outbox_dir = Some(e.outbox.to_string_lossy().to_string());
        let moved = flush_local_outbox(&e.cfg).unwrap();
        assert_eq!(moved, vec![e.outbox.join("id-1")]);
        assert!(!pkg.exists());
        let found = scan_inbox(&e.outbox, 120.0, chrono::Local::now().fixed_offset());
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn single_track_makes_mix() {
        let e = env(true);
        let mut m = Meta::load(&e.call).unwrap();
        m.audio_streams.mic = None;
        m.audio_streams.desktop = None;
        m.save(&e.call).unwrap();
        let p = export_package(&e.cfg, &e.call, fake_extract).unwrap();
        let pm = load_package(&p).unwrap();
        assert_eq!(pm.files.len(), 1);
        assert_eq!(pm.files[0].path, "mix.flac");
        assert_eq!(pm.files[0].role, "mix");
    }
}
