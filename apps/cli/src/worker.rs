//! Job queue worker (§4.1, F-50…F-59, F-70…F-77, F-80…F-84).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use callscribe_core::config::{DeleteShort, Device, LlmProviderKind, PipelineMode};
use callscribe_core::llm::{self, LlmError, LlmProvider};
use callscribe_core::pipeline::export::{self, Header, SummaryHeader};
use callscribe_core::pipeline::finalize::apply_title;
use callscribe_core::pipeline::meta::{AudioStreams, CallStatus, LlmInfo, Meta};
use callscribe_core::pipeline::postprocess::{PostProcessor, Segment, Utterance};
use callscribe_core::pipeline::summarize::{self, Prompts, SummaryResult};
use callscribe_core::storage::{CallRow, Db, Job, JobKind, WorkerLock};
use callscribe_core::stt::{EngineInfo, Speaker, TranscribeOptions, Transcriber};
use callscribe_core::{AppPaths, Config, models};
use callscribe_media::Ffmpeg;
use serde::{Deserialize, Serialize};

use crate::secrets::Keyring;

/// Per-job options stored in the queue as JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobOptions {
    /// Apply the title right away (CLI). The tray sets `false` and asks the user (F-44).
    #[serde(default)]
    pub auto_name: bool,
    /// Keep the source file (copy instead of move) when ingesting.
    #[serde(default)]
    pub keep_source: bool,
}

pub struct Worker {
    pub paths: AppPaths,
    pub cfg: Config,
    pub db: Db,
    rt: tokio::runtime::Runtime,
}

enum JobOutcome {
    Done,
    Retry { error: String, after: Duration },
}

pub(crate) fn local_str(meta: &Meta) -> String {
    meta.started()
        .map(|t| t.format("%d.%m.%Y %H:%M").to_string())
        .unwrap_or_default()
}

pub(crate) fn host() -> String {
    callscribe_platform::computer_name()
}

impl Worker {
    pub fn new(paths: AppPaths, cfg: Config) -> Result<Worker> {
        paths.ensure()?;
        let db = Db::open(&paths.db_file())?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        Ok(Worker { paths, cfg, db, rt })
    }

    /// Process the queue until it is empty; returns immediately if another process holds `worker.lock` (§4.1).
    pub fn run_queue(&self) -> Result<()> {
        loop {
            let Some(lock) = WorkerLock::try_acquire(&self.paths.worker_lock())? else {
                tracing::info!("очередь уже обрабатывает другой процесс");
                return Ok(());
            };
            // F-86: drop old recordings, keep transcripts and summaries.
            match callscribe_core::pipeline::hooks::apply_retention(&self.cfg) {
                Ok(0) => {}
                Ok(n) => tracing::info!("удалено записей по сроку хранения: {n}"),
                Err(e) => tracing::warn!("срок хранения: {e:#}"),
            }
            let stale = self.db.recover_stale()?;
            if stale > 0 {
                tracing::warn!("{stale} заданий остались от упавшего worker — продолжаю их");
            }
            while let Some(job) = self.db.next_due()? {
                self.wait_while_recording();
                self.db.mark_running(job.id)?;
                tracing::info!(
                    "задание {} ({:?}): {}",
                    job.id,
                    job.kind,
                    job.call_dir.display()
                );
                match self.run_job(&job) {
                    Ok(JobOutcome::Done) => {
                        self.db.mark_done(job.id)?;
                        // F-120: webhook / command after processing.
                        // В режиме remote звонок только выгружен: результатов для хука ещё нет.
                        if let Some(dir) = self.current_dir(job.id)
                            && Meta::load(&dir).is_ok_and(|m| m.status != CallStatus::Exported)
                        {
                            self.rt.block_on(callscribe_core::pipeline::hooks::on_complete(&self.cfg, &dir));
                        }
                    }
                    Ok(JobOutcome::Retry { error, after }) => {
                        tracing::warn!(
                            "задание {}: повтор через {} мин: {error}",
                            job.id,
                            after.as_secs() / 60
                        );
                        self.db.mark_retry(job.id, &error, after)?;
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        tracing::error!("задание {} упало: {msg}", job.id);
                        self.db.mark_failed(job.id, &msg)?;
                        if let Some(dir) = self.current_dir(job.id)
                            && let Ok(mut m) = Meta::load(&dir)
                        {
                            m.status = CallStatus::Failed;
                            m.errors.push(msg);
                            let _ = m.save(&dir);
                            let _ = self.index(&dir);
                        }
                    }
                }
            }
            drop(lock);
            // Re-check after releasing the lock: a job queued in the last moment must not hang (§4.1).
            if self.db.pending_count()? == 0 {
                return Ok(());
            }
        }
    }

    fn current_dir(&self, job_id: i64) -> Option<PathBuf> {
        let j = self.db.job(job_id).ok()??;
        j.call_dir.join("meta.json").is_file().then_some(j.call_dir)
    }

    /// F-51: pause between jobs and steps while the tray records.
    fn wait_while_recording(&self) {
        if !self.cfg.pipeline.defer_during_recording {
            return;
        }
        let mut logged = false;
        while self.db.recording_active() {
            if !logged {
                tracing::info!("идёт запись — очередь на паузе");
                logged = true;
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    fn progress(&self, job: &Job, step: &str, pct: f64) {
        let _ = self.db.set_progress(job.id, step, pct);
    }

    fn run_job(&self, job: &Job) -> Result<JobOutcome> {
        let opts: JobOptions = serde_json::from_value(job.options.clone()).unwrap_or_default();
        let dir = job.call_dir.clone();
        if !dir.join("meta.json").is_file() {
            bail!("нет meta.json в {}", dir.display());
        }
        match job.kind {
            JobKind::Process => {
                let dir = match self.ingest(job, &dir, &opts)? {
                    Some(d) => d,
                    None => return Ok(JobOutcome::Done), // short recording deleted (F-84)
                };
                // F-140: режим remote — вместо расшифровки выгружаем пакет на обработчик.
                if self.cfg.pipeline.mode == PipelineMode::Remote {
                    return self.export_remote(job, &dir);
                }
                self.wait_while_recording();
                let utts = self.transcribe(job, &dir)?;
                self.wait_while_recording();
                self.summarize_and_name(job, &dir, &utts, &opts)
            }
            JobKind::Summarize => {
                let utts = load_utterances(&dir)?;
                self.summarize_and_name(job, &dir, &utts, &opts)
            }
        }
    }

    /// F-141: после ремукса собрать пакет в `outbox_dir` (или в `.outbox`) и закончить задание.
    fn export_remote(&self, job: &Job, dir: &Path) -> Result<JobOutcome> {
        self.progress(job, "выгрузка пакета", 50.0);
        let ff = self.ffmpeg()?;
        // F-142: заодно переносим пакеты, ждавшие недоступную папку.
        if let Err(e) = callscribe_core::pipeline::remote::flush_local_outbox(&self.cfg) {
            tracing::warn!("перенос локальной очереди пакетов: {e:#}");
        }
        let pkg = callscribe_core::pipeline::remote::export_package(&self.cfg, dir, |v, i, o| {
            crate::remote::extract_flac(&ff, v, i, o)
        })?;
        tracing::info!("пакет на обработку: {}", pkg.display());
        self.index(dir)?;
        Ok(JobOutcome::Done)
    }

    // ---------------- step 1: ingest (F-52, F-53 prep, F-84) ----------------

    fn ingest(&self, job: &Job, dir: &Path, opts: &JobOptions) -> Result<Option<PathBuf>> {
        let mut meta = Meta::load(dir)?;
        if meta.files.video.is_some() && meta.source_file.is_none() {
            return Ok(Some(dir.to_path_buf()));
        }
        self.progress(job, "подготовка файла", 1.0);
        let ff = self.ffmpeg()?;
        let src = PathBuf::from(
            meta.source_file
                .clone()
                .context("в meta.json нет файла записи")?,
        );
        if !src.is_file() {
            bail!("файл записи не найден: {}", src.display());
        }
        callscribe_media::wait_until_released(&src, Duration::from_secs(600))?;
        let probe = ff.probe(&src)?;
        meta.duration_sec = Some(probe.duration_sec);
        if meta.ended_at.is_none()
            && let Ok(st) = meta.started()
        {
            let end = st + chrono::Duration::milliseconds((probe.duration_sec * 1000.0) as i64);
            meta.ended_at = Some(end.to_rfc3339_opts(chrono::SecondsFormat::Secs, false));
        }

        // F-84: too short.
        if probe.duration_sec < self.cfg.pipeline.min_duration_sec
            && self.cfg.pipeline.delete_short_recordings == DeleteShort::Auto
        {
            tracing::info!(
                "запись короче {} с — удаляю",
                self.cfg.pipeline.min_duration_sec
            );
            if !opts.keep_source {
                let _ = std::fs::remove_file(&src);
            }
            std::fs::remove_dir_all(dir)?;
            return Ok(None);
        }

        let base = dir.file_name().unwrap().to_string_lossy().to_string();
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let target_ext = match ext.as_str() {
            "mkv" if self.cfg.obs.remux_to_mp4 => {
                if probe.has_video { "mp4" } else { "m4a" }.to_string()
            }
            "mka" if self.cfg.obs.remux_to_mp4 => "m4a".to_string(),
            other => other.to_string(),
        };
        let dest = dir.join(format!("{base}.{target_ext}"));
        if target_ext != ext {
            self.progress(job, "ремукс", 3.0);
            match ff.remux(&src, &dest) {
                Ok(()) => {
                    if !opts.keep_source {
                        std::fs::remove_file(&src).ok();
                    }
                }
                Err(e) => {
                    // AAC-incompatible codecs etc.: keep the original container.
                    tracing::warn!("ремукс не удался ({e:#}); оставляю {ext}");
                    let dest = dir.join(format!("{base}.{ext}"));
                    move_or_copy(&src, &dest, opts.keep_source)?;
                    meta.files.video =
                        Some(dest.file_name().unwrap().to_string_lossy().to_string());
                }
            }
            if meta.files.video.is_none() {
                meta.files.video = Some(dest.file_name().unwrap().to_string_lossy().to_string());
            }
        } else {
            move_or_copy(&src, &dest, opts.keep_source)?;
            meta.files.video = Some(dest.file_name().unwrap().to_string_lossy().to_string());
        }
        meta.source_file = None;

        // Audio stream layout (F-53, §5.3 table).
        let n = probe.audio.len() as u32;
        let t = self.cfg.obs.tracks;
        meta.audio_streams = if n >= t.mic.max(t.desktop) {
            AudioStreams {
                mix: Some(t.mix - 1),
                mic: Some(t.mic - 1),
                desktop: Some(t.desktop - 1),
            }
        } else if n == 2 {
            meta.warnings.push(
                "в файле 2 аудиодорожки вместо 3: считаю первую «Я», вторую «Собеседник»".into(),
            );
            AudioStreams {
                mix: None,
                mic: Some(0),
                desktop: Some(1),
            }
        } else if n == 1 {
            meta.warnings.push(
                "в файле одна аудиодорожка — расшифровка без разделения на «Я» и «Собеседника»"
                    .into(),
            );
            AudioStreams {
                mix: Some(0),
                mic: None,
                desktop: None,
            }
        } else {
            bail!("в файле нет аудиодорожек");
        };
        meta.pipeline.mode = format!("{:?}", self.cfg.pipeline.mode).to_lowercase();
        meta.pipeline.host = host();
        meta.status = CallStatus::Queued;
        meta.save(dir)?;
        self.index(dir)?;
        Ok(Some(dir.to_path_buf()))
    }

    pub(crate) fn ffmpeg(&self) -> Result<Ffmpeg> {
        Ffmpeg::locate(&self.cfg.pipeline.ffmpeg_dir)
    }

    // ---------------- step 2: transcription (F-53…F-59) ----------------

    fn transcribe(&self, job: &Job, dir: &Path) -> Result<Vec<Utterance>> {
        if dir.join("transcript.json").is_file()
            && let Ok(u) = load_utterances(dir)
        {
            return Ok(u);
        }
        let mut meta = Meta::load(dir)?;
        meta.status = CallStatus::Transcribing;
        meta.save(dir)?;
        let video = dir.join(meta.files.video.clone().context("нет файла записи")?);
        let ff = self.ffmpeg()?;
        let tmp = self.paths.tmp_dir().join(&meta.id);
        std::fs::create_dir_all(&tmp)?;

        // Tracks to transcribe.
        let mut tracks: Vec<(Speaker, PathBuf)> = Vec::new();
        let s = &meta.audio_streams;
        match (s.mic, s.desktop) {
            (Some(m), Some(d)) => {
                for (sp, idx, name) in [
                    (Speaker::Me, m, "me.wav"),
                    (Speaker::Remote, d, "remote.wav"),
                ] {
                    let wav = tmp.join(name);
                    if !wav.is_file() {
                        ff.extract_track(&video, idx, &wav)?;
                    }
                    tracks.push((sp, wav));
                }
            }
            _ => {
                let wav = tmp.join("mix.wav");
                if !wav.is_file() {
                    ff.extract_track(&video, s.mix.unwrap_or(0), &wav)?;
                }
                tracks.push((Speaker::Mixed, wav));
            }
        }

        let opts = self.transcribe_opts(
            &self.cfg.transcription.language,
            &self.cfg.transcription.glossary,
        );
        let srt_name = format!(
            "{}.srt",
            Path::new(meta.files.video.as_deref().unwrap())
                .file_stem()
                .unwrap()
                .to_string_lossy()
        );
        let title = meta
            .title
            .clone()
            .unwrap_or_else(|| dir.file_name().unwrap().to_string_lossy().to_string());
        let (info, utts) =
            self.transcribe_tracks(job, &mut meta, dir, &title, &tracks, &srt_name, &opts)?;

        meta.files.transcript_md = Some("transcript.md".into());
        meta.files.transcript_json = Some("transcript.json".into());
        meta.files.subtitles = Some(srt_name);
        meta.pipeline.stt = Some(info);
        meta.save(dir)?;
        if !self.cfg.pipeline.keep_extracted_audio {
            let _ = std::fs::remove_dir_all(&tmp);
        }
        self.index(dir)?;
        Ok(utts)
    }

    /// Параметры распознавания: язык и глоссарий — из звонка (в режиме `remote` из пакета), остальное — из конфига.
    pub(crate) fn transcribe_opts(&self, language: &str, glossary: &[String]) -> TranscribeOptions {
        TranscribeOptions {
            language: match language {
                "auto" | "" => None,
                l => Some(l.to_string()),
            },
            beam_size: self.cfg.transcription.beam_size,
            initial_prompt: (!glossary.is_empty())
                .then(|| format!("Глоссарий: {}.", glossary.join(", "))),
            vad: self.cfg.transcription.vad,
        }
    }

    /// Расшифровка готовых дорожек 16 кГц (F-54…F-59) и запись `transcript.md`, `.srt`, `transcript.json` в `out_dir`.
    /// Общая для локальной обработки и для обработчика пакетов (`remote`). `meta.json` не сохраняет.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn transcribe_tracks(
        &self,
        job: &Job,
        meta: &mut Meta,
        out_dir: &Path,
        title: &str,
        tracks: &[(Speaker, PathBuf)],
        srt_name: &str,
        opts: &TranscribeOptions,
    ) -> Result<(EngineInfo, Vec<Utterance>)> {
        let dir = out_dir;
        let mut engine = self.load_engine(job, self.cfg.transcription.device != Device::Cpu)?;
        let mut segments: Vec<Segment> = Vec::new();
        let n = tracks.len() as f64;
        for (i, (speaker, wav)) in tracks.iter().enumerate() {
            let (pcm, sr) = callscribe_media::read_wav_mono_f32(wav)?;
            if sr != 16000 {
                bail!("{}: ожидалось 16 кГц, получено {sr}", wav.display());
            }
            if callscribe_media::peak_db(&pcm) < -60.0 {
                meta.warnings.push(format!(
                    "дорожка «{}» пустая (тишина) — пропущена",
                    speaker.label()
                ));
                continue;
            }
            let base = 10.0 + 75.0 * i as f64 / n;
            let label = format!("расшифровка: {}", speaker.label());
            let mut report =
                |p: f32| self.progress(job, &label, base + 75.0 / n * p as f64 / 100.0);
            let raw = match engine.transcribe(&pcm, opts, &mut report) {
                Ok(r) => r,
                Err(e) if engine.info().device == "gpu" => {
                    // F-54: GPU error → CPU.
                    tracing::warn!("ошибка GPU ({e:#}) — повторяю на CPU");
                    meta.warnings
                        .push(format!("ошибка GPU, расшифровка на CPU: {e}"));
                    engine = self.load_engine(job, false)?;
                    engine.transcribe(&pcm, opts, &mut report)?
                }
                Err(e) => return Err(e),
            };
            let mut raw = raw;
            let silent = callscribe_core::pipeline::postprocess::trim_to_audio(&pcm, &mut raw);
            let first_new = segments.len();
            segments.extend(raw.into_iter().zip(silent).map(|(r, quiet)| {
                let mut s = Segment::from_raw(r, *speaker);
                if quiet {
                    s.dropped = Some("silence".into());
                }
                s
            }));
            // F-60: tell the interlocutors apart (not «Я»: the microphone is one person).
            if self.cfg.transcription.diarization && *speaker != Speaker::Me {
                self.diarize_track(job, &pcm, &mut segments[first_new..], &mut meta.warnings);
            }
        }
        let mut info: EngineInfo = engine.info();
        info.language = opts.language.clone();
        drop(engine); // F-59: free VRAM before the LLM.

        let pp = PostProcessor::new(&self.cfg.transcription)?;
        let utts = pp.run(&mut segments);

        // Outputs (F-58).
        let header = Header {
            title,
            app: &meta.app.rule,
            started_local: &local_str(meta),
            duration_sec: meta.duration_sec.unwrap_or(0.0),
            model: &info.model,
        };
        callscribe_core::pipeline::meta::write_text_atomic(
            &dir.join("transcript.md"),
            &export::transcript_md(&header, &utts, &meta.warnings),
        )?;
        let with_speaker = tracks.iter().any(|(s, _)| *s != Speaker::Mixed)
            || segments.iter().any(|s| s.speaker_index.is_some());
        callscribe_core::pipeline::meta::write_text_atomic(
            &dir.join(srt_name),
            &export::srt(&segments, with_speaker),
        )?;
        // transcript.json last: it marks the step as done.
        callscribe_core::pipeline::meta::write_text_atomic(
            &dir.join("transcript.json"),
            &export::transcript_json(&info, &segments, &utts),
        )?;
        Ok((info, utts))
    }

    fn load_engine(&self, job: &Job, gpu: bool) -> Result<Box<dyn Transcriber>> {
        let tcfg = &self.cfg.transcription;
        match tcfg.engine.as_str() {
            "whisper" => {}
            "gigaam" | "parakeet" => return self.load_sherpa(job, &tcfg.engine),
            other => bail!("неизвестный движок «{other}»: whisper | gigaam | parakeet"),
        }
        let model = if gpu {
            &tcfg.model_gpu
        } else {
            &tcfg.model_cpu
        };
        let dir = self.cfg.models_dir(&self.paths);
        let model_path = self.download(
            job,
            &models::whisper_url(model),
            &dir.join(models::whisper_file(model)),
        )?;
        let vad = if tcfg.vad {
            Some(self.download(job, &models::vad_url(), &dir.join(models::VAD_MODEL))?)
        } else {
            None
        };
        callscribe_stt_whisper::quiet_native_logs();
        match callscribe_stt_whisper::WhisperTranscriber::load(&model_path, model, gpu, vad.clone())
        {
            Ok(t) => Ok(Box::new(t)),
            Err(e) if gpu => {
                tracing::warn!("модель не загрузилась на GPU ({e:#}) — CPU");
                self.load_engine(job, false)
            }
            Err(e) => Err(e),
        }
    }

    /// GigaAM / Parakeet via sherpa-onnx (F-61); CPU only.
    #[cfg(feature = "sherpa")]
    fn load_sherpa(&self, job: &Job, engine: &str) -> Result<Box<dyn Transcriber>> {
        use callscribe_stt_sherpa::{SherpaModel, SherpaTranscriber};
        let model = SherpaModel::from_engine(engine).context("неизвестный движок sherpa")?;
        let dir = self.cfg.models_dir(&self.paths);
        self.ensure_sherpa_archive(
            job,
            &models::sherpa_asr_url(model.dir_name()),
            model.dir_name(),
            &dir,
        )?;
        self.download(job, &models::sherpa_vad_url(), &dir.join(models::SHERPA_VAD))?;
        Ok(Box::new(SherpaTranscriber::load(model, &dir)?))
    }

    #[cfg(not(feature = "sherpa"))]
    fn load_sherpa(&self, _job: &Job, engine: &str) -> Result<Box<dyn Transcriber>> {
        bail!(
            "движок «{engine}» требует сборки с feature `sherpa` (cargo build --features sherpa; D-006)"
        )
    }

    #[cfg(feature = "sherpa")]
    fn ensure_sherpa_archive(&self, job: &Job, url: &str, name: &str, dir: &Path) -> Result<()> {
        let mut last = 0u64;
        self.rt.block_on(models::ensure_archive(url, name, dir, |done, total| {
            if done - last > 20 * 1024 * 1024 {
                last = done;
                let pct = total.map(|t| done as f64 * 100.0 / t as f64).unwrap_or(0.0);
                self.progress(job, &format!("скачивание {name}: {pct:.0}%"), 5.0);
            }
        }))?;
        Ok(())
    }

    /// Diarization of one track (F-60). A failure only costs the numbering, not the transcript.
    #[cfg(feature = "sherpa")]
    fn diarize_track(
        &self,
        job: &Job,
        pcm: &[f32],
        segs: &mut [Segment],
        warnings: &mut Vec<String>,
    ) {
        let dir = self.cfg.models_dir(&self.paths);
        let mut run = || -> Result<usize> {
            self.ensure_sherpa_archive(job, &models::pyannote_url(), models::PYANNOTE_DIR, &dir)?;
            self.download(
                job,
                &models::speaker_embedding_url(),
                &dir.join(models::SPEAKER_EMBEDDING),
            )?;
            self.progress(job, "диаризация", 85.0);
            let turns = callscribe_stt_sherpa::diarize(pcm, &dir)?;
            Ok(callscribe_core::pipeline::postprocess::assign_speakers(segs, &turns))
        };
        match run() {
            Ok(n) => tracing::info!("диаризация: {n} говорящих"),
            Err(e) => {
                tracing::warn!("диаризация не удалась: {e:#}");
                warnings.push(format!("диаризация не удалась: {e}"));
            }
        }
    }

    #[cfg(not(feature = "sherpa"))]
    fn diarize_track(
        &self,
        _job: &Job,
        _pcm: &[f32],
        _segs: &mut [Segment],
        warnings: &mut Vec<String>,
    ) {
        warnings.push("диаризация требует сборки с feature `sherpa` (D-006)".into());
    }

    fn download(&self, job: &Job, url: &str, dest: &Path) -> Result<PathBuf> {
        if dest.is_file() {
            return Ok(dest.to_path_buf());
        }
        let name = dest.file_name().unwrap().to_string_lossy().to_string();
        let mut last = 0u64;
        self.rt.block_on(models::ensure(url, dest, |done, total| {
            if done - last > 20 * 1024 * 1024 {
                last = done;
                let pct = total.map(|t| done as f64 * 100.0 / t as f64).unwrap_or(0.0);
                self.progress(job, &format!("скачивание {name}: {pct:.0}%"), 5.0);
            }
        }))
    }

    // ---------------- step 3–4: summary (F-70…F-77) and naming (F-44, F-80…F-82) ----------------

    fn summarize_and_name(
        &self,
        job: &Job,
        dir: &Path,
        utts: &[Utterance],
        opts: &JobOptions,
    ) -> Result<JobOutcome> {
        let mut meta = Meta::load(dir)?;
        let mut llm_title: Option<String> = None;

        let already = meta.pipeline.llm.is_some() && dir.join("summary.md").is_file();
        if !already && job.kind == JobKind::Process || job.kind == JobKind::Summarize {
            if utts.is_empty() {
                meta.warnings
                    .push("пустой транскрипт — саммари не делалось".into());
            } else {
                match self.pick_llm(&meta)? {
                    None => {}
                    Some(llm) => {
                        meta.status = CallStatus::Summarizing;
                        meta.save(dir)?;
                        self.progress(job, "саммари", 88.0);
                        match self.run_summary(&*llm, &mut meta, dir, utts) {
                            Ok(t) => llm_title = t,
                            Err(e)
                                if e.downcast_ref::<LlmError>()
                                    .is_some_and(|e| matches!(e, LlmError::Unavailable(_))) =>
                            {
                                meta.status = CallStatus::SummaryPending;
                                meta.save(dir)?;
                                self.index(dir)?;
                                let n = job.attempts.clamp(1, 5) as u64;
                                let after = Duration::from_secs(
                                    60 * [5, 15, 60, 180, 360][(n - 1) as usize],
                                );
                                return Ok(JobOutcome::Retry {
                                    error: format!("{e:#}"),
                                    after,
                                });
                            }
                            Err(e) => {
                                meta.status = CallStatus::SummaryPending;
                                meta.errors.push(format!("саммари: {e:#}"));
                                meta.save(dir)?;
                                self.index(dir)?;
                                bail!("саммари: {e:#}");
                            }
                        }
                    }
                }
            }
        } else if let Some(s) = meta.suggested_title.clone() {
            llm_title = Some(s);
        }

        // Naming.
        self.progress(job, "название", 98.0);
        let user_title = (meta.title_source.as_deref() == Some("user"))
            .then(|| meta.title.clone())
            .flatten();
        if opts.auto_name {
            let (title, source) = match (user_title, llm_title) {
                (Some(t), _) => (Some(t), "user"),
                (None, Some(t)) => (Some(t), "llm"),
                (None, None) => (None, "auto"),
            };
            meta.status = CallStatus::AwaitingName;
            meta.save(dir)?;
            let new_dir = apply_title(dir, title.as_deref(), source, &self.cfg.naming)?;
            let mut m = Meta::load(&new_dir)?;
            m.status = CallStatus::Done;
            m.pipeline.processed_at = Some(
                chrono::Local::now()
                    .fixed_offset()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
            );
            m.save(&new_dir)?;
            if new_dir != dir {
                self.db.set_call_dir(job.id, &new_dir)?;
            }
            self.index(&new_dir)?;
            tracing::info!("готово: {}", new_dir.display());
        } else {
            meta.suggested_title = user_title.or(llm_title);
            meta.status = CallStatus::AwaitingName;
            meta.pipeline.processed_at = Some(
                chrono::Local::now()
                    .fixed_offset()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
            );
            meta.save(dir)?;
            self.index(dir)?;
        }
        Ok(JobOutcome::Done)
    }

    /// F-71: pick the provider allowed for this call.
    pub(crate) fn pick_llm(&self, meta: &Meta) -> Result<Option<Box<dyn LlmProvider>>> {
        let l = &self.cfg.llm;
        if l.provider == LlmProviderKind::None {
            return Ok(None);
        }
        let primary = l.primary();
        let cloud_ok = l.allow_cloud && meta.cloud_allowed;
        let endpoint = if !llm::is_cloud_endpoint(&primary) || cloud_ok {
            Some(primary)
        } else {
            match l.fallback() {
                Some(f) if !llm::is_cloud_endpoint(&f) => Some(f),
                _ => None,
            }
        };
        let Some(e) = endpoint else {
            tracing::warn!("облачная LLM запрещена для этого звонка, локальной нет — без саммари");
            return Ok(None);
        };
        llm::build(&e, &Keyring)
    }

    pub(crate) fn run_summary(
        &self,
        llm: &dyn LlmProvider,
        meta: &mut Meta,
        dir: &Path,
        utts: &[Utterance],
    ) -> Result<Option<String>> {
        self.run_summary_with(
            llm,
            meta,
            dir,
            utts,
            &self.cfg.general.user_name,
            &self.cfg.transcription.glossary,
        )
    }

    /// То же, но имя пользователя и глоссарий задаёт вызывающий (в режиме `remote` они приходят в пакете).
    pub(crate) fn run_summary_with(
        &self,
        llm: &dyn LlmProvider,
        meta: &mut Meta,
        dir: &Path,
        utts: &[Utterance],
        user_name: &str,
        glossary: &[String],
    ) -> Result<Option<String>> {
        let marks: Vec<f64> = meta.bookmarks.iter().map(|b| b.at_sec).collect();
        let ctx = summarize::Context {
            app: meta.app.rule.clone(),
            date: local_str(meta),
            duration_sec: meta.duration_sec.unwrap_or(0.0),
            window_title: meta.app.window_title.clone(),
            user_name: user_name.to_string(),
            glossary: glossary.to_vec(),
            language: self.cfg.general.language.clone(),
            title_hint: (meta.title_source.as_deref() == Some("user"))
                .then(|| meta.title.clone())
                .flatten(),
            bookmarks: summarize::bookmark_fragments(utts, &marks, 30.0),
        };
        let prompts = Prompts::load(self.cfg.prompts_dir().as_deref());
        let outcome = self
            .rt
            .block_on(summarize::summarize(llm, &prompts, &ctx, utts))?;
        let (provider, model) = llm.describe();
        meta.pipeline.llm = Some(LlmInfo {
            provider,
            model,
            chunks: outcome.chunks,
        });
        let video = meta.files.video.clone();
        let header = SummaryHeader {
            started_local: &ctx.date,
            duration_sec: ctx.duration_sec,
            app: &ctx.app,
            user_name: &ctx.user_name,
            video_file: video.as_deref(),
            warnings: &meta.warnings,
        };
        let (md, title) = match outcome.result {
            SummaryResult::Ok(s) => (
                export::summary_md(&s, &header, &ctx.bookmarks),
                Some(s.title.trim().to_string()),
            ),
            SummaryResult::Raw { text, error } => {
                meta.warnings
                    .push(format!("ответ LLM не разобрался как JSON: {error}"));
                (
                    format!(
                        "# {}\n\n> ⚠ Ответ LLM не разобрался как JSON ({error}). Ниже — сырой ответ.\n\n{text}\n",
                        dir.file_name().unwrap().to_string_lossy()
                    ),
                    None,
                )
            }
        };
        callscribe_core::pipeline::meta::write_text_atomic(&dir.join("summary.md"), &md)?;
        meta.files.summary_md = Some("summary.md".into());
        meta.save(dir)?;
        Ok(title)
    }

    /// Update the SQLite index and FTS from the folder (F-83, F-90).
    pub fn index(&self, dir: &Path) -> Result<()> {
        let meta = Meta::load(dir)?;
        let transcript = std::fs::read_to_string(dir.join("transcript.md")).ok();
        let summary = std::fs::read_to_string(dir.join("summary.md")).ok();
        let row = CallRow {
            id: meta.id.clone(),
            call_dir: dir.to_string_lossy().to_string(),
            status: serde_json::to_value(meta.status)?
                .as_str()
                .unwrap_or("")
                .to_string(),
            title: meta.title.clone(),
            app: meta.app.rule.clone(),
            started_at: meta.started_at.clone(),
            duration_sec: meta.duration_sec,
        };
        self.db
            .upsert_call(&row, transcript.as_deref(), summary.as_deref())
    }
}

fn move_or_copy(src: &Path, dest: &Path, keep: bool) -> Result<()> {
    if keep {
        std::fs::copy(src, dest).with_context(|| format!("копирование {}", src.display()))?;
        return Ok(());
    }
    if std::fs::rename(src, dest).is_err() {
        // Different volume: copy with size check, then delete (§11, п. 5).
        std::fs::copy(src, dest)?;
        if std::fs::metadata(src)?.len() != std::fs::metadata(dest)?.len() {
            bail!("копия {} не совпала по размеру", dest.display());
        }
        std::fs::remove_file(src)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct TranscriptFile {
    utterances: Vec<Utterance>,
}

pub fn load_utterances(dir: &Path) -> Result<Vec<Utterance>> {
    let text =
        std::fs::read_to_string(dir.join("transcript.json")).context("нет transcript.json")?;
    Ok(serde_json::from_str::<TranscriptFile>(&text)?.utterances)
}
