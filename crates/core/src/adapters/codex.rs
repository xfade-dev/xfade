use super::{atomic_write, client_base_url, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Map, Value};
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
            Ok(s) => Ok(
                serde_json::from_str(&s).map_err(|e| CoreError::ConfigParse {
                    path: self.auth_path().display().to_string(),
                    msg: e.to_string(),
                })?,
            ),
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
            // Restore the original top-level `model`/`model_context_window` if
            // xfade captured them on the first third-party switch; otherwise
            // drop the keys xfade wrote (Codex would send the third-party
            // model / window to the official API).
            if let Some(original) = provider
                .extra
                .get("_original_model")
                .and_then(|v| v.as_str())
            {
                root.insert("model".into(), toml::Value::String(original.to_string()));
            } else {
                root.remove("model");
            }
            if let Some(original) = provider
                .extra
                .get("_original_context_window")
                .and_then(|v| v.as_i64())
            {
                root.insert(
                    "model_context_window".into(),
                    toml::Value::Integer(original),
                );
            } else {
                root.remove("model_context_window");
            }
        } else {
            let id = &provider.id;
            let url = provider
                .base_url
                .as_deref()
                .ok_or_else(|| CoreError::ConfigParse {
                    path: "provider".into(),
                    msg: format!("third-party provider '{id}' is missing base_url"),
                })?;
            // Codex appends /responses (or /chat/completions) to base_url;
            // a bare host must carry the /v1 suffix.
            let wire_api = provider
                .extra
                .get("wire_api")
                .and_then(|v| v.as_str())
                .unwrap_or("responses"); // newer Codex has deprecated wire_api = "chat"
            let url = client_base_url(url, "openai-completions");

            // Default model: without a top-level `model`, Codex uses its own
            // default (gpt-6-astra) instead of the provider's model.
            if let Some(model) = provider.extra.get("model").and_then(|m| m.as_str()) {
                root.insert("model".into(), toml::Value::String(model.to_string()));
            }
            // Optional real context window; Codex otherwise assumes its
            // 272k fallback for unknown models, mistiming auto-compact.
            if let Some(cw) = provider
                .extra
                .get("context_window")
                .and_then(|v| v.as_i64())
            {
                root.insert("model_context_window".into(), toml::Value::Integer(cw));
            }

            // Note: newer Codex treats a provider with neither env_key nor
            // requires_openai_auth as "no auth" and sends no Authorization header
            // (upstreams then 401 "No api key passed in"). Set requires_openai_auth
            // so Codex authenticates with the OPENAI_API_KEY written to auth.json
            // (it ignores env_key when requires_openai_auth is set).
            let mut prov = toml::map::Map::new();
            prov.insert("name".into(), toml::Value::String(id.clone()));
            prov.insert("base_url".into(), toml::Value::String(url.to_string()));
            prov.insert("wire_api".into(), toml::Value::String(wire_api.to_string()));
            prov.insert("requires_openai_auth".into(), toml::Value::Boolean(true));

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
            let obj = auth.as_object_mut().unwrap();
            // Remove official OAuth tokens (which hijack Authorization), switching back to apikey auth.
            if obj.remove("tokens").is_some() {
                obj.insert("preferred_auth_method".into(), json!("apikey"));
            }
            obj.insert("OPENAI_API_KEY".into(), json!(api_key.unwrap_or_default()));
            atomic_write(
                &self.auth_path(),
                format!("{}\n", serde_json::to_string_pretty(&auth)?).as_bytes(),
            )?;
        }

        atomic_write(
            &self.config_path(),
            toml::to_string_pretty(&cfg)
                .map_err(CoreError::from)?
                .as_bytes(),
        )?;
        // Scan the whole file for leftover deprecated wire_api="chat", which newer Codex's full-file validation rejects at startup.
        for pid in find_legacy_wire_api(&cfg) {
            eprintln!(
                "[xfade] warning: model_providers.{pid} in config.toml still uses deprecated wire_api=\"chat\";\
                 newer Codex will refuse to start, please change it to \"responses\""
            );
        }
        Ok(())
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
        if let Some(w) = prov
            .and_then(|p| p.get("wire_api"))
            .and_then(|v| v.as_str())
        {
            // upstream deprecated chat: normalize on import so the snapshot doesn't revive an unloadable config
            let w = if w == "chat" { "responses" } else { w };
            p.extra = json!({ "wire_api": w });
        }
        if let Some(m) = cfg.get("model").and_then(|v| v.as_str()) {
            match p.extra.as_object_mut() {
                Some(obj) => {
                    obj.insert("model".into(), json!(m));
                }
                None => p.extra = json!({ "model": m }),
            }
        }
        if let Some(cw) = cfg.get("model_context_window").and_then(|v| v.as_integer()) {
            match p.extra.as_object_mut() {
                Some(obj) => {
                    obj.insert("context_window".into(), json!(cw));
                }
                None => p.extra = json!({ "context_window": cw }),
            }
        }
        let auth = self.load_auth()?;
        let key = auth
            .get("OPENAI_API_KEY")
            .and_then(|v| v.as_str())
            .map(String::from);
        Ok(Some((p, key)))
    }

    /// Capture the original top-level `model`/`model_context_window` (if any)
    /// as the restore target when switching back to official.
    fn capture_original_state(&self) -> Result<Option<serde_json::Value>> {
        let cfg = self.load_toml()?;
        let mut out = Map::new();
        if let Some(m) = cfg.get("model").and_then(|v| v.as_str()) {
            out.insert("_original_model".to_string(), json!(m));
        }
        if let Some(cw) = cfg.get("model_context_window").and_then(|v| v.as_integer()) {
            out.insert("_original_context_window".to_string(), json!(cw));
        }
        if out.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Value::Object(out)))
        }
    }
}

/// Scan the config for provider ids whose `model_providers.*.wire_api == "chat"` (deprecated value).
/// Newer Codex validates the whole file and refuses to start if any section contains chat.
fn find_legacy_wire_api(cfg: &toml::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(provs) = cfg.get("model_providers").and_then(|v| v.as_table()) {
        for (pid, p) in provs {
            if p.get("wire_api").and_then(|v| v.as_str()) == Some("chat") {
                out.push(pid.clone());
            }
        }
    }
    out
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
        assert!(prov.get("env_key").is_none()); // don't write env_key; go through auth.json
                                                // requires_openai_auth must be set, else Codex sends no Authorization
                                                // header (upstream 401 "No api key passed in").
        assert!(prov["requires_openai_auth"].as_bool().unwrap());

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
        assert!(auth.get("tokens").is_none());
        assert_eq!(auth["preferred_auth_method"], "apikey");
        assert_eq!(auth["OPENAI_API_KEY"], "sk-1");
    }

    #[test]
    fn find_legacy_wire_api_detects_chat() {
        let cfg: toml::Value = toml::from_str(
            "[model_providers.kimi]\nname=\"kimi\"\nbase_url=\"https://x\"\nwire_api=\"chat\"\n\
             [model_providers.oa]\nname=\"oa\"\nbase_url=\"https://y\"\nwire_api=\"responses\"\n",
        )
        .unwrap();
        assert_eq!(find_legacy_wire_api(&cfg), vec!["kimi".to_string()]);
    }

    #[test]
    fn apply_removes_oauth_tokens() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
        std::fs::write(
            dir.path().join(".codex/auth.json"),
            json!({"tokens": {"access_token": "oa-tok", "id_token": "id"}, "OPENAI_API_KEY": "old"})
                .to_string(),
        )
        .unwrap();
        let p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        ad.apply(&p, Some("sk-new")).unwrap();
        let auth: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".codex/auth.json")).unwrap(),
        )
        .unwrap();
        assert!(auth.get("tokens").is_none());
        assert_eq!(auth["preferred_auth_method"], "apikey");
        assert_eq!(auth["OPENAI_API_KEY"], "sk-new");
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
    fn apply_writes_top_level_model() {
        // Regression: without a top-level `model`, interactive Codex falls
        // back to its default model (e.g. gpt-6-astra) and third-party
        // gateways reject the request.
        let (dir, ad) = setup();
        let mut p = Provider::new("yy", ToolKind::Codex, Some("https://x".into()));
        p.extra = json!({"model": "glm-5-2-260617"});
        ad.apply(&p, Some("k")).unwrap();
        let cfg = read_config(&dir);
        assert_eq!(cfg["model"].as_str().unwrap(), "glm-5-2-260617");
    }

    #[test]
    fn official_restores_or_removes_model() {
        let (dir, ad) = setup();
        let mut p = Provider::new("yy", ToolKind::Codex, Some("https://x".into()));
        p.extra = json!({"model": "glm-5-2-260617"});
        ad.apply(&p, Some("k")).unwrap();

        // No captured original → model key is dropped on official switch.
        let official = Provider::new("official", ToolKind::Codex, None);
        ad.apply(&official, None).unwrap();
        let cfg = read_config(&dir);
        assert!(cfg.get("model").is_none());

        // Captured original → restored (service::patch_original_capture puts
        // the captured state on the official provider's extra).
        let mut p2 = Provider::new("yy", ToolKind::Codex, Some("https://x".into()));
        p2.extra = json!({"model": "glm-5-2-260617"});
        ad.apply(&p2, Some("k")).unwrap();
        let mut official2 = Provider::new("official", ToolKind::Codex, None);
        official2.extra = json!({"_original_model": "gpt-5-codex"});
        ad.apply(&official2, None).unwrap();
        let cfg = read_config(&dir);
        assert_eq!(cfg["model"].as_str().unwrap(), "gpt-5-codex");
    }

    #[test]
    fn capture_original_state_reads_model() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
        std::fs::write(
            dir.path().join(".codex/config.toml"),
            "model = \"gpt-5-codex\"\n",
        )
        .unwrap();
        let captured = ad.capture_original_state().unwrap().unwrap();
        assert_eq!(captured["_original_model"], "gpt-5-codex");
    }

    #[test]
    fn apply_writes_context_window_when_provided() {
        let (dir, ad) = setup();
        let mut p = Provider::new("yy", ToolKind::Codex, Some("https://x".into()));
        p.extra = json!({"model": "m1", "context_window": 131072});
        ad.apply(&p, Some("k")).unwrap();
        let cfg = read_config(&dir);
        assert_eq!(cfg["model_context_window"].as_integer().unwrap(), 131072);

        // official switch drops it (no captured original)
        ad.apply(&Provider::new("official", ToolKind::Codex, None), None)
            .unwrap();
        let cfg = read_config(&dir);
        assert!(cfg.get("model_context_window").is_none());
    }

    #[test]
    fn wire_api_from_extra() {
        let (dir, ad) = setup();
        let mut p = Provider::new("oa", ToolKind::Codex, Some("https://x".into()));
        p.extra = json!({"wire_api": "chat"}); // verify extra can override the default
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
        assert_eq!(got.base_url.as_deref(), Some("https://x/v1"));
        assert_eq!(key.as_deref(), Some("sk-9"));
    }

    #[test]
    fn read_current_normalizes_deprecated_chat_wire_api() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
        std::fs::write(
            dir.path().join(".codex/config.toml"),
            "model_provider = \"old\"\n\n[model_providers.old]\nname = \"old\"\nbase_url = \"https://x\"\nwire_api = \"chat\"\n",
        ).unwrap();
        let (got, _key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.extra["wire_api"], "responses");
    }
}
