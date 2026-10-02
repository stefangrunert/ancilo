//! Secrets (API keys of cloud providers). Keys never go into `config.toml`,
//! logs, events or answers – only into a [`SecretStore`]: the system keychain
//! in production (see the daemon), memory in tests.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::Result;

pub trait SecretStore: Send + Sync + std::fmt::Debug {
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn set(&self, name: &str, value: &str) -> Result<()>;
    fn delete(&self, name: &str) -> Result<()>;
}

/// Keeps secrets in memory only (tests; also the fallback when no keychain
/// is available – keys are then lost at restart, never written to disk).
#[derive(Debug, Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

impl SecretStore for MemorySecrets {
    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(name.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<()> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

/// Masks a secret for display: `sk-…3f9a`.
pub fn mask(secret: &str) -> String {
    let tail: String = secret
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}
