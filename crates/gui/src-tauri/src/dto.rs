use agent_switch_core::models::{Provider, ToolKind};
use agent_switch_core::presets::Preset;
use agent_switch_core::proxy::Circuit;
use agent_switch_core::store::db::{RequestLog, StatsGroupBy, StatsRow};
use serde::{Deserialize, Serialize};
use std::time::Instant;

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
pub struct CircuitDto {
    pub provider_id: String,
    pub fails: u32,
    pub cooldown_remaining_secs: Option<u64>,
}

pub fn circuit_dto(provider_id: &str, c: &Circuit) -> CircuitDto {
    let cooldown_remaining_secs = c.cooldown_until.and_then(|t| {
        let now = Instant::now();
        if t > now {
            Some(t.saturating_duration_since(now).as_secs())
        } else {
            None
        }
    });
    CircuitDto {
        provider_id: provider_id.to_string(),
        fails: c.fails,
        cooldown_remaining_secs,
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
