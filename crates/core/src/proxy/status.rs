use axum::{extract::State, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

use super::{Circuit, ProxyService};

#[derive(Serialize, Deserialize)]
pub struct CircuitDto {
    pub provider_id: String,
    pub fails: u32,
    pub cooldown_remaining_secs: Option<u64>,
}

/// `Circuit` → `CircuitDto`：把 `cooldown_until: Instant` 换算为剩余冷却秒数。
/// core 与 GUI 共用，避免重复实现。
pub fn circuit_to_dto(provider_id: &str, c: &Circuit) -> CircuitDto {
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

/// `GET /__asw/status`：返回代理运行状态、路由配置与各 provider 熔断状态。
/// 经 auth_guard 校验（若设置了 auth_token）。
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
