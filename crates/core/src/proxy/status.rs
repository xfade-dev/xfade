use axum::{extract::State, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::{circuit_breaker::CircuitState, Circuit, ProxyService};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CircuitDto {
    pub provider_id: String,
    pub fails: u32,
    pub cooldown_remaining_secs: Option<u64>,
    #[serde(default)]
    pub state: CircuitState,
}

/// `Circuit` → `CircuitDto`: convert `cooldown_until_secs` into remaining cooldown seconds.
/// Shared by core and GUI to avoid duplicating the logic.
pub fn circuit_to_dto(provider_id: &str, c: &Circuit) -> CircuitDto {
    let now = super::now_secs();
    let cooldown_remaining_secs = if c.is_open(now) {
        Some((c.cooldown_until_secs - now) as u64)
    } else {
        None
    };
    let state = if c.is_open(now) {
        CircuitState::Open
    } else {
        CircuitState::Closed
    };
    CircuitDto {
        provider_id: provider_id.to_string(),
        fails: c.fails,
        cooldown_remaining_secs,
        state,
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
    /// Whether `/v1/models` aggregates all routes and posts are pinned by model.
    /// `default` keeps this DTO readable against an older daemon's response.
    #[serde(default)]
    pub models_merge: bool,
    /// Build version of the serving binary. `default` keeps the DTO readable
    /// against an older daemon's response that predates the field.
    #[serde(default)]
    pub version: String,
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
    let mut provider_ids: Vec<String> = svc.circuit_breaker.providers();
    for r in &routes {
        let already_tracked = provider_ids
            .iter()
            .any(|k| k == r || k.ends_with(&format!("/{r}")));
        if !already_tracked {
            provider_ids.push(r.clone());
        }
    }
    let circuits = provider_ids
        .into_iter()
        .map(|id| {
            let fails = svc.circuit_breaker.failure_count(&id);
            let cooldown_remaining_secs = svc.circuit_breaker.remaining_cooldown_secs(&id);
            let state = svc.circuit_breaker.state(&id);
            CircuitDto {
                provider_id: id,
                fails,
                cooldown_remaining_secs,
                state,
            }
        })
        .collect::<Vec<_>>();
    Json(StatusDto {
        running: true,
        host: svc.host.clone(),
        port: svc.port,
        auth_enabled: svc.auth_token.is_some(),
        routes,
        model_override,
        target_protocol,
        models_merge: svc.core.db().models_merge_enabled().unwrap_or(false),
        version: env!("CARGO_PKG_VERSION").to_string(),
        circuits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_dto_serialization_with_state() {
        let dto = CircuitDto {
            provider_id: "upstream-1".to_string(),
            fails: 2,
            cooldown_remaining_secs: Some(15),
            state: CircuitState::HalfOpen,
        };
        let s = serde_json::to_string(&dto).unwrap();
        assert!(s.contains("\"state\":\"HalfOpen\""));

        let deserialized: CircuitDto = serde_json::from_str(&s).unwrap();
        assert_eq!(deserialized, dto);
    }

    #[test]
    fn test_circuit_dto_deserialization_default_state() {
        let json = r#"{"provider_id":"p1","fails":0,"cooldown_remaining_secs":null}"#;
        let dto: CircuitDto = serde_json::from_str(json).unwrap();
        assert_eq!(dto.state, CircuitState::Closed);
    }
}
