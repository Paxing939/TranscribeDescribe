//! Tray controller: detection → state machine → OBS → queue → overlays (§4.1, §4.3, §5).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use callscribe_core::config::{DeleteShort, LlmProviderKind, RecordMode, RuleMode};
use callscribe_core::detection::detector::{Debouncer, SignalEvent, SilenceMonitor, Snapshot};
use callscribe_core::detection::rules::{AppMatch, RuleSet};
use callscribe_core::detection::state_machine::{
    Action, CallStateMachine, EndReason, Event, Session, Settings, State, Trigger,
};
use callscribe_core::pipeline::finalize::apply_title;
use callscribe_core::pipeline::ingest::{self, NewCall};
use callscribe_core::pipeline::meta::{AppInfo, Bookmark, CallStatus, Meta, Pause};
use callscribe_core::platform_api::{MicUsageProvider, WindowProvider};
use callscribe_core::storage::{Db, JobKind};
use callscribe_core::{AppPaths, Config, config_edit, llm};
use callscribe_obs::{ObsEvent, StartedRecording, Supervisor};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

use crate::overlays::Overlays;
use crate::tray_ui::{self, IconState, MenuModel};

pub enum Cmd {
    Ui {
        label: String,
        action: String,
        value: Value,
    },
    Menu(String),
    Quit,
}

struct Detection {
    rules: RuleSet,
    mic: Box<dyn MicUsageProvider>,
    windows: Box<dyn WindowProvider>,
    use_window: bool,
    use_mic: bool,
}

/// One OBS recording in progress.
struct ActiveRec {
    session: Session,
    started_wall: chrono::DateTime<chrono::FixedOffset>,
    started_ms: u64,
    obs_started: StartedRecording,
    paused_since: Option<(Instant, chrono::DateTime<chrono::FixedOffset>)>,
    paused_total: Duration,
    pauses: Vec<Pause>,
    bookmarks: Vec<f64>,
    detected_by: Vec<String>,
    began: Instant,
}

impl ActiveRec {
    fn elapsed_in_file(&self) -> f64 {
        let mut d = self.began.elapsed().saturating_sub(self.paused_total);
        if let Some((p, _)) = self.paused_since {
            d = d.saturating_sub(p.elapsed());
        }
        d.as_secs_f64()
    }
}

/// A finished recording waiting for the name dialog (F-43) before it goes to the queue.
struct PendingCall {
    file: PathBuf,
    rec_meta: RecInfo,
    deadline: Instant,
}

#[derive(Clone)]
struct RecInfo {
    app: Option<AppMatch>,
    trigger: Trigger,
    mode: RecordMode,
    screen: u32,
    cloud_allowed: bool,
    started_wall: chrono::DateTime<chrono::FixedOffset>,
    duration_sec: f64,
    pauses: Vec<Pause>,
    bookmarks: Vec<f64>,
    detected_by: Vec<String>,
    webcam: bool,
}

struct DoneEntry {
    dir: PathBuf,
    title: Option<String>,
    apply_at: Option<Instant>,
}

pub struct Controller {
    app: AppHandle,
    paths: AppPaths,
    cfg: Config,
    db: Db,
    overlays: Arc<Overlays>,
    obs: Supervisor,
    det: Arc<Mutex<Detection>>,
    debouncer: Debouncer,
    sm: CallStateMachine,
    silence: SilenceMonitor,
    t0: Instant,
    last_poll: Option<Instant>,
    detection_paused_until: Option<Option<Instant>>,
    rec: Option<ActiveRec>,
    last_detected_by: Vec<String>,
    detected_app: Option<AppMatch>,
    stopping_by_us: bool,
    pending: Option<PendingCall>,
    pending_short: Option<RecInfo>,
    short_file: Option<PathBuf>,
    worker: Option<Child>,
    notified_jobs: HashSet<i64>,
    done: HashMap<String, DoneEntry>,
    errors_seq: u32,
    last_heartbeat: Instant,
    last_menu_key: String,
    adopted_checked: bool,
    webcam_seen: bool,
    status_item: Option<tauri::menu::MenuItem<tauri::Wry>>,
    /// The tray icon shows the error state until then (F-02).
    error_until: Option<Instant>,
    last_remote_check: Option<Instant>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn local_now() -> chrono::DateTime<chrono::FixedOffset> {
    chrono::Local::now().fixed_offset()
}

fn sm_settings(cfg: &Config) -> Settings {
    Settings {
        prompt_timeout_sec: cfg.detection.prompt_timeout_sec,
        prompt_timeout_action: cfg.detection.prompt_timeout_action,
        countdown_sec: cfg.detection.auto_stop_countdown_sec,
        max_duration_sec: cfg.detection.max_duration_min * 60.0,
        default_mode: cfg.obs.default_record_mode,
        default_screen: cfg.obs.default_screen,
    }
}

fn obs_secret(cfg: &Config) -> Option<String> {
    use callscribe_core::llm::SecretResolver;
    cfg.obs
        .password
        .strip_prefix("keyring:")
        .and_then(|n| crate::secrets::Keyring.get(n).ok())
}

impl Controller {
    pub fn new(
        app: AppHandle,
        paths: AppPaths,
        cfg: Config,
        overlays: Arc<Overlays>,
    ) -> Result<Controller> {
        let db = Db::open(&paths.db_file())?;
        let rules = RuleSet::compile(&cfg.detection)?;
        let det = Detection {
            rules,
            mic: Box::new(callscribe_platform::CombinedMicProvider::new()),
            windows: Box::new(callscribe_platform::Win32WindowProvider::new()),
            use_window: cfg.detection.sources.window,
            use_mic: cfg.detection.sources.mic,
        };
        let obs = Supervisor::spawn(cfg.obs.clone(), obs_secret(&cfg), Duration::from_secs(30));
        Ok(Controller {
            debouncer: Debouncer::new(
                cfg.detection.start_debounce_sec,
                cfg.detection.end_debounce_sec,
            ),
            sm: CallStateMachine::new(sm_settings(&cfg)),
            silence: SilenceMonitor::new(
                cfg.detection.silence_threshold_db,
                cfg.detection.silence_timeout_sec,
            ),
            app,
            paths,
            db,
            overlays,
            obs,
            det: Arc::new(Mutex::new(det)),
            cfg,
            t0: Instant::now(),
            last_poll: None,
            detection_paused_until: None,
            rec: None,
            last_detected_by: vec![],
            detected_app: None,
            stopping_by_us: false,
            pending: None,
            pending_short: None,
            short_file: None,
            worker: None,
            notified_jobs: HashSet::new(),
            done: HashMap::new(),
            errors_seq: 0,
            last_heartbeat: Instant::now(),
            last_menu_key: String::new(),
            adopted_checked: false,
            webcam_seen: false,
            status_item: None,
            error_until: None,
            last_remote_check: None,
        })
    }

    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    fn mono_to_wall_ms(&self, t: f64) -> u64 {
        let delta = t - self.now();
        (now_ms() as i64 + (delta * 1000.0) as i64).max(0) as u64
    }

    pub async fn run(mut self, mut rx: mpsc::Receiver<Cmd>) {
        // Jobs left from an earlier run are notified only when they change.
        if let Ok(jobs) = self.db.recent_jobs(50) {
            self.notified_jobs.extend(
                jobs.into_iter()
                    .filter(|j| j.status == "done" || j.status == "failed")
                    .map(|j| j.id),
            );
        }
        self.restore_awaiting_names();
        if std::env::var_os("CALLSCRIBE_OPEN_HISTORY").is_some() {
            crate::history::open_window(&self.app);
        }
        let mut obs_events = self.obs.subscribe();
        let mut tick = tokio::time::interval(Duration::from_millis(1000));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        self.refresh_tray(true);
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Quit) | None => break,
                    Some(c) => self.on_cmd(c).await,
                },
                ev = obs_events.recv() => match ev {
                    Ok(e) => self.on_obs(e).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => obs_events = self.obs.subscribe(),
                },
                _ = tick.tick() => self.on_tick().await,
            }
        }
        self.shutdown().await;
    }

    async fn shutdown(&mut self) {
        let _ = self.db.set_recording_active(false);
        self.obs.stop();
        self.app.exit(0);
    }

    // ---------------------------------------------------------------- tick

    async fn on_tick(&mut self) {
        let now = self.now();

        // Detection poll (F-10, F-11, F-15).
        let interval = Duration::from_secs_f64(self.cfg.detection.poll_interval_sec.max(0.5));
        if self.last_poll.is_none_or(|t| t.elapsed() >= interval) {
            self.last_poll = Some(Instant::now());
            let snap = self.poll_detection().await;
            for ev in self.debouncer.update(&snap, now) {
                match ev {
                    SignalEvent::Started { app, detected_by } => {
                        tracing::info!("сигнал звонка: {} ({})", app.rule, detected_by.join("+"));
                        self.last_detected_by = detected_by.iter().map(|s| s.to_string()).collect();
                        self.detected_app = Some(app.clone());
                        self.dispatch(Event::SignalStarted(app)).await;
                    }
                    SignalEvent::Lost => {
                        tracing::info!("сигнал звонка пропал");
                        self.dispatch(Event::SignalLost).await;
                    }
                    SignalEvent::Returned => self.dispatch(Event::SignalReturned).await,
                }
            }
        }
        self.dispatch(Event::Tick).await;

        // Recording heartbeat for the queue (F-51, D-005).
        if self.rec.is_some() && self.last_heartbeat.elapsed() >= Duration::from_secs(30) {
            let _ = self.db.set_recording_active(true);
            self.last_heartbeat = Instant::now();
        }

        // F-25: adopt our own recording that survived a crash.
        if !self.adopted_checked
            && let Some(c) = self.obs.client()
        {
            self.adopted_checked = true;
            self.adopt_running_recording(&c).await;
        }

        // Name dialog timeout (F-43).
        if self
            .pending
            .as_ref()
            .is_some_and(|p| Instant::now() >= p.deadline)
        {
            self.overlays.close("naming");
            self.finalize_pending(None);
        }

        self.check_queue();
        self.check_remote();
        self.check_auto_apply();
        self.refresh_tray(false);
    }

    async fn poll_detection(&mut self) -> Snapshot {
        if !self.cfg.detection.enabled {
            return Snapshot::default();
        }
        if let Some(until) = self.detection_paused_until {
            match until {
                None => return Snapshot::default(),
                Some(t) if Instant::now() < t => return Snapshot::default(),
                Some(_) => self.detection_paused_until = None,
            }
        }
        let det = self.det.clone();
        let res = tokio::task::spawn_blocking(move || {
            let mut d = det.lock().unwrap();
            let need_windows = d.use_window || d.use_mic;
            let windows = if need_windows {
                d.windows.windows().unwrap_or_default()
            } else {
                vec![]
            };
            let mut snap = Snapshot::default();
            if d.use_window {
                let procs = d.windows.processes().unwrap_or_default();
                snap.window = d.rules.match_meeting(&windows, &procs);
            }
            let mut webcam = false;
            if d.use_mic {
                match d.mic.mic_users() {
                    Ok(users) => {
                        snap.mic = users
                            .iter()
                            .find_map(|u| d.rules.match_mic(&u.chain, &windows));
                    }
                    Err(e) => tracing::debug!("микрофон: {e:#}"),
                }
                webcam = d.mic.webcam_in_use();
            }
            (snap, webcam)
        })
        .await;
        match res {
            Ok((snap, webcam)) => {
                if webcam && self.rec.is_some() {
                    self.webcam_seen = true;
                }
                snap
            }
            Err(e) => {
                tracing::error!("детект упал: {e}");
                Snapshot::default()
            }
        }
    }

    /// Boxed: `execute` → `stop_recording` dispatches `StopCompleted` back here.
    fn dispatch(
        &mut self,
        ev: Event,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let now = self.now();
            let actions = self.sm.handle(ev, now);
            for a in actions {
                self.execute(a).await;
            }
        })
    }

    async fn execute(&mut self, a: Action) {
        match a {
            Action::ShowStartPrompt { app, deadline } => {
                let screens: Vec<u32> = (1..=self.cfg.obs.screen_scenes.len() as u32).collect();
                let (mode, screen) = (
                    app.record_mode.unwrap_or(self.cfg.obs.default_record_mode),
                    app.screen.unwrap_or(self.cfg.obs.default_screen),
                );
                let cloud = self.cfg.llm.provider != LlmProviderKind::None
                    && self.cfg.llm.allow_cloud
                    && llm::is_cloud_endpoint(&self.cfg.llm.primary());
                let h = 165.0
                    + if cloud { 26.0 } else { 0.0 }
                    + if self.cfg.overlay.remind_consent {
                        22.0
                    } else {
                        0.0
                    };
                self.overlays.show(
                    "prompt",
                    "prompt",
                    json!({
                        "view": "prompt", "app": app.rule, "windowTitle": app.window_title,
                        "screens": screens, "defaultMode": if mode == RecordMode::Audio { "audio" } else { "screen" },
                        "defaultScreen": screen, "deadlineMs": self.mono_to_wall_ms(deadline),
                        "showCloudToggle": cloud, "remindConsent": self.cfg.overlay.remind_consent
                    }),
                    (380.0, h),
                    false,
                );
            }
            Action::ShowKeepPrompt => {
                let app = self
                    .rec
                    .as_ref()
                    .and_then(|r| r.session.app.as_ref())
                    .map(|a| a.rule.clone())
                    .unwrap_or_default();
                self.overlays.show(
                    "keep",
                    "keep",
                    json!({"view": "keep", "app": app}),
                    (360.0, 100.0),
                    false,
                );
            }
            Action::ClosePrompt => {
                self.overlays.close("prompt");
                self.overlays.close("keep");
            }
            Action::StartRecording(session) => self.start_recording(session).await,
            Action::StopRecording { discard } => self.stop_recording(discard).await,
            Action::ShowEndPrompt { reason, deadline } => {
                let r = match reason {
                    EndReason::SignalLost => "signal_lost",
                    EndReason::Silence => "silence",
                    EndReason::MaxDuration => "max_duration",
                };
                self.overlays.show("end", "end", json!({"view": "end", "reason": r, "deadlineMs": self.mono_to_wall_ms(deadline)}), (360.0, 110.0), false);
            }
            Action::CloseEndPrompt => self.overlays.close("end"),
        }
    }

    // ---------------------------------------------------------------- recording

    async fn start_recording(&mut self, session: Session) {
        // F-85: free space.
        let calls = self.cfg.calls_dir();
        let incoming = calls.join(".incoming");
        let _ = std::fs::create_dir_all(&incoming);
        if let Ok(b) = callscribe_platform::free_space_bytes(&calls) {
            let gb = b as f64 / 1e9;
            if gb < self.cfg.storage.min_free_gb {
                self.error(
                    "Мало места на диске",
                    &format!(
                        "Свободно {gb:.1} ГБ в {} — запись может оборваться.",
                        calls.display()
                    ),
                    &[],
                );
            }
        }
        let client = match self.obs.client() {
            Some(c) => Some(c),
            None => callscribe_obs::connect_or_launch(
                &self.cfg.obs,
                obs_secret(&self.cfg),
                Duration::from_secs(30),
            )
            .await
            .ok(),
        };
        let Some(client) = client else {
            self.error(
                "Звонок не записывается",
                "OBS недоступен: не удалось подключиться к obs-websocket.",
                &[("retry_obs", "Повторить"), ("logs", "Открыть логи")],
            );
            self.sm.state = State::Suppressed;
            return;
        };
        let dir: &Path = if self.cfg.obs.override_record_dir {
            &incoming
        } else {
            Path::new("")
        };
        match client
            .start_recording(session.mode, session.screen, dir)
            .await
        {
            Ok(started) => {
                if let Some(w) = &started.warning {
                    self.error("Запись идёт с ограничением", w, &[]);
                }
                let _ = client.set_levels_enabled(true).await;
                let _ = self.db.set_recording_active(true);
                self.last_heartbeat = Instant::now();
                self.silence.reset(self.now());
                self.webcam_seen = false;
                let rec = ActiveRec {
                    session: session.clone(),
                    started_wall: local_now(),
                    started_ms: now_ms(),
                    obs_started: started,
                    paused_since: None,
                    paused_total: Duration::ZERO,
                    pauses: vec![],
                    bookmarks: vec![],
                    detected_by: if session.trigger == Trigger::Manual {
                        vec![]
                    } else {
                        self.last_detected_by.clone()
                    },
                    began: Instant::now(),
                };
                tracing::info!(
                    "запись начата ({:?}, экран {})",
                    session.mode,
                    session.screen
                );
                self.rec = Some(rec);
                self.show_pill();
            }
            Err(e) => {
                tracing::error!("старт записи: {e:#}");
                self.error(
                    "Не удалось начать запись",
                    &format!("{e:#}"),
                    &[("logs", "Открыть логи")],
                );
                self.sm.state = State::Suppressed;
            }
        }
    }

    fn show_pill(&mut self) {
        let Some(r) = &self.rec else { return };
        if self.cfg.overlay.recording_pill == "hidden" {
            return;
        }
        let app = r
            .session
            .app
            .as_ref()
            .map(|a| a.rule.clone())
            .unwrap_or_else(|| "Запись".into());
        let paused_ms = r.paused_total.as_millis() as u64;
        let payload = json!({
            "view": "pill", "app": app, "startedMs": r.started_ms, "paused": r.paused_since.is_some(),
            "pausedTotalMs": paused_ms,
            "pauseStartedMs": r.paused_since.map(|(p, _)| now_ms() - p.elapsed().as_millis() as u64),
            "style": if self.cfg.overlay.recording_pill == "dot" { "dot" } else { "full" }
        });
        if self.overlays.is_open("pill") {
            self.overlays.update("pill", payload);
        } else {
            self.overlays
                .show("pill", "pill", payload, (360.0, 48.0), false);
        }
    }

    async fn stop_recording(&mut self, discard: bool) {
        let Some(mut rec) = self.rec.take() else {
            self.dispatch(Event::StopCompleted {
                signal_present: self.debouncer.is_active(),
            })
            .await;
            return;
        };
        self.stopping_by_us = true;
        if let Some((p, at)) = rec.paused_since.take() {
            rec.pauses.push(Pause {
                at: at.to_rfc3339(),
                duration_sec: p.elapsed().as_secs_f64(),
            });
        }
        let duration = rec.elapsed_in_file();
        let mut file: Option<PathBuf> = None;
        if let Some(c) = self.obs.client() {
            match c.stop_recording().await {
                Ok(p) => file = Some(p),
                Err(e) => tracing::error!("стоп записи: {e:#}"),
            }
            if let Err(e) = c.restore(&rec.obs_started).await {
                tracing::warn!("{e:#}");
            }
            let _ = c.set_levels_enabled(false).await;
        }
        if file.is_none() {
            file = self.newest_incoming(rec.started_ms);
        }
        self.stopping_by_us = false;
        let _ = self.db.set_recording_active(false);
        self.overlays.close("pill");
        self.overlays.close("end");
        self.dispatch(Event::StopCompleted {
            signal_present: self.debouncer.is_active(),
        })
        .await;

        let Some(file) = file else {
            self.error(
                "Файл записи не найден",
                "OBS не сообщил путь к файлу, и в папке .incoming его нет.",
                &[("logs", "Открыть логи")],
            );
            return;
        };
        if discard {
            tokio::task::spawn_blocking(move || {
                let _ = callscribe_media::wait_until_released(&file, Duration::from_secs(120));
                let _ = std::fs::remove_file(&file);
            });
            return;
        }
        let info = RecInfo {
            app: rec.session.app.clone(),
            trigger: rec.session.trigger,
            mode: rec.session.mode,
            screen: rec.session.screen,
            cloud_allowed: rec.session.cloud_allowed,
            started_wall: rec.started_wall,
            duration_sec: duration,
            pauses: rec.pauses.clone(),
            bookmarks: rec.bookmarks.clone(),
            detected_by: rec.detected_by.clone(),
            webcam: self.webcam_seen,
        };
        self.after_recording(file, info);
    }

    /// F-84 short-recording check, then the name dialog (F-43).
    fn after_recording(&mut self, file: PathBuf, info: RecInfo) {
        if info.duration_sec < self.cfg.pipeline.min_duration_sec {
            match self.cfg.pipeline.delete_short_recordings {
                DeleteShort::Auto => {
                    tokio::task::spawn_blocking(move || {
                        let _ =
                            callscribe_media::wait_until_released(&file, Duration::from_secs(120));
                        let _ = std::fs::remove_file(&file);
                    });
                    return;
                }
                DeleteShort::Ask => {
                    self.overlays.show(
                        "short",
                        "short",
                        json!({"view": "short", "durationSec": info.duration_sec.round()}),
                        (340.0, 96.0),
                        false,
                    );
                    self.short_file = Some(file);
                    self.pending_short = Some(info);
                    return;
                }
                DeleteShort::Never => {}
            }
        }
        self.ask_name(file, info);
    }

    fn ask_name(&mut self, file: PathBuf, info: RecInfo) {
        let deadline = Instant::now() + Duration::from_secs(60);
        if self.pending.is_some() {
            // A second recording before the first was named: queue the first as is.
            self.finalize_pending(None);
        }
        self.overlays.show(
            "naming",
            "naming",
            json!({"view": "naming", "deadlineMs": now_ms() + 60_000}),
            (380.0, 120.0),
            true,
        );
        self.pending = Some(PendingCall {
            file,
            rec_meta: info,
            deadline,
        });
    }

    /// Create the call folder and queue processing (§4.1).
    fn finalize_pending(&mut self, title: Option<String>) {
        let Some(p) = self.pending.take() else { return };
        let i = p.rec_meta;
        let app = match &i.app {
            Some(a) => AppInfo {
                rule: a.rule.clone(),
                tag: a.tag.clone(),
                process: Some(a.process.clone()),
                window_title: a.window_title.clone(),
            },
            None => AppInfo {
                rule: "Запись".into(),
                tag: "call".into(),
                process: None,
                window_title: None,
            },
        };
        let res = ingest::create_call(
            &self.cfg,
            NewCall {
                started_at: i.started_wall,
                app,
                trigger: i.trigger.as_str(),
                source_file: &p.file,
                user_title: title,
                cloud_allowed: i.cloud_allowed,
            },
        )
        .and_then(|(dir, mut meta)| {
            meta.detected_by = i.detected_by.clone();
            meta.record_mode = Some(if i.mode == RecordMode::Audio {
                "audio".into()
            } else {
                "screen".into()
            });
            meta.screen = (i.mode == RecordMode::Screen).then_some(i.screen);
            meta.pauses = i.pauses.clone();
            meta.bookmarks = i
                .bookmarks
                .iter()
                .map(|&at| Bookmark {
                    at_sec: at,
                    note: None,
                })
                .collect();
            meta.webcam_used = i.webcam;
            meta.status = match self.cfg.pipeline.mode {
                callscribe_core::config::PipelineMode::RecordOnly => CallStatus::Recorded,
                _ => CallStatus::Queued,
            };
            meta.save(&dir)?;
            Ok(dir)
        });
        match res {
            Ok(dir) => {
                if self.cfg.pipeline.mode == callscribe_core::config::PipelineMode::RecordOnly {
                    tracing::info!("режим record_only: {}", dir.display());
                    return;
                }
                let opts = json!({"auto_name": false, "keep_source": false});
                match self.db.enqueue(&dir, JobKind::Process, &opts) {
                    Ok(id) => tracing::info!("в очереди: задание {id} — {}", dir.display()),
                    Err(e) => self.error("Не удалось поставить в очередь", &format!("{e:#}"), &[]),
                }
                self.check_queue();
            }
            Err(e) => self.error(
                "Не удалось сохранить запись",
                &format!("{e:#}"),
                &[("logs", "Открыть логи")],
            ),
        }
    }

    fn newest_incoming(&self, since_ms: u64) -> Option<PathBuf> {
        let dir = self.cfg.calls_dir().join(".incoming");
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                let t = m
                    .modified()
                    .ok()?
                    .duration_since(UNIX_EPOCH)
                    .ok()?
                    .as_millis() as u64;
                (m.is_file() && t + 5000 >= since_ms).then_some((t, e.path()))
            })
            .max_by_key(|(t, _)| *t)
            .map(|(_, p)| p)
    }

    async fn adopt_running_recording(&mut self, c: &callscribe_obs::ObsClient) {
        let Ok(st) = c.record_status().await else {
            return;
        };
        if !st.active || self.rec.is_some() {
            return;
        }
        let incoming = self.cfg.calls_dir().join(".incoming");
        let ours = match (&st.output_path, c.record_directory().await) {
            (Some(p), _) => p.starts_with(&incoming),
            (None, Ok(d)) => Path::new(&d) == incoming,
            _ => false,
        };
        if !ours {
            tracing::info!("OBS пишет вручную — не вмешиваюсь");
            return;
        }
        tracing::warn!("подхватываю запись, начатую до перезапуска (F-25)");
        let now = self.now();
        let session = Session {
            app: None,
            trigger: Trigger::Manual,
            mode: self.cfg.obs.default_record_mode,
            screen: self.cfg.obs.default_screen,
            cloud_allowed: true,
            started_at: now,
            max_deadline: now + self.cfg.detection.max_duration_min * 60.0,
            keep_pending: false,
        };
        let began = Instant::now() - Duration::from_millis(st.duration_ms);
        self.rec = Some(ActiveRec {
            session: session.clone(),
            started_wall: local_now() - chrono::Duration::milliseconds(st.duration_ms as i64),
            started_ms: now_ms() - st.duration_ms,
            obs_started: StartedRecording::default(),
            paused_since: None,
            paused_total: Duration::ZERO,
            pauses: vec![],
            bookmarks: vec![],
            detected_by: vec![],
            began,
        });
        self.sm.state = State::Recording(session);
        let _ = self.db.set_recording_active(true);
        self.show_pill();
    }

    // ---------------------------------------------------------------- OBS events

    async fn on_obs(&mut self, e: ObsEvent) {
        match e {
            ObsEvent::Levels { mic_db, desktop_db } => {
                if matches!(self.sm.state, State::Recording(_))
                    && self.silence.update(&[mic_db, desktop_db], self.now())
                {
                    self.dispatch(Event::SilenceTimeout).await;
                }
            }
            ObsEvent::RecordStopped { path } if self.rec.is_some() && !self.stopping_by_us => {
                // F-26: stopped outside of our control (disk full, OBS closed…).
                tracing::warn!("запись остановилась неожиданно");
                self.error(
                    "Запись остановилась неожиданно",
                    "OBS прекратил запись. Записанное будет обработано.",
                    &[("logs", "Открыть логи")],
                );
                let rec = self.rec.take().unwrap();
                let _ = self.db.set_recording_active(false);
                self.overlays.close("pill");
                self.overlays.close("end");
                self.sm.state = if self.debouncer.is_active() {
                    State::Suppressed
                } else {
                    State::Idle
                };
                if let Some(file) = path.or_else(|| self.newest_incoming(rec.started_ms)) {
                    let info = RecInfo {
                        app: rec.session.app.clone(),
                        trigger: rec.session.trigger,
                        mode: rec.session.mode,
                        screen: rec.session.screen,
                        cloud_allowed: rec.session.cloud_allowed,
                        started_wall: rec.started_wall,
                        duration_sec: rec.elapsed_in_file(),
                        pauses: rec.pauses.clone(),
                        bookmarks: rec.bookmarks.clone(),
                        detected_by: rec.detected_by.clone(),
                        webcam: self.webcam_seen,
                    };
                    self.after_recording(file, info);
                }
            }
            ObsEvent::Disconnected if self.rec.is_some() => {
                self.error("OBS отключился во время записи", "Если OBS закрыт, запись оборвалась; записанное будет обработано после переподключения.", &[]);
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------- commands

    async fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Menu(id) => self.on_menu(&id).await,
            Cmd::Ui {
                label,
                action,
                value,
            } => self.on_ui(&label, &action, value).await,
            Cmd::Quit => {}
        }
        self.refresh_tray(true);
    }

    async fn on_menu(&mut self, id: &str) {
        match id {
            "start:audio" => {
                self.dispatch(Event::ManualStart {
                    mode: RecordMode::Audio,
                    screen: 1,
                })
                .await
            }
            s if s.starts_with("start:screen:") => {
                let n: u32 = s
                    .rsplit(':')
                    .next()
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(1);
                self.dispatch(Event::ManualStart {
                    mode: RecordMode::Screen,
                    screen: n,
                })
                .await
            }
            "stop" => self.dispatch(Event::ManualStop).await,
            "hotkey:toggle" => {
                if self.rec.is_some() {
                    self.dispatch(Event::ManualStop).await
                } else {
                    let (mode, screen) = (
                        self.cfg.obs.default_record_mode,
                        self.cfg.obs.default_screen,
                    );
                    self.dispatch(Event::ManualStart { mode, screen }).await
                }
            }
            "pause" => self.toggle_pause().await,
            "bookmark" => self.bookmark(),
            "detect:pause:1h" => {
                self.detection_paused_until = Some(Some(Instant::now() + Duration::from_secs(3600)))
            }
            "detect:pause:forever" => self.detection_paused_until = Some(None),
            "detect:resume" => self.detection_paused_until = None,
            "open:calls" => {
                let d = self.cfg.calls_dir();
                let _ = std::fs::create_dir_all(&d);
                open_path(&d);
            }
            "open:config" => open_path(&self.paths.config_file()),
            "settings" => crate::settings::open_window(&self.app),
            "open:logs" => open_path(&self.paths.logs_dir()),
            "history" => crate::history::open_window(&self.app),
            "reload" => self.reload_config(),
            "doctor" => crate::spawn::doctor_console(),
            s if s.starts_with("recent:") => {
                let id = &s["recent:".len()..];
                if let Ok(rows) = self.db.recent_calls(20)
                    && let Some(r) = rows.iter().find(|r| r.id == id)
                {
                    let dir = PathBuf::from(&r.call_dir);
                    let summary = dir.join("summary.md");
                    open_path(if summary.is_file() { &summary } else { &dir });
                }
            }
            _ => {}
        }
    }

    async fn toggle_pause(&mut self) {
        let Some(c) = self.obs.client() else { return };
        let Some(r) = self.rec.as_mut() else { return };
        if let Some((p, at)) = r.paused_since.take() {
            if c.resume().await.is_ok() {
                r.paused_total += p.elapsed();
                r.pauses.push(Pause {
                    at: at.to_rfc3339(),
                    duration_sec: p.elapsed().as_secs_f64(),
                });
            } else {
                r.paused_since = Some((p, at));
            }
        } else if c.pause().await.is_ok() {
            // OBS 32 reports success but does not pause Hybrid MP4 recordings: verify.
            if c.record_status().await.is_ok_and(|s| s.paused) {
                if let Some(r) = self.rec.as_mut() {
                    r.paused_since = Some((Instant::now(), local_now()));
                }
            } else {
                self.error(
                    "Пауза не сработала",
                    "OBS не ставит на паузу запись в формате Hybrid MP4. Переключите формат записи на MKV (Настройки OBS → Вывод → Запись).",
                    &[],
                );
            }
        }
        self.show_pill();
    }

    fn bookmark(&mut self) {
        if let Some(r) = self.rec.as_mut() {
            let at = r.elapsed_in_file();
            r.bookmarks.push(at);
            tracing::info!("метка на {at:.1} с");
        }
    }

    async fn on_ui(&mut self, label: &str, action: &str, value: Value) {
        let base = label.split('-').next().unwrap_or(label);
        match (base, action) {
            ("prompt", "record") => {
                let mode = if value["mode"] == "screen" {
                    RecordMode::Screen
                } else {
                    RecordMode::Audio
                };
                let screen = value["screen"].as_u64().unwrap_or(1) as u32;
                let cloud_allowed = value["cloudAllowed"].as_bool().unwrap_or(true);
                self.dispatch(Event::UserRecord {
                    mode,
                    screen,
                    cloud_allowed,
                })
                .await;
            }
            ("prompt", "not_now") => self.dispatch(Event::UserNotNow).await,
            ("prompt", "always") | ("prompt", "never") => {
                let mode = if action == "always" {
                    RuleMode::Auto
                } else {
                    RuleMode::Ignore
                };
                if let Some(app) = self.detected_app.clone() {
                    let r = config_edit::set_rule_mode_in_file(
                        &self.paths.config_file(),
                        &app.rule,
                        &app.process,
                        mode,
                    );
                    if let Err(e) = r {
                        self.error("Не удалось сохранить правило", &format!("{e:#}"), &[]);
                    } else {
                        self.reload_config();
                    }
                }
                if action == "always" {
                    let app = self.detected_app.clone();
                    let mode = app
                        .as_ref()
                        .and_then(|a| a.record_mode)
                        .unwrap_or(self.cfg.obs.default_record_mode);
                    let screen = app
                        .as_ref()
                        .and_then(|a| a.screen)
                        .unwrap_or(self.cfg.obs.default_screen);
                    let cloud_allowed = value["cloudAllowed"].as_bool().unwrap_or(true);
                    self.dispatch(Event::UserRecord {
                        mode,
                        screen,
                        cloud_allowed,
                    })
                    .await;
                } else {
                    self.dispatch(Event::UserNotNow).await;
                }
            }
            ("keep", "keep") => self.dispatch(Event::KeepAnswer(true)).await,
            ("keep", "discard") => self.dispatch(Event::KeepAnswer(false)).await,
            ("pill", "pause") | ("pill", "resume") => self.toggle_pause().await,
            ("pill", "bookmark") => self.bookmark(),
            ("pill", "stop") => self.dispatch(Event::ManualStop).await,
            ("end", "stop") => self.dispatch(Event::UserStopNow).await,
            ("end", "continue") => {
                self.silence.reset(self.now());
                self.dispatch(Event::UserContinue).await
            }
            ("naming", "submit") => {
                self.overlays.close("naming");
                let t = value["title"]
                    .as_str()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string);
                self.finalize_pending(t);
            }
            ("naming", "skip") => {
                self.overlays.close("naming");
                self.finalize_pending(None);
            }
            ("short", "delete") => {
                self.overlays.close("short");
                self.pending_short = None;
                if let Some(f) = self.short_file.take() {
                    tokio::task::spawn_blocking(move || {
                        let _ = callscribe_media::wait_until_released(&f, Duration::from_secs(120));
                        let _ = std::fs::remove_file(&f);
                    });
                }
            }
            ("short", "keep") => {
                self.overlays.close("short");
                if let (Some(f), Some(i)) = (self.short_file.take(), self.pending_short.take()) {
                    self.ask_name(f, i);
                }
            }
            ("done", _) => self.on_done_action(label, action, value),
            ("error", a) => {
                self.overlays.close(label);
                match a {
                    "logs" => open_path(&self.paths.logs_dir()),
                    "retry_obs" => {
                        let _ = callscribe_obs::launch_obs(&self.cfg.obs);
                    }
                    "retry_job" => {
                        let _ = self.db.retry_now();
                        self.check_queue();
                    }
                    _ => {}
                }
            }
            _ => tracing::debug!("неизвестное действие {label}/{action}"),
        }
    }

    // ---------------------------------------------------------------- queue and results (F-44, F-45)

    fn check_queue(&mut self) {
        if let Some(ch) = self.worker.as_mut()
            && let Ok(Some(status)) = ch.try_wait()
        {
            tracing::info!("worker завершился: {status}");
            self.worker = None;
        }
        let recording = self.rec.is_some() && self.cfg.pipeline.defer_during_recording;
        if self.worker.is_none() && !recording && self.db.pending_count().unwrap_or(0) > 0 {
            match crate::spawn::worker() {
                Ok(ch) => self.worker = Some(ch),
                Err(e) => {
                    tracing::error!("не удалось запустить worker: {e:#}");
                }
            }
        }
        // Finished jobs.
        let Ok(jobs) = self.db.recent_jobs(20) else {
            return;
        };
        for j in jobs {
            if self.notified_jobs.contains(&j.id) {
                continue;
            }
            match j.status.as_str() {
                "done" => {
                    self.notified_jobs.insert(j.id);
                    if let Ok(m) = Meta::load(&j.call_dir)
                        && m.status == CallStatus::AwaitingName
                    {
                        self.show_done(&j.call_dir, &m);
                    }
                }
                "failed" => {
                    self.notified_jobs.insert(j.id);
                    let msg = j.error.clone().unwrap_or_default();
                    self.error(
                        "Обработка звонка упала",
                        &msg.chars().take(300).collect::<String>(),
                        &[("retry_job", "Повторить"), ("logs", "Открыть логи")],
                    );
                }
                _ => {}
            }
        }
    }

    /// F-142, F-144: move locally parked packages to the outbox and import results, every 30 s.
    fn check_remote(&mut self) {
        use callscribe_core::pipeline::remote;
        if self.cfg.pipeline.mode != callscribe_core::config::PipelineMode::Remote {
            return;
        }
        if self.last_remote_check.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
            return;
        }
        self.last_remote_check = Some(Instant::now());
        if let Err(e) = remote::flush_local_outbox(&self.cfg) {
            tracing::debug!("outbox: {e:#}");
        }
        let (outbox, _) = remote::resolve_outbox(&self.cfg);
        match remote::import_results(&self.cfg, &outbox) {
            Ok(rep) => {
                for dir in rep.imported {
                    let _ = self.reindex_one(&dir);
                    if let Ok(m) = Meta::load(&dir) {
                        self.show_done(&dir, &m);
                    }
                }
                for dir in rep.failed {
                    let _ = self.reindex_one(&dir);
                    self.error("Обработка на другой машине упала", &dir.display().to_string(), &[("logs", "Открыть логи")]);
                }
            }
            Err(e) => tracing::debug!("импорт результатов: {e:#}"),
        }
    }

    fn show_done(&mut self, dir: &Path, m: &Meta) {
        let key = m.id.clone();
        let label = format!("done-{}", &key[..8.min(key.len())]);
        let auto = (self.cfg.naming.auto_apply_after_min > 0.0)
            .then(|| Duration::from_secs_f64(self.cfg.naming.auto_apply_after_min * 60.0));
        let title = m.suggested_title.clone();
        self.overlays.show(
            &label,
            "done",
            json!({
                "view": "done", "title": title,
                "autoApplyMs": auto.map(|d| now_ms() + d.as_millis() as u64),
                "hasSummary": dir.join("summary.md").is_file()
            }),
            (400.0, 120.0),
            false,
        );
        self.done.insert(
            label,
            DoneEntry {
                dir: dir.to_path_buf(),
                title,
                apply_at: auto.map(|d| Instant::now() + d),
            },
        );
    }

    fn restore_awaiting_names(&mut self) {
        let Ok(rows) = self.db.recent_calls(20) else {
            return;
        };
        for r in rows.into_iter().filter(|r| r.status == "awaiting_name") {
            let dir = PathBuf::from(&r.call_dir);
            if let Ok(m) = Meta::load(&dir)
                && m.status == CallStatus::AwaitingName
            {
                self.show_done(&dir, &m);
            }
        }
    }

    fn on_done_action(&mut self, label: &str, action: &str, value: Value) {
        let Some(entry) = self.done.get(label) else {
            return;
        };
        let dir = entry.dir.clone();
        match action {
            "apply" => {
                let t = entry.title.clone();
                self.apply(label, &dir, t, "llm");
            }
            "edit" => {
                let t = value["title"]
                    .as_str()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string);
                self.apply(label, &dir, t, "user");
            }
            "open_summary" => open_path(&dir.join("summary.md")),
            "open_folder" => open_path(&dir),
            "dismiss" => self.overlays.close(label),
            _ => {}
        }
    }

    fn apply(&mut self, label: &str, dir: &Path, title: Option<String>, source: &str) {
        self.overlays.close(label);
        self.done.remove(label);
        match apply_title(dir, title.as_deref(), source, &self.cfg.naming) {
            Ok(new_dir) => {
                tracing::info!("название применено: {}", new_dir.display());
                let _ = self.reindex_one(&new_dir);
            }
            Err(e) => self.error("Не удалось переименовать", &format!("{e:#}"), &[]),
        }
    }

    fn check_auto_apply(&mut self) {
        let due: Vec<String> = self
            .done
            .iter()
            .filter(|(_, e)| e.apply_at.is_some_and(|t| Instant::now() >= t))
            .map(|(l, _)| l.clone())
            .collect();
        for l in due {
            if let Some(e) = self.done.get(&l) {
                let (dir, t) = (e.dir.clone(), e.title.clone());
                self.apply(&l, &dir, t, "auto");
            }
        }
    }

    fn reindex_one(&self, dir: &Path) -> Result<()> {
        let meta = Meta::load(dir)?;
        let row = callscribe_core::storage::CallRow {
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
        let t = std::fs::read_to_string(dir.join("transcript.md")).ok();
        let s = std::fs::read_to_string(dir.join("summary.md")).ok();
        self.db.upsert_call(&row, t.as_deref(), s.as_deref())
    }

    // ---------------------------------------------------------------- misc

    fn error(&mut self, title: &str, message: &str, actions: &[(&str, &str)]) {
        self.errors_seq += 1;
        self.error_until = Some(Instant::now() + Duration::from_secs(120));
        let label = format!("error-{}", self.errors_seq);
        let acts: Vec<Value> = actions
            .iter()
            .map(|(id, l)| json!({"id": id, "label": l}))
            .collect();
        tracing::warn!("{title}: {message}");
        self.overlays.show(
            &label,
            "error",
            json!({"view": "error", "title": title, "message": message, "actions": acts}),
            (380.0, 120.0),
            false,
        );
        crate::spawn::toast(&self.app, title, message);
    }

    fn reload_config(&mut self) {
        match Config::load_default_location(&self.paths)
            .and_then(|c| RuleSet::compile(&c.detection).map(|r| (c, r)))
        {
            Ok((cfg, rules)) => {
                {
                    let mut d = self.det.lock().unwrap();
                    d.rules = rules;
                    d.use_window = cfg.detection.sources.window;
                    d.use_mic = cfg.detection.sources.mic;
                }
                self.debouncer.start_debounce_sec = cfg.detection.start_debounce_sec;
                self.debouncer.end_debounce_sec = cfg.detection.end_debounce_sec;
                self.sm.settings = sm_settings(&cfg);
                self.silence.threshold_db = cfg.detection.silence_threshold_db;
                self.silence.timeout_sec = cfg.detection.silence_timeout_sec;
                *self.overlays.position.lock().unwrap() = cfg.overlay.position.clone();
                *self.overlays.exclude_from_capture.lock().unwrap() =
                    cfg.overlay.exclude_from_capture;
                self.cfg = cfg;
                tracing::info!("настройки перечитаны");
            }
            Err(e) => self.error("Ошибка в config.toml", &format!("{e:#}"), &[]),
        }
    }

    fn refresh_tray(&mut self, force: bool) {
        let (state, status) = match &self.sm.state {
            State::Recording(_) | State::EndSuspected { .. } => {
                let r = self.rec.as_ref();
                let secs = r.map(|r| r.elapsed_in_file()).unwrap_or(0.0) as u64;
                let app = r
                    .and_then(|r| r.session.app.as_ref())
                    .map(|a| a.rule.clone())
                    .unwrap_or_else(|| "вручную".into());
                let p = if r.is_some_and(|r| r.paused_since.is_some()) {
                    ", пауза"
                } else {
                    ""
                };
                (
                    IconState::Recording,
                    format!(
                        "Запись: {app}, {:02}:{:02}:{:02}{p}",
                        secs / 3600,
                        secs / 60 % 60,
                        secs % 60
                    ),
                )
            }
            State::Detected { app, .. } => (
                IconState::Detected,
                format!("Обнаружен звонок: {}", app.rule),
            ),
            _ => {
                let active = self.db.active_jobs().unwrap_or_default();
                if let Some(j) = active.iter().find(|j| j.status == "running") {
                    (
                        IconState::Processing,
                        format!(
                            "Обработка: {} {:.0}%",
                            j.step.clone().unwrap_or_default(),
                            j.progress
                        ),
                    )
                } else if !active.is_empty() {
                    (
                        IconState::Processing,
                        format!("В очереди: {}", active.len()),
                    )
                } else if self.error_until.is_some_and(|t| Instant::now() < t) {
                    (
                        IconState::Error,
                        "Была ошибка — см. уведомление или логи".to_string(),
                    )
                } else if self.obs.client().is_none() {
                    (IconState::Idle, "Ожидание (OBS не подключён)".to_string())
                } else if self.detection_paused_until.is_some() {
                    (IconState::Idle, "Отслеживание звонков на паузе".to_string())
                } else {
                    (IconState::Idle, "Ожидание звонка".to_string())
                }
            }
        };
        let recent: Vec<(String, String)> = self
            .db
            .recent_calls(10)
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                let date = r
                    .started_at
                    .get(..16)
                    .unwrap_or(&r.started_at)
                    .replace('T', " ");
                (r.id, format!("{date} — {}", r.title.unwrap_or(r.app)))
            })
            .collect();
        let model = MenuModel {
            status: status.clone(),
            recording: self.rec.is_some(),
            paused: self.rec.as_ref().is_some_and(|r| r.paused_since.is_some()),
            screens: self.cfg.obs.screen_scenes.len(),
            detection_paused: self.detection_paused_until.is_some(),
            recent: recent.clone(),
        };
        let Some(tray) = self.app.tray_by_id("main") else {
            return;
        };
        let _ = tray.set_tooltip(Some(format!("CallScribe — {status}")));
        let key = format!(
            "{state:?}|{}|{}|{}|{}|{:?}",
            model.recording, model.paused, model.detection_paused, model.screens, recent
        );
        let changed_state = !self.last_menu_key.starts_with(&format!("{state:?}|"));
        if changed_state || force {
            let _ = tray.set_icon(Some(tray_ui::icon(state)));
        }
        // Rebuild the menu only when its structure changed (an open menu would close);
        // the status line is updated in place every tick.
        if force || key != self.last_menu_key {
            if let Ok((menu, status_item)) = tray_ui::build_menu(&self.app, &model) {
                let _ = tray.set_menu(Some(menu));
                self.status_item = Some(status_item);
            }
        } else if let Some(item) = &self.status_item {
            let _ = item.set_text(&status);
        }
        self.last_menu_key = key;
    }
}

fn open_path(p: &Path) {
    if let Err(e) = tauri_plugin_opener::open_path(p, None::<&str>) {
        tracing::warn!("не удалось открыть {}: {e}", p.display());
    }
}

/// `view_data` for the overlay's own label.
pub fn view_data(app: &AppHandle, label: &str) -> Value {
    app.try_state::<Arc<Overlays>>()
        .and_then(|o| o.data_for(label))
        .unwrap_or(Value::Null)
}

pub fn ensure_dirs(cfg: &Config) -> Result<()> {
    std::fs::create_dir_all(cfg.calls_dir().join(".incoming")).context("папка звонков")
}
