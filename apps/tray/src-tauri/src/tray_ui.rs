//! Tray icon and menu (F-02, F-03).

use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Wry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconState {
    Idle,
    Detected,
    Recording,
    Processing,
    Error,
}

/// A 32×32 dot; colour encodes the state (F-02).
pub fn icon(state: IconState) -> Image<'static> {
    let (r, g, b) = match state {
        IconState::Idle => (140, 146, 156),
        IconState::Detected => (240, 180, 40),
        IconState::Recording => (226, 54, 54),
        IconState::Processing => (60, 130, 240),
        IconState::Error => (240, 120, 30),
    };
    let size = 32u32;
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    let c = (size as f32 - 1.0) / 2.0;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let (pr, pg, pb, a) = if d <= 9.0 {
                (r, g, b, 255)
            } else if d <= 13.5 {
                (235, 235, 235, 255)
            } else if d <= 14.5 {
                (235, 235, 235, ((14.5 - d) * 255.0) as u8)
            } else {
                (0, 0, 0, 0)
            };
            px.extend_from_slice(&[pr, pg, pb, a]);
        }
    }
    Image::new_owned(px, size, size)
}

pub struct MenuModel {
    pub status: String,
    pub recording: bool,
    pub paused: bool,
    pub screens: usize,
    pub detection_paused: bool,
    /// (id suffix, label) of recent calls.
    pub recent: Vec<(String, String)>,
}

pub fn build_menu(app: &AppHandle, m: &MenuModel) -> tauri::Result<(Menu<Wry>, MenuItem<Wry>)> {
    let status = MenuItem::with_id(app, "status", &m.status, false, None::<&str>)?;
    let sep = || PredefinedMenuItem::separator(app);

    let mut start_items: Vec<MenuItem<Wry>> = vec![MenuItem::with_id(
        app,
        "start:audio",
        "Только звук",
        !m.recording,
        None::<&str>,
    )?];
    for i in 1..=m.screens.max(1) {
        start_items.push(MenuItem::with_id(
            app,
            format!("start:screen:{i}"),
            format!("Экран {i}"),
            !m.recording,
            None::<&str>,
        )?);
    }
    let start_refs: Vec<&dyn IsMenuItem<Wry>> = start_items
        .iter()
        .map(|i| i as &dyn IsMenuItem<Wry>)
        .collect();
    let start = Submenu::with_items(app, "Начать запись", !m.recording, &start_refs)?;
    let stop = MenuItem::with_id(app, "stop", "Остановить запись", m.recording, None::<&str>)?;
    let pause = MenuItem::with_id(
        app,
        "pause",
        if m.paused {
            "Продолжить запись"
        } else {
            "Пауза"
        },
        m.recording,
        None::<&str>,
    )?;
    let mark = MenuItem::with_id(
        app,
        "bookmark",
        "Поставить метку",
        m.recording,
        None::<&str>,
    )?;

    let det = if m.detection_paused {
        Submenu::with_items(
            app,
            "Отслеживание звонков",
            true,
            &[&MenuItem::with_id(
                app,
                "detect:resume",
                "Возобновить",
                true,
                None::<&str>,
            )?],
        )?
    } else {
        Submenu::with_items(
            app,
            "Не отслеживать звонки",
            true,
            &[
                &MenuItem::with_id(app, "detect:pause:1h", "1 час", true, None::<&str>)?,
                &MenuItem::with_id(
                    app,
                    "detect:pause:forever",
                    "До перезапуска",
                    true,
                    None::<&str>,
                )?,
            ],
        )?
    };

    let recent_items: Vec<MenuItem<Wry>> = if m.recent.is_empty() {
        vec![MenuItem::with_id(
            app,
            "recent:none",
            "Пока нет",
            false,
            None::<&str>,
        )?]
    } else {
        m.recent
            .iter()
            .map(|(id, label)| {
                MenuItem::with_id(app, format!("recent:{id}"), label, true, None::<&str>)
            })
            .collect::<tauri::Result<_>>()?
    };
    let recent_refs: Vec<&dyn IsMenuItem<Wry>> = recent_items
        .iter()
        .map(|i| i as &dyn IsMenuItem<Wry>)
        .collect();
    let recent = Submenu::with_items(app, "Последние звонки", true, &recent_refs)?;

    let folder = MenuItem::with_id(
        app,
        "open:calls",
        "Открыть папку звонков",
        true,
        None::<&str>,
    )?;
    let history = MenuItem::with_id(app, "history", "История…", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Настройки…", true, None::<&str>)?;
    let config_file = MenuItem::with_id(app, "open:config", "Открыть config.toml", true, None::<&str>)?;
    let reload = MenuItem::with_id(app, "reload", "Перечитать настройки", true, None::<&str>)?;
    let doctor = MenuItem::with_id(app, "doctor", "Проверка системы", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "open:logs", "Логи", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &status,
            &sep()?,
            &start,
            &stop,
            &pause,
            &mark,
            &sep()?,
            &det,
            &recent,
            &history,
            &sep()?,
            &folder,
            &settings,
            &config_file,
            &reload,
            &doctor,
            &logs,
            &sep()?,
            &quit,
        ],
    )?;
    Ok((menu, status))
}
