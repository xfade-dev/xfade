use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use xfade_core::models::{ProviderProtocol, ToolKind};
use xfade_core::presets::Preset;

// CircuitDto is moved down into core; the GUI reuses it directly (avoiding duplication).
pub use xfade_core::proxy::status::CircuitDto;

#[derive(Serialize, Deserialize)]
pub struct AddProviderInput {
    pub id: String,
    pub tool: ToolKind,
    pub base_url: Option<String>,
    pub key: Option<String>,
    #[serde(default)]
    pub protocol: Option<ProviderProtocol>,
    #[serde(default)]
    pub extra: serde_json::Value,
    /// Official login provider; when false, blank base_url/model use global defaults.
    #[serde(default)]
    pub official: bool,
}

#[derive(Serialize, Deserialize)]
pub struct UpdateProviderInput {
    pub id: String,
    pub tool: ToolKind,
    pub base_url: Option<String>,
    pub key: Option<String>,
    #[serde(default)]
    pub protocol: Option<ProviderProtocol>,
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
            id: p.id,
            label: p.label,
            base_url: p.base_url,
            extra: p.extra,
            is_official,
        }
    }
}

#[derive(Serialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub managed: bool,
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
    #[serde(default)]
    pub route_models: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
pub struct ModelMappingDto {
    pub alias: String,
    pub provider_id: String,
    pub upstream_model: String,
}

#[derive(Serialize, Deserialize)]
pub struct ModelMapDto {
    pub mappings: Vec<ModelMappingDto>,
    pub models_merge: bool,
}

/// Global config, as exposed to the frontend (`api_key` is masked).
#[derive(Serialize)]
pub struct ConfigDto {
    pub secrets: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api: Option<String>,
    /// Masked hint of the stored global API key (`Some("sk-…abcd")`) or `None` when unset.
    pub api_key: Option<String>,
    pub keychain_allow_all: bool,
}

/// Frontend → backend config update. `None` leaves a field unchanged; an empty
/// string (`""`) clears the corresponding `base_url`/`model`/`api` field.
/// `api_key`: `None` = unchanged, `Some("")` = unchanged, `Some(value)` = set.
#[derive(Deserialize)]
pub struct ConfigInput {
    pub secrets: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api: Option<String>,
    pub api_key: Option<String>,
    pub keychain_allow_all: Option<bool>,
}

// Pass through core types that already derive Serialize: Provider / ToolKind / StatsRow / RequestLog / StatsGroupBy
// (used directly as command return values / arguments in commands.rs)
