//! `callscribe-tray.exe` — tray app (F-01…F-04, §4.1). GUI subsystem: no console (§11, п. 25).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod controller;
mod history;
mod settings;
mod overlays;
mod secrets;
mod spawn;
mod tray_ui;

use std::sync::Arc;

use callscribe_core::platform_api::Autostart;
use callscribe_core::{AppPaths, Config};
use serde_json::Value;
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tokio::sync::mpsc;

use crate::controller::{Cmd, Controller};
use crate::overlays::Overlays;

pub(crate) struct Tx(pub(crate) mpsc::Sender<Cmd>);

#[tauri::command]
fn view_data(window: tauri::WebviewWindow) -> Value {
    controller::view_data(window.app_handle(), window.label())
}

#[tauri::command]
async fn ui_action(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Tx>,
    action: String,
    value: Option<Value>,
) -> Result<(), String> {
    state
        .0
        .send(Cmd::Ui {
            label: window.label().to_string(),
            action,
            value: value.unwrap_or(Value::Null),
        })
        .await
        .map_err(|e| e.to_string())
}

fn init_logging(paths: &AppPaths) -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::prelude::*;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("tray")
        .filename_suffix("log")
        .max_log_files(7)
        .build(paths.logs_dir())
        .expect("log dir");
    let (nb, guard) = tracing_appender::non_blocking(appender);
    let filter = tracing_subscriber::EnvFilter::try_from_env("CALLSCRIBE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tao=warn,wry=warn,obws=warn"));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(nb)
                .with_ansi(false)
                .with_filter(filter),
        )
        .init();
    guard
}

fn main() {
    let paths = AppPaths::new();
    let _ = paths.ensure();
    let _guard = init_logging(&paths);
    std::panic::set_hook(Box::new(|info| tracing::error!("panic: {info}")));

    // F-103: no config yet → first run, the settings window opens as a wizard.
    let first_run = !paths.config_file().exists();
    let (cfg, cfg_error) = match Config::load_default_location(&paths) {
        Ok(c) => (c, None),
        Err(e) => {
            tracing::error!("config: {e:#}");
            (Config::default(), Some(format!("{e:#}")))
        }
    };
    tracing::info!("CallScribe {} запущен", env!("CARGO_PKG_VERSION"));
    let (tx, rx) = mpsc::channel::<Cmd>(64);

    let hotkeys = [
        (cfg.hotkeys.toggle_recording.clone(), "hotkey:toggle"),
        (cfg.hotkeys.bookmark.clone(), "bookmark"),
        (cfg.hotkeys.pause.clone(), "pause"),
    ];
    let tx_keys = tx.clone();
    let key_map: Vec<(String, &'static str)> = hotkeys
        .iter()
        .map(|(k, v)| (k.to_lowercase(), *v))
        .collect();

    let app = tauri::Builder::default()
        // single-instance must be the first plugin (§11, п. 26).
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {
            tracing::info!("повторный запуск — уже работаю в трее");
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |_app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let s = shortcut.into_string().to_lowercase();
                    if let Some((_, id)) = key_map
                        .iter()
                        .find(|(k, _)| normalize_hotkey(k) == normalize_hotkey(&s))
                    {
                        let _ = tx_keys.try_send(Cmd::Menu(id.to_string()));
                    }
                })
                .build(),
        )
        .manage(Tx(tx.clone()))
        .manage(settings::FirstRun(first_run))
        .invoke_handler(tauri::generate_handler![view_data, ui_action, history::history_list, history::history_action, settings::settings_get, settings::settings_save, settings::settings_doctor])
        .setup(move |app| {
            let handle = app.handle().clone();
            let _ = controller::ensure_dirs(&cfg);

            // F-04: autostart in HKCU\…\Run (own code, §11 п. 37). Debug builds never touch it.
            if let (false, Ok(exe)) = (cfg!(debug_assertions), std::env::current_exe()) {
                let a = callscribe_platform::RegistryAutostart;
                if let Err(e) = a.set_enabled(cfg.general.autostart, &exe) {
                    tracing::warn!("автозапуск: {e:#}");
                }
            }

            // F-110: global hotkeys.
            for (k, _) in &hotkeys {
                if let Err(e) = app.global_shortcut().register(k.as_str()) {
                    tracing::warn!("хоткей {k} не зарегистрирован: {e}");
                }
            }

            let overlays = Arc::new(Overlays::new(
                handle.clone(),
                cfg.overlay.position.clone(),
                cfg.overlay.exclude_from_capture,
            ));
            app.manage(overlays.clone());

            let tx_menu = tx.clone();
            TrayIconBuilder::with_id("main")
                .icon(tray_ui::icon(tray_ui::IconState::Idle))
                .tooltip("CallScribe")
                .show_menu_on_left_click(true)
                .on_menu_event(move |_app, ev| {
                    let id = ev.id().as_ref().to_string();
                    let cmd = if id == "quit" {
                        Cmd::Quit
                    } else {
                        Cmd::Menu(id)
                    };
                    let _ = tx_menu.try_send(cmd);
                })
                .build(app)?;

            // The OBS supervisor spawns tokio tasks, so the controller is created inside the runtime.
            let (h2, cfg2) = (handle.clone(), cfg.clone());
            tauri::async_runtime::spawn(async move {
                match Controller::new(h2.clone(), AppPaths::new(), cfg2, overlays) {
                    Ok(ctl) => ctl.run(rx).await,
                    Err(e) => {
                        tracing::error!("контроллер не запустился: {e:#}");
                        spawn::toast(&h2, "CallScribe не запустился", &format!("{e:#}"));
                    }
                }
            });
            if first_run {
                settings::open_window(&handle);
            }
            if let Some(e) = cfg_error.clone() {
                spawn::toast(
                    &handle,
                    "Ошибка в config.toml — работаю с настройками по умолчанию",
                    &e,
                );
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("tauri app");

    app.run(|_app, event| {
        // Closing the last overlay must not quit the tray app; «Выход» comes with Some(code) (§11, п. 26).
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}

/// "Ctrl+Alt+R" and "control+alt+KeyR" → the same string.
fn normalize_hotkey(s: &str) -> String {
    s.to_lowercase()
        .replace("control", "ctrl")
        .replace("key", "")
        .split('+')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("+")
}
