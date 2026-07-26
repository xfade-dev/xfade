use crate::adapters::{adapter_for, ToolAdapter};
use crate::backup::{backup_file, list_backups, restore};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use crate::store::db::Database;
use crate::store::secrets::{KeyringStore, SecretStore};
use std::path::{Path, PathBuf};

pub struct Core {
    db: Database,
    secrets: Box<dyn SecretStore>,
    home: PathBuf,
    data_dir: PathBuf,
}

impl Core {
    /// 测试与嵌入用：显式指定 home（工具配置根）与 data_dir（自身数据根）
    pub fn with_paths(home: &Path, data_dir: &Path, secrets: Box<dyn SecretStore>) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        Ok(Self {
            db: Database::open(&data_dir.join("agent-switch.db"))?,
            secrets,
            home: home.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// 真实环境：home = 用户主目录，data_dir = ~/.config/agent-switch，系统钥匙串
    pub fn for_current_user() -> Result<Self> {
        let home =
            dirs::home_dir().ok_or_else(|| CoreError::Keyring("cannot locate home dir".into()))?;
        let data = home.join(".config").join("agent-switch");
        Self::with_paths(&home, &data, Box::new(KeyringStore::new()))
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

    /// 编辑 provider（key 传 Some 时同时更换钥匙串中的密钥）
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

    /// 切换：备份 → 写入工具配置 → 更新 active
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
        adapter.apply(&p, key.as_deref())?;
        self.db.set_active(tool, id)
    }

    /// OpenCode 切官方时，把当前 active 的自定义 provider id 写入 extra.remove_provider
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

    /// 首次切换前：该工具若无 imported，则把当前实况存为 imported 快照
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

    /// 手动导入：覆盖更新 imported 快照
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
        let core = Core::with_paths(&home, &data, Box::new(MockStore::default())).unwrap();
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
}
