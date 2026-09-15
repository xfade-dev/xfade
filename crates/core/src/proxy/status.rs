use axum::{extract::State, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::{Circuit, ProxyService};

#[derive(Serialize, Deserialize)]
pub struct CircuitDto {
    pub provider_id: String,
    pub fails: u32,
    pub cooldown_remaining_secs: Option<u64>,
}

/// `Circuit` → `CircuitDto`: convert `cooldown_until_secs` into remaining cooldown seconds.
/// Shared by core and GUI to avoid duplicating the logic.
pub fn circuit_to_dto(provider_id: &str, c: &Circuit) -> CircuitDto {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let cooldown_remaining_secs = if c.cooldown_until_secs > 0 && now_secs < c.cooldown_until_secs {
        Some((c.cooldown_until_secs - now_secs) as u64)
    } else {
        None
    };
    CircuitDto {
        provider_id: provider_id.to_string(),
        fails: c.fails,
        cooldown_remaining_secs,
    }
}

#[derive(Serialize, Deserialize)]
pub struct StatusDto {
    pub running: bool,
    pub host: String,
    pub port: u16,
    pub auth_enabled: bool,
    pub routes: Vec<String>,
    pub model_override: Option<String>,
    pub target_protocol: String,
    pub circuits: Vec<CircuitDto>,
}

/// `GET /__xfade/status`: return the proxy run state, route config, and per-provider circuit status.
/// Checked by auth_guard (if an auth_token is set).
pub async fn handle(State(svc): State<Arc<ProxyService>>) -> impl IntoResponse {
    let (routes, model_override, target_protocol) = svc
        .core
        .db()
        .get_routes()
        .ok()
        .flatten()
        .unwrap_or((vec![], None, "chat".to_string()));
    let circuits = svc
        .circuits
        .lock()
        .unwrap()
        .iter()
        .map(|(id, c)| circuit_to_dto(id, c))
        .collect::<Vec<_>>();
    Json(StatusDto {
        running: true,
        host: svc.host.clone(),
        port: svc.port,
        auth_enabled: svc.auth_token.is_some(),
        routes,
        model_override,
        target_protocol,
        circuits,
    })
}
