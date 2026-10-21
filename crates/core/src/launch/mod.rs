//! Launch plans for `xfade run` and the instant syntax (`xfade <tool>`):
//! start a tool's binary with a provider injected **without touching the
//! tool's on-disk config**.
//!
//! - claude / aider: pure env injection (delegates to [`crate::run::run_env`]).
//! - codex: `-c` config overrides + the key in a process env var referenced
//!   via `env_key`. Verified against codex-cli 0.155.0 (2026-10-22): hits
//!   `<base_url>/responses` with `Authorization: Bearer <key>`, real
//!   `~/.codex` untouched.
//! - opencode: a whole inline config via `OPENCODE_CONFIG_CONTENT` (zero
//!   files). Verified against opencode 1.18.31 (2026-10-22): streams
//!   chat/completions with the inline provider, config files untouched.
//! - pi / omp / cline / hermes / openclaw: no verified injection contract;
//!   rejected with a `xfade use` hint.

use crate::adapters::client_base_url;
use crate::adapters::opencode::{models_for_extra, OPENAI_COMPAT};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use crate::run::run_env;
use serde_json::json;

/// Process env var carrying the API key for an injected codex provider.
/// Codex reads the key named by `env_key`; the value never lands in any file.
pub const CODEX_KEY_ENV: &str = "XFADE_CODEX_KEY";

/// How to launch a tool binary with a provider injected.
#[derive(Debug)]
pub enum LaunchPlan {
    /// Pure env injection; the tool's own args pass through unchanged.
    Env(Vec<(String, String)>),
    /// Env plus CLI args that must precede the user's args.
    EnvWithArgs {
        env: Vec<(String, String)>,
        args: Vec<String>,
    },
}

impl LaunchPlan {
    pub fn env(&self) -> &[(String, String)] {
        match self {
            LaunchPlan::Env(e) => e,
            LaunchPlan::EnvWithArgs { env, .. } => env,
        }
    }

    /// Args to prepend before the user's own args (empty for pure-env plans).
    pub fn args_prefix(&self) -> &[String] {
        match self {
            LaunchPlan::Env(_) => &[],
            LaunchPlan::EnvWithArgs { args, .. } => args,
        }
    }
}

/// Compute the launch plan for `provider`. `api_key` is the resolved key
/// (already checked by the caller for third-party providers).
pub fn launch_plan(provider: &Provider, api_key: Option<&str>) -> Result<LaunchPlan> {
    match provider.tool {
        ToolKind::ClaudeCode | ToolKind::Aider => Ok(LaunchPlan::Env(run_env(provider, api_key)?)),
        ToolKind::Codex => codex_plan(provider, api_key),
        ToolKind::OpenCode => opencode_plan(provider, api_key),
        tool => Err(CoreError::RunUnsupported(tool.label().to_string())),
    }
}

/// Quote a TOML basic-string value for `codex -c key=<value>`.
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A TOML key path segment: bare when possible, quoted otherwise (a provider
/// id containing `.` would otherwise split into nested tables).
fn toml_key(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        s.to_string()
    } else {
        toml_string(s)
    }
}

fn codex_plan(provider: &Provider, api_key: Option<&str>) -> Result<LaunchPlan> {
    let model = provider.extra.get("model").and_then(|v| v.as_str());
    if provider.is_official() {
        // Official: codex's own login state. A model override — not
        // expressible via env — still reaches codex as a -c override.
        return Ok(match model {
            Some(m) => LaunchPlan::EnvWithArgs {
                env: vec![],
                args: vec!["-c".into(), format!("model={}", toml_string(m))],
            },
            None => LaunchPlan::Env(vec![]),
        });
    }
    let base_url = provider
        .base_url
        .as_deref()
        .ok_or_else(|| CoreError::ConfigParse {
            path: "provider".into(),
            msg: format!("third-party provider '{}' is missing base_url", provider.id),
        })?;
    let key = api_key.ok_or_else(|| {
        CoreError::MissingApiKey(format!(
            "codex provider '{}' needs an API key (injected via ${CODEX_KEY_ENV})",
            provider.id
        ))
    })?;
    // Codex appends /responses (or /chat/completions) to base_url; a bare
    // host must carry the /v1 suffix.
    let url = client_base_url(base_url, "openai-completions");
    let wire_api = provider
        .extra
        .get("wire_api")
        .and_then(|v| v.as_str())
        .unwrap_or("responses"); // newer Codex has deprecated wire_api = "chat"
    let id = toml_key(&provider.id);
    let mut args = vec![
        "-c".into(),
        format!("model_provider={}", toml_string(&provider.id)),
        "-c".into(),
        format!("model_providers.{id}.name={}", toml_string(&provider.id)),
        "-c".into(),
        format!("model_providers.{id}.base_url={}", toml_string(&url)),
        "-c".into(),
        format!("model_providers.{id}.wire_api={}", toml_string(wire_api)),
        // env_key (not requires_openai_auth): the key comes from the process
        // env, so auth.json is never read or written.
        "-c".into(),
        format!(
            "model_providers.{id}.env_key={}",
            toml_string(CODEX_KEY_ENV)
        ),
    ];
    if let Some(m) = model {
        args.push("-c".into());
        args.push(format!("model={}", toml_string(m)));
    }
    if let Some(cw) = provider
        .extra
        .get("context_window")
        .and_then(|v| v.as_i64())
    {
        args.push("-c".into());
        args.push(format!("model_context_window={cw}"));
    }
    // Skips the codex_apps MCP startup tax (~10s); aivo does the same.
    args.push("--disable".into());
    args.push("apps".into());
    Ok(LaunchPlan::EnvWithArgs {
        env: vec![(CODEX_KEY_ENV.into(), key.to_string())],
        args,
    })
}

fn opencode_plan(provider: &Provider, api_key: Option<&str>) -> Result<LaunchPlan> {
    if provider.is_official() {
        return Ok(LaunchPlan::Env(vec![]));
    }
    let base_url = provider
        .base_url
        .as_deref()
        .ok_or_else(|| CoreError::ConfigParse {
            path: "provider".into(),
            msg: format!("third-party provider '{}' is missing base_url", provider.id),
        })?;
    let key = api_key.ok_or_else(|| {
        CoreError::MissingApiKey(format!(
            "opencode provider '{}' needs an API key (inlined into OPENCODE_CONFIG_CONTENT)",
            provider.id
        ))
    })?;
    // The openai-compatible SDK appends /chat/completions to baseURL, so a
    // bare host must carry the /v1 suffix (same rule as the file adapter).
    let url = client_base_url(base_url, "openai-completions");
    let id = &provider.id;
    let mut cfg = json!({
        "provider": {
            id: {
                "npm": OPENAI_COMPAT,
                "name": id,
                // apiKey inline: auth.json is never read or written.
                "options": { "baseURL": url, "apiKey": key },
                "models": models_for_extra(&provider.extra),
            }
        }
    });
    // Select the model when a single-model shorthand exists; otherwise let
    // opencode fall back to its own resolution.
    if let Some(m) = provider.extra.get("model").and_then(|v| v.as_str()) {
        cfg["model"] = json!(format!("{id}/{m}"));
    }
    Ok(LaunchPlan::Env(vec![(
        "OPENCODE_CONFIG_CONTENT".into(),
        serde_json::to_string(&cfg)?,
    )]))
}

/// Pre-launch check for codex: newer versions validate the WHOLE
/// ~/.codex/config.toml and refuse to start if any provider section still
/// uses the deprecated wire_api="chat" — even when -c overrides point
/// elsewhere. Returns a warning message naming the offending provider ids.
pub fn codex_legacy_wire_api_warning(home: &std::path::Path) -> Option<String> {
    let path = home.join(".codex").join("config.toml");
    let s = std::fs::read_to_string(path).ok()?;
    let cfg: toml::Value = toml::from_str(&s).ok()?;
    let ids = crate::adapters::codex::find_legacy_wire_api(&cfg);
    if ids.is_empty() {
        None
    } else {
        Some(format!(
            "model_providers.{} in ~/.codex/config.toml uses deprecated wire_api=\"chat\"; \
             newer Codex may refuse to start, please change it to \"responses\"",
            ids.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_provider(id: &str, base_url: Option<&str>, extra: serde_json::Value) -> Provider {
        let mut p = Provider::new(id, ToolKind::Codex, base_url.map(String::from));
        p.extra = extra;
        p
    }

    fn opencode_provider(id: &str, base_url: Option<&str>, extra: serde_json::Value) -> Provider {
        let mut p = Provider::new(id, ToolKind::OpenCode, base_url.map(String::from));
        p.extra = extra;
        p
    }

    #[test]
    fn codex_third_party_injects_c_overrides_and_env_key() {
        let p = codex_provider(
            "kimi",
            Some("https://api.moonshot.cn"),
            json!({"model": "kimi-k2.5"}),
        );
        let plan = launch_plan(&p, Some("sk-k")).unwrap();
        let LaunchPlan::EnvWithArgs { env, args } = plan else {
            panic!("expected EnvWithArgs");
        };
        assert_eq!(env, vec![(CODEX_KEY_ENV.to_string(), "sk-k".to_string())]);
        assert_eq!(
            args,
            vec![
                "-c",
                "model_provider=\"kimi\"",
                "-c",
                "model_providers.kimi.name=\"kimi\"",
                "-c",
                "model_providers.kimi.base_url=\"https://api.moonshot.cn/v1\"",
                "-c",
                "model_providers.kimi.wire_api=\"responses\"",
                "-c",
                "model_providers.kimi.env_key=\"XFADE_CODEX_KEY\"",
                "-c",
                "model=\"kimi-k2.5\"",
                "--disable",
                "apps",
            ]
        );
    }

    #[test]
    fn codex_wire_api_and_context_window_from_extra() {
        let p = codex_provider(
            "gw",
            Some("https://x/v1"),
            json!({"wire_api": "chat", "context_window": 131072}),
        );
        let plan = launch_plan(&p, Some("k")).unwrap();
        let args = plan.args_prefix();
        assert!(args.contains(&"model_providers.gw.wire_api=\"chat\"".to_string()));
        assert!(args.contains(&"model_context_window=131072".to_string()));
        // No model in extra → no model override arg.
        assert!(!args.iter().any(|a| a.starts_with("model=")));
    }

    #[test]
    fn codex_provider_id_with_dot_is_quoted_as_key() {
        // A dotted id would split into nested TOML tables; it must be quoted.
        let p = codex_provider("my.gw", Some("https://x/v1"), json!({}));
        let plan = launch_plan(&p, Some("k")).unwrap();
        let args = plan.args_prefix();
        assert!(args.contains(&"model_providers.\"my.gw\".base_url=\"https://x/v1\"".to_string()));
    }

    #[test]
    fn codex_values_with_quotes_are_escaped() {
        let p = codex_provider("k", Some("https://x/v1"), json!({"model": "a\"b"}));
        let plan = launch_plan(&p, Some("k")).unwrap();
        let args = plan.args_prefix();
        assert!(args.contains(&"model=\"a\\\"b\"".to_string()));
    }

    #[test]
    fn codex_official_with_model_uses_c_override_only() {
        let p = codex_provider("official", None, json!({"model": "gpt-5.1"}));
        let plan = launch_plan(&p, None).unwrap();
        assert!(plan.env().is_empty());
        assert_eq!(
            plan.args_prefix(),
            &["-c".to_string(), "model=\"gpt-5.1\"".to_string()]
        );
    }

    #[test]
    fn codex_official_without_model_is_plain_env() {
        let p = codex_provider("official", None, json!({}));
        let plan = launch_plan(&p, None).unwrap();
        assert!(plan.env().is_empty());
        assert!(plan.args_prefix().is_empty());
    }

    #[test]
    fn codex_third_party_requires_key() {
        let p = codex_provider("k", Some("https://x/v1"), json!({}));
        let err = launch_plan(&p, None).unwrap_err();
        assert!(matches!(err, CoreError::MissingApiKey(_)), "{err}");
    }

    #[test]
    fn opencode_third_party_inlines_full_config() {
        let p = opencode_provider(
            "glm",
            Some("https://open.bigmodel.cn/api/paas/v4"),
            json!({"model": "glm-5-2-260617"}),
        );
        let plan = launch_plan(&p, Some("sk-g")).unwrap();
        let env = plan.env();
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "OPENCODE_CONFIG_CONTENT");
        assert!(plan.args_prefix().is_empty());
        let cfg: serde_json::Value = serde_json::from_str(&env[0].1).unwrap();
        let prov = &cfg["provider"]["glm"];
        assert_eq!(prov["npm"], OPENAI_COMPAT);
        assert_eq!(
            prov["options"]["baseURL"],
            "https://open.bigmodel.cn/api/paas/v4"
        );
        assert_eq!(prov["options"]["apiKey"], "sk-g");
        assert_eq!(prov["models"]["glm-5-2-260617"]["name"], "glm-5-2-260617");
        assert_eq!(cfg["model"], "glm/glm-5-2-260617");
    }

    #[test]
    fn opencode_models_object_passes_through_without_model_selection() {
        let p = opencode_provider(
            "gw",
            Some("https://x/v1"),
            json!({"models": {"m1": {"name": "m1"}, "m2": {}}}),
        );
        let plan = launch_plan(&p, Some("k")).unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&plan.env()[0].1).unwrap();
        assert!(cfg["provider"]["gw"]["models"]["m2"].is_object());
        // No single-model shorthand → no top-level model selection.
        assert!(cfg.get("model").is_none());
    }

    #[test]
    fn opencode_official_is_plain_env() {
        let p = opencode_provider("official", None, json!({}));
        let plan = launch_plan(&p, None).unwrap();
        assert!(plan.env().is_empty());
        assert!(plan.args_prefix().is_empty());
    }

    #[test]
    fn claude_delegates_to_run_env() {
        let mut p = Provider::new(
            "kimi",
            ToolKind::ClaudeCode,
            Some("https://api.moonshot.cn/anthropic".into()),
        );
        p.extra = json!({"model": "kimi-k2.5"});
        let plan = launch_plan(&p, Some("sk")).unwrap();
        let LaunchPlan::Env(env) = plan else {
            panic!("expected Env");
        };
        assert!(env.iter().any(|(k, _)| k == "ANTHROPIC_BASE_URL"));
    }

    #[test]
    fn remaining_config_file_tools_are_rejected() {
        for tool in [
            ToolKind::Pi,
            ToolKind::OhMyPi,
            ToolKind::Cline,
            ToolKind::Hermes,
            ToolKind::OpenClaw,
        ] {
            let p = Provider::new("x", tool, Some("https://gw.example/v1".into()));
            let err = launch_plan(&p, Some("sk")).unwrap_err();
            assert!(err.to_string().contains("use `xfade use"), "{tool}: {err}");
        }
    }

    #[test]
    fn codex_legacy_wire_api_warning_names_provider() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
        std::fs::write(
            dir.path().join(".codex/config.toml"),
            "[model_providers.old]\nwire_api=\"chat\"\n",
        )
        .unwrap();
        let warn = codex_legacy_wire_api_warning(dir.path()).unwrap();
        assert!(warn.contains("old"), "{warn}");

        // Clean config → no warning.
        std::fs::write(
            dir.path().join(".codex/config.toml"),
            "[model_providers.new]\nwire_api=\"responses\"\n",
        )
        .unwrap();
        assert!(codex_legacy_wire_api_warning(dir.path()).is_none());
        // Missing config → no warning.
        let empty = tempfile::tempdir().unwrap();
        assert!(codex_legacy_wire_api_warning(empty.path()).is_none());
    }
}
