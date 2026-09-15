use crate::error::{CoreError, Result};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait SecretStore: Send + Sync {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()>;
    fn get(&self, key_ref: &str) -> Result<String>;
    fn delete(&self, key_ref: &str) -> Result<()>;

    /// Migrate a secret from the old key `from` to the new key `to` (used when the
    /// key_ref prefix changes). Returns `Ok(false)` when `from` does not exist.
    ///
    /// Default impl (for key_ref-keyed stores: MockStore/FileStore):
    /// get(from) → set(to) → delete(from). KeyringStore overrides this to cross service names.
    fn migrate_key(&self, from: &str, to: &str) -> Result<bool> {
        match self.get(from) {
            Ok(secret) => {
                self.set(to, &secret)?;
                self.delete(from)?;
                Ok(true)
            }
            Err(CoreError::SecretNotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// System keyring implementation (macOS Keychain / Windows Credential Manager / Linux Secret Service).
pub struct KeyringStore {
    service: String,
}

/// Current keyring service name.
pub const SERVICE: &str = "xfade";
/// Legacy service name (pre-v0.6), used to read old entries during migration.
pub const LEGACY_SERVICE: &str = "agent-switch";

impl KeyringStore {
    pub fn new() -> Self {
        Self {
            service: SERVICE.into(),
        }
    }

    fn entry(&self, key_ref: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, key_ref).map_err(|e| CoreError::Keyring(e.to_string()))
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeyringStore {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()> {
        self.entry(key_ref)?
            .set_password(secret)
            .map_err(|e| CoreError::Keyring(e.to_string()))
    }

    fn get(&self, key_ref: &str) -> Result<String> {
        self.entry(key_ref)?.get_password().map_err(|e| match e {
            keyring::Error::NoEntry => CoreError::SecretNotFound(key_ref.to_string()),
            other => CoreError::Keyring(other.to_string()),
        })
    }

    fn delete(&self, key_ref: &str) -> Result<()> {
        match self.entry(key_ref)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CoreError::Keyring(e.to_string())),
        }
    }

    /// Cross-service migration: the old entry lives at `(LEGACY_SERVICE, from)`, the new
    /// one is written to `(SERVICE, to)`. read old → write new → delete old; a hard
    /// failure at any step errors (the caller handles it best-effort and retries on
    /// next startup).
    fn migrate_key(&self, from: &str, to: &str) -> Result<bool> {
        let legacy = keyring::Entry::new(LEGACY_SERVICE, from)
            .map_err(|e| CoreError::Keyring(e.to_string()))?;
        match legacy.get_password() {
            Ok(secret) => {
                self.set(to, &secret)?;
                match legacy.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(true),
                    Err(e) => Err(CoreError::Keyring(e.to_string())),
                }
            }
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(CoreError::Keyring(e.to_string())),
        }
    }
}

/// In-memory implementation for tests.
#[derive(Default)]
pub struct MockStore {
    map: Mutex<HashMap<String, String>>,
}

/// File-persisted secret store, used both for CLI integration tests (shared across
/// processes) and as an opt-in alternative to the system keyring (via
/// `XFADE_SECRETS=file`). Secrets are stored as JSON with owner-only permissions.
pub struct FileStore {
    path: std::path::PathBuf,
}

impl FileStore {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    fn read_map(&self) -> HashMap<String, String> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write_map(&self, map: &HashMap<String, String>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CoreError::Keyring(format!("create secrets dir: {e}")))?;
        }
        let s = serde_json::to_string(map)
            .map_err(|e| CoreError::Keyring(format!("serialize secrets: {e}")))?;
        std::fs::write(&self.path, s)
            .map_err(|e| CoreError::Keyring(format!("write secrets: {e}")))?;
        // Restrict to owner read/write (best-effort; a no-op on non-unix platforms).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

impl SecretStore for FileStore {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()> {
        let mut map = self.read_map();
        map.insert(key_ref.to_string(), secret.to_string());
        self.write_map(&map)
    }

    fn get(&self, key_ref: &str) -> Result<String> {
        self.read_map()
            .get(key_ref)
            .cloned()
            .ok_or_else(|| CoreError::SecretNotFound(key_ref.to_string()))
    }

    fn delete(&self, key_ref: &str) -> Result<()> {
        let mut map = self.read_map();
        map.remove(key_ref);
        self.write_map(&map)
    }
}

impl SecretStore for MockStore {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()> {
        self.map
            .lock()
            .unwrap()
            .insert(key_ref.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, key_ref: &str) -> Result<String> {
        self.map
            .lock()
            .unwrap()
            .get(key_ref)
            .cloned()
            .ok_or_else(|| CoreError::SecretNotFound(key_ref.to_string()))
    }

    fn delete(&self, key_ref: &str) -> Result<()> {
        self.map.lock().unwrap().remove(key_ref);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_store_roundtrip() {
        let s = MockStore::default();
        s.set("a/b", "sk-123").unwrap();
        assert_eq!(s.get("a/b").unwrap(), "sk-123");
        s.set("a/b", "sk-456").unwrap();
        assert_eq!(s.get("a/b").unwrap(), "sk-456");
        s.delete("a/b").unwrap();
        assert!(s.get("a/b").is_err());
    }

    #[test]
    fn file_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path().join("secrets.json"));
        s.set("a/b", "sk-123").unwrap();
        assert_eq!(s.get("a/b").unwrap(), "sk-123");
        s.delete("a/b").unwrap();
        assert!(s.get("a/b").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn file_store_restricts_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let s = FileStore::new(path.clone());
        s.set("a/b", "sk-123").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secrets file must be owner-only");
    }
}
