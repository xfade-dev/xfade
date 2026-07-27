use crate::models::ToolKind;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub base_url: Option<&'static str>,
    pub extra: serde_json::Value,
}

impl Preset {
    pub fn is_official(&self) -> bool {
        self.base_url.is_none()
    }
}

/// 各工具的内置预设。第三方端点均为各厂商公开的兼容端点，新增前需核对。
pub fn presets_for(tool: ToolKind) -> Vec<Preset> {
    let official = Preset {
        id: "official",
        label: "官方登录",
        base_url: None,
        extra: serde_json::Value::Null,
    };
    let local_proxy = Preset {
        id: "local-proxy",
        label: "本地代理 (asw serve)",
        base_url: Some(local_proxy_base_url(tool)),
        extra: serde_json::Value::Null,
    };
    let mut v = vec![official, local_proxy];
    match tool {
        ToolKind::ClaudeCode => {
            v.extend([
                Preset {
                    id: "kimi",
                    label: "Kimi (Moonshot)",
                    base_url: Some("https://api.moonshot.cn/anthropic"),
                    extra: serde_json::Value::Null,
                },
                Preset {
                    id: "glm",
                    label: "GLM (智谱)",
                    base_url: Some("https://open.bigmodel.cn/api/anthropic"),
                    extra: serde_json::Value::Null,
                },
                Preset {
                    id: "deepseek",
                    label: "DeepSeek",
                    base_url: Some("https://api.deepseek.com/anthropic"),
                    extra: serde_json::Value::Null,
                },
            ]);
        }
        ToolKind::Codex => {
            v.extend([
                Preset {
                    id: "openrouter",
                    label: "OpenRouter",
                    base_url: Some("https://openrouter.ai/api/v1"),
                    extra: serde_json::json!({"wire_api": "responses"}),
                },
                Preset {
                    id: "kimi",
                    label: "Kimi (Moonshot)",
                    base_url: Some("https://api.moonshot.cn/v1"),
                    extra: serde_json::json!({"wire_api": "responses"}),
                },
            ]);
        }
        ToolKind::OpenCode => {
            v.extend([
                Preset {
                    id: "openrouter",
                    label: "OpenRouter",
                    base_url: Some("https://openrouter.ai/api/v1"),
                    extra: serde_json::Value::Null,
                },
                Preset {
                    id: "kimi",
                    label: "Kimi (Moonshot)",
                    base_url: Some("https://api.moonshot.cn/v1"),
                    extra: serde_json::Value::Null,
                },
            ]);
        }
    }
    v
}

/// 各工具 local-proxy 预设的 base_url：claude 直连根路径，codex/opencode 走 /v1。
fn local_proxy_base_url(tool: ToolKind) -> &'static str {
    match tool {
        ToolKind::ClaudeCode => "http://127.0.0.1:24860",
        ToolKind::Codex | ToolKind::OpenCode => "http://127.0.0.1:24860/v1",
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
            let mut ids: Vec<_> = list.iter().map(|p| p.id).collect();
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
