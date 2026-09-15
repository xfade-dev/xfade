use super::{atomic_write, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_yaml::Value;
use std::path::{Path, PathBuf};

/// Aider adapter.
///
/// Aider stores its config at `~/.aider.conf.yml` (YAML). For a third-party
/// OpenAI-compatible endpoint we set three fields:
/// - `model`            — the model id the upstream expects (only when set)
/// - `openai-api-key`   — the API key
/// - `openai-api-base`  — the OpenAI-compatible base URL
///
/// Switching back to official clears those three keys (leaving any user's own
/// `.aider.conf.yml` fields untouched).
pub struct AiderAdapter {
    home: PathBuf,
}

impl AiderAdapter {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn config_path(&self) -> PathBuf {
        self.home.join(".aider.conf.yml")
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

    /// Return the top-level mapping, resetting `doc` to an empty mapping if the
    /// file was empty or a non-mapping value.
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

impl ToolAdapter for AiderAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Aider
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.config_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut doc = self.load()?;
        let map = Self::ensure_mapping(&mut doc);

        if provider.is_official() {
            map.remove(Value::from("model"));
            map.remove(Value::from("openai-api-key"));
            map.remove(Value::from("openai-api-base"));
        } else {
            let base_url = provider
                .base_url
                .as_deref()
                .ok_or_else(|| CoreError::ConfigParse {
                    path: "provider".into(),
                    msg: format!("third-party provider '{}' is missing base_url", provider.id),
                })?;
            if let Some(model) = provider.extra.get("model").and_then(|m| m.as_str()) {
                // litellm (Aider's router) requires a `provider/model` prefix for custom
                // endpoints; without one it errors with "LLM Provider NOT provided".
                // A provider-less name is routed as `openai/<model>` via openai-api-base.
                let prefixed = if model.contains('/') {
                    model.to_string()
                } else {
                    format!("openai/{model}")
                };
                map.insert(Value::from("model"), Value::from(prefixed));
            }
            map.insert(
                Value::from("openai-api-key"),
                Value::from(api_key.unwrap_or_default()),
            );
            // Aider's `openai-api-base` maps to litellm's `api_base`, which requests
            // `{api_base}/chat/completions`; it must end with `/v1`. A bare host
            // otherwise routes to `{host}/chat/completions` and returns the gateway's
            // HTML index page.
            let base = base_url.trim_end_matches('/');
            let base = if base.ends_with("/v1") {
                base_url.to_string()
            } else {
                format!("{base}/v1")
            };
            map.insert(Value::from("openai-api-base"), Value::from(base));
            // new-api 类网关（BM TokenHub）的流式响应末尾有一个空 `choices` chunk，
            // litellm 会把它判成 "Empty response received from LLM"。默认非流式；
            // 可在标准端点上经 `--set stream=true` 覆盖。
            let stream = provider
                .extra
                .get("stream")
                .and_then(|s| s.as_str())
                .map(|s| s == "true")
                .unwrap_or(false);
            map.insert(Value::from("stream"), Value::from(stream));
        }

        self.save(&doc)
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let doc = self.load()?;
        let map = match doc.as_mapping() {
            Some(m) => m,
            None => return Ok(None),
        };
        let base = map
            .get(Value::from("openai-api-base"))
            .and_then(|v| v.as_str());
        let token = map
            .get(Value::from("openai-api-key"))
            .and_then(|v| v.as_str());
        if base.is_none() && token.is_none() {
            return Ok(None);
        }
        let mut p = Provider::new("imported", ToolKind::Aider, base.map(String::from));
        if let Some(m) = map.get(Value::from("model")).and_then(|v| v.as_str()) {
            p.extra = serde_json::json!({ "model": m });
        }
        Ok(Some((p, token.map(String::from))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, AiderAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = AiderAdapter::new(dir.path());
        (dir, ad)
    }

    fn read_yaml(dir: &tempfile::TempDir) -> Value {
        let s = std::fs::read_to_string(dir.path().join(".aider.conf.yml")).unwrap();
        serde_yaml::from_str(&s).unwrap()
    }

    #[test]
    fn apply_third_party_sets_openai_fields() {
        let (dir, ad) = setup();
        std::fs::write(
            dir.path().join(".aider.conf.yml"),
            "model: gpt-4o\nread: [CONVENTIONS.md]\n",
        )
        .unwrap();

        let mut p = Provider::new(
            "kimi",
            ToolKind::Aider,
            Some("https://api.moonshot.cn/v1".into()),
        );
        p.extra = json!({"model": "kimi-k2.5"});
        ad.apply(&p, Some("sk-test")).unwrap();

        let doc = read_yaml(&dir);
        assert_eq!(doc["model"].as_str().unwrap(), "openai/kimi-k2.5");
        assert_eq!(doc["openai-api-key"].as_str().unwrap(), "sk-test");
        assert_eq!(
            doc["openai-api-base"].as_str().unwrap(),
            "https://api.moonshot.cn/v1"
        );
        // untouched user field is preserved
        assert_eq!(doc["read"][0].as_str().unwrap(), "CONVENTIONS.md");
        // default is non-streaming (avoids new-api empty-choices bug)
        assert!(!doc["stream"].as_bool().unwrap());
    }

    #[test]
    fn apply_stream_true_overrides_default() {
        let (dir, ad) = setup();
        let mut p = Provider::new("k", ToolKind::Aider, Some("https://x".into()));
        p.extra = json!({"stream": "true"});
        ad.apply(&p, Some("sk")).unwrap();
        let doc = read_yaml(&dir);
        assert!(doc["stream"].as_bool().unwrap());
    }

    #[test]
    fn apply_preserves_existing_provider_prefix() {
        let (dir, ad) = setup();
        let mut p = Provider::new("k", ToolKind::Aider, Some("https://x".into()));
        p.extra = json!({"model": "deepseek/deepseek-chat"});
        ad.apply(&p, Some("sk")).unwrap();
        let doc = read_yaml(&dir);
        assert_eq!(doc["model"].as_str().unwrap(), "deepseek/deepseek-chat");
    }

    #[test]
    fn apply_appends_v1_to_bare_base_url() {
        let (dir, ad) = setup();
        let mut p = Provider::new("k", ToolKind::Aider, Some("http://127.0.0.1:24860".into()));
        p.extra = json!({"model": "glm"});
        ad.apply(&p, Some("sk")).unwrap();
        let doc = read_yaml(&dir);
        assert_eq!(
            doc["openai-api-base"].as_str().unwrap(),
            "http://127.0.0.1:24860/v1"
        );
    }

    #[test]
    fn apply_keeps_existing_v1_suffix() {
        let (dir, ad) = setup();
        let mut p = Provider::new("k", ToolKind::Aider, Some("https://x/v1".into()));
        p.extra = json!({"model": "glm"});
        ad.apply(&p, Some("sk")).unwrap();
        let doc = read_yaml(&dir);
        assert_eq!(doc["openai-api-base"].as_str().unwrap(), "https://x/v1");
    }

    #[test]
    fn apply_official_clears_openai_fields() {
        let (dir, ad) = setup();
        std::fs::write(
            dir.path().join(".aider.conf.yml"),
            "model: gpt-4o\nopenai-api-key: sk-1\nopenai-api-base: https://x\nread: [A]\n",
        )
        .unwrap();

        let official = Provider::new("official", ToolKind::Aider, None);
        ad.apply(&official, None).unwrap();

        let doc = read_yaml(&dir);
        assert!(doc.get("model").is_none());
        assert!(doc.get("openai-api-key").is_none());
        assert!(doc.get("openai-api-base").is_none());
        assert_eq!(doc["read"][0].as_str().unwrap(), "A");
    }

    #[test]
    fn apply_creates_config_when_missing() {
        let (dir, ad) = setup();
        let p = Provider::new("k", ToolKind::Aider, Some("https://x".into()));
        ad.apply(&p, Some("k1")).unwrap();
        assert!(dir.path().join(".aider.conf.yml").exists());
    }

    #[test]
    fn read_current_none_when_no_third_party() {
        let (_dir, ad) = setup();
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn read_current_imports_existing() {
        let (dir, ad) = setup();
        std::fs::write(
            dir.path().join(".aider.conf.yml"),
            "model: kimi-k2.5\nopenai-api-key: sk-9\nopenai-api-base: https://relay\n",
        )
        .unwrap();
        let (p, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(p.base_url.as_deref(), Some("https://relay"));
        assert_eq!(key.as_deref(), Some("sk-9"));
        assert_eq!(p.extra["model"], "kimi-k2.5");
    }
}
