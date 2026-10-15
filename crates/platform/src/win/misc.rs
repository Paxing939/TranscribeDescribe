//! Автозапуск (F-04), PATH пользователя (M5), свободное место (F-85), имя компьютера.

use crate::pure::{path_list_add, path_list_remove};
use anyhow::{Context, Result};
use callscribe_core::platform_api::Autostart;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::WindowsProgramming::GetComputerNameW;
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use windows::core::{PCWSTR, PWSTR};
use windows_registry::{CURRENT_USER, Type};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "CallScribe";

/// Автозапуск через `HKCU\...\Run` (§11 п. 37): `HKLM` не трогаем.
#[derive(Debug, Default, Clone, Copy)]
pub struct RegistryAutostart;

impl Autostart for RegistryAutostart {
    fn is_enabled(&self) -> Result<bool> {
        match CURRENT_USER.open(RUN_KEY) {
            Ok(k) => Ok(k.get_string(RUN_VALUE).is_ok()),
            Err(_) => Ok(false),
        }
    }

    fn set_enabled(&self, enabled: bool, exe: &Path) -> Result<()> {
        let key = CURRENT_USER
            .create(RUN_KEY)
            .context("открытие HKCU\\...\\Run")?;
        if enabled {
            key.set_string(RUN_VALUE, format!("\"{}\"", exe.display()))
                .context("запись автозапуска")?;
        } else if key.get_value(RUN_VALUE).is_ok() {
            key.remove_value(RUN_VALUE)
                .context("удаление автозапуска")?;
        }
        Ok(())
    }
}

/// Сообщить приложениям об изменении переменных окружения.
fn broadcast_env_change() {
    let env: Vec<u16> = "Environment"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(env.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            5000,
            None,
        );
    }
}

/// Прочитать `HKCU\Environment\Path`: (значение, это REG_EXPAND_SZ). Нет значения — пусто.
fn read_user_path(key: &windows_registry::Key) -> Result<(String, bool)> {
    match key.get_value("Path") {
        Ok(v) => {
            let expand = v.ty() == Type::ExpandString;
            Ok((
                String::try_from(v).context("PATH пользователя не строка")?,
                expand,
            ))
        }
        Err(_) => Ok((String::new(), true)),
    }
}

fn write_user_path(key: &windows_registry::Key, value: &str, expand: bool) -> Result<()> {
    if expand {
        key.set_expand_string("Path", value)
    } else {
        key.set_string("Path", value)
    }
    .context("запись PATH пользователя")
}

/// Добавить каталог в `HKCU\Environment\Path`. `true` — запись изменена.
pub fn add_to_user_path(dir: &Path) -> Result<bool> {
    let key = CURRENT_USER
        .create("Environment")
        .context("открытие HKCU\\Environment")?;
    let (cur, expand) = read_user_path(&key)?;
    let Some(new) = path_list_add(&cur, &dir.to_string_lossy()) else {
        return Ok(false);
    };
    write_user_path(&key, &new, expand)?;
    broadcast_env_change();
    Ok(true)
}

/// Убрать каталог из `HKCU\Environment\Path`. `true` — запись изменена.
pub fn remove_from_user_path(dir: &Path) -> Result<bool> {
    let key = CURRENT_USER
        .create("Environment")
        .context("открытие HKCU\\Environment")?;
    let (cur, expand) = read_user_path(&key)?;
    let Some(new) = path_list_remove(&cur, &dir.to_string_lossy()) else {
        return Ok(false);
    };
    write_user_path(&key, &new, expand)?;
    broadcast_env_change();
    Ok(true)
}

/// `callscribe cleanup` (F-130): убрать автозапуск и запись в PATH. Ошибка одного шага не
/// мешает второму; возвращается первая ошибка.
pub fn cleanup(exe_dir: &Path) -> Result<()> {
    let a = RegistryAutostart.set_enabled(false, Path::new(""));
    let p = remove_from_user_path(exe_dir).map(|_| ());
    a.and(p)
}

/// Свободное место на томе, где лежит `path` (для несуществующего пути — ближайший существующий предок).
pub fn free_space_bytes(path: &Path) -> Result<u64> {
    let existing = path.ancestors().find(|p| p.exists()).unwrap_or(path);
    let wide: Vec<u16> = existing
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free), None, None) }
        .with_context(|| format!("GetDiskFreeSpaceExW({})", existing.display()))?;
    Ok(free)
}

/// Имя компьютера (для `meta.json`); при сбое — `unknown`.
pub fn computer_name() -> String {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    match unsafe { GetComputerNameW(Some(PWSTR(buf.as_mut_ptr())), &mut len) } {
        Ok(()) => String::from_utf16_lossy(&buf[..len as usize]),
        Err(_) => std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into()),
    }
}
