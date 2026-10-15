//! Platform traits (§4.2). Implemented in `callscribe-platform`, faked in tests.

use crate::detection::rules::{ProcessInfo, WindowInfo};

/// One process holding the microphone right now.
#[derive(Debug, Clone)]
pub struct MicUser {
    /// The capturing process followed by its ancestors (closest first).
    pub chain: Vec<ProcessInfo>,
    /// Where the signal came from: `wasapi` or `registry`.
    pub via: &'static str,
}

pub trait MicUsageProvider: Send {
    fn mic_users(&mut self) -> anyhow::Result<Vec<MicUser>>;
    /// The webcam is in use (extra hint for meta).
    fn webcam_in_use(&mut self) -> bool {
        false
    }
}

pub trait WindowProvider: Send {
    /// Visible top-level windows worth considering (§11, п. 19).
    fn windows(&mut self) -> anyhow::Result<Vec<WindowInfo>>;
    /// Running processes (for `meeting_process`).
    fn processes(&mut self) -> anyhow::Result<Vec<ProcessInfo>>;
}

pub trait Autostart {
    fn is_enabled(&self) -> anyhow::Result<bool>;
    fn set_enabled(&self, enabled: bool, exe: &std::path::Path) -> anyhow::Result<()>;
}
