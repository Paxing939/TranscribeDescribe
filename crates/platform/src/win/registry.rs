//! Источник «реестр»: `CapabilityAccessManager\ConsentStore` (F-10, запасной способ).

use crate::pure::consent_key_to_process;
use anyhow::Result;
use callscribe_core::detection::rules::ProcessInfo;
use callscribe_core::platform_api::{MicUsageProvider, MicUser};
use windows_registry::{CURRENT_USER, Key};

const CONSENT: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";

/// Занято ли устройство прямо сейчас: `LastUsedTimeStop == 0`, при этом `LastUsedTimeStart != 0`.
fn in_use(key: &Key) -> bool {
    key.get_u64("LastUsedTimeStop").is_ok_and(|stop| stop == 0)
        && !matches!(key.get_u64("LastUsedTimeStart"), Ok(0))
}

/// Процессы, у которых сейчас занято устройство (`microphone` или `webcam`).
fn active_in_branch(branch: &str) -> Result<Vec<ProcessInfo>> {
    let root = CURRENT_USER.open(format!(r"{CONSENT}\{branch}"))?;
    let mut out = Vec::new();
    for name in root.keys()? {
        if name == "NonPackaged" {
            if let Ok(np) = root.open("NonPackaged") {
                for sub in np.keys()? {
                    if np.open(&sub).is_ok_and(|k| in_use(&k)) {
                        out.push(consent_key_to_process(&sub, true));
                    }
                }
            }
        } else if root.open(&name).is_ok_and(|k| in_use(&k)) {
            out.push(consent_key_to_process(&name, false));
        }
    }
    Ok(out)
}

#[derive(Debug, Default)]
pub struct RegistryMicProvider;

impl RegistryMicProvider {
    pub fn new() -> Self {
        Self
    }
}

impl MicUsageProvider for RegistryMicProvider {
    fn mic_users(&mut self) -> Result<Vec<MicUser>> {
        Ok(active_in_branch("microphone")?
            .into_iter()
            .map(|p| MicUser {
                chain: vec![p],
                via: "registry",
            })
            .collect())
    }

    fn webcam_in_use(&mut self) -> bool {
        active_in_branch("webcam").is_ok_and(|v| !v.is_empty())
    }
}
