pub mod aider;
pub mod claude_code;
pub mod cline;
pub mod codex;
pub mod omp;
pub mod opencode;
pub mod pi;

use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub trait ToolAdapter: Send + Sync {
    fn tool(&self) -> ToolKind;
    /// Config files modified on switch (the service layer backs these up).
    fn config_paths(&self) -> Vec<PathBuf>;
    /// Write the provider into the tool config.
    /// `provider.base_url == None` means switch back to official (clear/restore snapshot); api_key is ignored.
    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()>;
    /// Read the tool's currently-active config (used for the first import); returns Ok(None) when there's no third-party config.
    /// Returns (provider, api_key); api_key may be None (when the secret can't be read).
    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>>;

    /// Return the "original state" to restore when switching back to official, stored as a
    /// JSON object in `provider.extra` (keys use a `_` prefix, e.g. `_original_default_provider`).
    ///
    /// Defaults to `None`: claude_code/codex/opencode switch to official by "clearing env/model_provider",
    /// so they don't need to restore a field. OMP/Pi need to restore `defaultProvider`, so they override this to read the current value.
    ///
    /// Only called on the "first switch to third-party" when the current active provider has not saved the original state yet
    /// (see `service::Core::patch_original_capture`), to avoid overwriting the saved original value on every switch.
    fn capture_original_state(&self) -> Result<Option<serde_json::Value>> {
        Ok(None)
    }
}

pub fn adapter_for(tool: ToolKind, home: &Path) -> Box<dyn ToolAdapter> {
    match tool {
        ToolKind::ClaudeCode => Box::new(claude_code::ClaudeCodeAdapter::new(home)),
        ToolKind::Codex => Box::new(codex::CodexAdapter::new(home)),
        ToolKind::OpenCode => Box::new(opencode::OpenCodeAdapter::new(home)),
        ToolKind::Pi => Box::new(pi::PiAdapter::new(home)),
        ToolKind::OhMyPi => Box::new(omp::OmpAdapter::new(home)),
        ToolKind::Aider => Box::new(aider::AiderAdapter::new(home)),
        ToolKind::Cline => Box::new(cline::ClineAdapter::new(home)),
    }
}

/// Atomic write via temp file + rename; auto-creates parent directories.
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp-xfade");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Load a JSON object file, returning `{}` when the file is absent.
/// Errors if the top-level value is not an object.
pub(crate) fn load_json_or_empty(path: &Path) -> Result<Value> {
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

/// Save a JSON value as pretty-printed JSON with a trailing newline.
pub(crate) fn save_json_pretty(path: &Path, doc: &Value) -> Result<()> {
    atomic_write(
        path,
        format!("{}\n", serde_json::to_string_pretty(doc)?).as_bytes(),
    )
}

/// Read the current `defaultProvider` from a settings.json file as the restore target
/// when switching back to official. Returns None when the field is absent.
pub(crate) fn capture_default_provider(settings_path: &Path) -> Result<Option<Value>> {
    let settings = load_json_or_empty(settings_path)?;
    if let Some(dp) = settings.get("defaultProvider").and_then(|v| v.as_str()) {
        return Ok(Some(json!({ "_original_default_provider": dp })));
    }
    Ok(None)
}

/// If `base_url` is a bare host (no path, e.g. `http://host:3000`), append the
/// standard `/v1` prefix — OpenAI- and Anthropic-compatible APIs live under
/// `/v1`, and a bare-host URL otherwise hits the gateway's web UI, which
/// returns 200 HTML and breaks streaming clients with cryptic errors.
// ponytail: heuristic; gateways with a non-/v1 API path (e.g. /v1beta/openai)
// must spell out the full path in their provider base_url.
pub(crate) fn with_v1_if_bare_host(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let host = base.split_once("://").map(|(_, r)| r).unwrap_or(base);
    if host.contains('/') {
        base.to_string()
    } else {
        format!("{base}/v1")
    }
}

/// Adjust a provider base_url for the wire protocol a Pi-style client (`api`
/// field in models.json) will use. OpenAI SDKs append `/chat/completions` or
/// `/responses` to the base, so a bare host needs a `/v1` suffix; the Anthropic
/// SDK appends `/v1/messages` itself, so a `/v1` suffix must be stripped.
pub(crate) fn client_base_url(base: &str, api: &str) -> String {
    let base = base.trim_end_matches('/');
    if api == "anthropic-messages" {
        return base.strip_suffix("/v1").unwrap_or(base).to_string();
    }
    with_v1_if_bare_host(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/c.json");
        atomic_write(&target, b"{}").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"{}");
    }

    #[test]
    fn atomic_write_overwrites_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("c.json");
        atomic_write(&target, b"1").unwrap();
        atomic_write(&target, b"2").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"2");
        assert!(!dir.path().join("c.tmp-xfade").exists());
    }
}
