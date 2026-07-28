use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use super::ProxyService;

/// auth_token 校验中间件：设置 token 时要求 `Authorization: Bearer <token>`，
/// 未设置则放行。作用于 /v1/* 与 /__asw/status；/health 不挂此层。
pub async fn auth_guard(
    State(svc): State<Arc<ProxyService>>,
    req: Request,
    next: Next,
) -> Response {
    if let Some(token) = &svc.auth_token {
        let auth = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if auth != format!("Bearer {token}") {
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    }
    next.run(req).await
}
