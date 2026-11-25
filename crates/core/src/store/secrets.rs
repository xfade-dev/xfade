use crate::error::{CoreError, Result};
use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

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
///
/// Caches retrieved secrets in memory for the lifetime of the store so a
/// long-running daemon reads each key from the OS keychain at most once.
pub struct KeyringStore {
    service: String,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    allow_all_acl: bool,
    cache: RwLock<HashMap<String, String>>,
}

/// Current keyring service name.
pub const SERVICE: &str = "xfade";
/// Legacy service name (pre-v0.6), used to read old entries during migration.
pub const LEGACY_SERVICE: &str = "agent-switch";

impl KeyringStore {
    pub fn new() -> Self {
        Self::with_allow_all_acl(true)
    }

    pub fn with_allow_all_acl(allow_all_acl: bool) -> Self {
        Self {
            service: SERVICE.into(),
            allow_all_acl,
            cache: RwLock::new(HashMap::new()),
        }
    }

    fn entry(&self, key_ref: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, key_ref).map_err(|e| CoreError::Keyring(e.to_string()))
    }
}

#[cfg(target_os = "macos")]
fn quote_security_arg(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(target_os = "macos")]
fn set_password_macos_allow_all(service: &str, key_ref: &str, secret: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let cmd_line = format!(
        "add-generic-password -s {} -a {} -w {} -U -T \"\"\n",
        quote_security_arg(service),
        quote_security_arg(key_ref),
        quote_security_arg(secret),
    );
    let mut child = Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CoreError::Keyring(format!("spawn security: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(cmd_line.as_bytes())
            .map_err(|e| CoreError::Keyring(format!("write security stdin: {e}")))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| CoreError::Keyring(format!("wait security: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CoreError::Keyring(format!(
            "security add-generic-password failed: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

/// Read a password on a worker thread so a slow keychain read (macOS waiting
/// on an authorization prompt) can tell the user where to look instead of
/// hanging silently. The read is never aborted — a slow read must not be
/// misreported as a missing key.
fn get_password_with_hint(entry: keyring::Entry, key_ref: &str) -> Result<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(entry.get_password());
    });
    let result = match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(r) => r,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            eprintln!(
                "xfade: waiting on the OS keychain for '{key_ref}' — \
                 if an authorization dialog is showing, please allow it"
            );
            rx.recv().expect("keyring worker disconnected")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            unreachable!("keyring worker disconnected before sending")
        }
    };
    result.map_err(|e| match e {
        keyring::Error::NoEntry => CoreError::SecretNotFound(key_ref.to_string()),
        other => CoreError::Keyring(other.to_string()),
    })
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeyringStore {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            if self.allow_all_acl
                && !secret.contains('\n')
                && !secret.contains('\r')
                && !key_ref.contains('\n')
                && !key_ref.contains('\r')
            {
                set_password_macos_allow_all(&self.service, key_ref, secret)?;
                self.cache
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key_ref.to_string(), secret.to_string());
                return Ok(());
            }
        }
        self.entry(key_ref)?
            .set_password(secret)
            .map_err(|e| CoreError::Keyring(e.to_string()))?;
        self.cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key_ref.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, key_ref: &str) -> Result<String> {
        if let Some(cached) = self
            .cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key_ref)
        {
            return Ok(cached.clone());
        }
        let secret = get_password_with_hint(self.entry(key_ref)?, key_ref)?;
        self.cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key_ref.to_string(), secret.clone());
        Ok(secret)
    }

    fn delete(&self, key_ref: &str) -> Result<()> {
        match self.entry(key_ref)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {
                self.cache
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(key_ref);
                Ok(())
            }
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
        match get_password_with_hint(legacy, from) {
            Ok(secret) => {
                self.set(to, &secret)?;
                let legacy = keyring::Entry::new(LEGACY_SERVICE, from)
                    .map_err(|e| CoreError::Keyring(e.to_string()))?;
                match legacy.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(true),
                    Err(e) => Err(CoreError::Keyring(e.to_string())),
                }
            }
            Err(CoreError::SecretNotFound(_)) => Ok(false),
            Err(e) => Err(e),
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
        let s = serde_json::to_string(map)
            .map_err(|e| CoreError::Keyring(format!("serialize secrets: {e}")))?;
        // Created owner-only (0600) from the start and renamed into place, so the
        // secrets are never readable with wider permissions, even briefly.
        crate::fsutil::write_private(&self.path, s.as_bytes())
            .map_err(|e| CoreError::Keyring(format!("write secrets: {e}")))?;
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

    #[test]
    fn keyring_store_serves_from_in_memory_cache() {
        let store = KeyringStore::new();
        store
            .cache
            .write()
            .unwrap()
            .insert("xfade/claude/devops".into(), "sk-cached-123".into());
        assert_eq!(store.get("xfade/claude/devops").unwrap(), "sk-cached-123");
    }
}
