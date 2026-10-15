//! Обработка на другой машине (§5.13, F-140…F-144): `export`, `worker --inbox`, `import`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use callscribe_core::config::expand_home;
use callscribe_core::pipeline::meta::{AppInfo, CallStatus, Meta};
use callscribe_core::pipeline::remote::{
    self, PackageRef, PackageResult, RESULT_SUBTITLES, RESULT_SUMMARY, RESULT_TRANSCRIPT_JSON,
    RESULT_TRANSCRIPT_MD,
};
use callscribe_core::storage::{Job, JobKind};
use callscribe_core::stt::Speaker;
use callscribe_media::Ffmpeg;

use crate::worker::{self, Worker};

/// Дорожка звонка → 16 кГц моно; формат — по расширению `out` (flac или wav).
pub fn extract_flac(ff: &Ffmpeg, video: &Path, audio_index: u32, out: &Path) -> Result<()> {
    let map = format!("0:a:{audio_index}");
    ff.raw(&[
        "-i",
        &video.to_string_lossy(),
        "-map",
        &map,
        "-ac",
        "1",
        "-ar",
        "16000",
        &out.to_string_lossy(),
    ])
}

/// `callscribe export <call_dir>`: выгрузить уже записанный звонок пакетом (F-130).
pub fn export(w: &Worker, call_dir: &Path) -> Result<()> {
    let ff = w.ffmpeg()?;
    let meta = Meta::load(call_dir)?;
    if meta.files.video.is_none() {
        bail!(
            "в {} нет файла записи (сначала `callscribe process`, чтобы сделать ремукс)",
            call_dir.display()
        );
    }
    let moved = remote::flush_local_outbox(&w.cfg)?;
    if !moved.is_empty() {
        println!("перенесено пакетов из локальной очереди: {}", moved.len());
    }
    let pkg = remote::export_package(&w.cfg, call_dir, |v, i, o| extract_flac(&ff, v, i, o))?;
    w.index(call_dir)?;
    println!("пакет собран: {}", pkg.display());
    Ok(())
}

/// `callscribe import`: один проход `import_results` по `outbox_dir` (F-144).
pub fn import(w: &Worker) -> Result<()> {
    let outbox = w
        .cfg
        .pipeline
        .remote
        .outbox_dir
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(expand_home)
        .context("не задана pipeline.remote.outbox_dir")?;
    let moved = remote::flush_local_outbox(&w.cfg)?;
    if !moved.is_empty() {
        println!("перенесено пакетов из локальной очереди: {}", moved.len());
    }
    let rep = remote::import_results(&w.cfg, &outbox)?;
    for d in rep.imported.iter().chain(&rep.failed) {
        w.index(d)?;
    }
    for d in &rep.imported {
        println!("результаты получены: {}", d.display());
    }
    for d in &rep.failed {
        println!("ошибка обработки: {}", d.display());
    }
    if rep.imported.is_empty() && rep.failed.is_empty() {
        println!("новых результатов нет");
    }
    Ok(())
}

/// `callscribe worker --inbox <dir> [--once]` (F-143): брать готовые пакеты и обрабатывать их по очереди.
pub fn run_worker(w: &Worker, inbox: &Path, once: bool) -> Result<()> {
    if !inbox.is_dir() {
        bail!("папка пакетов не найдена: {}", inbox.display());
    }
    let host = worker::host();
    let poll = Duration::from_secs_f64(w.cfg.worker.poll_interval_sec.max(1.0));
    tracing::info!("обработчик {host}: слежу за {}", inbox.display());
    loop {
        let now = chrono::Local::now().fixed_offset();
        let ready = remote::scan_inbox(inbox, w.cfg.worker.claim_timeout_min, now);
        for mut pkg in ready {
            let id = pkg.meta.id.clone();
            if let Err(e) = remote::claim(&mut pkg, &host) {
                tracing::warn!("пакет {id}: не удалось занять: {e:#}");
                continue;
            }
            tracing::info!("пакет {id}: обработка");
            match process_package(w, &pkg) {
                Ok(result) => match remote::complete(&mut pkg, result) {
                    Ok(()) => tracing::info!("пакет {id}: готово"),
                    Err(e) => tracing::error!("пакет {id}: {e:#}"),
                },
                Err(e) => {
                    let msg = format!("{e:#}");
                    tracing::error!("пакет {id} упал: {msg}");
                    if let Err(e) = remote::fail(&mut pkg, &msg) {
                        tracing::error!("пакет {id}: не удалось записать ошибку: {e:#}");
                    }
                }
            }
        }
        if once {
            return Ok(());
        }
        std::thread::sleep(poll);
    }
}

/// Тот же пайплайн, что у локального worker (F-54…F-59, F-70…F-77), на дорожках из пакета.
/// Рабочие файлы лежат во временной папке; в пакет копируются только результаты.
fn process_package(w: &Worker, pkg: &PackageRef) -> Result<PackageResult> {
    let work = w.paths.tmp_dir().join(format!("remote-{}", pkg.meta.id));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let res = process_in(w, pkg, &work);
    if res.is_err() || !w.cfg.pipeline.keep_extracted_audio {
        let _ = std::fs::remove_dir_all(&work);
    }
    res
}

fn process_in(w: &Worker, pkg: &PackageRef, work: &Path) -> Result<PackageResult> {
    let call = &pkg.meta.call;
    let ff = w.ffmpeg()?;

    // FLAC/WAV из пакета → WAV 16 кГц для STT.
    let mut tracks: Vec<(Speaker, PathBuf)> = Vec::new();
    for f in &pkg.meta.files {
        let speaker = match f.role.as_str() {
            "mic" => Speaker::Me,
            "desktop" => Speaker::Remote,
            _ => Speaker::Mixed,
        };
        let wav = work.join(format!("{}.wav", f.path.replace('/', "_")));
        ff.extract_track(&pkg.file(&f.path), 0, &wav)?;
        tracks.push((speaker, wav));
    }
    if tracks.is_empty() {
        bail!("в пакете нет дорожек");
    }

    // Звонок для общей части пайплайна: meta.json живёт только в рабочей папке.
    let started = chrono::DateTime::parse_from_rfc3339(&call.started_at)
        .context("call.started_at в пакете")?;
    let mut meta = Meta::new(
        pkg.meta.id.clone(),
        AppInfo {
            rule: call.app.rule.clone(),
            tag: call.app.tag.clone(),
            process: None,
            window_title: call.app.window_title.clone(),
        },
        "remote",
        started,
    );
    meta.status = CallStatus::Transcribing;
    meta.duration_sec = call.duration_sec;
    meta.bookmarks = call.bookmarks.clone();
    meta.cloud_allowed = call.cloud_allowed; // F-71
    if let Some(t) = call.title_hint.clone().filter(|t| !t.trim().is_empty()) {
        meta.title = Some(t);
        meta.title_source = Some("user".into());
    }
    meta.save(work)?;

    // Параметры звонка — из пакета, остальное (модели, GPU, LLM) — из config.toml этой машины.
    let language = if call.language.trim().is_empty() {
        w.cfg.transcription.language.clone()
    } else {
        call.language.clone()
    };
    let mut glossary = w.cfg.transcription.glossary.clone();
    for g in &call.glossary {
        if !glossary.contains(g) {
            glossary.push(g.clone());
        }
    }
    let user_name = if call.user_name.trim().is_empty() {
        w.cfg.general.user_name.clone()
    } else {
        call.user_name.clone()
    };
    let opts = w.transcribe_opts(&language, &glossary);
    let title = meta
        .title
        .clone()
        .unwrap_or_else(|| format!("{} {}", call.app.rule, worker::local_str(&meta)));

    // Задания в очереди нет: прогресс пишется в никуда, модели при необходимости скачиваются.
    let job = Job {
        id: -1,
        call_dir: work.to_path_buf(),
        kind: JobKind::Process,
        status: "running".into(),
        attempts: 1,
        step: None,
        progress: 0.0,
        error: None,
        options: serde_json::Value::Null,
    };
    let (info, utts) =
        w.transcribe_tracks(&job, &mut meta, work, &title, &tracks, RESULT_SUBTITLES, &opts)?;
    meta.pipeline.mode = "remote".into();
    meta.pipeline.host = worker::host();
    meta.pipeline.stt = Some(info);

    // Саммари. Сбой LLM не роняет пакет: расшифровка важнее, саммари можно сделать позже (`resummarize`).
    let mut suggested = None;
    if utts.is_empty() {
        meta.warnings
            .push("пустой транскрипт — саммари не делалось".into());
    } else {
        match w.pick_llm(&meta) {
            Ok(None) => {}
            Ok(Some(llm)) => {
                match w.run_summary_with(&*llm, &mut meta, work, &utts, &user_name, &glossary) {
                    Ok(t) => suggested = t.filter(|t| !t.is_empty()),
                    Err(e) => {
                        tracing::warn!("саммари: {e:#}");
                        meta.warnings.push(format!("саммари не создано: {e:#}"));
                    }
                }
            }
            Err(e) => {
                tracing::warn!("LLM: {e:#}");
                meta.warnings.push(format!("саммари не создано: {e:#}"));
            }
        }
    }
    meta.pipeline.processed_at = Some(
        chrono::Local::now()
            .fixed_offset()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
    );

    // Результаты — в пакет; transcript.json последним.
    let mut files = Vec::new();
    for name in [
        RESULT_TRANSCRIPT_MD,
        RESULT_SUBTITLES,
        RESULT_SUMMARY,
        RESULT_TRANSCRIPT_JSON,
    ] {
        let src = work.join(name);
        if src.is_file() {
            let dest = pkg.file(name);
            let tmp = dest.with_extension("tmp");
            std::fs::copy(&src, &tmp)?;
            std::fs::rename(&tmp, &dest)?;
            files.push(name.to_string());
        }
    }
    Ok(PackageResult {
        suggested_title: suggested,
        files,
        pipeline: meta.pipeline,
        warnings: meta.warnings,
    })
}
