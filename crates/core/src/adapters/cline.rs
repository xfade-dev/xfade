use super::{client_base_url, load_json_or_empty, save_json_pretty, ToolAdapter};
use crate::error::Result;
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Cline (VS Code extension + `cline` CLI) adapter.
///
/// Cline stores configuration under `~/.cline/data/settings/`:
/// - `providers.json` — credentials + active provider (single source of truth):
///   `{ "version": 1, "lastUsedProvider": "...", "modes": {},
///      "providers": { "<id>": { "settings": {provider, apiKey, model,
///      baseUrl, ...}, "updatedAt": <ISO>, "tokenSource": "manual" } } }`
/// - `models.json` — provider/model registry feeding the pickers.
///
/// The entry is written under the built-in provider id `openai-compatible`:
/// Cline's runtime dispatches provider clients by the providers.json key
/// against a fixed table of known provider types, and a custom id (e.g.
/// `xfade`) fails with "Unknown or disabled provider". Custom base URLs are
/// likewise only honored for the OpenAI / OpenAI-compatible types. The
/// written shape mirrors `cline auth openai-compatible -k <key> -m <model>
/// -b <url>` output (verified end-to-end against Cline CLI 3.0.62).
///
/// Switching back to official removes both entries and restores the
/// previously captured `lastUsedProvider`.
pub struct ClineAdapter {
    home: PathBuf,
}

/// Provider id Cline dispatches to its OpenAI-compatible client (the only
/// built-in type that accepts a custom base URL).
const CLINE_PROVIDER_ID: &str = "openai-compatible";

impl ClineAdapter {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn settings_path(&self) -> PathBuf {
        self.home
            .join(".cline")
            .join("data")
            .join("settings")
            .join("providers.json")
    }

    fn models_path(&self) -> PathBuf {
        self.home
            .join(".cline")
            .join("data")
            .join("settings")
            .join("models.json")
    }
}

impl ToolAdapter for ClineAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Cline
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.settings_path(), self.models_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut doc = load_json_or_empty(&self.settings_path())?;
        let root = doc.as_object_mut().unwrap();
        root.entry("version".to_string())
            .or_insert_with(|| json!(1));
        root.entry("modes".to_string()).or_insert_with(|| json!({}));

        if provider.is_official() {
            if let Some(original) = provider
                .extra
                .get("_original_last_used_provider")
                .and_then(|v| v.as_str())
            {
                root.insert("lastUsedProvider".to_string(), json!(original));
            } else {
                root.remove("lastUsedProvider");
            }
            if let Some(p) = root.get_mut("providers").and_then(|v| v.as_object_mut()) {
                p.remove(CLINE_PROVIDER_ID);
                // Clean up entries written by the pre-fix adapter.
                p.remove("xfade");
            }
            save_json_pretty(&self.settings_path(), &doc)?;

            let mut models = load_json_or_empty(&self.models_path())?;
            if let Some(p) = models
                .as_object_mut()
                .unwrap()
                .get_mut("providers")
                .and_then(|v| v.as_object_mut())
            {
                p.remove(CLINE_PROVIDER_ID);
                p.remove("xfade");
            }
            save_json_pretty(&self.models_path(), &models)
        } else {
            let base_url = provider
                .base_url
                .as_deref()
                .unwrap_or("http://127.0.0.1:24860");
            // The openai-compatible client appends /chat/completions to the
            // base, so a bare host needs the /v1 suffix.
            let base_url = client_base_url(base_url, "openai-completions");
            let mut settings = json!({
                "provider": CLINE_PROVIDER_ID,
                "apiKey": api_key.unwrap_or("placeholder"),
                "baseUrl": base_url,
            });
            let obj = settings.as_object_mut().unwrap();
            if let Some(model) = provider.extra.get("model").and_then(|m| m.as_str()) {
                obj.insert("model".to_string(), json!(model));
            }
            if let Some(cw) = provider
                .extra
                .get("context_window")
                .and_then(|v| v.as_i64())
            {
                obj.insert("contextWindow".to_string(), json!(cw));
            }
            if let Some(mt) = provider.extra.get("max_tokens").and_then(|v| v.as_i64()) {
                obj.insert("maxTokens".to_string(), json!(mt));
            }
            let model = provider.extra.get("model").and_then(|m| m.as_str());
            let context_window = provider
                .extra
                .get("context_window")
                .and_then(|v| v.as_i64());

            let providers = root
                .entry("providers".to_string())
                .or_insert_with(|| json!({}));
            if !providers.is_object() {
                *providers = json!({});
            }
            let providers_obj = providers.as_object_mut().unwrap();
            // Clean up entries written by the pre-fix adapter.
            providers_obj.remove("xfade");
            providers_obj.insert(
                CLINE_PROVIDER_ID.to_string(),
                json!({
                    "settings": settings,
                    "updatedAt": now_iso(),
                    "tokenSource": "manual",
                }),
            );
            root.insert("lastUsedProvider".to_string(), json!(CLINE_PROVIDER_ID));
            save_json_pretty(&self.settings_path(), &doc)?;

            // models.json: register the provider + its model so both show up
            // in Cline's provider/model pickers.
            let mut models_doc = load_json_or_empty(&self.models_path())?;
            let mroot = models_doc.as_object_mut().unwrap();
            mroot
                .entry("version".to_string())
                .or_insert_with(|| json!(1));
            let mproviders = mroot
                .entry("providers".to_string())
                .or_insert_with(|| json!({}));
            if !mproviders.is_object() {
                *mproviders = json!({});
            }
            let mut provider_meta = json!({
                "name": CLINE_PROVIDER_ID,
                "baseUrl": base_url,
            });
            let mut models_map = json!({});
            if let Some(model) = model {
                let mut entry = json!({ "name": model });
                if let Some(cw) = context_window {
                    entry
                        .as_object_mut()
                        .unwrap()
                        .insert("contextWindow".to_string(), json!(cw));
                }
                provider_meta
                    .as_object_mut()
                    .unwrap()
                    .insert("defaultModelId".to_string(), json!(model));
                models_map
                    .as_object_mut()
                    .unwrap()
                    .insert(model.to_string(), entry);
            }
            let mproviders_obj = mproviders.as_object_mut().unwrap();
            mproviders_obj.remove("xfade");
            mproviders_obj.insert(
                CLINE_PROVIDER_ID.to_string(),
                json!({
                    "provider": provider_meta,
                    "models": models_map,
                }),
            );
            save_json_pretty(&self.models_path(), &models_doc)
        }
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let doc = load_json_or_empty(&self.settings_path())?;
        let providers = doc.get("providers");
        // Prefer the current key; fall back to the legacy "xfade" entry so
        // import still works for configs written by the pre-fix adapter.
        let entry = providers
            .and_then(|p| p.get(CLINE_PROVIDER_ID))
            .or_else(|| providers.and_then(|p| p.get("xfade")))
            .and_then(|e| e.get("settings"));
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
        let mut p = Provider::new("xfade", ToolKind::Cline, base_url);
        let mut extra = serde_json::Map::new();
        if let Some(m) = entry.get("model").and_then(|v| v.as_str()) {
            extra.insert("model".to_string(), json!(m));
        }
        if let Some(cw) = entry.get("contextWindow").and_then(|v| v.as_i64()) {
            extra.insert("context_window".to_string(), json!(cw));
        }
        if let Some(mt) = entry.get("maxTokens").and_then(|v| v.as_i64()) {
            extra.insert("max_tokens".to_string(), json!(mt));
        }
        if !extra.is_empty() {
            p.extra = Value::Object(extra);
        }
        Ok(Some((p, api_key)))
    }

    /// Capture the current `lastUsedProvider` as the restore target when
    /// switching back to official.
    fn capture_original_state(&self) -> Result<Option<Value>> {
        let doc = load_json_or_empty(&self.settings_path())?;
        if let Some(id) = doc.get("lastUsedProvider").and_then(|v| v.as_str()) {
            // Skip self-references (current and legacy xfade-written values).
            if id != CLINE_PROVIDER_ID && id != "xfade" {
                return Ok(Some(json!({ "_original_last_used_provider": id })));
            }
        }
        Ok(None)
    }
}

fn now_iso() -> String {
    // Cline's zod schema requires an ISO datetime string.
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, ClineAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = ClineAdapter::new(dir.path());
        (dir, ad)
    }

    fn read_doc(dir: &tempfile::TempDir) -> Value {
        let path = dir.path().join(".cline/data/settings/providers.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn write_doc(dir: &tempfile::TempDir, v: Value) {
        let path = dir.path().join(".cline/data/settings/providers.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, v.to_string()).unwrap();
    }

    fn provider(base: &str, extra: Value) -> Provider {
        let mut p = Provider::new("xfade", ToolKind::Cline, Some(base.into()));
        p.extra = extra;
        p
    }

    #[test]
    fn apply_writes_openai_compatible_entry_and_last_used() {
        let (dir, ad) = setup();
        ad.apply(
            &provider("http://127.0.0.1:24860", json!({"model": "glm-5-2-260617"})),
            Some("sk-test"),
        )
        .unwrap();
        let doc = read_doc(&dir);
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["lastUsedProvider"], "openai-compatible");
        let s = &doc["providers"]["openai-compatible"]["settings"];
        assert_eq!(s["provider"], "openai-compatible");
        assert_eq!(s["apiKey"], "sk-test");
        assert_eq!(s["baseUrl"], "http://127.0.0.1:24860/v1");
        assert_eq!(s["model"], "glm-5-2-260617");
        assert_eq!(
            doc["providers"]["openai-compatible"]["tokenSource"],
            "manual"
        );
        // The entry mirrors `cline auth openai-compatible` output: no
        // protocol/client fields.
        assert!(s.get("protocol").is_none());
        assert!(s.get("client").is_none());
    }

    #[test]
    fn apply_removes_legacy_xfade_entry() {
        let (dir, ad) = setup();
        write_doc(
            &dir,
            json!({"version":1, "lastUsedProvider":"xfade", "modes":{}, "providers":{"xfade": {"settings": {"provider":"xfade"}}}}),
        );
        ad.apply(
            &provider("http://127.0.0.1:24860", json!({"model": "m"})),
            Some("k"),
        )
        .unwrap();
        let doc = read_doc(&dir);
        assert!(doc["providers"].get("xfade").is_none());
        assert!(doc["providers"].get("openai-compatible").is_some());
    }

    #[test]
    fn official_removes_entry_and_restores_last_used() {
        let (dir, ad) = setup();
        write_doc(
            &dir,
            json!({"version":1, "lastUsedProvider":"openrouter", "modes":{}, "providers":{}}),
        );
        ad.apply(
            &provider("http://127.0.0.1:24860", json!({"model": "m"})),
            Some("k"),
        )
        .unwrap();
        let mut official = Provider::new("official", ToolKind::Cline, None);
        official.extra = json!({"_original_last_used_provider": "openrouter"});
        ad.apply(&official, None).unwrap();
        let doc = read_doc(&dir);
        assert_eq!(doc["lastUsedProvider"], "openrouter");
        assert!(doc["providers"].get("openai-compatible").is_none());
    }

    #[test]
    fn read_current_returns_openai_compatible_entry() {
        let (_dir, ad) = setup();
        ad.apply(
            &provider("http://127.0.0.1:24860", json!({"model": "m"})),
            Some("sk-9"),
        )
        .unwrap();
        let (got, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.base_url.as_deref(), Some("http://127.0.0.1:24860/v1"));
        assert_eq!(key.as_deref(), Some("sk-9"));
    }

    #[test]
    fn read_current_falls_back_to_legacy_xfade_entry() {
        let (dir, ad) = setup();
        write_doc(
            &dir,
            json!({"version":1, "lastUsedProvider":"xfade", "modes":{}, "providers":{"xfade": {"settings": {"provider":"xfade", "baseUrl":"http://x/v1", "apiKey":"sk-legacy"}}}}),
        );
        let (got, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.base_url.as_deref(), Some("http://x/v1"));
        assert_eq!(key.as_deref(), Some("sk-legacy"));
    }

    #[test]
    fn read_current_none_without_entry() {
        let (_dir, ad) = setup();
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn capture_original_state_reads_last_used() {
        let (dir, ad) = setup();
        write_doc(
            &dir,
            json!({"version":1, "lastUsedProvider":"openrouter", "modes":{}, "providers":{}}),
        );
        let captured = ad.capture_original_state().unwrap().unwrap();
        assert_eq!(captured["_original_last_used_provider"], "openrouter",);
    }

    #[test]
    fn capture_original_state_ignores_self_references() {
        let (dir, ad) = setup();
        for id in ["xfade", "openai-compatible"] {
            write_doc(
                &dir,
                json!({"version":1, "lastUsedProvider":id, "modes":{}, "providers":{}}),
            );
            assert!(ad.capture_original_state().unwrap().is_none());
        }
    }
}
