use super::{atomic_write, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct CodexAdapter {
    home: PathBuf,
}

impl CodexAdapter {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn config_path(&self) -> PathBuf {
        self.home.join(".codex").join("config.toml")
    }

    fn auth_path(&self) -> PathBuf {
        self.home.join(".codex").join("auth.json")
    }

    fn load_toml(&self) -> Result<toml::Value> {
        match std::fs::read_to_string(self.config_path()) {
            Ok(s) => toml::from_str(&s).map_err(|e| CoreError::ConfigParse {
                path: self.config_path().display().to_string(),
                msg: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(toml::Value::Table(Default::default()))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn load_auth(&self) -> Result<Value> {
        match std::fs::read_to_string(self.auth_path()) {
            Ok(s) => Ok(serde_json::from_str(&s).map_err(|e| CoreError::ConfigParse {
                path: self.auth_path().display().to_string(),
                msg: e.to_string(),
            })?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(e) => Err(e.into()),
        }
    }
}

impl ToolAdapter for CodexAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Codex
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.config_path(), self.auth_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut cfg = self.load_toml()?;
        let root = cfg.as_table_mut().ok_or_else(|| CoreError::ConfigParse {
            path: self.config_path().display().to_string(),
            msg: "top-level is not a table".into(),
        })?;

        if provider.is_official() {
            root.remove("model_provider");
        } else {
            let id = &provider.id;
            let url = provider.base_url.as_deref().unwrap();
            let wire_api = provider
                .extra
                .get("wire_api")
                .and_then(|v| v.as_str())
                .unwrap_or("responses"); // 新版 Codex 已废弃 wire_api = "chat"
            let env_key = format!("{}_API_KEY", id.to_uppercase().replace('-', "_"));

            let mut prov = toml::map::Map::new();
            prov.insert("name".into(), toml::Value::String(id.clone()));
            prov.insert("base_url".into(), toml::Value::String(url.to_string()));
            prov.insert("wire_api".into(), toml::Value::String(wire_api.to_string()));
            prov.insert("env_key".into(), toml::Value::String(env_key));

            let providers = root
                .entry("model_providers")
                .or_insert_with(|| toml::Value::Table(Default::default()));
            if !providers.is_table() {
                *providers = toml::Value::Table(Default::default());
            }
            providers
                .as_table_mut()
                .unwrap()
                .insert(id.clone(), toml::Value::Table(prov));
            root.insert("model_provider".into(), toml::Value::String(id.clone()));

            let mut auth = self.load_auth()?;
            if !auth.is_object() {
                auth = json!({});
            }
            auth.as_object_mut().unwrap().insert(
                "OPENAI_API_KEY".into(),
                json!(api_key.unwrap_or_default()),
            );
            atomic_write(
                &self.auth_path(),
                format!("{}\n", serde_json::to_string_pretty(&auth)?).as_bytes(),
            )?;
        }

        atomic_write(
            &self.config_path(),
            toml::to_string_pretty(&cfg)
                .map_err(|e| CoreError::Toml(e.to_string()))?
                .as_bytes(),
        )
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let cfg = self.load_toml()?;
        let Some(active_id) = cfg.get("model_provider").and_then(|v| v.as_str()) else {
            return Ok(None);
        };
        let prov = cfg.get("model_providers").and_then(|m| m.get(active_id));
        let base_url = prov
            .and_then(|p| p.get("base_url"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let mut p = Provider::new("imported", ToolKind::Codex, base_url);
        if let Some(w) = prov.and_then(|p| p.get("wire_api")).and_then(|v| v.as_str()) {
            p.extra = json!({ "wire_api": w });
        }
        let auth = self.load_auth()?;
        let key = auth
            .get("OPENAI_API_KEY")
            .and_then(|v| v.as_str())
            .map(String::from);
        Ok(Some((p, key)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, CodexAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = CodexAdapter::new(dir.path());
        (dir, ad)
    }

    fn read_config(dir: &tempfile::TempDir) -> toml::Value {
        let s = std::fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap();
        toml::from_str(&s).unwrap()
    }

    #[test]
    fn apply_third_party_writes_toml_and_auth() {
        let (dir, ad) = setup();
        let p = Provider::new(
            "kimi",
            ToolKind::Codex,
            Some("https://api.moonshot.cn/v1".into()),
        );
        ad.apply(&p, Some("sk-k")).unwrap();

        let cfg = read_config(&dir);
        assert_eq!(cfg["model_provider"].as_str().unwrap(), "kimi");
        let prov = &cfg["model_providers"]["kimi"];
        assert_eq!(
            prov["base_url"].as_str().unwrap(),
            "https://api.moonshot.cn/v1"
        );
        assert_eq!(prov["wire_api"].as_str().unwrap(), "responses");
        assert_eq!(prov["env_key"].as_str().unwrap(), "KIMI_API_KEY");

        let auth: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".codex/auth.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(auth["OPENAI_API_KEY"], "sk-k");
    }

    #[test]
    fn apply_preserves_other_toml_sections() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
        std::fs::write(
            dir.path().join(".codex/config.toml"),
            "model = \"gpt-5\"\napproval_policy = \"never\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".codex/auth.json"),
            json!({"tokens": {"x": 1}}).to_string(),
        )
        .unwrap();

        let p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        ad.apply(&p, Some("sk-1")).unwrap();

        let cfg = read_config(&dir);
        assert_eq!(cfg["model"].as_str().unwrap(), "gpt-5");
        assert_eq!(cfg["approval_policy"].as_str().unwrap(), "never");
        let auth: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".codex/auth.json")).unwrap(),
        )
        .unwrap();
        assert!(auth.get("tokens").is_some());
        assert_eq!(auth["OPENAI_API_KEY"], "sk-1");
    }

    #[test]
    fn apply_official_removes_model_provider_only() {
        let (dir, ad) = setup();
        let p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        ad.apply(&p, Some("sk-1")).unwrap();
        let official = Provider::new("official", ToolKind::Codex, None);
        ad.apply(&official, None).unwrap();

        let cfg = read_config(&dir);
        assert!(cfg.get("model_provider").is_none());
        assert!(cfg["model_providers"].get("kimi").is_some());
    }

    #[test]
    fn wire_api_from_extra() {
        let (dir, ad) = setup();
        let mut p = Provider::new("oa", ToolKind::Codex, Some("https://x".into()));
        p.extra = json!({"wire_api": "chat"}); // 验证 extra 能覆盖默认值
        ad.apply(&p, Some("k")).unwrap();
        let cfg = read_config(&dir);
        assert_eq!(
            cfg["model_providers"]["oa"]["wire_api"].as_str().unwrap(),
            "chat"
        );
    }

    #[test]
    fn read_current_none_without_model_provider() {
        let (_dir, ad) = setup();
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn read_current_imports() {
        let (_dir, ad) = setup();
        let p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        ad.apply(&p, Some("sk-9")).unwrap();
        let (got, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.id, "imported");
        assert_eq!(got.base_url.as_deref(), Some("https://x"));
        assert_eq!(key.as_deref(), Some("sk-9"));
    }
}
