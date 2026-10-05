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

/// Deletes every key of a home (its keychain service) – when Ancilo is
/// removed. Returns how many were deleted.
///
/// macOS lists the service's entries. Elsewhere the keychain cannot be
/// listed: the names Ancilo uses are deleted (`web.serper`, and a key per
/// model – `model_keys`, the home's models).
pub fn delete_all(service: &str, model_keys: &[String]) -> Result<usize> {
    #[cfg(target_os = "macos")]
    let names = {
        let _ = model_keys;
        macos_names(service)?
    };
    #[cfg(not(target_os = "macos"))]
    let names: Vec<String> = std::iter::once(crate::web::SERPER_SECRET.to_string())
        .chain(model_keys.iter().cloned())
        .collect();
    let mut deleted = 0;
    for name in names {
        match keyring::Entry::new(service, &name).and_then(|e| e.delete_credential()) {
            Ok(()) => deleted += 1,
            Err(keyring::Error::NoEntry) => {}
            Err(e) => {
                return Err(ancilo_core::Error::unavailable(ancilo_core::messages::msg(
                    "remove.key_left",
                    &[("name", &name), ("why", &e)],
                )));
            }
        }
    }
    Ok(deleted)
}

/// The names (accounts) of a service's keychain entries.
#[cfg(target_os = "macos")]
fn macos_names(service: &str) -> Result<Vec<String>> {
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
    const NOT_FOUND: i32 = -25300; // errSecItemNotFound
    let found = match ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(service)
        .load_attributes(true)
        .limit(Limit::All)
        .search()
    {
        Ok(found) => found,
        Err(e) if e.code() == NOT_FOUND => return Ok(Vec::new()),
        Err(e) => {
            return Err(ancilo_core::Error::unavailable(ancilo_core::messages::msg(
                "remove.keychain_unreadable",
                &[("why", &e)],
            )));
        }
    };
    Ok(found
        .iter()
        .filter_map(|r| r.simplify_dict()?.get("acct").cloned())
        .collect())
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

    /// Removing Ancilo finds a home's keys in the real keychain.
    #[test]
    #[ignore = "touches the real system keychain"]
    fn delete_all_finds_the_services_keys() {
        let service = format!("ancilo-test-{}", std::process::id());
        let s = KeyringSecrets::new(&service);
        s.set("web.serper", "a").unwrap();
        s.set("model:x", "b").unwrap();
        assert_eq!(delete_all(&service, &["model:x".into()]).unwrap(), 2);
        assert_eq!(s.get("model:x").unwrap(), None);
        assert_eq!(delete_all(&service, &[]).unwrap(), 0, "nothing left");
    }
}
