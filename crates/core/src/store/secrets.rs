use crate::error::{CoreError, Result};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait SecretStore: Send + Sync {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()>;
    fn get(&self, key_ref: &str) -> Result<String>;
    fn delete(&self, key_ref: &str) -> Result<()>;
}

/// 系统钥匙串实现（macOS Keychain / Windows Credential Manager / Linux Secret Service）
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self { service: "agent-switch".into() }
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
        self.entry(key_ref)?
            .get_password()
            .map_err(|e| match e {
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
}

/// 测试用内存实现
#[derive(Default)]
pub struct MockStore {
    map: Mutex<HashMap<String, String>>,
}

impl SecretStore for MockStore {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()> {
        self.map.lock().unwrap().insert(key_ref.to_string(), secret.to_string());
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
}
