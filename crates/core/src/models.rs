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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderProtocol {
    #[default]
    Chat,
    Messages,
    Responses,
}

impl ProviderProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Messages => "messages",
            Self::Responses => "responses",
        }
    }

    /// Default endpoint path suffix for this protocol.
    pub fn default_endpoint(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Messages => "messages",
            Self::Responses => "responses",
        }
    }
}

impl std::fmt::Display for ProviderProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for ProviderProtocol {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "chat" | "openai" => Ok(Self::Chat),
            "messages" | "anthropic" => Ok(Self::Messages),
            "responses" | "codex" => Ok(Self::Responses),
            _ => Err(format!(
                "unknown protocol: '{s}' (expected chat|messages|responses)"
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
    pub protocol: ProviderProtocol,
    #[serde(default)]
    pub extra: serde_json::Value,
    #[serde(default)]
    pub is_active: bool,
}

impl Provider {
    pub fn new(id: impl Into<String>, tool: ToolKind, base_url: Option<String>) -> Self {
        let id = id.into();
        let protocol = if (tool == ToolKind::ClaudeCode && base_url.is_none())
            || base_url
                .as_deref()
                .is_some_and(|u| u.contains("anthropic.com"))
        {
            ProviderProtocol::Messages
        } else {
            ProviderProtocol::Chat
        };
        Self {
            key_ref: format!("xfade/{}/{}", tool.as_str(), id),
            id,
            tool,
            base_url,
            protocol,
            extra: serde_json::Value::Null,
            is_active: false,
        }
    }

    pub fn with_protocol(mut self, protocol: ProviderProtocol) -> Self {
        self.protocol = protocol;
        self
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

    #[test]
    fn provider_protocol_serde_and_from_str() {
        assert_eq!("chat".parse::<ProviderProtocol>().unwrap(), ProviderProtocol::Chat);
        assert_eq!("messages".parse::<ProviderProtocol>().unwrap(), ProviderProtocol::Messages);
        assert_eq!("anthropic".parse::<ProviderProtocol>().unwrap(), ProviderProtocol::Messages);
        assert_eq!("responses".parse::<ProviderProtocol>().unwrap(), ProviderProtocol::Responses);
        assert_eq!("codex".parse::<ProviderProtocol>().unwrap(), ProviderProtocol::Responses);
    }

    #[test]
    fn provider_protocol_inference_defaults() {
        let p_claude = Provider::new("official", ToolKind::ClaudeCode, None);
        assert_eq!(p_claude.protocol, ProviderProtocol::Messages);

        let p_anthropic = Provider::new("anthropic-custom", ToolKind::ClaudeCode, Some("https://api.anthropic.com".into()));
        assert_eq!(p_anthropic.protocol, ProviderProtocol::Messages);

        let p_chat = Provider::new("deepseek", ToolKind::ClaudeCode, Some("https://api.deepseek.com".into()));
        assert_eq!(p_chat.protocol, ProviderProtocol::Chat);
    }
}
