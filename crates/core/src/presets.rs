use crate::models::ToolKind;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Preset {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

impl Preset {
    pub fn is_official(&self) -> bool {
        self.base_url.is_none()
    }
}

fn p(id: &str, label: &str, base_url: Option<&str>, extra: serde_json::Value) -> Preset {
    Preset {
        id: id.to_string(),
        label: label.to_string(),
        base_url: base_url.map(|s| s.to_string()),
        extra,
    }
}

/// OpenAI-protocol presets shared by Pi/OMP/Aider/Cline (OpenRouter/Kimi/DeepSeek).
fn openai_presets() -> Vec<Preset> {
    vec![
        p(
            "openrouter",
            "OpenRouter",
            Some("https://openrouter.ai/api/v1"),
            serde_json::Value::Null,
        ),
        p(
            "kimi",
            "Kimi (Moonshot)",
            Some("https://api.moonshot.cn/v1"),
            serde_json::Value::Null,
        ),
        p(
            "deepseek",
            "DeepSeek",
            Some("https://api.deepseek.com"),
            serde_json::Value::Null,
        ),
    ]
}

/// Built-in presets per tool + user custom presets.
/// Third-party endpoints are each vendor's public compatibility endpoint; verify before adding.
/// User custom presets are loaded from `~/.config/xfade/presets.json` (same shape as built-in Preset, id/base_url required).
pub fn presets_for(tool: ToolKind) -> Vec<Preset> {
    let mut v = vec![
        p("official", "Official", None, serde_json::Value::Null),
        p(
            "local-proxy",
            "Local proxy (xfade serve)",
            Some(local_proxy_base_url(tool)),
            serde_json::Value::Null,
        ),
    ];
    match tool {
        ToolKind::ClaudeCode => {
            v.extend([
                p(
                    "kimi",
                    "Kimi (Moonshot)",
                    Some("https://api.moonshot.cn/anthropic"),
                    serde_json::Value::Null,
                ),
                p(
                    "glm",
                    "GLM (Zhipu)",
                    Some("https://open.bigmodel.cn/api/anthropic"),
                    serde_json::Value::Null,
                ),
                p(
                    "deepseek",
                    "DeepSeek",
                    Some("https://api.deepseek.com/anthropic"),
                    serde_json::Value::Null,
                ),
            ]);
        }
        ToolKind::Codex => {
            v.extend([
                p(
                    "openrouter",
                    "OpenRouter",
                    Some("https://openrouter.ai/api/v1"),
                    serde_json::json!({"wire_api": "responses"}),
                ),
                p(
                    "kimi",
                    "Kimi (Moonshot)",
                    Some("https://api.moonshot.cn/v1"),
                    serde_json::json!({"wire_api": "responses"}),
                ),
            ]);
        }
        ToolKind::OpenCode => {
            v.extend([
                p(
                    "openrouter",
                    "OpenRouter",
                    Some("https://openrouter.ai/api/v1"),
                    serde_json::Value::Null,
                ),
                p(
                    "kimi",
                    "Kimi (Moonshot)",
                    Some("https://api.moonshot.cn/v1"),
                    serde_json::Value::Null,
                ),
            ]);
        }
        ToolKind::Pi | ToolKind::OhMyPi | ToolKind::Aider | ToolKind::Cline => {
            v.extend(openai_presets());
        }
        ToolKind::Hermes => {
            v.extend([
                p(
                    "openrouter",
                    "OpenRouter",
                    Some("https://openrouter.ai/api/v1"),
                    serde_json::json!({"model": "anthropic/claude-sonnet-4"}),
                ),
                p(
                    "kimi",
                    "Kimi (Moonshot)",
                    Some("https://api.moonshot.cn/v1"),
                    serde_json::json!({"model": "kimi-k2.5"}),
                ),
                p(
                    "deepseek",
                    "DeepSeek",
                    Some("https://api.deepseek.com"),
                    serde_json::json!({"model": "deepseek-chat"}),
                ),
            ]);
        }
        ToolKind::OpenClaw => {
            v.extend([
                p(
                    "openrouter",
                    "OpenRouter",
                    Some("https://openrouter.ai/api/v1"),
                    serde_json::json!({"model": "anthropic/claude-sonnet-4"}),
                ),
                p(
                    "kimi",
                    "Kimi (Moonshot)",
                    Some("https://api.moonshot.cn/v1"),
                    serde_json::json!({"model": "kimi-k2.5"}),
                ),
                p(
                    "deepseek",
                    "DeepSeek",
                    Some("https://api.deepseek.com"),
                    serde_json::json!({"model": "deepseek-chat"}),
                ),
            ]);
        }
    }
    // Load user custom presets
    if let Some(custom) = load_custom_presets() {
        let builtin_ids: std::collections::HashSet<String> =
            v.iter().map(|p| p.id.clone()).collect();
        for c in custom {
            if !builtin_ids.contains(&c.id) {
                v.push(c);
            }
        }
    }
    v
}

/// Load user custom presets from `~/.config/xfade/presets.json`.
/// Format: `[{"id":"...","label":"...","base_url":"...","extra":{...}},...]`.
/// Silently skips the file (missing/unparsable) and individual malformed entries.
fn load_custom_presets() -> Option<Vec<Preset>> {
    let home = dirs::home_dir()?;
    let path = home.join(".config").join("xfade").join("presets.json");
    let data = std::fs::read_to_string(&path).ok()?;
    let raw: Vec<serde_json::Value> = serde_json::from_str(&data).ok()?;
    Some(
        raw.into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect(),
    )
}

/// Base URL of each tool's local-proxy preset.
/// Pi and OMP both use the OpenAI-compatible protocol, going through the /v1 path.
fn local_proxy_base_url(tool: ToolKind) -> &'static str {
    match tool {
        ToolKind::ClaudeCode => "http://127.0.0.1:24860",
        ToolKind::Codex
        | ToolKind::OpenCode
        | ToolKind::Pi
        | ToolKind::OhMyPi
        | ToolKind::Aider
        | ToolKind::Cline
        | ToolKind::Hermes
        | ToolKind::OpenClaw => "http://127.0.0.1:24860/v1",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_official_preset() {
        for t in ToolKind::ALL {
            assert!(presets_for(t).iter().any(|p| p.is_official()));
        }
    }

    #[test]
    fn preset_ids_unique_per_tool() {
        for t in ToolKind::ALL {
            let list = presets_for(t);
            let mut ids: Vec<_> = list.iter().map(|p| p.id.clone()).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), list.len());
        }
    }
}
