use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use super::ProxyService;

/// auth_token check middleware: when a token is set, require `Authorization: Bearer <token>`;
/// otherwise pass through. Applies to /v1/* and /__xfade/status; /health does not mount this layer.
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
