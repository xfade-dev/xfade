use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

/// Secret storage backend.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SecretsBackend {
    /// System keychain (macOS Keychain / Windows Credential Manager / Linux Secret Service).
    #[default]
    Keyring,
    /// Local JSON file (`secrets.json`, owner-only 0600). Avoids keychain auth prompts.
    File,
}

impl std::fmt::Display for SecretsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SecretsBackend::Keyring => "keyring",
            SecretsBackend::File => "file",
        })
    }
}

impl FromStr for SecretsBackend {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "keyring" => Ok(SecretsBackend::Keyring),
            "file" => Ok(SecretsBackend::File),
            _ => Err(format!(
                "unknown secrets backend: {s} (expected keyring|file)"
            )),
        }
    }
}

/// Key_ref used to store the global API key in the secret store (not in
/// `config.json`, so it stays under the secret backend's protection).
pub const GLOBAL_API_KEY_REF: &str = "xfade/global/api_key";

/// Global config, persisted at `<data_dir>/config.json`.
///
/// `base_url` and `model` act as shared defaults for every tool (like CC Switch's
/// "general" config): a provider that doesn't specify its own value falls back to
/// these. The global API key lives in the secret store under [`GLOBAL_API_KEY_REF`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Secret storage backend for all tools.
    #[serde(default)]
    pub secrets: SecretsBackend,
    /// Global default base URL, applied when a provider omits `base_url`.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Global default model, applied when a provider omits `model`.
    #[serde(default)]
    pub model: Option<String>,
    /// Global default API type for Pi/OMP (`openai-completions` | `anthropic-messages`).
    /// Ignored by tools that don't support multiple wire protocols (claude/codex/opencode).
    #[serde(default)]
    pub api: Option<String>,
}

impl Config {
    /// Load config from `<dir>/config.json`; a missing or invalid file yields defaults.
    pub fn load(dir: &Path) -> Config {
        let path = dir.join("config.json");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist config to `<dir>/config.json` (pretty-printed).
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let s = serde_json::to_string_pretty(self)?;
        std::fs::write(dir.join("config.json"), s)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::load(dir.path());
        assert_eq!(c.secrets, SecretsBackend::Keyring);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config {
            secrets: SecretsBackend::File,
            base_url: Some("https://x".into()),
            model: Some("glm-5".into()),
            api: Some("anthropic-messages".into()),
        };
        c.save(dir.path()).unwrap();
        let loaded = Config::load(dir.path());
        assert_eq!(loaded.secrets, SecretsBackend::File);
        assert_eq!(loaded.base_url.as_deref(), Some("https://x"));
        assert_eq!(loaded.model.as_deref(), Some("glm-5"));
        assert_eq!(loaded.api.as_deref(), Some("anthropic-messages"));
    }

    #[test]
    fn backend_roundtrip_str() {
        assert_eq!(
            "file".parse::<SecretsBackend>().unwrap(),
            SecretsBackend::File
        );
        assert_eq!(
            "keyring".parse::<SecretsBackend>().unwrap(),
            SecretsBackend::Keyring
        );
        assert!(SecretsBackend::File.to_string() == "file");
        assert!("bogus".parse::<SecretsBackend>().is_err());
    }
}
