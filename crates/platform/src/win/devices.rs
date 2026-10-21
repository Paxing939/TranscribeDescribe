//! Windows default audio devices: OBS inputs set to «По умолчанию» open these.

use anyhow::{Context, Result, anyhow};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{EDataFlow, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eConsole, eRender};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, STGM_READ};

/// Default devices (console role): `(capture, render)` friendly names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefaultAudioDevices {
    pub capture: Option<String>,
    pub render: Option<String>,
}

fn friendly_name(enumerator: &IMMDeviceEnumerator, flow: EDataFlow) -> Option<String> {
    unsafe {
        let device = enumerator.GetDefaultAudioEndpoint(flow, eConsole).ok()?;
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        let s = value.to_string();
        (!s.is_empty()).then_some(s)
    }
}

/// Runs on its own MTA thread (§11 п. 28).
pub fn default_audio_devices() -> Result<DefaultAudioDevices> {
    std::thread::Builder::new()
        .name("default-audio-devices".into())
        .spawn(|| -> Result<DefaultAudioDevices> {
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("CoInitializeEx")?;
                let r = (|| {
                    let e: IMMDeviceEnumerator =
                        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).context("IMMDeviceEnumerator")?;
                    Ok(DefaultAudioDevices { capture: friendly_name(&e, eCapture), render: friendly_name(&e, eRender) })
                })();
                CoUninitialize();
                r
            }
        })?
        .join()
        .map_err(|_| anyhow!("поток аудиоустройств упал"))?
}
