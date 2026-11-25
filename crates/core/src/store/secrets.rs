use crate::error::{CoreError, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};

static SUPPRESS_STDERR_HINTS: AtomicBool = AtomicBool::new(false);

/// RAII guard that suppresses `eprintln!` keychain-wait hints on stderr while
/// active (e.g., while the TUI owns the alternate screen in raw mode).
pub struct StderrHintGuard {
    prev: bool,
}

impl StderrHintGuard {
    pub fn suppress() -> Self {
        let prev = SUPPRESS_STDERR_HINTS.swap(true, Ordering::Relaxed);
        Self { prev }
    }
}

impl Drop for StderrHintGuard {
    fn drop(&mut self) {
        SUPPRESS_STDERR_HINTS.store(self.prev, Ordering::Relaxed);
    }
}

pub trait SecretStore: Send + Sync {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()>;
    fn get(&self, key_ref: &str) -> Result<String>;
    fn delete(&self, key_ref: &str) -> Result<()>;

    /// Refresh the OS keychain ACL for the single master key (if this store uses
    /// a master-key vault). Returns `Ok(true)` if an existing master key was
    /// refreshed, `Ok(false)` if not applicable or no master key exists yet.
    fn normalize_master_key(&self) -> Result<bool> {
        Ok(false)
    }

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

    // `security add-generic-password -U` updates the password in-place while
    // preserving the existing SecAccess ACL. Delete any existing entry first
    // so the recreated item genuinely receives the `-T ""` (allow-all) ACL.
    let entry =
        keyring::Entry::new(service, key_ref).map_err(|e| CoreError::Keyring(e.to_string()))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(e) => return Err(CoreError::Keyring(e.to_string())),
    }

    let cmd_line = format!(
        "add-generic-password -s {} -a {} -w {} -T \"\"\n",
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
        // Restore via keyring if `security` failed after we deleted the old item.
        let _ = entry.set_password(secret);
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
            if !SUPPRESS_STDERR_HINTS.load(Ordering::Relaxed) {
                eprintln!(
                    "xfade: waiting on the OS keychain for '{key_ref}' — \
                     if an authorization dialog is showing, please allow it"
                );
            }
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

/// Keyring account name for the single 256-bit master key used by [`VaultStore`].
pub const MASTER_KEY_REF: &str = "xfade/master_key";
const VAULT_AAD: &[u8] = b"xfade-vault-v1";

#[derive(serde::Serialize, serde::Deserialize)]
struct VaultEnvelope {
    v: u8,
    nonce: String,
    data: String,
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(s: &str) -> std::result::Result<Vec<u8>, String> {
    let bytes = s.trim().as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("odd hex length".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        let hi = (pair[0] as char)
            .to_digit(16)
            .ok_or_else(|| "invalid hex char".to_string())?;
        let lo = (pair[1] as char)
            .to_digit(16)
            .ok_or_else(|| "invalid hex char".to_string())?;
        out.push(((hi << 4) | lo) as u8);
    }
    Ok(out)
}

fn decode_master_key(hex_str: &str) -> Result<[u8; 32]> {
    let raw = decode_hex(hex_str).map_err(|e| CoreError::Keyring(format!("master key: {e}")))?;
    let key: [u8; 32] = raw
        .try_into()
        .map_err(|_| CoreError::Keyring("master key must be 32 bytes".into()))?;
    Ok(key)
}

/// Single-master-key encrypted secret store.
///
/// Stores **at most one** master key (`xfade/master_key`) in the OS keyring and
/// encrypts all provider API keys with AES-256-GCM inside `<data_dir>/vault.enc`
/// (`0600` permissions). Existing per-provider keyring entries are lazily
/// migrated into `vault.enc` on first read so upgrades never trigger N keychain
/// prompts for new keys.
pub struct VaultStore {
    vault_path: std::path::PathBuf,
    master_keyring: std::sync::Arc<dyn SecretStore>,
    write_lock: Mutex<()>,
    cached_master_key: RwLock<Option<[u8; 32]>>,
}

impl VaultStore {
    pub fn new(
        vault_path: std::path::PathBuf,
        master_keyring: std::sync::Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            vault_path,
            master_keyring,
            write_lock: Mutex::new(()),
            cached_master_key: RwLock::new(None),
        }
    }

    fn get_master_key(&self) -> Result<[u8; 32]> {
        if let Some(key) = *self
            .cached_master_key
            .read()
            .unwrap_or_else(|e| e.into_inner())
        {
            return Ok(key);
        }
        let hex_str = self.master_keyring.get(MASTER_KEY_REF)?;
        let key = decode_master_key(&hex_str)?;
        *self
            .cached_master_key
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(key);
        Ok(key)
    }

    fn get_or_create_master_key(&self) -> Result<[u8; 32]> {
        match self.get_master_key() {
            Ok(key) => Ok(key),
            Err(CoreError::SecretNotFound(_)) => {
                let mut key = [0u8; 32];
                ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut key)
                    .map_err(|_| CoreError::Keyring("generate master key failed".into()))?;
                let hex_str = encode_hex(&key);
                self.master_keyring.set(MASTER_KEY_REF, &hex_str)?;
                *self
                    .cached_master_key
                    .write()
                    .unwrap_or_else(|e| e.into_inner()) = Some(key);
                Ok(key)
            }
            Err(e) => Err(e),
        }
    }

    fn read_vault(&self, key: &[u8; 32]) -> Result<HashMap<String, String>> {
        let raw = match std::fs::read_to_string(&self.vault_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(e) => return Err(CoreError::Keyring(format!("read vault: {e}"))),
        };
        let env: VaultEnvelope = serde_json::from_str(&raw)
            .map_err(|e| CoreError::Keyring(format!("parse vault: {e}")))?;
        if env.v != 1 {
            return Err(CoreError::Keyring(format!(
                "unsupported vault version {}",
                env.v
            )));
        }
        let nonce_vec =
            decode_hex(&env.nonce).map_err(|e| CoreError::Keyring(format!("vault nonce: {e}")))?;
        let nonce_arr: [u8; 12] = nonce_vec
            .try_into()
            .map_err(|_| CoreError::Keyring("vault nonce must be 12 bytes".into()))?;
        let mut buf =
            decode_hex(&env.data).map_err(|e| CoreError::Keyring(format!("vault data: {e}")))?;

        let unbound = ring::aead::UnboundKey::new(&ring::aead::AES_256_GCM, key)
            .map_err(|_| CoreError::Keyring("init AES-256-GCM key failed".into()))?;
        let opening_key = ring::aead::LessSafeKey::new(unbound);
        let plaintext = opening_key
            .open_in_place(
                ring::aead::Nonce::assume_unique_for_key(nonce_arr),
                ring::aead::Aad::from(VAULT_AAD),
                &mut buf,
            )
            .map_err(|_| CoreError::Keyring("vault decryption failed".into()))?;

        serde_json::from_slice(plaintext)
            .map_err(|e| CoreError::Keyring(format!("decode vault map: {e}")))
    }

    fn write_vault(&self, key: &[u8; 32], map: &HashMap<String, String>) -> Result<()> {
        let mut buf = serde_json::to_vec(map)
            .map_err(|e| CoreError::Keyring(format!("serialize vault: {e}")))?;
        let mut nonce_arr = [0u8; 12];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce_arr)
            .map_err(|_| CoreError::Keyring("generate vault nonce failed".into()))?;

        let unbound = ring::aead::UnboundKey::new(&ring::aead::AES_256_GCM, key)
            .map_err(|_| CoreError::Keyring("init AES-256-GCM key failed".into()))?;
        let sealing_key = ring::aead::LessSafeKey::new(unbound);
        sealing_key
            .seal_in_place_append_tag(
                ring::aead::Nonce::assume_unique_for_key(nonce_arr),
                ring::aead::Aad::from(VAULT_AAD),
                &mut buf,
            )
            .map_err(|_| CoreError::Keyring("vault encryption failed".into()))?;

        let env = VaultEnvelope {
            v: 1,
            nonce: encode_hex(&nonce_arr),
            data: encode_hex(&buf),
        };
        let encoded = serde_json::to_vec(&env)
            .map_err(|e| CoreError::Keyring(format!("serialize vault envelope: {e}")))?;
        crate::fsutil::write_private(&self.vault_path, &encoded)
            .map_err(|e| CoreError::Keyring(format!("write vault: {e}")))?;
        Ok(())
    }
}

impl SecretStore for VaultStore {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let key = self.get_or_create_master_key()?;
        let mut map = if self.vault_path.exists() {
            self.read_vault(&key)?
        } else {
            HashMap::new()
        };
        map.insert(key_ref.to_string(), secret.to_string());
        self.write_vault(&key, &map)?;
        if key_ref != MASTER_KEY_REF {
            let _ = self.master_keyring.delete(key_ref);
        }
        Ok(())
    }

    fn get(&self, key_ref: &str) -> Result<String> {
        if self.vault_path.exists() {
            let key = self.get_master_key()?;
            let map = self.read_vault(&key)?;
            if let Some(secret) = map.get(key_ref) {
                return Ok(secret.clone());
            }
        }
        if key_ref == MASTER_KEY_REF {
            return Err(CoreError::SecretNotFound(key_ref.to_string()));
        }
        // Lazy migration from legacy per-provider OS keyring entry.
        let secret = self.master_keyring.get(key_ref)?;
        self.set(key_ref, &secret)?;
        Ok(secret)
    }

    fn delete(&self, key_ref: &str) -> Result<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.vault_path.exists() {
            let key = self.get_master_key()?;
            let mut map = self.read_vault(&key)?;
            if map.remove(key_ref).is_some() {
                self.write_vault(&key, &map)?;
            }
        }
        if key_ref != MASTER_KEY_REF {
            let _ = self.master_keyring.delete(key_ref);
        }
        Ok(())
    }

    fn normalize_master_key(&self) -> Result<bool> {
        match self.master_keyring.get(MASTER_KEY_REF) {
            Ok(hex_str) => {
                self.master_keyring.set(MASTER_KEY_REF, &hex_str)?;
                Ok(true)
            }
            Err(CoreError::SecretNotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn migrate_key(&self, from: &str, to: &str) -> Result<bool> {
        match self.get(from) {
            Ok(secret) => {
                self.set(to, &secret)?;
                self.delete(from)?;
                Ok(true)
            }
            Err(CoreError::SecretNotFound(_)) => self.master_keyring.migrate_key(from, to),
            Err(e) => Err(e),
        }
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

    #[test]
    fn vault_store_roundtrip_encrypts_at_rest_and_uses_single_master_key() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingMockStore {
            inner: MockStore,
            get_calls: AtomicUsize,
            set_calls: AtomicUsize,
        }
        impl SecretStore for CountingMockStore {
            fn set(&self, key_ref: &str, secret: &str) -> Result<()> {
                self.set_calls.fetch_add(1, Ordering::SeqCst);
                self.inner.set(key_ref, secret)
            }
            fn get(&self, key_ref: &str) -> Result<String> {
                self.get_calls.fetch_add(1, Ordering::SeqCst);
                self.inner.get(key_ref)
            }
            fn delete(&self, key_ref: &str) -> Result<()> {
                self.inner.delete(key_ref)
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("vault.enc");
        let master_ring = std::sync::Arc::new(CountingMockStore {
            inner: MockStore::default(),
            get_calls: AtomicUsize::new(0),
            set_calls: AtomicUsize::new(0),
        });
        let vault = VaultStore::new(vault_path.clone(), master_ring.clone());

        for i in 0..20 {
            vault
                .set(&format!("xfade/claude/p{i}"), &format!("sk-secret-{i}"))
                .unwrap();
        }
        for i in 0..20 {
            assert_eq!(
                vault.get(&format!("xfade/claude/p{i}")).unwrap(),
                format!("sk-secret-{i}")
            );
        }

        // Only ONE master key was created & read in the OS keyring across 20 providers.
        assert_eq!(master_ring.set_calls.load(Ordering::SeqCst), 1);
        assert_eq!(master_ring.get_calls.load(Ordering::SeqCst), 1);
        assert!(master_ring.inner.get(MASTER_KEY_REF).is_ok());

        // Ciphertext on disk must not contain plaintext secrets.
        let disk = std::fs::read_to_string(&vault_path).unwrap();
        assert!(!disk.contains("sk-secret-"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&vault_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Fresh VaultStore instance (cold cache) reads master key once and decrypts all 20.
        let vault2 = VaultStore::new(vault_path, master_ring.clone());
        for i in 0..20 {
            assert_eq!(
                vault2.get(&format!("xfade/claude/p{i}")).unwrap(),
                format!("sk-secret-{i}")
            );
        }
        assert_eq!(master_ring.get_calls.load(Ordering::SeqCst), 2);

        vault2.delete("xfade/claude/p0").unwrap();
        assert!(matches!(
            vault2.get("xfade/claude/p0"),
            Err(CoreError::SecretNotFound(_))
        ));
    }

    #[test]
    fn vault_store_lazily_migrates_legacy_keyring_entries() {
        let dir = tempfile::tempdir().unwrap();
        let master_ring = std::sync::Arc::new(MockStore::default());
        master_ring
            .set("xfade/claude/office", "sk-legacy-office")
            .unwrap();

        let vault = VaultStore::new(dir.path().join("vault.enc"), master_ring.clone());
        assert_eq!(
            vault.get("xfade/claude/office").unwrap(),
            "sk-legacy-office"
        );
        // Legacy entry should be removed from master_ring and stored in vault.enc.
        assert!(matches!(
            master_ring.get("xfade/claude/office"),
            Err(CoreError::SecretNotFound(_))
        ));
        assert_eq!(
            vault.get("xfade/claude/office").unwrap(),
            "sk-legacy-office"
        );
    }
}
