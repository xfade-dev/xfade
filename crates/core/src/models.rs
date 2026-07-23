use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolKind {
    ClaudeCode,
    Codex,
    OpenCode,
}

impl ToolKind {
    pub const ALL: [ToolKind; 3] = [ToolKind::ClaudeCode, ToolKind::Codex, ToolKind::OpenCode];

    pub fn as_str(&self) -> &'static str {
        match self {
            ToolKind::ClaudeCode => "claude",
            ToolKind::Codex => "codex",
            ToolKind::OpenCode => "opencode",
        }
    }
}

impl fmt::Display for ToolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ToolKind {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "claude" | "claude-code" => Ok(ToolKind::ClaudeCode),
            "codex" => Ok(ToolKind::Codex),
            "opencode" => Ok(ToolKind::OpenCode),
            _ => Err(format!("unknown tool: {s} (expected claude|codex|opencode)")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    pub id: String,
    pub tool: ToolKind,
    pub base_url: Option<String>,
    pub key_ref: String,
    #[serde(default)]
    pub extra: serde_json::Value,
    #[serde(default)]
    pub is_active: bool,
}

impl Provider {
    pub fn new(id: impl Into<String>, tool: ToolKind, base_url: Option<String>) -> Self {
        let id = id.into();
        Self {
            key_ref: format!("agent-switch/{}/{}", tool.as_str(), id),
            id,
            tool,
            base_url,
            extra: serde_json::Value::Null,
            is_active: false,
        }
    }

    /// base_url 为 None 表示官方登录（清除第三方配置）
    pub fn is_official(&self) -> bool {
        self.base_url.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_kind_roundtrip() {
        for t in ToolKind::ALL {
            let s = t.as_str();
            assert_eq!(s.parse::<ToolKind>().unwrap(), t);
        }
    }

    #[test]
    fn provider_key_ref_scoped_by_tool_and_id() {
        let p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        assert_eq!(p.key_ref, "agent-switch/codex/kimi");
        assert!(!p.is_active);
        assert!(p.extra.is_null());
    }

    #[test]
    fn provider_serde_roundtrip() {
        let p = Provider::new("a", ToolKind::ClaudeCode, None);
        let s = serde_json::to_string(&p).unwrap();
        let back: Provider = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }
}
