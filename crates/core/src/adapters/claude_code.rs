use super::{atomic_write, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const ENV_BASE: &str = "ANTHROPIC_BASE_URL";
const ENV_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
const ENV_MODEL: &str = "ANTHROPIC_MODEL";

pub struct ClaudeCodeAdapter {
    home: PathBuf,
}

impl ClaudeCodeAdapter {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn settings_path(&self) -> PathBuf {
        self.home.join(".claude").join("settings.json")
    }

    fn load(&self) -> Result<Value> {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(s) => {
                let v: Value = serde_json::from_str(&s).map_err(|e| CoreError::ConfigParse {
                    path: self.settings_path().display().to_string(),
                    msg: e.to_string(),
                })?;
                if !v.is_object() {
                    return Err(CoreError::ConfigParse {
                        path: self.settings_path().display().to_string(),
                        msg: "top-level value is not an object".into(),
                    });
                }
                Ok(v)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, doc: &Value) -> Result<()> {
        atomic_write(
            &self.settings_path(),
            format!("{}\n", serde_json::to_string_pretty(doc)?).as_bytes(),
        )
    }
}

impl ToolAdapter for ClaudeCodeAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::ClaudeCode
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.settings_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut doc = self.load()?;
        let root = doc.as_object_mut().expect("load guarantees object");
        let env = root.entry("env").or_insert_with(|| json!({}));
        if !env.is_object() {
            *env = json!({});
        }
        let env = env.as_object_mut().unwrap();

        if provider.is_official() {
            env.remove(ENV_BASE);
            env.remove(ENV_TOKEN);
            env.remove(ENV_MODEL);
        } else {
            let base_url = provider
                .base_url
                .as_deref()
                .ok_or_else(|| CoreError::ConfigParse {
                    path: "provider".into(),
                    msg: format!("third-party provider '{}' is missing base_url", provider.id),
                })?;
            env.insert(ENV_BASE.into(), json!(base_url));
            env.insert(ENV_TOKEN.into(), json!(api_key.unwrap_or_default()));
            match provider.extra.get("model").and_then(|m| m.as_str()) {
                Some(model) => {
                    env.insert(ENV_MODEL.into(), json!(model));
                }
                None => {
                    env.remove(ENV_MODEL);
                }
            }
        }
        self.save(&doc)
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let doc = self.load()?;
        let Some(env) = doc.get("env") else {
            return Ok(None);
        };
        let base = env.get(ENV_BASE).and_then(|v| v.as_str());
        let token = env.get(ENV_TOKEN).and_then(|v| v.as_str());
        if base.is_none() && token.is_none() {
            return Ok(None);
        }
        let mut p = Provider::new("imported", ToolKind::ClaudeCode, base.map(String::from));
        if let Some(m) = env.get(ENV_MODEL).and_then(|v| v.as_str()) {
            p.extra = json!({ "model": m });
        }
        Ok(Some((p, token.map(String::from))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, ClaudeCodeAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = ClaudeCodeAdapter::new(dir.path());
        (dir, ad)
    }

    #[test]
    fn apply_third_party_sets_env_keys() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            json!({"model": "opus", "env": {"OTHER": "keep"}}).to_string(),
        )
        .unwrap();

        let mut p = Provider::new(
            "kimi",
            ToolKind::ClaudeCode,
            Some("https://api.moonshot.cn/anthropic".into()),
        );
        p.extra = json!({"model": "kimi-k2.5"});
        ad.apply(&p, Some("sk-test")).unwrap();

        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            doc["env"]["ANTHROPIC_BASE_URL"],
            "https://api.moonshot.cn/anthropic"
        );
        assert_eq!(doc["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-test");
        assert_eq!(doc["env"]["ANTHROPIC_MODEL"], "kimi-k2.5");
        assert_eq!(doc["env"]["OTHER"], "keep");
        assert_eq!(doc["model"], "opus");
    }

    #[test]
    fn apply_official_removes_env_keys() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            json!({"env": {"ANTHROPIC_BASE_URL": "x", "ANTHROPIC_AUTH_TOKEN": "y", "OTHER": "keep"}})
                .to_string(),
        )
        .unwrap();

        let official = Provider::new("official", ToolKind::ClaudeCode, None);
        ad.apply(&official, None).unwrap();

        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(doc["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert!(doc["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert_eq!(doc["env"]["OTHER"], "keep");
    }

    #[test]
    fn apply_creates_settings_when_missing() {
        let (dir, ad) = setup();
        let p = Provider::new("k", ToolKind::ClaudeCode, Some("https://x".into()));
        ad.apply(&p, Some("k1")).unwrap();
        assert!(dir.path().join(".claude/settings.json").exists());
    }

    #[test]
    fn read_current_none_when_no_third_party() {
        let (_dir, ad) = setup();
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn read_current_imports_existing() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            json!({"env": {"ANTHROPIC_BASE_URL": "https://relay", "ANTHROPIC_AUTH_TOKEN": "sk-9"}})
                .to_string(),
        )
        .unwrap();
        let (p, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(p.base_url.as_deref(), Some("https://relay"));
        assert_eq!(key.as_deref(), Some("sk-9"));
        assert_eq!(p.id, "imported");
    }
}
