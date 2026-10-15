//! Child processes: worker (§4.1), doctor console, toasts.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

fn cli_exe() -> Result<PathBuf> {
    let me = std::env::current_exe()?;
    let p = me.with_file_name(if cfg!(windows) {
        "callscribe.exe"
    } else {
        "callscribe"
    });
    if p.is_file() {
        Ok(p)
    } else {
        anyhow::bail!("не найден {}", p.display())
    }
}

/// `callscribe.exe run-queue`: hidden console, below-normal priority, no pipes (§4.1).
pub fn worker() -> Result<Child> {
    let exe = cli_exe()?;
    let mut cmd = Command::new(&exe);
    cmd.arg("run-queue")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    let ch = cmd
        .spawn()
        .with_context(|| format!("запуск {}", exe.display()))?;
    tracing::info!("worker запущен (pid {})", ch.id());
    Ok(ch)
}

/// «Проверка системы»: a visible console with `callscribe doctor --wait` (F-03).
pub fn doctor_console() {
    let Ok(exe) = cli_exe() else {
        tracing::error!("callscribe.exe не найден рядом с треем");
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.args(["doctor", "--wait"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }
    if let Err(e) = cmd.spawn() {
        tracing::error!("doctor: {e}");
    }
}

/// Duplicate signal through the system notification (no buttons on Windows, §11 п. 36).
pub fn toast(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}
