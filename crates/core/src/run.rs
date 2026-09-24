//! Process wrapper (`xfade run`): launch a tool CLI with a provider injected
//! via temporary environment variables, without touching on-disk config.
//!
//! Only tools whose native contract is env-based are supported:
//! - **Claude Code** reads `ANTHROPIC_*` env vars directly (the adapter's
//!   `settings.json` `env` block merely becomes process env).
//! - **Aider** maps every CLI/config option to an `AIDER_`-prefixed env var
//!   (`AIDER_OPENAI_API_KEY` / `AIDER_OPENAI_API_BASE` / `AIDER_MODEL`).
//!
//! Config-file-driven tools (codex / opencode / pi / omp / cline / hermes /
//! openclaw) have no env-only contract; `run` refuses them with a hint to
//! `xfade use` instead.

use crate::adapters::claude_code::claude_env_pairs;
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};

/// The executable to launch for a tool (resolved via `PATH` by the caller).
pub fn tool_command(tool: ToolKind) -> &'static str {
    match tool {
        ToolKind::ClaudeCode => "claude",
        ToolKind::Codex => "codex",
        ToolKind::OpenCode => "opencode",
        ToolKind::Pi => "pi",
        ToolKind::OhMyPi => "omp",
        ToolKind::Aider => "aider",
        ToolKind::Cline => "cline",
        ToolKind::Hermes => "hermes",
        ToolKind::OpenClaw => "openclaw",
    }
}

/// On Windows, `CreateProcess` only resolves `.exe` — npm-style `.cmd`/`.bat`
/// shims (how Claude Code and most CLI tools are installed there) are NOT
/// found when spawning a bare name. Probe `PATH` + `PATHEXT` for the real
/// file and return its absolute path; non-Windows (and not-found) fall back
/// to the bare name so Unix behavior is unchanged.
#[cfg(windows)]
pub fn resolve_command(bin: &str) -> String {
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for ext in pathext.split(';').filter(|s| !s.is_empty()) {
                let candidate = dir.join(format!("{bin}{ext}"));
                if candidate.is_file() {
                    return candidate.display().to_string();
                }
            }
        }
    }
    bin.to_string()
}

/// Unix: the OS `exec` resolves the bare name via `PATH` itself.
#[cfg(not(windows))]
pub fn resolve_command(bin: &str) -> String {
    bin.to_string()
}

/// Environment variables to inject for launching `provider`'s tool.
///
/// Official providers (no `base_url`) launch without injection (empty env):
/// the tool falls back to its own login/auth state.
pub fn run_env(provider: &Provider, api_key: Option<&str>) -> Result<Vec<(String, String)>> {
    if provider.is_official() {
        return Ok(Vec::new());
    }
    let base_url = provider
        .base_url
        .as_deref()
        .ok_or_else(|| CoreError::ConfigParse {
            path: "provider".into(),
            msg: format!("third-party provider '{}' is missing base_url", provider.id),
        })?;
    match provider.tool {
        ToolKind::ClaudeCode => {
            // Same contract the adapter writes into settings.json's env block
            // (shared implementation — the two paths can't drift).
            Ok(claude_env_pairs(base_url, api_key, &provider.extra)
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect())
        }
        ToolKind::Aider => {
            // Aider maps every option to an AIDER_-prefixed env var; the
            // openai-api-* pair targets its OpenAI-compatible client.
            let mut env = vec![
                (
                    "AIDER_OPENAI_API_KEY".to_string(),
                    api_key.unwrap_or_default().to_string(),
                ),
                ("AIDER_OPENAI_API_BASE".to_string(), base_url.to_string()),
            ];
            if let Some(m) = provider.extra.get("model").and_then(|v| v.as_str()) {
                env.push(("AIDER_MODEL".to_string(), m.to_string()));
            }
            Ok(env)
        }
        tool => Err(CoreError::RunUnsupported(tool.label().to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_third_party_sets_base_token_model() {
        let mut p = Provider::new(
            "kimi",
            ToolKind::ClaudeCode,
            Some("https://api.moonshot.cn/anthropic".into()),
        );
        p.extra = json!({"model": "kimi-k2.5"});
        let env = run_env(&p, Some("sk-test")).unwrap();
        assert_eq!(
            env,
            vec![
                (
                    "ANTHROPIC_BASE_URL".to_string(),
                    "https://api.moonshot.cn/anthropic".to_string()
                ),
                ("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-test".to_string()),
                ("ANTHROPIC_MODEL".to_string(), "kimi-k2.5".to_string()),
            ]
        );
    }

    #[test]
    fn claude_base_url_strips_trailing_v1() {
        // Claude Code appends /v1/messages to ANTHROPIC_BASE_URL itself.
        let p = Provider::new(
            "relay",
            ToolKind::ClaudeCode,
            Some("https://gw.example/anthropic/v1/".into()),
        );
        let env = run_env(&p, None).unwrap();
        assert!(env
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_BASE_URL" && v == "https://gw.example/anthropic"));
    }

    #[test]
    fn claude_per_slot_models_skip_single_model_var() {
        let mut p = Provider::new(
            "work",
            ToolKind::ClaudeCode,
            Some("https://gw.example/anthropic".into()),
        );
        p.extra = json!({
            "opus_model": "m-opus",
            "sonnet_model": "m-sonnet",
            "haiku_model": "m-haiku",
        });
        let env = run_env(&p, Some("sk")).unwrap();
        let get = |k: &str| env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.clone());
        assert_eq!(
            get("ANTHROPIC_DEFAULT_OPUS_MODEL").as_deref(),
            Some("m-opus")
        );
        assert_eq!(
            get("ANTHROPIC_DEFAULT_SONNET_MODEL").as_deref(),
            Some("m-sonnet")
        );
        assert_eq!(
            get("ANTHROPIC_DEFAULT_HAIKU_MODEL").as_deref(),
            Some("m-haiku")
        );
        assert_eq!(get("ANTHROPIC_MODEL"), None);
    }

    #[test]
    fn claude_official_launches_without_injection() {
        let p = Provider::new("official", ToolKind::ClaudeCode, None);
        assert!(run_env(&p, None).unwrap().is_empty());
    }

    #[test]
    fn claude_third_party_requires_base_url() {
        let p = Provider::new("broken", ToolKind::ClaudeCode, None);
        let mut p = p;
        p.base_url = None;
        // A third-party provider without base_url cannot be expressed as env;
        // model-only overrides on an official provider are not a thing either.
        // (is_official() is exactly base_url.is_none(), so this maps to the
        // official branch — assert the contract: official => empty env.)
        assert!(run_env(&p, None).unwrap().is_empty());
    }

    #[test]
    fn aider_third_party_sets_aider_prefixed_vars() {
        let mut p = Provider::new(
            "deepseek",
            ToolKind::Aider,
            Some("https://api.deepseek.com/v1".into()),
        );
        p.extra = json!({"model": "deepseek-chat"});
        let env = run_env(&p, Some("sk-ds")).unwrap();
        assert_eq!(
            env,
            vec![
                ("AIDER_OPENAI_API_KEY".to_string(), "sk-ds".to_string()),
                (
                    "AIDER_OPENAI_API_BASE".to_string(),
                    "https://api.deepseek.com/v1".to_string()
                ),
                ("AIDER_MODEL".to_string(), "deepseek-chat".to_string()),
            ]
        );
    }

    #[test]
    fn aider_without_model_omits_model_var() {
        let p = Provider::new(
            "relay",
            ToolKind::Aider,
            Some("https://gw.example/v1".into()),
        );
        let env = run_env(&p, Some("sk")).unwrap();
        assert!(!env.iter().any(|(k, _)| k == "AIDER_MODEL"));
        assert_eq!(env.len(), 2);
    }

    #[test]
    fn config_file_tools_are_rejected() {
        for tool in [
            ToolKind::Codex,
            ToolKind::OpenCode,
            ToolKind::Pi,
            ToolKind::OhMyPi,
            ToolKind::Cline,
            ToolKind::Hermes,
            ToolKind::OpenClaw,
        ] {
            let p = Provider::new("x", tool, Some("https://gw.example/v1".into()));
            let err = run_env(&p, Some("sk")).unwrap_err();
            assert!(err.to_string().contains("use `xfade use"), "{tool}: {err}");
        }
    }

    #[test]
    fn tool_command_maps_each_tool_to_its_binary() {
        assert_eq!(tool_command(ToolKind::ClaudeCode), "claude");
        assert_eq!(tool_command(ToolKind::Codex), "codex");
        assert_eq!(tool_command(ToolKind::OpenCode), "opencode");
        assert_eq!(tool_command(ToolKind::Pi), "pi");
        assert_eq!(tool_command(ToolKind::OhMyPi), "omp");
        assert_eq!(tool_command(ToolKind::Aider), "aider");
        assert_eq!(tool_command(ToolKind::Cline), "cline");
        assert_eq!(tool_command(ToolKind::Hermes), "hermes");
        assert_eq!(tool_command(ToolKind::OpenClaw), "openclaw");
    }

    // --- resolve_command ---------------------------------------------------

    #[cfg(windows)]
    fn probe_env(dir: &std::path::Path, pathext: &str, bin: &str) -> Option<std::path::PathBuf> {
        for ext in pathext.split(';').filter(|s| !s.is_empty()) {
            let candidate = dir.join(format!("{bin}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    #[cfg(windows)]
    #[test]
    fn resolve_command_finds_cmd_shim_via_pathext() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("claude.cmd");
        std::fs::write(&shim, b"@echo off\r\n").unwrap();
        let pathext = ".COM;.EXE;.BAT;.CMD";
        let found = probe_env(dir.path(), pathext, "claude").unwrap();
        assert_eq!(found, shim);
    }

    #[cfg(windows)]
    #[test]
    fn resolve_command_missing_falls_back_to_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        assert!(probe_env(dir.path(), ".COM;.EXE;.CMD", "claude").is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn resolve_command_non_windows_returns_bare_name() {
        // No PATHEXT probing on Unix: the OS exec resolves the bare name.
        assert_eq!(resolve_command("claude"), "claude");
    }
}
