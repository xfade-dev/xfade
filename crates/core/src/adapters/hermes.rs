use super::{atomic_write, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_yaml::Value;

/// Hermes Agent adapter.
///
/// Hermes (`~/.hermes/`) splits config across `config.yaml` (non-secret:
/// model, provider, terminal, mcp_servers, …) and `.env` (API keys). For a
/// third-party OpenAI-compatible endpoint the "Custom endpoint" provider keeps
/// everything in `config.yaml` under a single `model:` block:
///
/// ```yaml
/// model:
///   default: <model>
///   provider: custom
///   base_url: <base_url>   # OpenAI-compatible; needs the /v1 suffix
///   api_key: <api_key>
/// ```
///
/// Switching back to official restores the previously captured `model:` block
/// (or removes it when none was captured), leaving every other `config.yaml`
/// field (terminal, mcp_servers, …) untouched.
pub struct HermesAdapter {
    home: std::path::PathBuf,
}

impl HermesAdapter {
    pub fn new(home: &std::path::Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn config_path(&self) -> std::path::PathBuf {
        self.home.join(".hermes").join("config.yaml")
    }

    fn load(&self) -> Result<Value> {
        match std::fs::read_to_string(self.config_path()) {
            Ok(s) => {
                if s.trim().is_empty() {
                    return Ok(Value::Mapping(Default::default()));
                }
                let v: Value = serde_yaml::from_str(&s).map_err(|e| CoreError::ConfigParse {
                    path: self.config_path().display().to_string(),
                    msg: e.to_string(),
                })?;
                Ok(v)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Value::Mapping(Default::default()))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, doc: &Value) -> Result<()> {
        let s = serde_yaml::to_string(doc).map_err(|e| CoreError::ConfigParse {
            path: self.config_path().display().to_string(),
            msg: e.to_string(),
        })?;
        atomic_write(&self.config_path(), s.as_bytes())
    }

    fn ensure_mapping(doc: &mut Value) -> &mut serde_yaml::Mapping {
        if !matches!(doc, Value::Mapping(_)) {
            *doc = Value::Mapping(Default::default());
        }
        match doc {
            Value::Mapping(m) => m,
            _ => unreachable!("just ensured mapping"),
        }
    }
}

impl ToolAdapter for HermesAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Hermes
    }

    fn config_paths(&self) -> Vec<std::path::PathBuf> {
        vec![self.config_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut doc = self.load()?;
        let map = Self::ensure_mapping(&mut doc);

        if provider.is_official() {
            if let Some(orig) = provider
                .extra
                .get("_original_model")
                .filter(|v| !v.is_null())
            {
                // `_original_model` is a serde_json::Value captured from the
                // live YAML config; round-trip it back to a serde_yaml::Value.
                let yaml_val = serde_yaml::to_value(orig).map_err(|e| CoreError::ConfigParse {
                    path: self.config_path().display().to_string(),
                    msg: e.to_string(),
                })?;
                map.insert(Value::from("model"), yaml_val);
            } else {
                map.remove(Value::from("model"));
            }
        } else {
            let base_url = provider
                .base_url
                .as_deref()
                .ok_or_else(|| CoreError::ConfigParse {
                    path: "provider".into(),
                    msg: format!("third-party provider '{}' is missing base_url", provider.id),
                })?;
            // Hermes' custom endpoint dispatches OpenAI-wire requests at
            // {base_url}/chat/completions, so a bare host needs the /v1 suffix.
            let base = super::with_v1_if_bare_host(base_url);
            let mut model = serde_yaml::Mapping::new();
            model.insert(Value::from("provider"), Value::from("custom"));
            if let Some(m) = provider.extra.get("model").and_then(|m| m.as_str()) {
                model.insert(Value::from("default"), Value::from(m));
            }
            model.insert(Value::from("base_url"), Value::from(base));
            model.insert(
                Value::from("api_key"),
                Value::from(api_key.unwrap_or_default()),
            );
            map.insert(Value::from("model"), Value::Mapping(model));
        }

        self.save(&doc)
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let doc = self.load()?;
        let map = match doc.as_mapping() {
            Some(m) => m,
            None => return Ok(None),
        };
        let model = match map.get(Value::from("model")).and_then(|v| v.as_mapping()) {
            Some(m) => m,
            None => return Ok(None),
        };
        // Only the custom-endpoint provider is a third-party switch; OAuth
        // providers (Nous Portal / Anthropic / Codex …) carry no base_url.
        let is_custom = model
            .get(Value::from("provider"))
            .and_then(|v| v.as_str())
            .map(|p| p == "custom")
            .unwrap_or(false);
        if !is_custom {
            return Ok(None);
        }
        let base_url = model
            .get(Value::from("base_url"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let api_key = model
            .get(Value::from("api_key"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let mut p = Provider::new("imported", ToolKind::Hermes, base_url);
        if let Some(m) = model.get(Value::from("default")).and_then(|v| v.as_str()) {
            p.extra = serde_json::json!({ "model": m });
        }
        Ok(Some((p, api_key)))
    }

    /// Capture the existing `model:` block so switching back to official can
    /// restore it (an OAuth provider's `model:` block would otherwise be lost).
    fn capture_original_state(&self) -> Result<Option<serde_json::Value>> {
        let doc = self.load()?;
        let Some(model) = doc.get(Value::from("model")).cloned() else {
            return Ok(None);
        };
        // Skip an already-custom block (a prior xfade switch) — only capture a
        // non-custom provider block as the original to restore.
        let is_custom = model
            .as_mapping()
            .and_then(|m| m.get(Value::from("provider")))
            .and_then(|v| v.as_str())
            .map(|p| p == "custom")
            .unwrap_or(false);
        if is_custom {
            return Ok(None);
        }
        Ok(Some(serde_json::json!({ "_original_model": model })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, HermesAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = HermesAdapter::new(dir.path());
        (dir, ad)
    }

    fn read_yaml(dir: &tempfile::TempDir) -> serde_yaml::Value {
        let s = std::fs::read_to_string(dir.path().join(".hermes/config.yaml")).unwrap();
        serde_yaml::from_str(&s).unwrap()
    }

    fn write_yaml(dir: &tempfile::TempDir, s: &str) {
        let path = dir.path().join(".hermes/config.yaml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, s).unwrap();
    }

    fn provider(base: &str, extra: serde_json::Value) -> Provider {
        let mut p = Provider::new("kimi", ToolKind::Hermes, Some(base.into()));
        p.extra = extra;
        p
    }

    #[test]
    fn apply_third_party_writes_custom_model_block() {
        let (dir, ad) = setup();
        write_yaml(&dir, "terminal:\n  backend: local\n");
        ad.apply(
            &provider("https://api.moonshot.cn", json!({"model": "kimi-k2.5"})),
            Some("sk-test"),
        )
        .unwrap();
        let doc = read_yaml(&dir);
        let model = &doc["model"];
        assert_eq!(model["provider"].as_str().unwrap(), "custom");
        assert_eq!(model["default"].as_str().unwrap(), "kimi-k2.5");
        assert_eq!(
            model["base_url"].as_str().unwrap(),
            "https://api.moonshot.cn/v1"
        );
        assert_eq!(model["api_key"].as_str().unwrap(), "sk-test");
        // Untouched user field is preserved.
        assert_eq!(doc["terminal"]["backend"].as_str().unwrap(), "local");
    }

    #[test]
    fn apply_keeps_existing_v1_suffix() {
        let (dir, ad) = setup();
        ad.apply(
            &provider("https://api.moonshot.cn/v1", json!({})),
            Some("k"),
        )
        .unwrap();
        let doc = read_yaml(&dir);
        assert_eq!(
            doc["model"]["base_url"].as_str().unwrap(),
            "https://api.moonshot.cn/v1"
        );
    }

    #[test]
    fn official_removes_model_block_without_capture() {
        let (dir, ad) = setup();
        write_yaml(&dir, "terminal:\n  backend: local\n");
        ad.apply(&provider("https://x", json!({})), Some("k"))
            .unwrap();
        let official = Provider::new("official", ToolKind::Hermes, None);
        ad.apply(&official, None).unwrap();
        let doc = read_yaml(&dir);
        assert!(doc.get("model").is_none());
        assert_eq!(doc["terminal"]["backend"].as_str().unwrap(), "local");
    }

    #[test]
    fn official_restores_captured_model_block() {
        let (dir, ad) = setup();
        write_yaml(
            &dir,
            "model:\n  provider: openrouter\n  default: anthropic/claude\n",
        );
        let captured = ad.capture_original_state().unwrap().unwrap();
        // Switch to third-party.
        let mut third = provider("https://x", json!({}));
        third.extra = json!({ "_original_model": captured["_original_model"] });
        ad.apply(&third, Some("k")).unwrap();
        assert_eq!(
            read_yaml(&dir)["model"]["provider"].as_str().unwrap(),
            "custom"
        );
        // Switch back to official restores the OAuth block.
        let mut official = Provider::new("official", ToolKind::Hermes, None);
        official.extra = json!({ "_original_model": captured["_original_model"] });
        ad.apply(&official, None).unwrap();
        let doc = read_yaml(&dir);
        assert_eq!(doc["model"]["provider"].as_str().unwrap(), "openrouter");
        assert_eq!(
            doc["model"]["default"].as_str().unwrap(),
            "anthropic/claude"
        );
    }

    #[test]
    fn read_current_returns_custom_endpoint() {
        let (_dir, ad) = setup();
        ad.apply(&provider("https://x", json!({"model": "m"})), Some("sk-9"))
            .unwrap();
        let (got, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.base_url.as_deref(), Some("https://x/v1"));
        assert_eq!(key.as_deref(), Some("sk-9"));
        assert_eq!(got.extra["model"], "m");
    }

    #[test]
    fn read_current_ignores_oauth_provider() {
        let (dir, ad) = setup();
        write_yaml(&dir, "model:\n  provider: openrouter\n  default: x\n");
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn capture_skips_already_custom_block() {
        let (dir, ad) = setup();
        write_yaml(
            &dir,
            "model:\n  provider: custom\n  base_url: https://x/v1\n",
        );
        assert!(ad.capture_original_state().unwrap().is_none());
    }
}
