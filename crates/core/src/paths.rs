//! Application directories (§7.1, §11 п. 41).
//!
//! `directories::ProjectDirs` adds `config`/`data` subfolders on Windows, so we use
//! `BaseDirs` and append our own folder name.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
}

#[cfg(windows)]
const APP_DIR: &str = "CallScribe";
#[cfg(not(windows))]
const APP_DIR: &str = "callscribe";

impl AppPaths {
    /// Standard locations: `%APPDATA%\CallScribe` and `%LOCALAPPDATA%\CallScribe`.
    /// `CALLSCRIBE_HOME` overrides both (tests, portable use).
    pub fn new() -> AppPaths {
        if let Some(home) = std::env::var_os("CALLSCRIBE_HOME") {
            return AppPaths::at(Path::new(&home));
        }
        let base = directories::BaseDirs::new().expect("no home directory");
        AppPaths {
            config_dir: base.config_dir().join(APP_DIR),
            data_dir: base.data_local_dir().join(APP_DIR),
        }
    }

    /// Everything under one root.
    pub fn at(root: &Path) -> AppPaths {
        AppPaths {
            config_dir: root.to_path_buf(),
            data_dir: root.to_path_buf(),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("data.db")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }
    pub fn worker_lock(&self) -> PathBuf {
        self.data_dir.join("worker.lock")
    }
    pub fn tmp_dir(&self) -> PathBuf {
        self.data_dir.join("tmp")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(self.logs_dir())?;
        std::fs::create_dir_all(self.tmp_dir())?;
        Ok(())
    }
}

impl Default for AppPaths {
    fn default() -> Self {
        AppPaths::new()
    }
}
