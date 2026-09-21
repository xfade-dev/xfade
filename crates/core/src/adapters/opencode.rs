use super::{atomic_write, client_base_url, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const OPENAI_COMPAT: &str = "@ai-sdk/openai-compatible";

pub struct OpenCodeAdapter {
    home: PathBuf,
}

impl OpenCodeAdapter {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn config_path(&self) -> PathBuf {
        self.home
            .join(".config")
            .join("opencode")
            .join("opencode.json")
    }

    fn auth_path(&self) -> PathBuf {
        self.home
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json")
    }

    fn load_json(&self, path: &Path) -> Result<Value> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let v: Value = serde_json::from_str(&s).map_err(|e| CoreError::ConfigParse {
                    path: path.display().to_string(),
                    msg: e.to_string(),
                })?;
                if !v.is_object() {
                    return Err(CoreError::ConfigParse {
                        path: path.display().to_string(),
                        msg: "top-level is not an object".into(),
                    });
                }
                Ok(v)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(e) => Err(e.into()),
        }
    }

    fn save_json(&self, path: &Path, doc: &Value) -> Result<()> {
        atomic_write(
            path,
            format!("{}\n", serde_json::to_string_pretty(doc)?).as_bytes(),
        )
    }
}

impl ToolAdapter for OpenCodeAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::OpenCode
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.config_path(), self.auth_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut doc = self.load_json(&self.config_path())?;
        let mut auth = self.load_json(&self.auth_path())?;

        let providers = doc
            .as_object_mut()
            .unwrap()
            .entry("provider")
            .or_insert_with(|| json!({}));
        if !providers.is_object() {
            *providers = json!({});
        }
        let providers = providers.as_object_mut().unwrap();
        let auth_obj = auth.as_object_mut().unwrap();

        if provider.is_official() {
            if let Some(id) = provider
                .extra
                .get("remove_provider")
                .and_then(|v| v.as_str())
            {
                providers.remove(id);
                auth_obj.remove(id);
            }
        } else {
            let id = &provider.id;
            let base_url = provider
                .base_url
                .as_deref()
                .ok_or_else(|| CoreError::ConfigParse {
                    path: "provider".into(),
                    msg: format!("third-party provider '{id}' is missing base_url"),
                })?;
            // Canonical form: `models` object ({ "<model-id>": { "name": ... } }).
            // Accept `model = "<id>"` as a shorthand and synthesize that object.
            let models = provider
                .extra
                .get("models")
                .cloned()
                .or_else(|| {
                    provider.extra.get("model").and_then(|m| m.as_str()).map(|m| {
                        let mut obj = serde_json::Map::new();
                        obj.insert(m.to_string(), json!({ "name": m }));
                        serde_json::Value::Object(obj)
                    })
                })
                .unwrap_or_else(|| json!({}));
            // The openai-compatible SDK appends /chat/completions to baseURL,
            // so a bare host must carry the /v1 suffix.
            let base_url = client_base_url(base_url, "openai-completions");
            providers.insert(
                id.clone(),
                json!({
                    "npm": OPENAI_COMPAT,
                    "name": id,
                    "options": { "baseURL": base_url },
                    "models": models,
                }),
            );
            auth_obj.insert(
                id.clone(),
                json!({ "type": "api", "key": api_key.unwrap_or_default() }),
            );
        }

        self.save_json(&self.auth_path(), &auth)?;
        self.save_json(&self.config_path(), &doc)
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let doc = self.load_json(&self.config_path())?;
        let Some(providers) = doc.get("provider").and_then(|v| v.as_object()) else {
            return Ok(None);
        };
        for (id, prov) in providers {
            if prov.get("npm").and_then(|v| v.as_str()) != Some(OPENAI_COMPAT) {
                continue;
            }
            let base_url = prov
                .get("options")
                .and_then(|o| o.get("baseURL"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let mut p = Provider::new("imported", ToolKind::OpenCode, base_url);
            if let Some(models) = prov.get("models") {
                p.extra = json!({ "models": models.clone() });
            }
            let auth = self.load_json(&self.auth_path())?;
            let key = auth
                .get(id)
                .and_then(|a| a.get("key"))
                .and_then(|v| v.as_str())
                .map(String::from);
            return Ok(Some((p, key)));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, OpenCodeAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = OpenCodeAdapter::new(dir.path());
        (dir, ad)
    }

    fn read_doc(dir: &tempfile::TempDir) -> serde_json::Value {
        let s = std::fs::read_to_string(dir.path().join(".config/opencode/opencode.json")).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    fn read_auth(dir: &tempfile::TempDir) -> serde_json::Value {
        let s =
            std::fs::read_to_string(dir.path().join(".local/share/opencode/auth.json")).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn apply_third_party_writes_provider_and_auth() {
        let (dir, ad) = setup();
        let mut p = Provider::new(
            "kimi",
            ToolKind::OpenCode,
            Some("https://api.moonshot.cn/v1".into()),
        );
        p.extra = json!({"models": {"kimi-k2.5": {}}});
        ad.apply(&p, Some("sk-k")).unwrap();

        let doc = read_doc(&dir);
        let prov = &doc["provider"]["kimi"];
        assert_eq!(prov["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(prov["options"]["baseURL"], "https://api.moonshot.cn/v1");
        assert!(prov["models"].get("kimi-k2.5").is_some());

        let auth = read_auth(&dir);
        assert_eq!(auth["kimi"]["type"], "api");
        assert_eq!(auth["kimi"]["key"], "sk-k");
    }

    #[test]
    fn model_shorthand_synthesizes_models() {
        let (dir, ad) = setup();
        let mut p = Provider::new("office", ToolKind::OpenCode, Some("https://x".into()));
        p.extra = json!({"model": "deepseek-v4-flash"});
        ad.apply(&p, Some("k")).unwrap();

        let doc = read_doc(&dir);
        let models = &doc["provider"]["office"]["models"];
        assert_eq!(models["deepseek-v4-flash"]["name"], "deepseek-v4-flash");
    }

    #[test]
    fn apply_official_removes_custom_provider() {
        let (dir, ad) = setup();
        let p = Provider::new("kimi", ToolKind::OpenCode, Some("https://x".into()));
        ad.apply(&p, Some("k")).unwrap();

        let mut official = Provider::new("official", ToolKind::OpenCode, None);
        official.extra = json!({"remove_provider": "kimi"});
        ad.apply(&official, None).unwrap();

        let doc = read_doc(&dir);
        assert!(doc["provider"].get("kimi").is_none());
        let auth = read_auth(&dir);
        assert!(auth.get("kimi").is_none());
    }

    #[test]
    fn apply_preserves_unrelated_keys() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".config/opencode")).unwrap();
        std::fs::write(
            dir.path().join(".config/opencode/opencode.json"),
            json!({"theme": "dark", "provider": {"builtin-x": {"npm": "@ai-sdk/anthropic"}}})
                .to_string(),
        )
        .unwrap();

        let p = Provider::new("kimi", ToolKind::OpenCode, Some("https://x".into()));
        ad.apply(&p, Some("k")).unwrap();

        let doc = read_doc(&dir);
        assert_eq!(doc["theme"], "dark");
        assert!(doc["provider"].get("builtin-x").is_some());
    }

    #[test]
    fn read_current_none_when_no_custom() {
        let (_dir, ad) = setup();
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn read_current_imports_first_custom() {
        let (_dir, ad) = setup();
        let p = Provider::new("kimi", ToolKind::OpenCode, Some("https://x".into()));
        ad.apply(&p, Some("sk-9")).unwrap();
        let (got, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.base_url.as_deref(), Some("https://x/v1"));
        assert_eq!(key.as_deref(), Some("sk-9"));
    }
}
