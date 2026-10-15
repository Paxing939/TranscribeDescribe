//! Secrets in the OS keyring (F-101): `keyring:<name>` → Windows Credential Manager.

use anyhow::{Context, Result};
use callscribe_core::llm::SecretResolver;

pub const SERVICE: &str = "CallScribe";

pub struct Keyring;

impl SecretResolver for Keyring {
    fn get(&self, name: &str) -> Result<String> {
        keyring::Entry::new(SERVICE, name)?
            .get_password()
            .with_context(|| {
                format!("секрет «{name}» не найден в хранилище (callscribe secret set {name})")
            })
    }
}

pub fn set(name: &str, value: &str) -> Result<()> {
    keyring::Entry::new(SERVICE, name)?.set_password(value)?;
    Ok(())
}

pub fn delete(name: &str) -> Result<()> {
    keyring::Entry::new(SERVICE, name)?.delete_credential()?;
    Ok(())
}
