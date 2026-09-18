use crate::adapters::{adapter_for, ToolAdapter};
use crate::backup::{backup_file, list_backups, restore};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use crate::store::config::{Config, SecretsBackend, GLOBAL_API_KEY_REF};
use crate::store::db::Database;
use crate::store::secrets::{FileStore, KeyringStore, SecretStore};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Resolve the secrets backend: the env var overrides the global config file,
/// which overrides the default (keyring).
fn resolve_secrets_backend(data_dir: &Path) -> Result<SecretsBackend> {
    if let Ok(v) = std::env::var("XFADE_SECRETS") {
        return v.parse().map_err(|e: String| CoreError::ConfigParse {
            path: "XFADE_SECRETS".into(),
            msg: e,
        });
    }
    Ok(Config::load(data_dir).secrets)
}

#[derive(Clone)]
pub struct Core {
    db: Database,
    secrets: Arc<dyn SecretStore>,
    home: PathBuf,
    data_dir: PathBuf,
}

impl Core {
    /// Test/embedding entry: explicitly set `home` (tool config root) and
    /// `data_dir` (self data root).
    pub fn with_paths(home: &Path, data_dir: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let core = Self {
            db: Database::open(&data_dir.join("xfade.db"))?,
            secrets,
            home: home.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
        };
        core.migrate_legacy_key_refs();
        Ok(core)
    }

    /// One-time migration of the legacy key_ref prefix (`agent-switch/` → `xfade/`):
    /// - Migrate the secret from the old key to the new key (KeyringStore crosses
    ///   service names; Mock/FileMock use key_ref as the key, so the default impl works).
    /// - Update `db.providers.key_ref` in sync.
    ///
    /// Best-effort and idempotent: a failed item is skipped (retried on next startup);
    /// providers whose key_ref lacks the legacy prefix are skipped. Providers without a
    /// secret (official / key never set) still get their key_ref format unified.
    fn migrate_legacy_key_refs(&self) {
        const LEGACY_PREFIX: &str = "agent-switch/";
        let Ok(providers) = self.db.list(None) else {
            return;
        };
        for p in providers {
            let Some(rest) = p.key_ref.strip_prefix(LEGACY_PREFIX) else {
                continue;
            };
            let new_ref = format!("xfade/{rest}");
            // The secret may already live under the new key (e.g. a prior half-finished
            // migration). In that case sync the DB key_ref directly and skip touching the
            // legacy service — reading the legacy service can trigger a keychain
            // authorization prompt that the user may cancel, which would otherwise leave
            // the DB stuck on the legacy prefix forever.
            match self.secrets.get(&new_ref) {
                Ok(_) => {
                    if let Err(e) = self.db.update_key_ref(p.tool, &p.id, &new_ref) {
                        eprintln!(
                            "[xfade] keyring migration: failed to update key_ref for {}/{}: {e}",
                            p.tool, p.id
                        );
                    }
                    continue;
                }
                Err(CoreError::SecretNotFound(_)) => {}
                Err(e) => {
                    eprintln!(
                        "[xfade] keyring migration: failed to probe new key {rest}: {e} (will retry on next startup)"
                    );
                    continue;
                }
            }
            // New key has no secret yet; migrate from the legacy key (cross-service).
            // Only update the DB on success (or when the old key has no secret, Ok(false)).
            // On failure, keep the legacy prefix and retry on next startup — this avoids
            // the DB pointing at the new key while the secret is still stuck in the old
            // keyring, which would permanently skip that provider.
            match self.secrets.migrate_key(&p.key_ref, &new_ref) {
                Ok(_) => {
                    if let Err(e) = self.db.update_key_ref(p.tool, &p.id, &new_ref) {
                        eprintln!(
                            "[xfade] keyring migration: failed to update key_ref for {}/{}: {e}",
                            p.tool, p.id
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[xfade] keyring migration: failed to migrate {rest}: {e} (will retry on next startup)"
                    );
                }
            }
        }
    }

    /// Production entry: `home` = user home dir, `data_dir` = `~/.config/xfade`,
    /// system keyring.
    pub fn for_current_user() -> Result<Self> {
        let home =
            dirs::home_dir().ok_or_else(|| CoreError::Keyring("cannot locate home dir".into()))?;
        let data = home.join(".config").join("xfade");
        Self::with_paths(&home, &data, Arc::new(KeyringStore::new()))
    }

    /// Build a `Core` from environment variables, shared by CLI and GUI.
    /// - `HOME`: tool config root (required).
    /// - `XFADE_DATA_DIR`: self data dir, defaults to `$HOME/.config/xfade`.
    /// - Secret backend resolution (highest to lowest priority):
    ///   1. `XFADE_SECRETS` env (`file` | `keyring`),
    ///   2. `<data_dir>/config.json` `secrets` field (see `xfade config`),
    ///   3. default: system keyring.
    pub fn from_env() -> Result<Self> {
        let home = std::env::var("HOME").map_err(|_| CoreError::Keyring("HOME not set".into()))?;
        let home = Path::new(&home);
        let data = std::env::var_os("XFADE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config").join("xfade"));
        let use_file = resolve_secrets_backend(&data)? == SecretsBackend::File;
        let secrets: Arc<dyn SecretStore> = if use_file {
            Arc::new(FileStore::new(data.join("secrets.json")))
        } else {
            Arc::new(KeyringStore::new())
        };
        Self::with_paths(home, &data, secrets)
    }

    fn adapter(&self, tool: ToolKind) -> Box<dyn ToolAdapter> {
        adapter_for(tool, &self.home)
    }

    pub fn add_provider(&self, mut provider: Provider, api_key: Option<&str>) -> Result<()> {
        if let Some(key) = api_key {
            self.secrets.set(&provider.key_ref, key)?;
        }
        provider.is_active = false;
        self.db.upsert(&provider)
    }

    /// Read the global API key (shared default for all tools), if set.
    pub fn global_api_key(&self) -> Option<String> {
        self.secrets.get(GLOBAL_API_KEY_REF).ok()
    }

    /// Set the global API key (shared default for all tools).
    pub fn set_global_api_key(&self, key: &str) -> Result<()> {
        self.secrets.set(GLOBAL_API_KEY_REF, key)
    }

    /// The global config (`<data_dir>/config.json`).
    pub fn config(&self) -> Config {
        Config::load(&self.data_dir)
    }

    /// Update global config fields and persist them.
    ///
    /// `None` leaves a field unchanged; `Some("")` clears it (sets `None`).
    /// The `secrets` backend change only affects newly-created `Core` instances
    /// (a running GUI/daemon keeps its already-resolved backend until restart).
    pub fn update_config(
        &self,
        secrets: Option<&str>,
        base_url: Option<&str>,
        model: Option<&str>,
        api: Option<&str>,
    ) -> Result<Config> {
        let mut cfg = self.config();
        if let Some(s) = secrets {
            cfg.secrets = s.parse().map_err(|e: String| CoreError::ConfigParse {
                path: "secrets".into(),
                msg: e,
            })?;
        }
        if let Some(u) = base_url {
            cfg.base_url = if u.is_empty() {
                None
            } else {
                Some(u.to_string())
            };
        }
        if let Some(m) = model {
            cfg.model = if m.is_empty() {
                None
            } else {
                Some(m.to_string())
            };
        }
        if let Some(a) = api {
            cfg.api = if a.is_empty() {
                None
            } else {
                Some(a.to_string())
            };
        }
        cfg.save(&self.data_dir)?;
        Ok(cfg)
    }

    /// Edit a provider (when `key` is `Some`, also rotate the keyring secret).
    pub fn update_provider(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        if self.db.get(provider.tool, &provider.id)?.is_none() {
            return Err(CoreError::ProviderNotFound(format!(
                "{}/{}",
                provider.tool, provider.id
            )));
        }
        if let Some(key) = api_key {
            self.secrets.set(&provider.key_ref, key)?;
        }
        self.db.upsert(provider)
    }

    pub fn list(&self, tool: Option<ToolKind>) -> Result<Vec<Provider>> {
        self.db.list(tool)
    }

    pub fn current(&self, tool: ToolKind) -> Result<Option<Provider>> {
        Ok(self.db.list(Some(tool))?.into_iter().find(|p| p.is_active))
    }

    pub fn remove(&self, tool: ToolKind, id: &str) -> Result<()> {
        let p = self
            .db
            .get(tool, id)?
            .ok_or_else(|| CoreError::ProviderNotFound(format!("{tool}/{id}")))?;
        if p.is_active {
            return Err(CoreError::ActiveProviderRemoval(format!("{tool}/{id}")));
        }
        self.secrets.delete(&p.key_ref)?;
        self.db.delete(tool, id)
    }

    /// Switch provider: backup → write tool config → mark active.
    pub fn use_provider(&self, tool: ToolKind, id: &str) -> Result<()> {
        self.ensure_imported(tool)?;
        let p = self
            .db
            .get(tool, id)?
            .ok_or_else(|| CoreError::ProviderNotFound(format!("{tool}/{id}")))?;

        let key = if p.is_official() {
            None
        } else {
            Some(self.secrets.get(&p.key_ref)?)
        };

        let adapter = self.adapter(tool);
        let backup_dir = self.data_dir.join("backups").join(tool.as_str());
        for path in adapter.config_paths() {
            backup_file(&path, &backup_dir)?;
        }

        let p = self.patch_official_removal(tool, p)?;
        let p = self.patch_original_capture(tool, p)?;
        adapter.apply(&p, key.as_deref())?;
        self.db.set_active(tool, id)
    }

    /// When switching OpenCode back to official, write the current active custom
    /// provider id into `extra.remove_provider`.
    fn patch_official_removal(&self, tool: ToolKind, mut p: Provider) -> Result<Provider> {
        if tool == ToolKind::OpenCode && p.is_official() {
            if let Some(cur) = self.current(tool)? {
                if !cur.is_official() {
                    p.extra = serde_json::json!({ "remove_provider": cur.id });
                }
            }
        }
        Ok(p)
    }

    /// Populate the `_original_*` fields before `apply`, so adapters can restore
    /// the original config when switching back to official (OMP/Pi:
    /// `_original_default_provider`; Codex: `_original_model` /
    /// `_original_context_window`; Cline: `_original_last_used_provider`).
    ///
    /// Flow:
    /// - **Switch to third-party**: for every `_original_*` key missing from
    ///   `p.extra`, inherit it from the current active third-party provider first
    ///   (so a rapid A→B switch doesn't lose the earliest original value); if
    ///   still missing, call `adapter.capture_original_state()` to read the live
    ///   config now. Once obtained, write into `p.extra` and `upsert` to persist,
    ///   so a later switch to official can read it.
    /// - **Switch to official**: inherit every `_original_*` key from the current
    ///   active third-party provider (not persisted; used immediately).
    ///
    /// For claude_code/opencode: `capture_original_state` returns `None` by
    /// default and the active provider never carries these fields, so both
    /// branches are no-ops.
    fn patch_original_capture(&self, tool: ToolKind, mut p: Provider) -> Result<Provider> {
        if !p.is_official() {
            // Switch to third-party: inherit from the current active
            // third-party provider first, then from the live config.
            let mut captured = self
                .current(tool)?
                .filter(|c| !c.is_official())
                .and_then(|c| original_state_keys(&c.extra))
                .unwrap_or_default();
            if captured.is_empty() {
                captured = self
                    .adapter(tool)
                    .capture_original_state()?
                    .and_then(|v| original_state_keys(&v))
                    .unwrap_or_default();
            }
            if !captured.is_empty() {
                let mut extra = p.extra.as_object().cloned().unwrap_or_default();
                let changed = merge_missing(&mut extra, &captured);
                p.extra = serde_json::Value::Object(extra);
                if changed {
                    self.db.upsert(&p)?; // persist so a later switch to official can read it
                }
            }
        } else {
            // Switch to official: read the original values from the current
            // active third-party provider (not persisted; used immediately).
            if let Some(cur) = self.current(tool)? {
                if !cur.is_official() {
                    if let Some(orig) = original_state_keys(&cur.extra) {
                        let mut extra = p.extra.as_object().cloned().unwrap_or_default();
                        merge_missing(&mut extra, &orig);
                        p.extra = serde_json::Value::Object(extra);
                    }
                }
            }
        }
        Ok(p)
    }

    /// Before the first switch: if the tool has no `imported` snapshot, capture the
    /// current live config as the `imported` snapshot.
    pub fn ensure_imported(&self, tool: ToolKind) -> Result<()> {
        if self.db.get(tool, "imported")?.is_some() {
            return Ok(());
        }
        if let Some((p, key)) = self.adapter(tool).read_current()? {
            if let Some(k) = key.as_deref() {
                self.secrets.set(&p.key_ref, k)?;
            }
            self.db.upsert(&p)?;
        }
        Ok(())
    }

    /// Manual import: overwrite-update the `imported` snapshot.
    pub fn import(&self, tool: ToolKind) -> Result<Option<Provider>> {
        match self.adapter(tool).read_current()? {
            Some((p, key)) => {
                if let Some(k) = key.as_deref() {
                    self.secrets.set(&p.key_ref, k)?;
                }
                self.db.upsert(&p)?;
                Ok(Some(p))
            }
            None => Ok(None),
        }
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn secrets(&self) -> &dyn SecretStore {
        &*self.secrets
    }

    pub fn backups(&self, tool: ToolKind) -> Result<Vec<PathBuf>> {
        list_backups(&self.data_dir.join("backups").join(tool.as_str()))
    }

    pub fn restore_backup(&self, tool: ToolKind, backup: &Path) -> Result<()> {
        let file_name =
            backup
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| CoreError::ConfigParse {
                    path: backup.display().to_string(),
                    msg: "bad backup name".into(),
                })?;
        let original = file_name
            .rsplit_once('.')
            .map(|(n, _)| n)
            .unwrap_or(file_name);
        let adapter = self.adapter(tool);
        let target = adapter
            .config_paths()
            .into_iter()
            .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(original))
            .ok_or_else(|| CoreError::ConfigParse {
                path: backup.display().to_string(),
                msg: format!("no config file named {original}"),
            })?;
        restore(backup, &target)
    }
}

/// Extract every `_original_*` key from a provider's `extra` (or a
/// `capture_original_state` payload). Returns None when there are none.
fn original_state_keys(
    extra: &serde_json::Value,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let obj = extra.as_object()?;
    let out: serde_json::Map<String, serde_json::Value> = obj
        .iter()
        .filter(|(k, _)| k.starts_with("_original_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Merge `src` entries into `dst` for keys `dst` doesn't already have.
/// Returns whether anything was added.
fn merge_missing(
    dst: &mut serde_json::Map<String, serde_json::Value>,
    src: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    let mut changed = false;
    for (k, v) in src {
        if !dst.contains_key(k) {
            dst.insert(k.clone(), v.clone());
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Provider, ToolKind};
    use crate::store::secrets::MockStore;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, Core) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        let core = Core::with_paths(&home, &data, Arc::new(MockStore::default())).unwrap();
        (dir, core)
    }

    #[test]
    fn add_and_use_claude_provider() {
        let (dir, core) = setup();
        let p = Provider::new(
            "kimi",
            ToolKind::ClaudeCode,
            Some("https://api.moonshot.cn/anthropic".into()),
        );
        core.add_provider(p, Some("sk-test")).unwrap();
        core.use_provider(ToolKind::ClaudeCode, "kimi").unwrap();

        let s = std::fs::read_to_string(dir.path().join("home/.claude/settings.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(doc["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-test");

        let cur = core.current(ToolKind::ClaudeCode).unwrap().unwrap();
        assert_eq!(cur.id, "kimi");
    }

    #[test]
    fn first_use_auto_imports_current_config() {
        let (dir, core) = setup();
        std::fs::create_dir_all(dir.path().join("home/.claude")).unwrap();
        std::fs::write(
            dir.path().join("home/.claude/settings.json"),
            json!({"env": {"ANTHROPIC_BASE_URL": "https://relay", "ANTHROPIC_AUTH_TOKEN": "sk-old"}}).to_string(),
        ).unwrap();

        let p = Provider::new("kimi", ToolKind::ClaudeCode, Some("https://x".into()));
        core.add_provider(p, Some("sk-new")).unwrap();
        core.use_provider(ToolKind::ClaudeCode, "kimi").unwrap();

        let imported = core
            .list(Some(ToolKind::ClaudeCode))
            .unwrap()
            .into_iter()
            .find(|p| p.id == "imported")
            .unwrap();
        assert_eq!(imported.base_url.as_deref(), Some("https://relay"));

        core.use_provider(ToolKind::ClaudeCode, "imported").unwrap();
        let s = std::fs::read_to_string(dir.path().join("home/.claude/settings.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(doc["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-old");
    }

    #[test]
    fn use_official_clears_claude_env() {
        let (dir, core) = setup();
        core.add_provider(
            Provider::new("kimi", ToolKind::ClaudeCode, Some("https://x".into())),
            Some("k"),
        )
        .unwrap();
        core.add_provider(Provider::new("official", ToolKind::ClaudeCode, None), None)
            .unwrap();
        core.use_provider(ToolKind::ClaudeCode, "kimi").unwrap();
        core.use_provider(ToolKind::ClaudeCode, "official").unwrap();

        let s = std::fs::read_to_string(dir.path().join("home/.claude/settings.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(doc["env"].get("ANTHROPIC_BASE_URL").is_none());
    }

    #[test]
    fn remove_active_rejected() {
        let (_dir, core) = setup();
        core.add_provider(
            Provider::new("kimi", ToolKind::Codex, Some("https://x".into())),
            Some("k"),
        )
        .unwrap();
        core.use_provider(ToolKind::Codex, "kimi").unwrap();
        let err = core.remove(ToolKind::Codex, "kimi").unwrap_err();
        assert!(matches!(err, CoreError::ActiveProviderRemoval(_)));
    }

    #[test]
    fn use_creates_backup() {
        let (dir, core) = setup();
        std::fs::create_dir_all(dir.path().join("home/.claude")).unwrap();
        std::fs::write(dir.path().join("home/.claude/settings.json"), "{}").unwrap();
        core.add_provider(
            Provider::new("kimi", ToolKind::ClaudeCode, Some("https://x".into())),
            Some("k"),
        )
        .unwrap();
        core.use_provider(ToolKind::ClaudeCode, "kimi").unwrap();
        let backups = std::fs::read_dir(dir.path().join("data/backups/claude")).unwrap();
        assert_eq!(backups.count(), 1);
    }

    #[test]
    fn use_unknown_provider_errors() {
        let (_dir, core) = setup();
        assert!(core.use_provider(ToolKind::Codex, "nope").is_err());
    }

    #[test]
    fn use_opencode_official_removes_previous_custom() {
        let (dir, core) = setup();
        core.add_provider(
            Provider::new("kimi", ToolKind::OpenCode, Some("https://x".into())),
            Some("k"),
        )
        .unwrap();
        core.add_provider(Provider::new("official", ToolKind::OpenCode, None), None)
            .unwrap();
        core.use_provider(ToolKind::OpenCode, "kimi").unwrap();
        core.use_provider(ToolKind::OpenCode, "official").unwrap();

        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("home/.config/opencode/opencode.json"))
                .unwrap(),
        )
        .unwrap();
        assert!(doc["provider"].get("kimi").is_none());
    }

    #[test]
    fn core_clone_shares_db() {
        let (_dir, core) = setup();
        core.add_provider(
            Provider::new("kimi", ToolKind::Codex, Some("https://x".into())),
            Some("k"),
        )
        .unwrap();
        let core2 = core.clone();
        core2
            .add_provider(
                Provider::new("bak", ToolKind::Codex, Some("https://y".into())),
                Some("k2"),
            )
            .unwrap();
        assert_eq!(core.list(Some(ToolKind::Codex)).unwrap().len(), 2);
    }

    // ----- P0-1: OMP/Pi restore the original defaultProvider on switch to official -----

    fn read_settings_pi(dir: &tempfile::TempDir) -> serde_json::Value {
        let s = std::fs::read_to_string(dir.path().join("home/.pi/agent/settings.json")).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    fn write_initial_pi(dir: &tempfile::TempDir) {
        std::fs::create_dir_all(dir.path().join("home/.pi/agent")).unwrap();
        std::fs::write(
            dir.path().join("home/.pi/agent/settings.json"),
            json!({"defaultProvider": "anthropic", "defaultModel": "claude-sonnet-4"}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn pi_switch_to_custom_then_official_restores_default_provider() {
        let (dir, core) = setup();
        write_initial_pi(&dir);

        // Add + switch to third-party
        let mut p = Provider::new("kimi", ToolKind::Pi, Some("http://x".into()));
        p.extra = json!({"model": "gpt-4o"});
        core.add_provider(p, Some("sk-test")).unwrap();
        core.use_provider(ToolKind::Pi, "kimi").unwrap();

        // After switching to third-party: defaultProvider should be set to xfade
        assert_eq!(read_settings_pi(&dir)["defaultProvider"], "xfade");
        // And the third-party provider's extra should persist _original_default_provider
        let stored = core
            .list(Some(ToolKind::Pi))
            .unwrap()
            .into_iter()
            .find(|p| p.id == "kimi")
            .unwrap();
        assert_eq!(stored.extra["_original_default_provider"], "anthropic");

        // Add + switch to official
        core.add_provider(Provider::new("official", ToolKind::Pi, None), None)
            .unwrap();
        core.use_provider(ToolKind::Pi, "official").unwrap();

        // After switching to official: defaultProvider should restore to anthropic
        assert_eq!(
            read_settings_pi(&dir)["defaultProvider"],
            "anthropic",
            "switching to official must restore the original defaultProvider"
        );
    }

    #[test]
    fn pi_switch_between_two_customs_preserves_original_default() {
        // Rapid A→B switch: B should inherit A's saved _original_default_provider
        // rather than reading "xfade".
        let (dir, core) = setup();
        write_initial_pi(&dir);

        let mut a = Provider::new("a", ToolKind::Pi, Some("http://a".into()));
        a.extra = json!({"model": "gpt-4o"});
        core.add_provider(a, Some("k1")).unwrap();

        let mut b = Provider::new("b", ToolKind::Pi, Some("http://b".into()));
        b.extra = json!({"model": "gpt-4o"});
        core.add_provider(b, Some("k2")).unwrap();

        core.use_provider(ToolKind::Pi, "a").unwrap();
        core.use_provider(ToolKind::Pi, "b").unwrap();

        let stored_b = core
            .list(Some(ToolKind::Pi))
            .unwrap()
            .into_iter()
            .find(|p| p.id == "b")
            .unwrap();
        assert_eq!(
            stored_b.extra["_original_default_provider"], "anthropic",
            "B should inherit A's saved original value, not treat \"xfade\" as the original"
        );

        // Switch to official to restore
        core.add_provider(Provider::new("official", ToolKind::Pi, None), None)
            .unwrap();
        core.use_provider(ToolKind::Pi, "official").unwrap();
        assert_eq!(read_settings_pi(&dir)["defaultProvider"], "anthropic");
    }

    #[test]
    fn omp_switch_to_custom_then_official_restores_default_provider() {
        let (dir, core) = setup();
        std::fs::create_dir_all(dir.path().join("home/.omp/agent")).unwrap();
        std::fs::write(
            dir.path().join("home/.omp/agent/settings.json"),
            json!({"defaultProvider": "anthropic", "defaultModel": "claude-sonnet-4"}).to_string(),
        )
        .unwrap();

        let mut p = Provider::new("kimi", ToolKind::OhMyPi, Some("http://x".into()));
        p.extra = json!({"model": "gpt-4o"});
        core.add_provider(p, Some("sk-test")).unwrap();
        core.use_provider(ToolKind::OhMyPi, "kimi").unwrap();

        let s = std::fs::read_to_string(dir.path().join("home/.omp/agent/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["defaultProvider"], "xfade");

        core.add_provider(Provider::new("official", ToolKind::OhMyPi, None), None)
            .unwrap();
        core.use_provider(ToolKind::OhMyPi, "official").unwrap();

        let s = std::fs::read_to_string(dir.path().join("home/.omp/agent/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["defaultProvider"], "anthropic");
    }

    #[test]
    fn claude_codex_opencode_unaffected_by_original_capture() {
        // Regression: capture_original_state defaults to None for claude/codex/opencode,
        // so patch_original_capture must not write _original_default_provider into extra.
        let (dir, core) = setup();
        core.add_provider(
            Provider::new("kimi", ToolKind::ClaudeCode, Some("https://x".into())),
            Some("k"),
        )
        .unwrap();
        core.use_provider(ToolKind::ClaudeCode, "kimi").unwrap();
        let stored = core
            .list(Some(ToolKind::ClaudeCode))
            .unwrap()
            .into_iter()
            .find(|p| p.id == "kimi")
            .unwrap();
        assert!(
            stored.extra.get("_original_default_provider").is_none(),
            "claude_code must not get _original_default_provider written"
        );
        let _ = dir;
    }

    #[test]
    fn codex_original_model_restored_through_service_layer() {
        // Regression: patch_original_capture used to only recognize
        // _original_default_provider, so Codex's _original_model capture was
        // silently dropped and switching back to official deleted the user's
        // top-level `model` instead of restoring it.
        let (dir, core) = setup();
        let codex_dir = dir.path().join("home/.codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("config.toml"), "model = \"gpt-5-codex\"\n").unwrap();

        let mut p = Provider::new("glm", ToolKind::Codex, Some("https://x/v1".into()));
        p.extra = json!({"model": "glm-5-2-260617"});
        core.add_provider(p, Some("k")).unwrap();
        core.use_provider(ToolKind::Codex, "glm").unwrap();

        // The capture must have been persisted on the third-party provider.
        let stored = core
            .list(Some(ToolKind::Codex))
            .unwrap()
            .into_iter()
            .find(|p| p.id == "glm")
            .unwrap();
        assert_eq!(stored.extra["_original_model"], "gpt-5-codex");

        core.add_provider(Provider::new("official", ToolKind::Codex, None), None)
            .unwrap();
        core.use_provider(ToolKind::Codex, "official").unwrap();

        let cfg = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            cfg.contains("model = \"gpt-5-codex\""),
            "original model must be restored, got: {cfg}"
        );
    }

    #[test]
    fn cline_original_last_used_restored_through_service_layer() {
        let (dir, core) = setup();
        let settings_dir = dir.path().join("home/.cline/data/settings");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("providers.json"),
            json!({"version":1, "lastUsedProvider":"openrouter", "modes":{}, "providers":{}})
                .to_string(),
        )
        .unwrap();

        let mut p = Provider::new(
            "glm",
            ToolKind::Cline,
            Some("http://127.0.0.1:24860".into()),
        );
        p.extra = json!({"model": "glm-5-2-260617"});
        core.add_provider(p, Some("k")).unwrap();
        core.use_provider(ToolKind::Cline, "glm").unwrap();

        core.add_provider(Provider::new("official", ToolKind::Cline, None), None)
            .unwrap();
        core.use_provider(ToolKind::Cline, "official").unwrap();

        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(settings_dir.join("providers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(doc["lastUsedProvider"], "openrouter");
    }

    // ----- keyring migration: agent-switch/ prefix → xfade/ -----

    /// Seed legacy data: a provider in the DB with an `agent-switch/`-prefixed
    /// key_ref, and the corresponding key holding a secret in the store.
    fn seed_legacy(core: &Core, secrets: &MockStore) {
        let mut p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        p.key_ref = "agent-switch/codex/kimi".into();
        core.db.upsert(&p).unwrap();
        secrets.set("agent-switch/codex/kimi", "sk-legacy").unwrap();

        // A provider without a secret (official): should also get its key_ref unified
        let mut official = Provider::new("official", ToolKind::Codex, None);
        official.key_ref = "agent-switch/codex/official".into();
        core.db.upsert(&official).unwrap();
    }

    #[test]
    fn keyring_migration_moves_secret_and_updates_db() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        let secrets = Arc::new(MockStore::default());

        // Seed legacy data using a bare DB + store first
        {
            let core = Core::with_paths(&home, &data, secrets.clone()).unwrap();
            seed_legacy(&core, &secrets);
        }

        // Re-initialize Core → triggers migration
        let core = Core::with_paths(&home, &data, secrets.clone()).unwrap();

        // db key_ref has been updated to the new prefix
        let stored = core
            .list(Some(ToolKind::Codex))
            .unwrap()
            .into_iter()
            .find(|p| p.id == "kimi")
            .unwrap();
        assert_eq!(stored.key_ref, "xfade/codex/kimi");

        // secret moved to the new key, old key removed
        assert_eq!(core.secrets().get("xfade/codex/kimi").unwrap(), "sk-legacy");
        assert!(core.secrets().get("agent-switch/codex/kimi").is_err());

        // the provider without a secret also got its key_ref format unified
        let official = core
            .list(Some(ToolKind::Codex))
            .unwrap()
            .into_iter()
            .find(|p| p.id == "official")
            .unwrap();
        assert_eq!(official.key_ref, "xfade/codex/official");
    }

    #[test]
    fn keyring_migration_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        let secrets = Arc::new(MockStore::default());

        {
            let core = Core::with_paths(&home, &data, secrets.clone()).unwrap();
            seed_legacy(&core, &secrets);
        }

        // Initialize twice in a row (migration is idempotent: no error, no data loss)
        Core::with_paths(&home, &data, secrets.clone()).unwrap();
        let core = Core::with_paths(&home, &data, secrets.clone()).unwrap();

        assert_eq!(
            core.secrets().get("xfade/codex/kimi").unwrap(),
            "sk-legacy",
            "repeated migration must not lose the secret"
        );
    }

    #[test]
    fn new_providers_use_xfade_prefix() {
        let p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        assert_eq!(p.key_ref, "xfade/codex/kimi");
        assert!(p.key_ref.starts_with("xfade/"));
    }

    #[test]
    fn keyring_migration_syncs_db_when_secret_already_at_new_key() {
        // Regression: a prior half-finished migration left the secret at the NEW key
        // but the DB key_ref still on the legacy prefix. Migration must sync the DB
        // directly (and must NOT try to read the legacy key, which on KeyringStore
        // would hit the legacy service and can trigger a cancelled keychain prompt).
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        let secrets = Arc::new(MockStore::default());

        {
            let core = Core::with_paths(&home, &data, secrets.clone()).unwrap();
            let mut p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
            p.key_ref = "agent-switch/codex/kimi".into();
            core.db.upsert(&p).unwrap();
            // secret already migrated to the new key; legacy key holds nothing
            secrets.set("xfade/codex/kimi", "sk-new").unwrap();
        }

        let core = Core::with_paths(&home, &data, secrets.clone()).unwrap();
        let stored = core
            .list(Some(ToolKind::Codex))
            .unwrap()
            .into_iter()
            .find(|p| p.id == "kimi")
            .unwrap();
        assert_eq!(stored.key_ref, "xfade/codex/kimi");
        assert_eq!(core.secrets().get("xfade/codex/kimi").unwrap(), "sk-new");
    }
}
