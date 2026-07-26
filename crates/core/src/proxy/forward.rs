use super::ProxyService;
use crate::store::db::RequestLog;
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::Response,
};
use std::sync::Arc;
use std::time::Instant;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const STRIP_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "authorization",
    "connection",
    "transfer-encoding",
];

fn endpoint_path(endpoint: &str) -> &'static str {
    match endpoint {
        "chat" => "/chat/completions",
        "responses" => "/responses",
        "messages" => "/messages",
        "models" => "/models",
        _ => "/chat/completions",
    }
}

fn endpoint_from_path(path: &str) -> &'static str {
    match path {
        "/v1/chat/completions" => "chat",
        "/v1/responses" => "responses",
        "/v1/messages" => "messages",
        "/v1/models" => "models",
        _ => "chat",
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn extract_model(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(|s| s.to_string()))
}

async fn try_forward(
    client: &reqwest::Client,
    method: &str,
    base_url: &str,
    endpoint: &str,
    headers: &HeaderMap,
    body: &[u8],
    auth_key: Option<&str>,
) -> std::result::Result<(StatusCode, Bytes), String> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{}{}", base, endpoint_path(endpoint));

    let mut req = match method {
        "GET" => client.get(&url),
        _ => client.post(&url),
    };

    for (name, value) in headers.iter() {
        let name_str = name.as_str().to_lowercase();
        if !STRIP_HEADERS.contains(&name_str.as_str()) {
            if let Ok(v) = value.to_str() {
                req = req.header(name.as_str(), v);
            }
        }
    }

    if let Some(key) = auth_key {
        req = req.header("Authorization", format!("Bearer {key}"));
    }

    if method != "GET" {
        req = req.body(body.to_vec());
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    Ok((status, bytes))
}

async fn forward_with_failover(
    svc: &ProxyService,
    method: &str,
    endpoint: &str,
    headers: &HeaderMap,
    body: &[u8],
    model: Option<String>,
) -> Response {
    let start = Instant::now();
    let routes = match svc.core.db().get_routes() {
        Ok(Some(r)) if !r.is_empty() => r,
        _ => {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body("no routes configured; use `asw proxy use <name>`".into())
                .unwrap();
        }
    };

    let providers = svc.core.list(None).unwrap_or_default();
    let provider_map: std::collections::HashMap<&String, &crate::models::Provider> =
        providers.iter().map(|p| (&p.id, p)).collect();

    let mut last_err: Option<(StatusCode, String)> = None;

    for route_id in &routes {
        if svc.circuit_open(route_id) {
            continue;
        }

        let provider = match provider_map.get(route_id) {
            Some(p) => *p,
            None => continue,
        };

        let base_url = match &provider.base_url {
            Some(u) => u.clone(),
            None => continue,
        };

        let auth_key = if provider.is_official() {
            None
        } else {
            svc.core.secrets().get(&provider.key_ref).ok()
        };

        match try_forward(
            &svc.client,
            method,
            &base_url,
            endpoint,
            headers,
            body,
            auth_key.as_deref(),
        )
        .await
        {
            Ok((status, bytes)) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                let usage = super::usage::Usage::from_json(&bytes);

                let _ = svc.core.db().insert_request_log(&RequestLog {
                    ts: now_rfc3339(),
                    endpoint: endpoint.to_string(),
                    model: model.clone(),
                    provider_id: route_id.clone(),
                    status: status.as_u16() as i64,
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    duration_ms,
                    error: None,
                });

                if status.is_success() || status.is_redirection() {
                    svc.record_success(route_id);
                    return Response::builder()
                        .status(status)
                        .body(axum::body::Body::from(bytes))
                        .unwrap();
                }

                if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    svc.record_failure(route_id);
                    last_err = Some((status, String::from_utf8_lossy(&bytes).to_string()));
                    continue;
                }

                // other 4xx: passthrough, no failover
                return Response::builder()
                    .status(status)
                    .body(axum::body::Body::from(bytes))
                    .unwrap();
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                let _ = svc.core.db().insert_request_log(&RequestLog {
                    ts: now_rfc3339(),
                    endpoint: endpoint.to_string(),
                    model: model.clone(),
                    provider_id: route_id.clone(),
                    status: 0,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    duration_ms,
                    error: Some(e.clone()),
                });
                svc.record_failure(route_id);
                last_err = Some((StatusCode::BAD_GATEWAY, e));
                continue;
            }
        }
    }

    match last_err {
        Some((status, text)) => Response::builder()
            .status(status)
            .body(text.into())
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body("no usable provider".into())
            .unwrap(),
    }
}

pub async fn handle(
    State(svc): State<Arc<ProxyService>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let endpoint = endpoint_from_path(uri.path());

    if let Some(token) = &svc.auth_token {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if auth != format!("Bearer {token}") {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body("unauthorized".into())
                .unwrap();
        }
    }

    let model = extract_model(&body);
    forward_with_failover(&svc, "POST", endpoint, &headers, &body, model).await
}

pub async fn handle_get(
    State(svc): State<Arc<ProxyService>>,
    headers: HeaderMap,
) -> Response {
    let endpoint = "models";
    if let Some(token) = &svc.auth_token {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if auth != format!("Bearer {token}") {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body("unauthorized".into())
                .unwrap();
        }
    }
    forward_with_failover(&svc, "GET", endpoint, &headers, &[], None).await
}
