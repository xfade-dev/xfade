use serde::{Deserialize, Serialize};
use xfade_core::models::ToolKind;
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

/// Global config, as exposed to the frontend (`api_key` is masked).
#[derive(Serialize)]
pub struct ConfigDto {
    pub secrets: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api: Option<String>,
    /// Masked hint of the stored global API key (`Some("sk-1…")`) or `None` when unset.
    pub api_key: Option<String>,
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
}

// Pass through core types that already derive Serialize: Provider / ToolKind / StatsRow / RequestLog / StatsGroupBy
// (used directly as command return values / arguments in commands.rs)
