//! Запуск OBS (F-21, §11 п. 2).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use callscribe_core::config::{Obs, expand_home};

use crate::client::{ObsClient, is_unreachable};

const POLL_PERIOD: Duration = Duration::from_secs(1);

/// Запустить OBS: рабочая директория = папка exe (иначе OBS не находит свои
/// файлы и падает), аргументы — `launch_args`. Процесс отвязан от нашего.
pub fn launch_obs(cfg: &Obs) -> Result<()> {
    let exe: PathBuf = expand_home(&cfg.exe_path);
    if !exe.is_file() {
        bail!("OBS не найден по пути {} (obs.exe_path)", exe.display());
    }
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("у пути {} нет родительской папки", exe.display()))?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.current_dir(dir)
        .args(&cfg.launch_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: OBS переживёт закрытие нашего процесса.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    cmd.spawn()
        .with_context(|| format!("не удалось запустить {}", exe.display()))?;
    Ok(())
}

/// Запущен ли процесс OBS (только Windows; в остальных ОС — `false`).
async fn obs_process_running() -> bool {
    #[cfg(windows)]
    {
        let out = tokio::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq obs64.exe", "/NH", "/FO", "CSV"])
            .output()
            .await;
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .to_lowercase()
                .contains("obs64.exe"),
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Подключиться; если OBS не запущен и `launch_if_not_running` — запустить и
/// ждать websocket до `wait` (F-21, 30 с). Неверный пароль и подобные ошибки
/// возвращаются сразу, без запуска.
pub async fn connect_or_launch(
    cfg: &Obs,
    secret: Option<String>,
    wait: Duration,
) -> Result<ObsClient> {
    let first = match ObsClient::connect(cfg, secret.clone()).await {
        Ok(c) => return Ok(c),
        Err(e) if !is_unreachable(&e) => return Err(e),
        Err(e) => e,
    };
    if !cfg.launch_if_not_running {
        return Err(first);
    }
    if obs_process_running().await {
        return Err(first.context(
            "OBS запущен, но websocket не отвечает: включите «Инструменты → Настройки сервера WebSocket»",
        ));
    }
    launch_obs(cfg)?;
    let deadline = Instant::now() + wait;
    let mut last = first;
    while Instant::now() < deadline {
        tokio::time::sleep(POLL_PERIOD).await;
        match ObsClient::connect(cfg, secret.clone()).await {
            Ok(c) => return Ok(c),
            Err(e) => last = e,
        }
    }
    Err(last.context(format!(
        "OBS запущен, но websocket не ответил за {} с",
        wait.as_secs()
    )))
}
