use super::{
    atomic_write, capture_default_provider, load_json_or_empty, save_json_pretty, ToolAdapter,
};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Oh My Pi (OMP) adapter.
///
/// OMP stores its config at `~/.omp/agent/`:
/// - `settings.json` — `defaultProvider`, `defaultModel`, `roles`
/// - `models.yml` — custom providers in YAML format
/// - `auth.json` — credential store (not modified by xfade)
///
/// Strategy:
/// 1. Modify `settings.json` to point `defaultProvider` to `xfade`.
/// 2. Add/replace the `xfade` provider entry in `models.yml`.
///
/// When switching back to official: restore the original `defaultProvider`
/// and remove the xfade entry from `models.yml`.
pub struct OmpAdapter {
    home: PathBuf,
}

impl OmpAdapter {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn agent_dir(&self) -> PathBuf {
        self.home.join(".omp").join("agent")
    }

    fn settings_path(&self) -> PathBuf {
        self.agent_dir().join("settings.json")
    }

    fn models_path(&self) -> PathBuf {
        self.agent_dir().join("models.yml")
    }

    /// Parse a minimal YAML `models.yml` and return a JSON Value representation.
    /// Only handles the `providers` top-level key with basic scalar fields
    /// (baseUrl, api, apiKey, models array).
    fn load_models_yml(&self) -> Result<Value> {
        let text = match std::fs::read_to_string(self.models_path()) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({"providers": {}}));
            }
            Err(e) => return Err(e.into()),
        };
        let yaml_val: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|e| {
            CoreError::ConfigParse {
                path: self.models_path().display().to_string(),
                msg: format!("YAML parse error: {e}"),
            }
        })?;
        // Convert serde_yaml::Value → serde_json::Value
        let json_str = serde_json::to_string(&yaml_val).map_err(|e| CoreError::ConfigParse {
            path: self.models_path().display().to_string(),
            msg: e.to_string(),
        })?;
        let v: Value = serde_json::from_str(&json_str).map_err(|e| CoreError::ConfigParse {
            path: self.models_path().display().to_string(),
            msg: e.to_string(),
        })?;
        if !v.is_object() {
            return Err(CoreError::ConfigParse {
                path: self.models_path().display().to_string(),
                msg: "models.yml top-level must be an object".into(),
            });
        }
        Ok(v)
    }

    fn save_models_yml(&self, doc: &Value) -> Result<()> {
        // Convert JSON → YAML
        let yaml_str = serde_yaml::to_string(doc).map_err(|e| CoreError::ConfigParse {
            path: self.models_path().display().to_string(),
            msg: format!("YAML serialize error: {e}"),
        })?;
        std::fs::create_dir_all(self.models_path().parent().unwrap())?;
        atomic_write(&self.models_path(), yaml_str.as_bytes())
    }
}

impl ToolAdapter for OmpAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::OhMyPi
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.settings_path(), self.models_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut settings = load_json_or_empty(&self.settings_path())?;
        let mut models = self.load_models_yml()?;

        let settings_obj = settings.as_object_mut().unwrap();

        if provider.is_official() {
            // Restore original defaultProvider if saved.
            if let Some(original) = provider
                .extra
                .get("_original_default_provider")
                .and_then(|v| v.as_str())
            {
                settings_obj.insert("defaultProvider".to_string(), json!(original));
            }
            // Remove xfade provider from models.yml.
            let providers = models
                .as_object_mut()
                .and_then(|m| m.get_mut("providers"))
                .and_then(|v| v.as_object_mut());
            if let Some(providers) = providers {
                providers.remove("xfade");
            }
        } else {
            // The original defaultProvider for the first switch to third-party is written
            // into provider.extra by service::patch_original_capture before apply; here we
            // only write the target value.
            settings_obj.insert("defaultProvider".to_string(), json!("xfade"));

            // Add/update xfade provider in models.yml.
            let models_obj = models.as_object_mut().unwrap();
            let providers = models_obj
                .entry("providers")
                .or_insert_with(|| json!({}));
            let providers = providers.as_object_mut().ok_or_else(|| CoreError::ConfigParse {
                path: self.models_path().display().to_string(),
                msg: "models.yml 'providers' must be an object".into(),
            })?;
            let base_url = provider.base_url.as_deref().unwrap_or("http://127.0.0.1:9413");
            let model_id = provider
                .extra
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("gpt-4o");
            let api_type = provider
                .extra
                .get("api")
                .and_then(|v| v.as_str())
                .unwrap_or("openai-completions");
            providers.insert(
                "xfade".to_string(),
                json!({
                    "baseUrl": base_url,
                    "api": api_type,
                    "apiKey": api_key.unwrap_or("placeholder"),
                    "models": [
                        {
                            "id": model_id,
                            "name": model_id,
                            "contextWindow": 128000
                        }
                    ]
                }),
            );
        }

        self.save_models_yml(&models)?;
        save_json_pretty(&self.settings_path(), &settings)
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let models = self.load_models_yml()?;
        let providers = models
            .get("providers")
            .and_then(|v| v.as_object());
        let Some(providers) = providers else {
            return Ok(None);
        };
        for (id, prov) in providers {
            if id != "xfade" {
                continue;
            }
            let base_url = prov
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .map(String::from);
            let api_key = prov
                .get("apiKey")
                .and_then(|v| v.as_str())
                .map(String::from);
            let mut p = Provider::new("xfade", ToolKind::OhMyPi, base_url);
            if let Some(models_list) = prov.get("models") {
                p.extra = json!({"models": models_list.clone()});
            }
            return Ok(Some((p, api_key)));
        }
        Ok(None)
    }

    /// Read the current `defaultProvider` from settings.json as the restore target when
    /// switching back to official. Returns None if settings.json is absent or has no
    /// defaultProvider field.
    fn capture_original_state(&self) -> Result<Option<serde_json::Value>> {
        capture_default_provider(&self.settings_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, OmpAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = OmpAdapter::new(dir.path());
        (dir, ad)
    }

    fn write_initial(dir: &tempfile::TempDir) {
        std::fs::create_dir_all(dir.path().join(".omp/agent")).unwrap();
        std::fs::write(
            dir.path().join(".omp/agent/settings.json"),
            json!({"defaultProvider": "anthropic", "defaultModel": "claude-sonnet-4"}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn apply_third_party_writes_models_and_settings() {
        let (dir, ad) = setup();
        write_initial(&dir);

        let mut p = Provider::new("xfade", ToolKind::OhMyPi, Some("http://x".into()));
        p.extra = json!({"model": "gpt-4o"});
        ad.apply(&p, Some("sk-test")).unwrap();

        // Verify models.yml was created.
        let models_raw =
            std::fs::read_to_string(dir.path().join(".omp/agent/models.yml")).unwrap();
        assert!(models_raw.contains("xfade"));
        assert!(models_raw.contains("http://x"));
        assert!(models_raw.contains("openai-completions"));

        // Verify settings.json.
        let settings_raw =
            std::fs::read_to_string(dir.path().join(".omp/agent/settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&settings_raw).unwrap();
        assert_eq!(settings["defaultProvider"], "xfade");
    }

    #[test]
    fn apply_official_restores_default() {
        let (dir, ad) = setup();
        write_initial(&dir);

        // First apply third-party.
        let mut p = Provider::new("xfade", ToolKind::OhMyPi, Some("http://x".into()));
        p.extra = json!({"model": "gpt-4o", "_original_default_provider": "anthropic"});
        ad.apply(&p, Some("k")).unwrap();

        // Then restore official.
        let mut official = Provider::new("official", ToolKind::OhMyPi, None);
        official.extra = json!({"_original_default_provider": "anthropic"});
        ad.apply(&official, None).unwrap();

        let settings_raw =
            std::fs::read_to_string(dir.path().join(".omp/agent/settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&settings_raw).unwrap();
        assert_eq!(settings["defaultProvider"], "anthropic");
    }

    #[test]
    fn read_current_returns_xfade_entry() {
        let (_dir, ad) = setup();
        let mut p = Provider::new("xfade", ToolKind::OhMyPi, Some("http://x".into()));
        p.extra = json!({"model": "gpt-4o"});
        ad.apply(&p, Some("sk-9")).unwrap();

        let (got, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.base_url.as_deref(), Some("http://x"));
        assert_eq!(key.as_deref(), Some("sk-9"));
    }

    #[test]
    fn read_current_none_when_no_xfade() {
        let (_dir, ad) = setup();
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn capture_original_state_reads_default_provider() {
        let (dir, ad) = setup();
        write_initial(&dir);
        let captured = ad.capture_original_state().unwrap().unwrap();
        assert_eq!(
            captured["_original_default_provider"], "anthropic",
            "should read the defaultProvider from settings.json"
        );
    }

    #[test]
    fn capture_original_state_none_when_no_settings() {
        let (_dir, ad) = setup();
        assert!(ad.capture_original_state().unwrap().is_none());
    }

    #[test]
    fn capture_original_state_none_when_no_default_provider_field() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".omp/agent")).unwrap();
        std::fs::write(
            dir.path().join(".omp/agent/settings.json"),
            json!({"defaultModel": "x"}).to_string(),
        )
        .unwrap();
        assert!(ad.capture_original_state().unwrap().is_none());
    }
}
