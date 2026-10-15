//! CallScribe core: everything that does not need Tauri, OBS, GPU or native STT libraries.

pub mod config;
pub mod config_edit;
pub mod detection;
pub mod llm;
pub mod models;
pub mod paths;
pub mod pipeline;
pub mod platform_api;
pub mod storage;
pub mod stt;

pub use config::Config;
pub use paths::AppPaths;
