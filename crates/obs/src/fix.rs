//! Автоисправление настроек OBS (F-27 «Исправить»): меняет только то, что
//! отсутствует или неверно, и возвращает список изменений.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use callscribe_core::config::Obs;
use obws::requests::inputs::{Create, SetSettings};
use obws::requests::profiles::SetParameter;

use crate::checks::{ParamChange, plan_audio_profile, plan_input_tracks, plan_main_profile};
use crate::client::ObsClient;

/// Параметры профиля «только звук», которые читаем перед планированием.
const AUDIO_KEYS: &[(&str, &str)] = &[
    ("Output", "Mode"),
    ("AdvOut", "RecType"),
    ("AdvOut", "FFOutputToFile"),
    ("AdvOut", "FFFormat"),
    ("AdvOut", "FFFormatMimeType"),
    ("AdvOut", "FFExtension"),
    ("AdvOut", "FFVEncoderId"),
    ("AdvOut", "FFVEncoder"),
    ("AdvOut", "FFAEncoderId"),
    ("AdvOut", "FFAEncoder"),
    ("AdvOut", "FFAudioMixes"),
];

/// Виды источника, которые считаются захватом экрана.
const CAPTURE_KINDS: &[&str] = &["monitor_capture", "display_capture", "screen_capture"];
/// Свойства, в которых источник захвата монитора хранит выбранный экран
/// (новое и старое имя).
const MONITOR_PROPS: &[&str] = &["monitor_id", "monitor"];

fn tracks_list(t: &[u32]) -> String {
    t.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
}

impl ObsClient {
    /// Привести OBS к состоянию, которого ждёт [`ObsClient::check_settings`], и
    /// вернуть список изменений. Идёт запись или стрим — ошибка, ничего не
    /// меняется. Если часть шагов упала, остальные всё равно выполняются, а
    /// ошибка содержит и причины, и уже сделанные изменения.
    pub async fn fix_settings(&self, cfg: &Obs) -> Result<Vec<String>> {
        let c = &self.inner.client;
        if c.recording()
            .status()
            .await
            .context("GetRecordStatus")?
            .active
        {
            bail!("идёт запись: менять настройки OBS сейчас нельзя");
        }
        if c.streaming()
            .status()
            .await
            .context("GetStreamStatus")?
            .active
        {
            bail!("OBS стримит: менять настройки сейчас нельзя");
        }

        let mut changes = Vec::new();
        let mut errors = Vec::new();
        if let Err(e) = self.fix_main_profile(cfg, &mut changes).await {
            errors.push(format!("текущий профиль: {e:#}"));
        }
        if let Err(e) = self.fix_input_tracks(cfg, &mut changes).await {
            errors.push(format!("дорожки источников: {e:#}"));
        }
        if let Err(e) = self.fix_scenes(cfg, &mut changes).await {
            errors.push(format!("сцены: {e:#}"));
        }
        if let Err(e) = self.fix_audio_profile(cfg, &mut changes).await {
            errors.push(format!("профиль «{}»: {e:#}", cfg.audio_profile));
        }
        if errors.is_empty() {
            Ok(changes)
        } else {
            Err(anyhow!(
                "не всё удалось исправить: {}. Уже изменено: {}",
                errors.join("; "),
                if changes.is_empty() {
                    "ничего".to_owned()
                } else {
                    changes.join("; ")
                }
            ))
        }
    }

    async fn set_param(&self, p: &ParamChange) -> Result<()> {
        self.inner
            .client
            .profiles()
            .set_parameter(SetParameter {
                category: p.category,
                name: p.name,
                value: Some(&p.value),
            })
            .await
            .with_context(|| format!("SetProfileParameter {}/{}", p.category, p.name))
    }

    async fn fix_main_profile(&self, cfg: &Obs, changes: &mut Vec<String>) -> Result<()> {
        let t = cfg.tracks;
        let mode = self.param("Output", "Mode").await?;
        let tracks = self.param("AdvOut", "RecTracks").await?;
        let f2 = self.param("AdvOut", "RecFormat2").await?;
        let f1 = self.param("AdvOut", "RecFormat").await?;
        let plan = plan_main_profile(
            mode.as_deref(),
            tracks.as_deref(),
            f2.as_deref(),
            f1.as_deref(),
            &[t.mix, t.mic, t.desktop],
        );
        if plan.is_empty() {
            return Ok(());
        }
        let profile = self.inner.client.profiles().current().await?;
        for p in &plan {
            self.set_param(p).await?;
            changes.push(format!("Профиль «{profile}»: {}", p.note));
        }
        Ok(())
    }

    async fn fix_input_tracks(&self, cfg: &Obs, changes: &mut Vec<String>) -> Result<()> {
        let c = &self.inner.client;
        let t = cfg.tracks;
        let inputs = self.special_inputs().await?;
        for (label, name, req, forb) in [
            ("микрофон", inputs.mic, [t.mix, t.mic], [t.desktop]),
            ("звук системы", inputs.desktop, [t.mix, t.desktop], [t.mic]),
        ] {
            let Some(name) = name else {
                bail!("не найден источник OBS: {label}");
            };
            let now = c.inputs().audio_tracks(name.as_str().into()).await?;
            let plan = plan_input_tracks(&now, &req, &forb);
            if plan.iter().all(Option::is_none) {
                continue;
            }
            c.inputs()
                .set_audio_tracks(name.as_str().into(), plan)
                .await
                .with_context(|| format!("SetInputAudioTracks «{name}»"))?;
            let on: Vec<u32> = (1..=6)
                .filter(|n| plan[*n as usize - 1] == Some(true))
                .collect();
            let off: Vec<u32> = (1..=6)
                .filter(|n| plan[*n as usize - 1] == Some(false))
                .collect();
            let mut s = format!("Источник «{name}» ({label}):");
            if !on.is_empty() {
                s += &format!(" включены дорожки {};", tracks_list(&on));
            }
            if !off.is_empty() {
                s += &format!(" выключены дорожки {};", tracks_list(&off));
            }
            changes.push(s.trim_end_matches(';').to_owned());
        }
        Ok(())
    }

    async fn fix_scenes(&self, cfg: &Obs, changes: &mut Vec<String>) -> Result<()> {
        let c = &self.inner.client;
        let scenes = c.scenes().list().await.context("GetSceneList")?;
        for (i, name) in cfg.screen_scenes.iter().enumerate() {
            let exists = scenes.scenes.iter().any(|s| s.id.name == *name);
            if !exists {
                c.scenes()
                    .create(name)
                    .await
                    .with_context(|| format!("CreateScene «{name}»"))?;
                changes.push(format!("Создана сцена «{name}»"));
            }
            let has_capture = if exists {
                c.scene_items()
                    .list(name.as_str().into())
                    .await?
                    .iter()
                    .any(|it| {
                        it.input_kind
                            .as_deref()
                            .is_some_and(|k| CAPTURE_KINDS.iter().any(|ck| k.starts_with(ck)))
                    })
            } else {
                false
            };
            if !has_capture {
                self.add_monitor_capture(name, i, changes).await?;
            }
            self.fit_captures_to_canvas(name, changes).await?;
        }
        Ok(())
    }

    /// Захват монитора в натуральную величину больше холста (2560×1440 на 1920×1080) —
    /// в записи видна только часть экрана. Вписываем источник в холст («Растянуть по размеру экрана»).
    pub(crate) async fn capture_misfits(&self, scene: &str) -> Result<Vec<(i64, String)>> {
        let c = &self.inner.client;
        let video = c.config().video_settings().await.context("GetVideoSettings")?;
        let (w, h) = (video.base_width as f32, video.base_height as f32);
        let mut out = Vec::new();
        for it in c.scene_items().list(scene.into()).await? {
            let is_capture = it
                .input_kind
                .as_deref()
                .is_some_and(|k| CAPTURE_KINDS.iter().any(|ck| k.starts_with(ck)));
            if !is_capture {
                continue;
            }
            let t = c.scene_items().transform(scene.into(), it.id).await?;
            let fitted = t.bounds_type != obws::common::BoundsType::None
                && (t.bounds_width - w).abs() < 1.0
                && (t.bounds_height - h).abs() < 1.0
                && t.position_x.abs() < 1.0
                && t.position_y.abs() < 1.0;
            if !fitted {
                out.push((it.id, it.source_name.clone()));
            }
        }
        Ok(out)
    }

    async fn fit_captures_to_canvas(&self, scene: &str, changes: &mut Vec<String>) -> Result<()> {
        use obws::common::{Alignment, BoundsType};
        use obws::requests::scene_items::{Bounds, Position, SceneItemTransform, SetTransform};
        let c = &self.inner.client;
        let video = c.config().video_settings().await.context("GetVideoSettings")?;
        for (id, name) in self.capture_misfits(scene).await? {
            c.scene_items()
                .set_transform(SetTransform {
                    scene: scene.into(),
                    item_id: id,
                    transform: SceneItemTransform {
                        position: Some(Position { x: Some(0.0), y: Some(0.0) }),
                        alignment: Some(Alignment::TOP | Alignment::LEFT),
                        bounds: Some(Bounds {
                            r#type: Some(BoundsType::ScaleInner),
                            alignment: Some(Alignment::CENTER),
                            width: Some(video.base_width as f32),
                            height: Some(video.base_height as f32),
                        }),
                        ..Default::default()
                    },
                })
                .await
                .with_context(|| format!("SetSceneItemTransform «{name}»"))?;
            changes.push(format!(
                "В сцене «{scene}» источник «{name}» вписан в холст {}×{}",
                video.base_width, video.base_height
            ));
        }
        Ok(())
    }

    /// Добавить в сцену источник «Захват монитора» и выбрать в нём монитор `index`.
    async fn add_monitor_capture(
        &self,
        scene: &str,
        index: usize,
        changes: &mut Vec<String>,
    ) -> Result<()> {
        let c = &self.inner.client;
        let kinds = c
            .inputs()
            .list_kinds(true)
            .await
            .context("GetInputKindList")?;
        if !kinds.iter().any(|k| k == "monitor_capture") {
            bail!("в этой версии OBS нет источника monitor_capture (захват монитора)");
        }
        let taken: Vec<String> = c
            .inputs()
            .list(None)
            .await
            .context("GetInputList")?
            .into_iter()
            .map(|i| i.id.name)
            .collect();
        let base = format!("Захват экрана {}", index + 1);
        let input = if taken.contains(&base) {
            format!("{base} (Call)")
        } else {
            base
        };
        c.inputs()
            .create(Create {
                scene: scene.into(),
                input: &input,
                kind: "monitor_capture",
                settings: None::<serde_json::Value>,
                enabled: Some(true),
            })
            .await
            .with_context(|| format!("CreateInput «{input}»"))?;

        let mut chosen = None;
        for prop in MONITOR_PROPS {
            if let Ok(items) = c
                .inputs()
                .properties_list_property_items(input.as_str().into(), prop)
                .await
                // Первый пункт списка — заглушка «выберите экран» с пустым значением.
                && let Some(item) = items
                    .iter()
                    .filter(|i| i.enabled && i.value.as_str().is_some_and(|v| !v.is_empty()))
                    .nth(index)
            {
                chosen = Some((*prop, item.name.clone(), item.value.clone()));
                break;
            }
        }
        match chosen {
            Some((prop, label, value)) => {
                let settings = serde_json::json!({ prop: value });
                c.inputs()
                    .set_settings(SetSettings {
                        input: input.as_str().into(),
                        settings: &settings,
                        overlay: Some(true),
                    })
                    .await
                    .with_context(|| format!("SetInputSettings «{input}»"))?;
                changes.push(format!(
                    "В сцену «{scene}» добавлен захват монитора {} ({label}), источник «{input}»",
                    index + 1
                ));
            }
            None => changes.push(format!(
                "В сцену «{scene}» добавлен источник «{input}», но монитор {} не найден: \
                 выбран монитор по умолчанию, проверьте вручную",
                index + 1
            )),
        }
        Ok(())
    }

    async fn fix_audio_profile(&self, cfg: &Obs, changes: &mut Vec<String>) -> Result<()> {
        let c = &self.inner.client;
        let name = cfg.audio_profile.as_str();
        let list = c.profiles().list().await.context("GetProfileList")?;
        let orig = list.current.clone();
        let exists = list.profiles.iter().any(|p| p == name);
        if !exists {
            // CreateProfile сам переключает на созданный профиль.
            self.switch_profile(name, true).await?;
            changes.push(format!("Создан профиль «{name}»"));
        } else if orig != name {
            self.switch_profile(name, false).await?;
        }
        let res = self.apply_audio_params(cfg, changes).await;
        if orig != name {
            let back = self
                .switch_profile(&orig, false)
                .await
                .with_context(|| format!("возврат на профиль «{orig}»"));
            res.and(back)
        } else {
            res
        }
    }

    /// Выполняется внутри профиля `audio_profile` (он уже текущий).
    async fn apply_audio_params(&self, cfg: &Obs, changes: &mut Vec<String>) -> Result<()> {
        let t = cfg.tracks;
        let mut cur: HashMap<(&str, &str), String> = HashMap::new();
        for (cat, name) in AUDIO_KEYS {
            if let Some(v) = self.param(cat, name).await? {
                cur.insert((cat, name), v);
            }
        }
        let plan = plan_audio_profile(
            &|c, n| cur.get(&(c, n)).cloned(),
            &[t.mix, t.mic, t.desktop],
        );
        let mut notes: Vec<&str> = Vec::new();
        for p in &plan {
            self.set_param(p).await?;
            if !notes.contains(&p.note) {
                notes.push(p.note);
            }
        }
        for n in notes {
            changes.push(format!("Профиль «{}»: {n}", cfg.audio_profile));
        }
        Ok(())
    }
}
