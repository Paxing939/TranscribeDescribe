//! Источник «WASAPI»: активные аудиосессии на устройствах записи (F-10, §11 п. 28–29).

use super::proc::{build_chain, snapshot};
use super::registry::RegistryMicProvider;
use anyhow::{Context, Result, anyhow};
use callscribe_core::platform_api::{MicUsageProvider, MicUser};
use std::collections::HashSet;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator, eCapture,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::Interface;

/// PID всех процессов с активной сессией на любом активном устройстве записи.
/// Вызывать только из потока с MTA.
fn active_capture_pids() -> Result<Vec<u32>> {
    let mut pids = Vec::new();
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .context("создание IMMDeviceEnumerator")?;
        let devices = enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .context("EnumAudioEndpoints")?;
        for i in 0..devices.GetCount()? {
            // Устройство могло пропасть между перечислением и обращением — пропускаем.
            let Ok(device) = devices.Item(i) else {
                continue;
            };
            let Ok(mgr) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let Ok(sessions) = mgr.GetSessionEnumerator() else {
                continue;
            };
            let count = sessions.GetCount().unwrap_or(0);
            for j in 0..count {
                let Ok(ctl) = sessions.GetSession(j) else {
                    continue;
                };
                if !matches!(ctl.GetState(), Ok(s) if s == AudioSessionStateActive) {
                    continue;
                }
                let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                if ctl2.IsSystemSoundsSession() == S_OK {
                    continue;
                }
                // windows-rs превращает AUDCLNT_S_NO_SINGLE_PROCESS в Ok с мусорным PID,
                // поэтому вызываем через vtable и сверяем HRESULT с S_OK.
                let mut pid = 0u32;
                let hr =
                    (Interface::vtable(&ctl2).GetProcessId)(Interface::as_raw(&ctl2), &mut pid);
                if hr == S_OK && pid != 0 && !pids.contains(&pid) {
                    pids.push(pid);
                }
            }
        }
    }
    Ok(pids)
}

/// Выполнить перечисление в отдельном потоке со своим `CoInitializeEx(MTA)` (§11 п. 28).
fn pids_on_com_thread() -> Result<Vec<u32>> {
    std::thread::Builder::new()
        .name("callscribe-wasapi".into())
        .spawn(|| {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }.context("CoInitializeEx")?;
            let r = active_capture_pids();
            unsafe { CoUninitialize() };
            r
        })
        .context("запуск потока WASAPI")?
        .join()
        .map_err(|_| anyhow!("поток WASAPI завершился паникой"))?
}

/// Основной источник микрофона: сессии WASAPI + цепочка родительских процессов.
#[derive(Debug, Default)]
pub struct WasapiMicProvider;

impl WasapiMicProvider {
    pub fn new() -> Self {
        Self
    }
}

impl MicUsageProvider for WasapiMicProvider {
    fn mic_users(&mut self) -> Result<Vec<MicUser>> {
        let pids = pids_on_com_thread()?;
        if pids.is_empty() {
            return Ok(Vec::new());
        }
        let snap = snapshot()?;
        let mut seen = HashSet::new();
        Ok(pids
            .into_iter()
            .filter(|p| seen.insert(*p))
            .map(|pid| build_chain(pid, &snap))
            .filter(|c| !c.is_empty())
            .map(|chain| MicUser {
                chain,
                via: "wasapi",
            })
            .collect())
    }

    fn webcam_in_use(&mut self) -> bool {
        RegistryMicProvider.webcam_in_use()
    }
}

/// WASAPI + записи реестра, которых WASAPI не покрыл. Если WASAPI не работает, один раз
/// пишет в лог и дальше использует только реестр.
#[derive(Debug, Default)]
pub struct CombinedMicProvider {
    wasapi: WasapiMicProvider,
    registry: RegistryMicProvider,
    wasapi_failed_logged: bool,
}

impl CombinedMicProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MicUsageProvider for CombinedMicProvider {
    fn mic_users(&mut self) -> Result<Vec<MicUser>> {
        let (mut users, wasapi_ok) = match self.wasapi.mic_users() {
            Ok(u) => {
                self.wasapi_failed_logged = false;
                (u, true)
            }
            Err(e) => {
                if !self.wasapi_failed_logged {
                    tracing::warn!("WASAPI недоступен, используем только реестр: {e:#}");
                    self.wasapi_failed_logged = true;
                }
                (Vec::new(), false)
            }
        };
        match self.registry.mic_users() {
            Ok(reg) => {
                for r in reg {
                    let covered = users.iter().any(|u| {
                        u.chain.iter().any(|p| {
                            r.chain.iter().any(|rp| {
                                p.exe_name.eq_ignore_ascii_case(&rp.exe_name)
                                    || matches!((&p.package_family, &rp.package_family),
                                        (Some(a), Some(b)) if a.eq_ignore_ascii_case(b))
                            })
                        })
                    });
                    if !covered {
                        users.push(r);
                    }
                }
                Ok(users)
            }
            // WASAPI отработал, реестр — лишь дополнение.
            Err(_) if wasapi_ok => Ok(users),
            Err(e) => Err(e),
        }
    }

    fn webcam_in_use(&mut self) -> bool {
        self.registry.webcam_in_use()
    }
}
