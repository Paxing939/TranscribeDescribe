//! Заглушки для не-Windows: пустые списки или ошибки.

use anyhow::{Result, bail};
use callscribe_core::detection::rules::{ProcessInfo, WindowInfo};
use callscribe_core::platform_api::{Autostart, MicUsageProvider, MicUser, WindowProvider};
use std::path::Path;

macro_rules! mic_stub {
    ($($name:ident),*) => {$(
        #[derive(Debug, Default)]
        pub struct $name;
        impl $name {
            pub fn new() -> Self { Self }
        }
        impl MicUsageProvider for $name {
            fn mic_users(&mut self) -> Result<Vec<MicUser>> { Ok(Vec::new()) }
        }
    )*};
}
mic_stub!(WasapiMicProvider, RegistryMicProvider, CombinedMicProvider);

#[derive(Debug, Default)]
pub struct Win32WindowProvider;
impl Win32WindowProvider {
    pub fn new() -> Self {
        Self
    }
}
impl WindowProvider for Win32WindowProvider {
    fn windows(&mut self) -> Result<Vec<WindowInfo>> {
        Ok(Vec::new())
    }
    fn processes(&mut self) -> Result<Vec<ProcessInfo>> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Default)]
pub struct RegistryAutostart;
impl Autostart for RegistryAutostart {
    fn is_enabled(&self) -> Result<bool> {
        Ok(false)
    }
    fn set_enabled(&self, _enabled: bool, _exe: &Path) -> Result<()> {
        bail!("автозапуск реализован только для Windows")
    }
}

pub fn add_to_user_path(_dir: &Path) -> Result<bool> {
    bail!("PATH пользователя поддерживается только в Windows")
}
pub fn remove_from_user_path(_dir: &Path) -> Result<bool> {
    bail!("PATH пользователя поддерживается только в Windows")
}
pub fn cleanup(_exe_dir: &Path) -> Result<()> {
    Ok(())
}

pub fn free_space_bytes(_path: &Path) -> Result<u64> {
    bail!("свободное место поддерживается только в Windows")
}
pub fn computer_name() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into())
}
