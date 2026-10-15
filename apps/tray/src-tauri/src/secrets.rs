//! `keyring:<name>` → Windows Credential Manager (F-101). Same service name as the CLI.

use anyhow::{Context, Result};
use callscribe_core::llm::SecretResolver;

pub struct Keyring;

impl SecretResolver for Keyring {
    fn get(&self, name: &str) -> Result<String> {
        keyring::Entry::new("CallScribe", name)?
            .get_password()
            .with_context(|| format!("секрет «{name}» не найден"))
    }
}

pub fn set(name: &str, value: &str) -> Result<()> {
    keyring::Entry::new("CallScribe", name)?.set_password(value)?;
    Ok(())
}
