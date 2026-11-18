use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::sync::Arc;

use super::ProxyService;
use crate::models::ToolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIdentity(pub ToolKind);

/// True for `localhost` and any loopback IP literal (`127.0.0.0/8`, `::1`),
/// with or without IPv6 brackets.
pub(crate) fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Hostname part of a `Host` header value (`host`, `host:port`, `[v6]:port`).
fn host_of_authority(authority: &str) -> &str {
    let a = authority.trim();
    if let Some(rest) = a.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match a.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => a,
    }
}

/// Hostname part of an `Origin` header value (`scheme://authority`).
fn host_of_origin(origin: &str) -> Option<&str> {
    let (_, authority) = origin.trim().split_once("://")?;
    Some(host_of_authority(authority))
}

/// Constant-time token comparison (hashes both sides so length does not leak).
fn token_matches(expected: &str, presented: &str) -> bool {
    let a = Sha256::digest(expected.as_bytes());
    let b = Sha256::digest(presented.as_bytes());
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// The credential a client presented: `Authorization: Bearer <t>` (OpenAI-style
/// clients) or `x-api-key: <t>` (Anthropic-native clients).
fn presented_token(headers: &HeaderMap) -> Option<&str> {
    if let Some(t) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(t);
    }
    headers.get("x-api-key").and_then(|v| v.to_str().ok())
}

/// Browser-origin defence for loopback-bound listeners.
///
/// Any web page can make a browser send requests to `127.0.0.1`, and DNS
/// rebinding can make an attacker's hostname resolve there. Both would let a
/// malicious site spend the user's upstream keys through the proxy. We reject
/// requests whose `Host` is not a loopback name, and requests that carry a
/// non-loopback `Origin` (browsers always attach one to cross-site calls;
/// CLI/SDK clients do not). When the operator deliberately binds a
/// non-loopback address a token is mandatory (enforced at startup) and these
/// checks are skipped.
pub async fn host_origin_guard(
    State(svc): State<Arc<ProxyService>>,
    req: Request,
    next: Next,
) -> Response {
    if is_loopback_host(&svc.host) {
        let headers = req.headers();
        if let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
            if !is_loopback_host(host_of_authority(host)) {
                return (StatusCode::FORBIDDEN, "forbidden: unexpected Host header")
                    .into_response();
            }
        }
        if let Some(origin) = headers.get(header::ORIGIN) {
            let ok = origin
                .to_str()
                .ok()
                .and_then(host_of_origin)
                .is_some_and(is_loopback_host);
            if !ok {
                return (StatusCode::FORBIDDEN, "forbidden: cross-origin request").into_response();
            }
        }
    }
    next.run(req).await
}

/// auth_token check middleware: when a token is set, require it as
/// `Authorization: Bearer <token>` or `x-api-key: <token>`; otherwise pass
/// through. Applies to /v1/* and /__xfade/status; /health does not mount this layer.
pub async fn auth_guard(
    State(svc): State<Arc<ProxyService>>,
    mut req: Request,
    next: Next,
) -> Response {
    let presented = presented_token(req.headers());
    if let Some(tok) = presented {
        if let Some(tool) = svc.token_manager.validate(tok) {
            req.extensions_mut().insert(ClientIdentity(tool));
            req.headers_mut().remove(header::AUTHORIZATION);
            req.headers_mut().remove("x-api-key");
            return next.run(req).await;
        }
        if tok.starts_with("xfade-local-") {
            return (
                StatusCode::UNAUTHORIZED,
                "unauthorized: invalid client token",
            )
                .into_response();
        }
    }
    if let Some(token) = &svc.auth_token {
        let ok = presented.is_some_and(|p| token_matches(token, p));
        if !ok {
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        for h in [
            "localhost",
            "LOCALHOST",
            "127.0.0.1",
            "127.9.9.9",
            "::1",
            "[::1]",
        ] {
            assert!(is_loopback_host(h), "{h}");
        }
        for h in [
            "evil.com",
            "127.0.0.1.evil.com",
            "localhost.evil.com",
            "0.0.0.0",
            "192.168.1.5",
            "",
        ] {
            assert!(!is_loopback_host(h), "{h}");
        }
    }

    #[test]
    fn authority_and_origin_parsing() {
        assert_eq!(host_of_authority("127.0.0.1:24860"), "127.0.0.1");
        assert_eq!(host_of_authority("localhost"), "localhost");
        assert_eq!(host_of_authority("[::1]:24860"), "::1");
        assert_eq!(host_of_authority("evil.com:80"), "evil.com");
        assert_eq!(host_of_origin("http://localhost:3000"), Some("localhost"));
        assert_eq!(host_of_origin("https://evil.com"), Some("evil.com"));
        assert_eq!(host_of_origin("null"), None);
    }

    #[test]
    fn token_comparison() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("abc", "abcd"));
        assert!(!token_matches("abc", ""));
    }

    async fn test_auth_call_with_token(
        tm: Arc<crate::store::tokens::TokenManager>,
        token: &str,
    ) -> (StatusCode, Option<ClientIdentity>) {
        use axum::middleware::from_fn_with_state;
        use axum::routing::post;
        use axum::Router;
        use tower::ServiceExt;

        let dir = tempfile::tempdir().unwrap();
        let core = crate::proxy::testsupport::test_core(&dir, &[]);
        let mut svc = ProxyService::new(core);
        svc.token_manager = tm;
        let svc = Arc::new(svc);

        let captured = Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();

        let app = Router::new()
            .route(
                "/test",
                post(|req: axum::extract::Request| async move {
                    let id = req.extensions().get::<ClientIdentity>().copied();
                    *captured_clone.lock().unwrap() = id;
                    StatusCode::OK
                }),
            )
            .layer(from_fn_with_state(svc, auth_guard));

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/test")
            .header("Authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let id = captured.lock().unwrap().take();
        (status, id)
    }

    #[tokio::test]
    async fn test_auth_guard_accepts_client_token_and_tags_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::store::Db::open_in_dir(dir.path()).unwrap());
        let tm = Arc::new(crate::store::tokens::TokenManager::new(db));
        let token = tm.get_or_create(ToolKind::ClaudeCode).unwrap();

        let (status, identity) = test_auth_call_with_token(tm.clone(), &token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(identity, Some(ClientIdentity(ToolKind::ClaudeCode)));
    }

    #[tokio::test]
    async fn test_auth_guard_accepts_x_api_key_and_strips_headers() {
        use axum::middleware::from_fn_with_state;
        use axum::routing::post;
        use axum::Router;
        use tower::ServiceExt;

        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::store::Db::open_in_dir(dir.path()).unwrap());
        let tm = Arc::new(crate::store::tokens::TokenManager::new(db));
        let token = tm.get_or_create(ToolKind::Codex).unwrap();

        let core = crate::proxy::testsupport::test_core(&dir, &[]);
        let mut svc = ProxyService::new(core);
        svc.token_manager = tm.clone();
        let svc = Arc::new(svc);

        let captured_headers = Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured_headers.clone();

        let app = Router::new()
            .route(
                "/test",
                post(|req: axum::extract::Request| async move {
                    *captured_clone.lock().unwrap() = Some(req.headers().clone());
                    StatusCode::OK
                }),
            )
            .layer(from_fn_with_state(svc, auth_guard));

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/test")
            .header("x-api-key", &token)
            .header("x-other-header", "keep-me")
            .body(axum::body::Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let hdrs = captured_headers.lock().unwrap().take().unwrap();
        assert!(
            !hdrs.contains_key("x-api-key"),
            "x-api-key must be stripped"
        );
        assert!(
            !hdrs.contains_key("authorization"),
            "authorization must be stripped"
        );
        assert_eq!(hdrs.get("x-other-header").unwrap(), "keep-me");
    }

    #[tokio::test]
    async fn test_auth_guard_rejects_invalid_local_token() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::store::Db::open_in_dir(dir.path()).unwrap());
        let tm = Arc::new(crate::store::tokens::TokenManager::new(db));

        let (status, identity) =
            test_auth_call_with_token(tm, "xfade-local-claude-fake123456").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(identity, None);
    }
}
