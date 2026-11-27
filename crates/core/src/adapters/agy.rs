use super::{load_json_or_empty, save_json_pretty, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const ENV_GEMINI_API_KEY: &str = "GEMINI_API_KEY";
pub const ENV_GOOGLE_GEMINI_BASE_URL: &str = "GOOGLE_GEMINI_BASE_URL";

/// Build the `GEMINI_*` env pairs for `agy` when targeting a third-party or
/// local-proxy endpoint. `GOOGLE_GEMINI_BASE_URL` must be the root URL without
/// a trailing `/v1beta` or `/`.
pub(crate) fn agy_env_pairs(base_url: &str, api_key: Option<&str>) -> Vec<(&'static str, String)> {
    let base = base_url.trim_end_matches('/');
    vec![
        (ENV_GOOGLE_GEMINI_BASE_URL, base.to_string()),
        (ENV_GEMINI_API_KEY, api_key.unwrap_or_default().to_string()),
    ]
}

/// Build the `customModelsConfig` entry for `agy`'s `settings.json` when a
/// model is configured alongside a `base_url`.
pub(crate) fn agy_custom_models_config(base_url: &str, model: &str) -> Value {
    let base = base_url.trim_end_matches('/');
    json!({
        "customModels": [
            {
                "modelName": format!("gemini-api://{base}/models/{model}"),
                "label": model,
            }
        ]
    })
}

pub struct AgyAdapter {
    home: PathBuf,
}

impl AgyAdapter {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn settings_path(&self) -> PathBuf {
        self.home.join(".gemini/antigravity-cli/settings.json")
    }
}

impl ToolAdapter for AgyAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Agy
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.settings_path()]
    }

    fn apply(&self, provider: &Provider, _api_key: Option<&str>) -> Result<()> {
        let path = self.settings_path();
        let mut doc = load_json_or_empty(&path)?;
        let root = doc
            .as_object_mut()
            .expect("load_json_or_empty guarantees object");

        if provider.is_official() {
            root.remove("modelProvider");
            root.remove("customModelsConfig");
            if let Some(orig) = provider.extra.get("_original_model") {
                if orig.is_null() {
                    root.remove("model");
                } else {
                    root.insert("model".into(), orig.clone());
                }
            }
        } else {
            let base_url = provider
                .base_url
                .as_deref()
                .ok_or_else(|| CoreError::ConfigParse {
                    path: "provider".into(),
                    msg: format!("third-party provider '{}' is missing base_url", provider.id),
                })?;
            root.insert("modelProvider".into(), json!("gemini"));
            if let Some(model) = provider.extra.get("model").and_then(|v| v.as_str()) {
                root.insert("model".into(), json!(model));
                root.insert(
                    "customModelsConfig".into(),
                    agy_custom_models_config(base_url, model),
                );
            } else {
                root.remove("customModelsConfig");
            }
        }

        save_json_pretty(&path, &doc)
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let doc = load_json_or_empty(&self.settings_path())?;
        if doc.get("modelProvider").and_then(|v| v.as_str()) != Some("gemini") {
            return Ok(None);
        }

        let mut base_url: Option<String> = None;
        let mut model: Option<String> = doc
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if let Some(first_model) = doc
            .get("customModelsConfig")
            .and_then(|c| c.get("customModels"))
            .and_then(|m| m.as_array())
            .and_then(|arr| arr.first())
        {
            if let Some(name) = first_model.get("modelName").and_then(|v| v.as_str()) {
                if let Some(rest) = name.strip_prefix("gemini-api://") {
                    if let Some((b, m)) = rest.rsplit_once("/models/") {
                        if !b.is_empty() {
                            base_url = Some(b.to_string());
                        }
                        if model.is_none() && !m.is_empty() {
                            model = Some(m.to_string());
                        }
                    }
                }
            }
        }

        let mut p = Provider::new("imported", ToolKind::Agy, base_url);
        if let Some(m) = model {
            p.extra = json!({ "model": m });
        }
        Ok(Some((p, None)))
    }

    fn capture_original_state(&self) -> Result<Option<Value>> {
        let doc = load_json_or_empty(&self.settings_path())?;
        if let Some(m) = doc.get("model") {
            return Ok(Some(json!({ "_original_model": m })));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_third_party_and_revert_official() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".gemini/antigravity-cli/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            json!({
                "model": "Gemini 4 Hydrogen (Medium)",
                "notifications": true
            })
            .to_string(),
        )
        .unwrap();

        let ad = AgyAdapter::new(dir.path());
        let orig = ad.capture_original_state().unwrap().unwrap();

        let mut p = Provider::new("relay", ToolKind::Agy, Some("https://gw.example/".into()));
        p.extra = json!({"model": "gemini-2.5-pro"});
        ad.apply(&p, Some("sk-secret")).unwrap();

        let doc = load_json_or_empty(&path).unwrap();
        assert_eq!(doc["modelProvider"], "gemini");
        assert_eq!(doc["model"], "gemini-2.5-pro");
        assert_eq!(doc["notifications"], true);
        assert_eq!(
            doc["customModelsConfig"]["customModels"][0]["modelName"],
            "gemini-api://https://gw.example/models/gemini-2.5-pro"
        );
        // Secret must never be written to settings.json
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("sk-secret"));

        let (imported, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(imported.base_url.as_deref(), Some("https://gw.example"));
        assert_eq!(imported.extra["model"], "gemini-2.5-pro");
        assert!(key.is_none());

        let mut official = Provider::new("official", ToolKind::Agy, None);
        official.extra = orig;
        ad.apply(&official, None).unwrap();

        let reverted = load_json_or_empty(&path).unwrap();
        assert!(reverted.get("modelProvider").is_none());
        assert!(reverted.get("customModelsConfig").is_none());
        assert_eq!(reverted["model"], "Gemini 4 Hydrogen (Medium)");
        assert!(ad.read_current().unwrap().is_none());
    }
}
