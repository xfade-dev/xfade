use agent_switch_core::models::ToolKind;
use agent_switch_core::presets::Preset;
use serde::{Deserialize, Serialize};

// CircuitDto 下沉到 core，GUI 直接复用（避免重复实现）。
pub use agent_switch_core::proxy::status::CircuitDto;

#[derive(Serialize, Deserialize)]
pub struct AddProviderInput {
    pub id: String,
    pub tool: ToolKind,
    pub base_url: Option<String>,
    pub key: Option<String>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
pub struct UpdateProviderInput {
    pub id: String,
    pub tool: ToolKind,
    pub base_url: Option<String>,
    pub key: Option<String>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Serialize)]
pub struct PresetDto {
    pub id: String,
    pub label: String,
    pub base_url: Option<String>,
    pub extra: serde_json::Value,
    pub is_official: bool,
}

impl From<Preset> for PresetDto {
    fn from(p: Preset) -> Self {
        let is_official = p.is_official();
        Self {
            id: p.id.to_string(),
            label: p.label.to_string(),
            base_url: p.base_url.map(String::from),
            extra: p.extra,
            is_official,
        }
    }
}

#[derive(Serialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub auth_enabled: bool,
    pub circuits: Vec<CircuitDto>,
}

#[derive(Serialize, Deserialize)]
pub struct RoutesDto {
    pub routes: Vec<String>,
    pub model_override: Option<String>,
    pub target_protocol: String,
}

// core 已 Serialize 的类型透传：Provider / ToolKind / StatsRow / RequestLog / StatsGroupBy
// （在 commands.rs 中直接作为 command 返回值/参数使用）
