use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolKind {
    ClaudeCode,
    Codex,
    OpenCode,
    Pi,
    OhMyPi,
    Aider,
    Cline,
    Hermes,
    OpenClaw,
}

impl ToolKind {
    pub const ALL: [ToolKind; 9] = [
        ToolKind::ClaudeCode,
        ToolKind::Codex,
        ToolKind::OpenCode,
        ToolKind::Pi,
        ToolKind::OhMyPi,
        ToolKind::Aider,
        ToolKind::Cline,
        ToolKind::Hermes,
        ToolKind::OpenClaw,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
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

    /// Human-readable label for UI surfaces (tray menu, pickers).
    pub fn label(&self) -> &'static str {
        match self {
            ToolKind::ClaudeCode => "Claude Code",
            ToolKind::Codex => "Codex",
            ToolKind::OpenCode => "OpenCode",
            ToolKind::Pi => "Pi",
            ToolKind::OhMyPi => "Oh My Pi",
            ToolKind::Aider => "Aider",
            ToolKind::Cline => "Cline",
            ToolKind::Hermes => "Hermes Agent",
            ToolKind::OpenClaw => "OpenClaw",
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
            "pi" => Ok(ToolKind::Pi),
            "omp" | "oh-my-pi" | "ohmy" => Ok(ToolKind::OhMyPi),
            "aider" => Ok(ToolKind::Aider),
            "cline" => Ok(ToolKind::Cline),
            "hermes" => Ok(ToolKind::Hermes),
            "openclaw" => Ok(ToolKind::OpenClaw),
            _ => Err(format!(
                "unknown tool: {s} (expected claude|codex|opencode|pi|omp|aider|cline|hermes|openclaw)"
            )),
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
            key_ref: format!("xfade/{}/{}", tool.as_str(), id),
            id,
            tool,
            base_url,
            extra: serde_json::Value::Null,
            is_active: false,
        }
    }

    /// `base_url == None` means official login (clears third-party config).
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
        assert_eq!(p.key_ref, "xfade/codex/kimi");
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
