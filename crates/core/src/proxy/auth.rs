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
    req: Request,
    next: Next,
) -> Response {
    if let Some(token) = &svc.auth_token {
        let ok = presented_token(req.headers()).is_some_and(|p| token_matches(token, p));
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
}
