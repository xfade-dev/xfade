use crate::models::ToolKind;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Preset {
    pub id: String,
    pub label: String,
    pub base_url: Option<String>,
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

/// Built-in presets per tool + user custom presets.
/// Third-party endpoints are each vendor's public compatibility endpoint; verify before adding.
/// User custom presets are loaded from `~/.config/xfade/presets.json` (same shape as built-in Preset, id/base_url required).
pub fn presets_for(tool: ToolKind) -> Vec<Preset> {
    let mut v = vec![
        p("official", "Official", None, serde_json::Value::Null),
        p("local-proxy", "Local proxy (xfade serve)", Some(local_proxy_base_url(tool)), serde_json::Value::Null),
    ];
    match tool {
        ToolKind::ClaudeCode => {
            v.extend([
                p("kimi", "Kimi (Moonshot)", Some("https://api.moonshot.cn/anthropic"), serde_json::Value::Null),
                p("glm", "GLM (Zhipu)", Some("https://open.bigmodel.cn/api/anthropic"), serde_json::Value::Null),
                p("deepseek", "DeepSeek", Some("https://api.deepseek.com/anthropic"), serde_json::Value::Null),
            ]);
        }
        ToolKind::Codex => {
            v.extend([
                p("openrouter", "OpenRouter", Some("https://openrouter.ai/api/v1"), serde_json::json!({"wire_api": "responses"})),
                p("kimi", "Kimi (Moonshot)", Some("https://api.moonshot.cn/v1"), serde_json::json!({"wire_api": "responses"})),
            ]);
        }
        ToolKind::OpenCode => {
            v.extend([
                p("openrouter", "OpenRouter", Some("https://openrouter.ai/api/v1"), serde_json::Value::Null),
                p("kimi", "Kimi (Moonshot)", Some("https://api.moonshot.cn/v1"), serde_json::Value::Null),
            ]);
        }
        ToolKind::Pi => {
            v.extend([
                p("openrouter", "OpenRouter", Some("https://openrouter.ai/api/v1"), serde_json::Value::Null),
                p("kimi", "Kimi (Moonshot)", Some("https://api.moonshot.cn/v1"), serde_json::Value::Null),
                p("deepseek", "DeepSeek", Some("https://api.deepseek.com"), serde_json::Value::Null),
            ]);
        }
        ToolKind::OhMyPi => {
            v.extend([
                p("openrouter", "OpenRouter", Some("https://openrouter.ai/api/v1"), serde_json::Value::Null),
                p("kimi", "Kimi (Moonshot)", Some("https://api.moonshot.cn/v1"), serde_json::Value::Null),
                p("deepseek", "DeepSeek", Some("https://api.deepseek.com"), serde_json::Value::Null),
            ]);
        }
        ToolKind::Aider => {
            v.extend([
                p("openrouter", "OpenRouter", Some("https://openrouter.ai/api/v1"), serde_json::Value::Null),
                p("kimi", "Kimi (Moonshot)", Some("https://api.moonshot.cn/v1"), serde_json::Value::Null),
                p("deepseek", "DeepSeek", Some("https://api.deepseek.com"), serde_json::Value::Null),
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
/// Silently skip if the file is missing or unparsable (does not affect built-in presets).
fn load_custom_presets() -> Option<Vec<Preset>> {
    let home = dirs::home_dir()?;
    let path = home.join(".config").join("xfade").join("presets.json");
    let data = std::fs::read_to_string(&path).ok()?;
    let raw: Vec<serde_json::Value> = serde_json::from_str(&data).ok()?;
    let mut out = Vec::new();
    for item in raw {
        let id = item.get("id")?.as_str()?.to_string();
        let label = item.get("label")?.as_str()?.to_string();
        let base_url = item.get("base_url").and_then(|v| v.as_str()).map(|s| s.to_string());
        let extra = item.get("extra").cloned().unwrap_or(serde_json::Value::Null);
        out.push(Preset { id, label, base_url, extra });
    }
    Some(out)
}

/// Base URL of each tool's local-proxy preset.
/// Pi and OMP both use the OpenAI-compatible protocol, going through the /v1 path.
fn local_proxy_base_url(tool: ToolKind) -> &'static str {
    match tool {
        ToolKind::ClaudeCode => "http://127.0.0.1:24860",
        ToolKind::Codex | ToolKind::OpenCode | ToolKind::Pi | ToolKind::OhMyPi
        | ToolKind::Aider => {
            "http://127.0.0.1:24860/v1"
        }
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

    #[test]
    fn preset_serializes() {
        let p = presets_for(crate::models::ToolKind::Codex)[0].clone();
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert!(v["id"].is_string());
        assert!(v["label"].is_string());
    }
}
