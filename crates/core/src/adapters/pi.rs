use super::{capture_default_provider, load_json_or_empty, save_json_pretty, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Pi Coding Agent adapter.
///
/// Pi stores its config at `~/.pi/agent/`:
/// - `settings.json` — `defaultProvider`, `defaultModel`
/// - `models.json` — custom providers (`providers.<id>.baseUrl` + `apiKey`)
/// - `auth.json` — credential store (not modified by xfade)
///
/// When applying a third-party provider:
/// 1. Add/replace a provider entry in `models.json`
/// 2. Set `defaultProvider` in `settings.json` to xfade's local proxy
///
/// When switching back to official: restore `defaultProvider` to the
/// original value (saved on first apply) and remove the xfade entry.
pub struct PiAdapter {
    home: PathBuf,
}

impl PiAdapter {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn agent_dir(&self) -> PathBuf {
        self.home.join(".pi").join("agent")
    }

    fn settings_path(&self) -> PathBuf {
        self.agent_dir().join("settings.json")
    }

    fn models_path(&self) -> PathBuf {
        self.agent_dir().join("models.json")
    }
}

impl ToolAdapter for PiAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Pi
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.settings_path(), self.models_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut settings = load_json_or_empty(&self.settings_path())?;
        let mut models = load_json_or_empty(&self.models_path())?;

        let settings_obj = settings.as_object_mut().unwrap();
        let models_obj = models.as_object_mut().unwrap();

        if provider.is_official() {
            // Restore original defaultProvider if saved.
            if let Some(original) = provider
                .extra
                .get("_original_default_provider")
                .and_then(|v| v.as_str())
            {
                settings_obj.insert("defaultProvider".to_string(), json!(original));
            }
            // Remove xfade provider entry from models.json.
            if let Some(providers) = models_obj
                .get_mut("providers")
                .and_then(|v| v.as_object_mut())
            {
                providers.remove("xfade");
            }
        } else {
            // The original defaultProvider for the first switch to third-party is written
            // into provider.extra by service::patch_original_capture before apply; here we
            // only write the target value.
            settings_obj.insert("defaultProvider".to_string(), json!("xfade"));

            // Add/update xfade provider in models.json.
            let providers = models_obj.entry("providers").or_insert_with(|| json!({}));
            let providers = providers
                .as_object_mut()
                .ok_or_else(|| CoreError::ConfigParse {
                    path: self.models_path().display().to_string(),
                    msg: "models.json 'providers' must be an object".into(),
                })?;
            let base_url = provider
                .base_url
                .as_deref()
                .unwrap_or("http://127.0.0.1:9413");
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
                        { "id": provider.extra.get("model").and_then(|v| v.as_str()).unwrap_or("gpt-4o") }
                    ]
                }),
            );
        }

        save_json_pretty(&self.models_path(), &models)?;
        save_json_pretty(&self.settings_path(), &settings)
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let models = load_json_or_empty(&self.models_path())?;
        let providers = models.get("providers").and_then(|v| v.as_object());
        let Some(providers) = providers else {
            return Ok(None);
        };
        // Look for the xfade provider entry.
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
            let mut p = Provider::new("xfade", ToolKind::Pi, base_url);
            if let Some(models) = prov.get("models") {
                p.extra = json!({ "models": models.clone() });
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
    use serde_json::Value;

    fn setup() -> (tempfile::TempDir, PiAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = PiAdapter::new(dir.path());
        (dir, ad)
    }

    fn read_settings(dir: &tempfile::TempDir) -> Value {
        let s = std::fs::read_to_string(dir.path().join(".pi/agent/settings.json")).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    fn read_models(dir: &tempfile::TempDir) -> Value {
        let s = std::fs::read_to_string(dir.path().join(".pi/agent/models.json")).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    fn write_initial(dir: &tempfile::TempDir) {
        std::fs::create_dir_all(dir.path().join(".pi/agent")).unwrap();
        std::fs::write(
            dir.path().join(".pi/agent/settings.json"),
            json!({"defaultProvider": "anthropic", "defaultModel": "claude-sonnet-4"}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn apply_third_party_writes_models_and_switches_default() {
        let (dir, ad) = setup();
        write_initial(&dir);

        let mut p = Provider::new(
            "xfade",
            ToolKind::Pi,
            Some("http://127.0.0.1:9413/v1".into()),
        );
        p.extra = json!({"model": "gpt-4o"});
        ad.apply(&p, Some("sk-test")).unwrap();

        let models = read_models(&dir);
        let xfade = &models["providers"]["xfade"];
        assert_eq!(xfade["baseUrl"], "http://127.0.0.1:9413/v1");
        assert_eq!(xfade["api"], "openai-completions");
        assert_eq!(xfade["models"][0]["id"], "gpt-4o");

        let settings = read_settings(&dir);
        assert_eq!(settings["defaultProvider"], "xfade");
    }

    #[test]
    fn apply_official_restores_default() {
        let (dir, ad) = setup();
        write_initial(&dir);

        // First apply third-party.
        let mut p = Provider::new("xfade", ToolKind::Pi, Some("http://x".into()));
        p.extra = json!({"model": "gpt-4o", "_original_default_provider": "anthropic"});
        ad.apply(&p, Some("k")).unwrap();

        // Then restore official.
        let official = Provider::new("official", ToolKind::Pi, None);
        // The CLI saves _original_default_provider into extra before calling apply.
        let mut official = official;
        official.extra = json!({"_original_default_provider": "anthropic"});
        ad.apply(&official, None).unwrap();

        let settings = read_settings(&dir);
        assert_eq!(settings["defaultProvider"], "anthropic");
    }

    #[test]
    fn read_current_returns_xfade_entry() {
        let (_dir, ad) = setup();
        let mut p = Provider::new("xfade", ToolKind::Pi, Some("http://x".into()));
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
        std::fs::create_dir_all(dir.path().join(".pi/agent")).unwrap();
        std::fs::write(
            dir.path().join(".pi/agent/settings.json"),
            json!({"defaultModel": "x"}).to_string(),
        )
        .unwrap();
        assert!(ad.capture_original_state().unwrap().is_none());
    }
}
