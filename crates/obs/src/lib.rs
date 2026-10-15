//! OBS-клиент (F-20…F-28): подключение, запуск OBS, команды записи, события,
//! уровни звука, проверка настроек.
//!
//! Переподключение: [`Supervisor`] держит соединение с backoff и переживает
//! перезапуск OBS; без него трей должен сам вызвать [`ObsClient::connect`]
//! после [`ObsEvent::Disconnected`].

pub mod checks;
pub mod client;
pub mod fix;
pub mod launch;
pub mod levels;
pub mod supervisor;
pub mod ws_config;

pub use client::{Inputs, ObsClient, ObsEvent, RecordStatus, StartedRecording, resolve_password};
pub use launch::{connect_or_launch, launch_obs};
pub use supervisor::{Backoff, ConnState, Supervisor};
pub use ws_config::{WsConfig, read_websocket_config};
