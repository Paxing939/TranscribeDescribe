//! Клиент obs-websocket v5 поверх `obws` (F-20, F-22…F-27).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use callscribe_core::config::{Obs, RecordMode};
use futures_util::StreamExt;
use obws::client::ConnectConfig;
use obws::events::{Event, EventStream, OutputState};
use obws::requests::EventSubscription;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::checks;
use crate::levels::{LevelAggregator, peak_of};
use crate::ws_config::{WsConfig, read_websocket_config};

/// Подписка по умолчанию: все обычные события + метры уровней (не входят в ALL).
const SUBS_WITH_METERS: EventSubscription =
    EventSubscription::ALL.union(EventSubscription::INPUT_VOLUME_METERS);
const SUBS_NO_METERS: EventSubscription = EventSubscription::ALL;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LEVELS_PERIOD: Duration = Duration::from_secs(1);
const STOP_EVENT_WAIT: Duration = Duration::from_secs(10);
const PROFILE_SWITCH_WAIT: Duration = Duration::from_secs(15);
const RESTORE_WAIT: Duration = Duration::from_secs(15);

/// События клиента для трея и детектора.
#[derive(Debug, Clone, PartialEq)]
pub enum ObsEvent {
    RecordStarted,
    /// `path` — путь файла, если OBS его сообщил.
    RecordStopped {
        path: Option<PathBuf>,
    },
    RecordPaused,
    RecordResumed,
    /// Соединение потеряно (OBS закрыт или websocket остановлен).
    Disconnected,
    /// Раз в секунду: пик за секунду по входам микрофона и звука системы, дБ.
    /// Приходит, только пока OBS шлёт метры (§11 п. 14).
    Levels {
        mic_db: f64,
        desktop_db: f64,
    },
}

/// Состояние записи OBS (`GetRecordStatus`).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordStatus {
    pub active: bool,
    pub paused: bool,
    /// Файл текущей записи, если клиент его знает. obs-websocket не отдаёт путь
    /// в `GetRecordStatus`, поэтому он известен лишь после `RecordFileChanged`
    /// (сплит файла); для записи, начатой до запуска клиента, обычно `None`
    /// (папку записи даёт [`ObsClient::record_directory`]).
    pub output_path: Option<PathBuf>,
    /// Длительность записи в мс. Что именно считает OBS во время паузы — проверить (§11 п. 15).
    pub duration_ms: u64,
}

/// Имена входов (после учёта `mic_input` / `desktop_input`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inputs {
    pub mic: Option<String>,
    pub desktop: Option<String>,
}

/// Audio device of a global OBS input (see [`ObsClient::audio_devices`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// `mic` | `desktop`.
    pub role: &'static str,
    pub input: String,
    pub device_id: String,
    /// Device name as OBS shows it, if known.
    pub label: Option<String>,
    pub is_default: bool,
}

/// Virtual / loopback devices that are almost never the microphone you talk into.
pub fn is_virtual_device(name: &str) -> bool {
    const MARKERS: &[&str] = &[
        "virtual", "oculus", "steam streaming", "vb-audio", "cable", "voicemeeter", "obs-", "nvidia virtual",
        "loopback", "stereo mix", "стерео микшер", "wave link", "sonar",
    ];
    let n = name.to_lowercase();
    MARKERS.iter().any(|m| n.contains(m))
}

/// Human-readable report of the devices OBS records from, plus warnings.
/// `default_capture` / `default_render` — Windows defaults (resolve «По умолчанию»).
pub fn describe_audio_devices(
    devs: &[AudioDevice],
    default_capture: Option<&str>,
    default_render: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let mut report = Vec::new();
    let mut warnings = Vec::new();
    for d in devs {
        let what = if d.role == "mic" { "микрофон" } else { "звук системы" };
        let actual = if d.is_default {
            if d.role == "mic" { default_capture } else { default_render }.map(|s| format!("{s} (устройство Windows по умолчанию)"))
        } else {
            d.label.clone()
        };
        match &actual {
            Some(a) => report.push(format!("«{}» ({what}): {a}", d.input)),
            None => {
                report.push(format!("«{}» ({what}): устройство не найдено ({})", d.input, d.device_id));
                warnings.push(format!(
                    "Устройство источника «{}» не найдено — возможно, оно отключено. Выберите его в OBS: Настройки → Аудио.",
                    d.input
                ));
            }
        }
        if d.role == "mic" && actual.as_deref().is_some_and(is_virtual_device) {
            warnings.push(format!(
                "OBS пишет микрофон с виртуального устройства: {}. Ваш голос, скорее всего, не попадёт в запись. \
                 Выберите настоящий микрофон в OBS: Настройки → Аудио → Mic/Aux.",
                actual.unwrap_or_default()
            ));
        }
    }
    (report, warnings)
}

/// Что изменил `start_recording` — по этому `restore` возвращает всё назад.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartedRecording {
    /// Исходный профиль, если переключали.
    pub prev_profile: Option<String>,
    /// Исходная программная сцена, если переключали.
    pub prev_scene: Option<String>,
    /// Исходная папка записи (того профиля, в котором шла запись), если меняли.
    pub prev_record_dir: Option<String>,
    /// Исходное значение `AdvOut/FFFilePath`, если менялось: для вывода
    /// Custom FFmpeg (профиль «только звук») `SetRecordDirectory` не действует,
    /// файл пишется в `FFFilePath`.
    pub prev_ff_path: Option<String>,
    /// Предупреждение для оверлея (OBS стримит, §10).
    pub warning: Option<String>,
}

pub(crate) struct Inner {
    pub(crate) client: obws::Client,
    pub(crate) cfg: Obs,
    tx: broadcast::Sender<ObsEvent>,
    last_path: Arc<Mutex<Option<PathBuf>>>,
    alive: watch::Receiver<bool>,
    task: JoinHandle<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Подключение к OBS. Дёшево клонируется.
///
/// Метры уровней включены с момента подключения; [`ObsClient::set_levels_enabled`]
/// отключает их вне записи (§11 п. 33).
#[derive(Clone)]
pub struct ObsClient {
    pub(crate) inner: Arc<Inner>,
}

/// Пароль для подключения (F-20): `auto` — из конфига obs-websocket,
/// `keyring:<имя>` — готовое значение в `secret` (его достаёт вызывающий),
/// иначе значение как есть. `None` — без пароля.
pub fn resolve_password(cfg: &Obs, secret: Option<String>) -> Result<Option<String>> {
    resolve_password_with(&cfg.password, read_websocket_config(), secret)
}

pub(crate) fn resolve_password_with(
    password: &str,
    ws: Option<WsConfig>,
    secret: Option<String>,
) -> Result<Option<String>> {
    let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
    if password == "auto" {
        return match ws {
            Some(c) if c.auth_required => Ok(non_empty(c.server_password)),
            Some(_) => Ok(None),
            None => bail!(
                "пароль OBS = auto, но конфиг obs-websocket не найден или не читается \
                 (OBS ни разу не запускался?)"
            ),
        };
    }
    if let Some(name) = password.strip_prefix("keyring:") {
        return match secret {
            Some(s) => Ok(non_empty(s)),
            None => bail!("пароль OBS «keyring:{name}» не передан: секрет не найден в хранилище"),
        };
    }
    Ok(non_empty(password.to_owned()))
}

/// Ошибка вида «OBS не запущен / порт закрыт» (а не «неверный пароль»).
pub(crate) fn is_unreachable(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<obws::error::Error>(),
        Some(obws::error::Error::Connect(_) | obws::error::Error::Timeout)
    )
}

pub(crate) fn warn_line(what: &str, e: &obws::error::Error) -> String {
    format!("Не удалось проверить {what}: {e}")
}

impl ObsClient {
    /// Подключиться к OBS (F-20). Пароль — см. [`resolve_password`].
    pub async fn connect(cfg: &Obs, secret: Option<String>) -> Result<ObsClient> {
        let (tx, _) = broadcast::channel(256);
        Self::connect_with_sender(cfg, secret, tx).await
    }

    /// То же, но события идут в переданный канал (его делит [`crate::Supervisor`]).
    pub(crate) async fn connect_with_sender(
        cfg: &Obs,
        secret: Option<String>,
        tx: broadcast::Sender<ObsEvent>,
    ) -> Result<ObsClient> {
        let password = resolve_password(cfg, secret)?;
        let client =
            obws::Client::connect_with_config(ConnectConfig {
                host: cfg.host.as_str(),
                port: cfg.port,
                dangerous: None,
                password: password.as_deref(),
                event_subscriptions: Some(SUBS_WITH_METERS),
                broadcast_capacity: 512,
                connect_timeout: CONNECT_TIMEOUT,
            })
            .await
            .map_err(|e| {
                let text = match &e {
                obws::error::Error::Connect(_) | obws::error::Error::Timeout => format!(
                    "OBS недоступен на {}:{} (не запущен или websocket выключен)",
                    cfg.host, cfg.port
                ),
                _ => "не удалось подключиться к obs-websocket (проверьте пароль и версию OBS 30.2+)"
                    .to_owned(),
            };
                anyhow::Error::new(e).context(text)
            })?;

        let inputs = resolve_inputs(&client, cfg).await.unwrap_or_else(|e| {
            tracing::warn!("не удалось определить входы микрофона и звука системы: {e}");
            Inputs::default()
        });
        let stream = client.events().context("поток событий OBS недоступен")?;
        let last_path = Arc::new(Mutex::new(None));
        let (alive_tx, alive) = watch::channel(true);
        let task = tokio::spawn(event_loop(
            stream,
            tx.clone(),
            last_path.clone(),
            LevelAggregator::new(inputs.mic, inputs.desktop),
            alive_tx,
        ));
        Ok(ObsClient {
            inner: Arc::new(Inner {
                client,
                cfg: cfg.clone(),
                tx,
                last_path,
                alive,
                task,
            }),
        })
    }

    /// Подписка на события. Канал переживает переподключения только у `Supervisor`.
    pub fn subscribe(&self) -> broadcast::Receiver<ObsEvent> {
        self.inner.tx.subscribe()
    }

    /// Соединение ещё живо.
    pub fn is_connected(&self) -> bool {
        *self.inner.alive.borrow()
    }

    /// Завершается, когда соединение потеряно.
    pub async fn closed(&self) {
        let mut rx = self.inner.alive.clone();
        let _ = rx.wait_for(|alive| !*alive).await;
    }

    /// Включить или выключить метры уровней без переподключения (`Reidentify`).
    pub async fn set_levels_enabled(&self, enabled: bool) -> Result<()> {
        let subs = if enabled {
            SUBS_WITH_METERS
        } else {
            SUBS_NO_METERS
        };
        self.inner
            .client
            .reidentify(subs)
            .await
            .context("не удалось сменить подписку на события OBS")
    }

    pub async fn record_status(&self) -> Result<RecordStatus> {
        let s = self
            .inner
            .client
            .recording()
            .status()
            .await
            .context("GetRecordStatus")?;
        let output_path = if s.active {
            self.inner.last_path.lock().unwrap().clone()
        } else {
            None
        };
        Ok(RecordStatus {
            active: s.active,
            paused: s.paused,
            output_path,
            duration_ms: u64::try_from(s.duration.whole_milliseconds()).unwrap_or(0),
        })
    }

    /// Текущая папка записи OBS (для F-25: запись в `.incoming` — наша).
    pub async fn record_directory(&self) -> Result<String> {
        self.inner
            .client
            .config()
            .record_directory()
            .await
            .context("GetRecordDirectory")
    }

    /// Старт записи (F-22): порядок `SetCurrentProfile` → `SetRecordDirectory` →
    /// `StartRecord` (§11 п. 23). При ошибке уже сделанные изменения откатываются.
    /// Если OBS стримит — профиль и сцена не трогаются, в результате `warning` (§10).
    pub async fn start_recording(
        &self,
        mode: RecordMode,
        screen: u32,
        record_dir: &Path,
    ) -> Result<StartedRecording> {
        let c = &self.inner.client;
        if c.recording()
            .status()
            .await
            .context("GetRecordStatus")?
            .active
        {
            bail!("OBS уже пишет запись");
        }
        let streaming = c
            .streaming()
            .status()
            .await
            .context("GetStreamStatus")?
            .active;
        let mut st = StartedRecording::default();
        if let Err(e) = self
            .start_inner(mode, screen, record_dir, streaming, &mut st)
            .await
        {
            if let Err(re) = self.restore(&st).await {
                tracing::warn!("откат настроек OBS после неудачного старта: {re:#}");
            }
            return Err(e);
        }
        Ok(st)
    }

    async fn start_inner(
        &self,
        mode: RecordMode,
        screen: u32,
        record_dir: &Path,
        streaming: bool,
        st: &mut StartedRecording,
    ) -> Result<()> {
        let c = &self.inner.client;
        let cfg = &self.inner.cfg;
        if streaming {
            st.warning = Some(match mode {
                RecordMode::Audio => "OBS стримит: профиль не переключён, запись идёт в текущем \
                                      профиле (со звуком и видео)."
                    .to_owned(),
                RecordMode::Screen => {
                    "OBS стримит: сцена не переключена, пишется текущая сцена.".to_owned()
                }
            });
        } else {
            match mode {
                RecordMode::Audio => {
                    let profiles = c.profiles().list().await.context("GetProfileList")?;
                    if profiles.current != cfg.audio_profile {
                        if profiles.profiles.contains(&cfg.audio_profile) {
                            st.prev_profile = Some(profiles.current);
                            self.switch_profile(&cfg.audio_profile, false).await?;
                        } else {
                            // Better a recording with video than none: OBS is not set up yet.
                            st.warning = Some(format!(
                                "Профиля OBS «{}» нет — запись идёт в текущем профиле (со звуком и видео). \
                                 Настройте OBS: callscribe obs-setup.",
                                cfg.audio_profile
                            ));
                        }
                    }
                }
                RecordMode::Screen => {
                    let name = usize::try_from(screen)
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .and_then(|i| cfg.screen_scenes.get(i));
                    let current = c
                        .scenes()
                        .current_program_scene()
                        .await
                        .context("GetCurrentProgramScene")?
                        .id
                        .name;
                    match name {
                        Some(name) if &current != name => {
                            match c.scenes().set_current_program_scene(name.as_str()).await {
                                Ok(()) => st.prev_scene = Some(current),
                                Err(e) => {
                                    tracing::warn!("сцена OBS «{name}»: {e}");
                                    st.warning = Some(format!(
                                        "Сцены OBS «{name}» нет — пишется текущая сцена «{current}». \
                                         Настройте OBS: callscribe obs-setup."
                                    ));
                                }
                            }
                        }
                        Some(_) => {}
                        None => {
                            st.warning = Some(format!(
                                "Для экрана {screen} не задана сцена OBS (obs.screen_scenes) — пишется текущая сцена «{current}»."
                            ));
                        }
                    }
                }
            }
        }
        if cfg.override_record_dir {
            // Читаем уже после смены профиля: папка записи принадлежит профилю.
            st.prev_record_dir = Some(self.record_directory().await?);
            c.config()
                .set_record_directory(&record_dir.to_string_lossy())
                .await
                .context("SetRecordDirectory (нужен OBS 30+)")?;
            let ffmpeg_out = self
                .param("AdvOut", "RecType")
                .await
                .ok()
                .flatten()
                .as_deref()
                == Some("FFmpeg")
                && self.param("Output", "Mode").await.ok().flatten().as_deref() == Some("Advanced");
            if ffmpeg_out {
                st.prev_ff_path = self.param("AdvOut", "FFFilePath").await.ok().flatten();
                c.profiles()
                    .set_parameter(obws::requests::profiles::SetParameter {
                        category: "AdvOut",
                        name: "FFFilePath",
                        value: Some(&record_dir.to_string_lossy()),
                    })
                    .await
                    .context("SetProfileParameter AdvOut/FFFilePath")?;
            }
        }
        c.recording().start().await.context("StartRecord")?;
        Ok(())
    }

    /// Стоп записи (F-23): путь файла из ответа `StopRecord`, иначе из события
    /// `RecordStateChanged`.
    pub async fn stop_recording(&self) -> Result<PathBuf> {
        let mut rx = self.subscribe();
        let path = self
            .inner
            .client
            .recording()
            .stop()
            .await
            .context("StopRecord")?;
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
        let wait = async {
            loop {
                match rx.recv().await {
                    Ok(ObsEvent::RecordStopped { path }) => return path,
                    Err(broadcast::error::RecvError::Closed) => return None,
                    _ => {}
                }
            }
        };
        tokio::time::timeout(STOP_EVENT_WAIT, wait)
            .await
            .ok()
            .flatten()
            .ok_or_else(|| anyhow!("OBS остановил запись, но не сообщил путь к файлу"))
    }

    /// Вернуть профиль, сцену и папку записи (после стопа). Выполняет все шаги,
    /// даже если один из них не удался; ошибки собираются в одну.
    pub async fn restore(&self, st: &StartedRecording) -> Result<()> {
        let c = &self.inner.client;
        let mut errors: Vec<String> = Vec::new();
        // `StopRecord` возвращается раньше, чем вывод остановлен (OutputRunning).
        if self.wait_record_inactive(RESTORE_WAIT).await.is_err() {
            errors.push("запись ещё идёт".to_owned());
        }
        // Папка — в профиле, где шла запись, поэтому до возврата профиля.
        if let Some(dir) = &st.prev_record_dir
            && let Err(e) = c.config().set_record_directory(dir).await
        {
            errors.push(format!("папка записи: {e}"));
        }
        if let Some(p) = &st.prev_ff_path
            && let Err(e) = c
                .profiles()
                .set_parameter(obws::requests::profiles::SetParameter {
                    category: "AdvOut",
                    name: "FFFilePath",
                    value: Some(p),
                })
                .await
        {
            errors.push(format!("папка вывода FFmpeg: {e}"));
        }
        if let Some(scene) = &st.prev_scene
            && let Err(e) = c.scenes().set_current_program_scene(scene.as_str()).await
        {
            errors.push(format!("сцена «{scene}»: {e}"));
        }
        if let Some(profile) = &st.prev_profile
            && let Err(e) = self.switch_profile(profile, false).await
        {
            errors.push(format!("профиль «{profile}»: {e:#}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "не удалось вернуть настройки OBS: {}",
                errors.join("; ")
            ))
        }
    }

    /// Переключить (или создать с переключением) профиль и дождаться события
    /// `CurrentProfileChanged`: сам запрос возвращается раньше, чем профиль
    /// загружен, и параметры, записанные в этот момент, попадают в старый профиль.
    pub(crate) async fn switch_profile(&self, name: &str, create: bool) -> Result<()> {
        let c = &self.inner.client;
        if !create && c.profiles().current().await.context("GetCurrentProfile")? == name {
            return Ok(());
        }
        let mut events = c.events().context("поток событий OBS недоступен")?;
        if create {
            c.profiles().create(name).await.context("CreateProfile")?;
        } else {
            c.profiles()
                .set_current(name)
                .await
                .context("SetCurrentProfile")?;
        }
        let wait = async {
            while let Some(ev) = events.next().await {
                if let Event::CurrentProfileChanged { name: n } = ev
                    && n == name
                {
                    return true;
                }
            }
            false
        };
        match tokio::time::timeout(PROFILE_SWITCH_WAIT, wait).await {
            Ok(true) => Ok(()),
            _ => bail!("OBS не подтвердил переключение на профиль «{name}»"),
        }
    }

    /// Ждать, пока вывод записи станет неактивным.
    async fn wait_record_inactive(&self, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let active = self
                .inner
                .client
                .recording()
                .status()
                .await
                .context("GetRecordStatus")?
                .active;
            if !active {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("запись OBS не остановилась за {} с", timeout.as_secs());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn pause(&self) -> Result<()> {
        self.inner
            .client
            .recording()
            .pause()
            .await
            .context("PauseRecord")
    }

    pub async fn resume(&self) -> Result<()> {
        self.inner
            .client
            .recording()
            .resume()
            .await
            .context("ResumeRecord")
    }

    /// Входы микрофона и звука системы с учётом `mic_input` / `desktop_input`.
    pub async fn special_inputs(&self) -> Result<Inputs> {
        resolve_inputs(&self.inner.client, &self.inner.cfg).await
    }

    /// Which audio devices the mic and desktop inputs use (`device_id` + its label in OBS).
    /// `is_default` — «По умолчанию»: the real device is the Windows default, which the caller resolves.
    pub async fn audio_devices(&self) -> Result<Vec<AudioDevice>> {
        let c = &self.inner.client;
        let inputs = self.special_inputs().await?;
        let mut out = Vec::new();
        for (role, name) in [("mic", inputs.mic), ("desktop", inputs.desktop)] {
            let Some(name) = name else { continue };
            let settings = c
                .inputs()
                .settings::<serde_json::Value>(name.as_str().into())
                .await
                .with_context(|| format!("GetInputSettings «{name}»"))?;
            let id = settings.settings["device_id"].as_str().unwrap_or("default").to_string();
            let label = c
                .inputs()
                .properties_list_property_items(name.as_str().into(), "device_id")
                .await
                .ok()
                .and_then(|items| items.into_iter().find(|i| i.value.as_str() == Some(id.as_str())))
                .map(|i| i.name);
            out.push(AudioDevice {
                role,
                input: name,
                is_default: id == "default" || id.is_empty(),
                device_id: id,
                label,
            });
        }
        Ok(out)
    }

    pub(crate) async fn param(
        &self,
        category: &str,
        name: &str,
    ) -> std::result::Result<Option<String>, obws::error::Error> {
        let p = self
            .inner
            .client
            .profiles()
            .parameter(category, name)
            .await?;
        Ok(p.value.or(p.default_value))
    }

    /// Проверка настроек OBS (F-27): список проблем по-русски, пустой — всё в порядке.
    /// Параметры вывода читаются из *текущего* профиля; содержимое профиля
    /// `audio_profile` (контейнер, дорожки) проверяется только по наличию.
    pub async fn check_settings(&self, cfg: &Obs) -> Vec<String> {
        let c = &self.inner.client;
        let mut out: Vec<String> = Vec::new();
        let t = cfg.tracks;

        let advanced = match self.param("Output", "Mode").await {
            Ok(v) => {
                let problem = checks::check_output_mode(v.as_deref());
                let ok = problem.is_none();
                out.extend(problem);
                ok
            }
            Err(e) => {
                out.push(warn_line("режим вывода", &e));
                false
            }
        };
        if advanced {
            match self.param("AdvOut", "RecTracks").await {
                Ok(v) => {
                    out.extend(checks::check_rec_tracks(
                        v.as_deref(),
                        &[t.mix, t.mic, t.desktop],
                    ));
                }
                Err(e) => out.push(warn_line("дорожки записи", &e)),
            }
            match (
                self.param("AdvOut", "RecFormat2").await,
                self.param("AdvOut", "RecFormat").await,
            ) {
                (Err(e), _) | (_, Err(e)) => out.push(warn_line("формат записи", &e)),
                (Ok(f2), Ok(f)) => {
                    out.extend(checks::check_rec_format(f2.as_deref(), f.as_deref()))
                }
            }
        }

        match self.special_inputs().await {
            Ok(inputs) => {
                for (label, name, tracks, forbidden) in [
                    ("микрофон", inputs.mic, [t.mix, t.mic], [t.desktop]),
                    ("звук системы", inputs.desktop, [t.mix, t.desktop], [t.mic]),
                ] {
                    let Some(name) = name else {
                        out.push(format!(
                            "Не найден источник OBS: {label} (GetSpecialInputs)."
                        ));
                        continue;
                    };
                    match c.inputs().audio_tracks(name.as_str().into()).await {
                        Ok(en) => {
                            out.extend(checks::check_input_tracks(&name, &en, &tracks, &forbidden))
                        }
                        Err(e) => out.push(warn_line(&format!("дорожки источника «{name}»"), &e)),
                    }
                }
            }
            Err(e) => out.push(format!(
                "Не удалось определить источники микрофона и звука: {e:#}"
            )),
        }

        match c.scenes().list().await {
            Ok(list) => {
                for want in &cfg.screen_scenes {
                    if !list.scenes.iter().any(|s| s.id.name == *want) {
                        out.push(format!(
                            "Сцена OBS «{want}» не найдена (создайте по §14 README)."
                        ));
                    } else if let Ok(misfits) = self.capture_misfits(want).await {
                        for (_, src) in misfits {
                            out.push(format!(
                                "В сцене «{want}» захват «{src}» не вписан в холст — в записи будет обрезанный экран."
                            ));
                        }
                    }
                }
            }
            Err(e) => out.push(warn_line("список сцен", &e)),
        }
        match c.profiles().list().await {
            Ok(list) => {
                if !list.profiles.contains(&cfg.audio_profile) {
                    out.push(format!(
                        "Профиль OBS «{}» не найден (создайте по §14 README).",
                        cfg.audio_profile
                    ));
                }
            }
            Err(e) => out.push(warn_line("список профилей", &e)),
        }
        out
    }
}

pub(crate) async fn resolve_inputs(client: &obws::Client, cfg: &Obs) -> Result<Inputs> {
    let pick = |setting: &str| {
        let s = setting.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("auto") {
            None
        } else {
            Some(s.to_owned())
        }
    };
    let (mic, desktop) = (pick(&cfg.mic_input), pick(&cfg.desktop_input));
    if mic.is_some() && desktop.is_some() {
        return Ok(Inputs { mic, desktop });
    }
    let sp = client
        .inputs()
        .specials()
        .await
        .context("GetSpecialInputs")?;
    Ok(Inputs {
        mic: mic.or(sp.mic1),
        desktop: desktop.or(sp.desktop1),
    })
}

async fn event_loop(
    mut stream: EventStream,
    tx: broadcast::Sender<ObsEvent>,
    last_path: Arc<Mutex<Option<PathBuf>>>,
    mut agg: LevelAggregator,
    alive: watch::Sender<bool>,
) {
    let mut tick = tokio::time::interval(LEVELS_PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            ev = stream.next() => {
                let Some(ev) = ev else { break };
                match ev {
                    Event::InputVolumeMeters { inputs } => {
                        agg.feed(inputs.iter().map(|i| (i.name.as_str(), peak_of(&i.levels))));
                    }
                    Event::RecordFileChanged { path } => {
                        *last_path.lock().unwrap() = Some(PathBuf::from(path));
                    }
                    Event::RecordStateChanged { state, path, .. } => {
                        let out = match state {
                            OutputState::Started => Some(ObsEvent::RecordStarted),
                            OutputState::Stopped => {
                                let known = last_path.lock().unwrap().take();
                                Some(ObsEvent::RecordStopped {
                                    path: path.map(PathBuf::from).or(known),
                                })
                            }
                            OutputState::Paused => Some(ObsEvent::RecordPaused),
                            OutputState::Resumed => Some(ObsEvent::RecordResumed),
                            _ => None,
                        };
                        if let Some(out) = out {
                            let _ = tx.send(out);
                        }
                    }
                    _ => {}
                }
            }
            _ = tick.tick() => {
                if let Some((mic_db, desktop_db)) = agg.flush() {
                    let _ = tx.send(ObsEvent::Levels { mic_db, desktop_db });
                }
            }
        }
    }
    let _ = alive.send(false);
    let _ = tx.send(ObsEvent::Disconnected);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(auth: bool, pw: &str) -> Option<WsConfig> {
        Some(WsConfig {
            server_enabled: true,
            server_port: 4455,
            server_password: pw.into(),
            auth_required: auth,
        })
    }

    #[test]
    fn password_auto() {
        assert_eq!(
            resolve_password_with("auto", ws(true, "pw"), None)
                .unwrap()
                .as_deref(),
            Some("pw")
        );
        assert_eq!(
            resolve_password_with("auto", ws(false, "pw"), None).unwrap(),
            None
        );
        assert!(resolve_password_with("auto", None, None).is_err());
    }

    #[test]
    fn password_keyring_and_literal() {
        assert_eq!(
            resolve_password_with("keyring:obs", None, Some("s".into()))
                .unwrap()
                .as_deref(),
            Some("s")
        );
        assert!(resolve_password_with("keyring:obs", None, None).is_err());
        assert_eq!(
            resolve_password_with("abc", None, None).unwrap().as_deref(),
            Some("abc")
        );
        assert_eq!(resolve_password_with("", None, None).unwrap(), None);
    }
}
