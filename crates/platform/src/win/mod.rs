//! Реализация для Windows.

mod devices;
mod mic;
mod misc;
mod proc;
mod registry;
mod window;

pub use devices::{DefaultAudioDevices, default_audio_devices};
pub use mic::{CombinedMicProvider, WasapiMicProvider};
pub use misc::{
    RegistryAutostart, add_to_user_path, cleanup, computer_name, free_space_bytes,
    remove_from_user_path,
};
pub use registry::RegistryMicProvider;
pub use window::Win32WindowProvider;
