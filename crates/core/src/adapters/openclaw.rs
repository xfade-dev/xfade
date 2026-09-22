use super::{client_base_url, load_json5_or_empty, save_json_pretty, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// OpenClaw adapter.
///
/// OpenClaw reads an optional JSON5 config from `~/.openclaw/openclaw.json`.
/// A third-party OpenAI/Anthropic-compatible endpoint is wired in as a custom
/// provider under `models.providers`, and set as the agent's primary model:
///
/// ```json5
/// {
///   agents: { defaults: { model: { primary: "xfade/<model>" } } },
///   models: { providers: { xfade: {
///     baseUrl: "<base_url>",
///     apiKey: "<api_key>",
///     api: "openai-completions",
///     models: [{ id: "<model>", name: "<model>" }],
///   } } },
/// }
/// ```
///
/// Switching back to official removes the `xfade` provider entry and restores
/// the previously captured `agents.defaults.model.primary`.
///
/// The config is read as JSON5 (comments, trailing commas, unquoted keys, single
/// quotes) since OpenClaw's native format is JSON5; writes are plain JSON (a
/// valid JSON5 subset OpenClaw accepts). A hand-edited config with comments is
/// therefore preserved on read, but its comments are dropped when xfade rewrites
/// the file on the next switch.
pub struct OpenClawAdapter {
    home: PathBuf,
}

/// Provider id OpenClaw dispatches our custom endpoint through.
const XFADE_PROVIDER_ID: &str = "xfade";

impl OpenClawAdapter {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn config_path(&self) -> PathBuf {
        self.home.join(".openclaw").join("openclaw.json")
    }

    /// Walk `agents.defaults.model.primary` as a mutable string value,
    /// creating the nesting as needed.
    fn primary_mut(doc: &mut Value) -> Option<&mut Value> {
        Some(
            doc.as_object_mut()?
                .entry("agents".to_string())
                .or_insert_with(|| json!({}))
                .as_object_mut()?
                .entry("defaults".to_string())
                .or_insert_with(|| json!({}))
                .as_object_mut()?
                .entry("model".to_string())
                .or_insert_with(|| json!({}))
                .as_object_mut()?
                .entry("primary".to_string())
                .or_insert(Value::Null),
        )
    }
}

impl ToolAdapter for OpenClawAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::OpenClaw
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.config_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut doc = load_json5_or_empty(&self.config_path())?;

        if provider.is_official() {
            if let Some(p) = provider
                .extra
                .get("_original_primary")
                .and_then(|v| v.as_str())
            {
                if let Some(v) = Self::primary_mut(&mut doc) {
                    *v = json!(p);
                }
            } else if let Some(primary) = Self::primary_mut(&mut doc) {
                // No captured original: clear the xfade primary we set.
                if primary
                    .as_str()
                    .map(|s| s.starts_with("xfade/"))
                    .unwrap_or(false)
                {
                    *primary = Value::Null;
                }
            }
            if let Some(providers) = doc
                .as_object_mut()
                .and_then(|o| o.get_mut("models"))
                .and_then(|m| m.as_object_mut())
                .and_then(|m| m.get_mut("providers"))
                .and_then(|p| p.as_object_mut())
            {
                providers.remove(XFADE_PROVIDER_ID);
            }
        } else {
            // OpenClaw dispatches a provider via `agents.defaults.model.primary =
            // "<provider>/<model>"`, so a model id is required to activate the
            // xfade provider — without it the provider is registered but never
            // selected (half-state).
            let model = provider
                .extra
                .get("model")
                .and_then(|m| m.as_str())
                .ok_or_else(|| CoreError::ConfigParse {
                    path: "provider".into(),
                    msg: format!(
                        "OpenClaw third-party provider '{}' is missing `model` (required to set agents.defaults.model.primary)",
                        provider.id
                    ),
                })?;
            let api = provider
                .extra
                .get("api")
                .and_then(|v| v.as_str())
                .unwrap_or("openai-completions");
            let base_url = provider
                .base_url
                .as_deref()
                .unwrap_or("http://127.0.0.1:24860/v1");
            // OpenClaw dispatches requests through the provider's `api` adapter:
            // the OpenAI-compatible adapters append /chat/completions (so a bare
            // host needs the /v1 suffix), while the Anthropic adapter appends
            // /v1/messages itself (so a /v1 suffix must be stripped).
            let base_url = client_base_url(base_url, api);

            let provider_meta = json!({
                "baseUrl": base_url,
                "apiKey": api_key.unwrap_or_default(),
                "api": api,
                "models": [{ "id": model, "name": model }],
            });

            let providers = doc
                .as_object_mut()
                .unwrap()
                .entry("models".to_string())
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .unwrap()
                .entry("providers".to_string())
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .unwrap();
            providers.insert(XFADE_PROVIDER_ID.to_string(), provider_meta);

            // Set the agent's primary model to the xfade provider.
            *Self::primary_mut(&mut doc).unwrap() = json!(format!("xfade/{model}"));
        }

        save_json_pretty(&self.config_path(), &doc)
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let doc = load_json5_or_empty(&self.config_path())?;
        let entry = doc
            .get("models")
            .and_then(|m| m.get("providers"))
            .and_then(|p| p.get(XFADE_PROVIDER_ID));
        let Some(entry) = entry else {
            return Ok(None);
        };
        let base_url = entry
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(String::from);
        let api_key = entry
            .get("apiKey")
            .and_then(|v| v.as_str())
            .map(String::from);
        let mut p = Provider::new("imported", ToolKind::OpenClaw, base_url);
        if let Some(m) = entry
            .get("models")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str())
        {
            p.extra = json!({ "model": m });
        }
        Ok(Some((p, api_key)))
    }

    /// Capture the current `agents.defaults.model.primary` so switching back to
    /// official can restore it.
    fn capture_original_state(&self) -> Result<Option<Value>> {
        let doc = load_json5_or_empty(&self.config_path())?;
        if let Some(primary) = doc
            .get("agents")
            .and_then(|a| a.get("defaults"))
            .and_then(|d| d.get("model"))
            .and_then(|m| m.get("primary"))
            .and_then(|v| v.as_str())
        {
            // Skip self-references (current xfade-written value).
            if !primary.starts_with("xfade/") {
                return Ok(Some(json!({ "_original_primary": primary })));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, OpenClawAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = OpenClawAdapter::new(dir.path());
        (dir, ad)
    }

    fn read_doc(dir: &tempfile::TempDir) -> Value {
        let path = dir.path().join(".openclaw/openclaw.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn write_doc(dir: &tempfile::TempDir, v: Value) {
        let path = dir.path().join(".openclaw/openclaw.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, v.to_string()).unwrap();
    }

    fn provider(base: &str, extra: Value) -> Provider {
        let mut p = Provider::new("kimi", ToolKind::OpenClaw, Some(base.into()));
        p.extra = extra;
        p
    }

    #[test]
    fn apply_writes_xfade_provider_and_primary() {
        let (dir, ad) = setup();
        ad.apply(
            &provider("http://127.0.0.1:24860", json!({"model": "glm-5"})),
            Some("sk-test"),
        )
        .unwrap();
        let doc = read_doc(&dir);
        let prov = &doc["models"]["providers"]["xfade"];
        assert_eq!(prov["baseUrl"], "http://127.0.0.1:24860/v1");
        assert_eq!(prov["apiKey"], "sk-test");
        assert_eq!(prov["api"], "openai-completions");
        assert_eq!(prov["models"][0]["id"], "glm-5");
        assert_eq!(doc["agents"]["defaults"]["model"]["primary"], "xfade/glm-5");
    }

    #[test]
    fn apply_anthropic_api_strips_v1_suffix() {
        let (dir, ad) = setup();
        ad.apply(
            &provider(
                "https://api.x.ai/v1",
                json!({"model": "m", "api": "anthropic-messages"}),
            ),
            Some("k"),
        )
        .unwrap();
        let doc = read_doc(&dir);
        let prov = &doc["models"]["providers"]["xfade"];
        assert_eq!(prov["api"], "anthropic-messages");
        // Anthropic SDK appends /v1/messages itself, so the /v1 suffix is stripped.
        assert_eq!(prov["baseUrl"], "https://api.x.ai");
    }

    #[test]
    fn apply_preserves_existing_config() {
        let (dir, ad) = setup();
        write_doc(
            &dir,
            json!({"channels": {"whatsapp": {"allowFrom": ["+1"]}}}),
        );
        ad.apply(&provider("http://x", json!({"model": "m"})), Some("k"))
            .unwrap();
        let doc = read_doc(&dir);
        assert_eq!(doc["channels"]["whatsapp"]["allowFrom"][0], "+1");
        assert_eq!(doc["agents"]["defaults"]["model"]["primary"], "xfade/m");
    }

    #[test]
    fn official_removes_xfade_and_restores_primary() {
        let (dir, ad) = setup();
        write_doc(
            &dir,
            json!({"agents": {"defaults": {"model": {"primary": "anthropic/claude"}}}}),
        );
        let captured = ad.capture_original_state().unwrap().unwrap();
        // Switch to third-party (merge the captured original into extra so the
        // model field is preserved).
        let mut third = provider("http://x", json!({"model": "m"}));
        third.extra.as_object_mut().unwrap().insert(
            "_original_primary".to_string(),
            captured["_original_primary"].clone(),
        );
        ad.apply(&third, Some("k")).unwrap();
        assert_eq!(
            read_doc(&dir)["agents"]["defaults"]["model"]["primary"],
            "xfade/m"
        );
        let mut official = Provider::new("official", ToolKind::OpenClaw, None);
        official.extra = json!({ "_original_primary": captured["_original_primary"] });
        ad.apply(&official, None).unwrap();
        let doc = read_doc(&dir);
        assert_eq!(
            doc["agents"]["defaults"]["model"]["primary"],
            "anthropic/claude"
        );
        assert!(doc["models"]["providers"].get("xfade").is_none());
    }

    #[test]
    fn read_current_returns_xfade_entry() {
        let (_dir, ad) = setup();
        ad.apply(&provider("http://x", json!({"model": "m"})), Some("sk-9"))
            .unwrap();
        let (got, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.base_url.as_deref(), Some("http://x/v1"));
        assert_eq!(key.as_deref(), Some("sk-9"));
        assert_eq!(got.extra["model"], "m");
    }

    #[test]
    fn read_current_none_without_entry() {
        let (_dir, ad) = setup();
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn read_current_parses_json5_config() {
        let (dir, ad) = setup();
        // JSON5: // comment, unquoted keys, trailing commas, single quotes.
        let path = dir.path().join(".openclaw/openclaw.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
  // hand-edited comment
  agents: { defaults: { model: { primary: 'xfade/glm-5' } } },
  models: { providers: { xfade: {
    baseUrl: 'http://x/v1',
    apiKey: 'sk-5',
    api: 'openai-completions',
    models: [{ id: 'glm-5', name: 'glm-5' }],
  } } },
}"#,
        )
        .unwrap();
        let (got, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.base_url.as_deref(), Some("http://x/v1"));
        assert_eq!(key.as_deref(), Some("sk-5"));
        assert_eq!(got.extra["model"], "glm-5");
    }

    #[test]
    fn apply_rewrites_json5_config_preserving_fields() {
        let (dir, ad) = setup();
        // A hand-edited JSON5 config (comment + unquoted keys) must not break
        // `apply`: it parses, and the untouched sibling field survives the
        // rewrite (the comment is dropped, which is acceptable).
        let path = dir.path().join(".openclaw/openclaw.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\n  // keep this channel\n  channels: { whatsapp: { allowFrom: ['+1'] } },\n}\n",
        )
        .unwrap();
        ad.apply(&provider("http://x", json!({"model": "m"})), Some("k"))
            .unwrap();
        let doc = read_doc(&dir);
        assert_eq!(doc["channels"]["whatsapp"]["allowFrom"][0], "+1");
        assert_eq!(doc["agents"]["defaults"]["model"]["primary"], "xfade/m");
    }

    #[test]
    fn capture_reads_primary() {
        let (dir, ad) = setup();
        write_doc(
            &dir,
            json!({"agents": {"defaults": {"model": {"primary": "anthropic/claude"}}}}),
        );
        let captured = ad.capture_original_state().unwrap().unwrap();
        assert_eq!(captured["_original_primary"], "anthropic/claude");
    }

    #[test]
    fn capture_ignores_xfade_primary() {
        let (dir, ad) = setup();
        write_doc(
            &dir,
            json!({"agents": {"defaults": {"model": {"primary": "xfade/m"}}}}),
        );
        assert!(ad.capture_original_state().unwrap().is_none());
    }

    #[test]
    fn apply_without_model_errors() {
        let (_dir, ad) = setup();
        let err = ad
            .apply(&provider("http://x", json!({})), Some("k"))
            .unwrap_err();
        assert!(err.to_string().contains("missing `model`"));
    }
}
