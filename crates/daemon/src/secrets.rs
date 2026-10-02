//! API keys in the system keychain (macOS Keychain, Linux kernel keyring).
//! If the keychain is unavailable, keys are kept in memory only – never on disk.

use ancilo_core::Result;
use ancilo_core::secrets::{MemorySecrets, SecretStore};

#[derive(Debug)]
pub struct KeyringSecrets {
    service: String,
    fallback: MemorySecrets,
}

impl KeyringSecrets {
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_string(),
            fallback: MemorySecrets::default(),
        }
    }

    fn entry(&self, name: &str) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(&self.service, name)
    }
}

impl SecretStore for KeyringSecrets {
    fn get(&self, name: &str) -> Result<Option<String>> {
        // A key kept in memory because the keychain refused it is the newer one.
        if let Some(v) = self.fallback.get(name)? {
            return Ok(Some(v));
        }
        match self.entry(name).and_then(|e| e.get_password()) {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => self.fallback.get(name),
            Err(e) => {
                tracing::warn!(error = %e, "keychain not readable");
                self.fallback.get(name)
            }
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<()> {
        if let Err(e) = self.entry(name).and_then(|e| e.set_password(value)) {
            tracing::warn!(error = %e, "keychain not writable – key kept in memory until restart");
            return self.fallback.set(name, value);
        }
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        self.fallback.delete(name)?;
        match self.entry(name).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            // Saying "deleted" while the key is still there would be wrong.
            Err(e) => Err(ancilo_core::Error::unavailable(format!(
                "the key could not be removed from the keychain: {e}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: M6-AC-06
    /// Real keychain round trip (may ask for permission on first use).
    #[test]
    #[ignore = "touches the real system keychain"]
    fn keychain_round_trip() {
        let s = KeyringSecrets::new("ancilo-test");
        let name = format!("t-{}", std::process::id());
        s.set(&name, "secret-value").unwrap();
        assert_eq!(s.get(&name).unwrap().as_deref(), Some("secret-value"));
        s.delete(&name).unwrap();
        assert_eq!(s.get(&name).unwrap(), None);
    }
}
