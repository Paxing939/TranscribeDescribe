//! Платформенный слой (F-04, F-10, F-11): микрофон, окна, автозапуск, PATH.
//!
//! Весь код Windows — под `#[cfg(windows)]`; на других системах — заглушки,
//! возвращающие пустые списки или ошибки, чтобы крейт собирался везде.

pub mod pure;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod stub;
#[cfg(not(windows))]
pub use stub::*;
