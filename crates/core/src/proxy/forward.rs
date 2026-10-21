//! HTTP forward pipeline: routes resolution → body conversion → upstream
//! request → streaming/non-streaming response handling → failover.
//!
//! # Clippy allowances
//!
//! Forward-pipeline functions naturally have many params (svc/headers/body/endpoint/model/route_id/...).
//! The `result_large_err` / `type_complexity` lints are resolved via named types (`ResolvedRoutes`,
//! `PreparedBodies`) and `Box<Response>`; only `too_many_arguments` remains allowed, as the
//! forwarding scenario is inherently parameter-heavy:
#![allow(clippy::too_many_arguments)]

use super::usage::Usage;
use super::ProxyService;
use crate::store::db::RequestLog;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::Response,
};
use futures::StreamExt;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const STRIP_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "authorization",
    "connection",
    "transfer-encoding",
    "anthropic-version",
    "anthropic-beta",
];

/// Tail ring buffer size kept while streaming to extract usage at the end (~8KB).
const SSE_TAIL_CAP: usize = 8 * 1024;

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

/// If this is a chat completion request that asked for streaming but did not
/// already specify `stream_options`, inject `{"stream_options":{"include_usage":true}}`
/// and return the rewritten body. Otherwise return `None` (use the original body).
fn maybe_inject_stream_options(endpoint: &str, body: &[u8]) -> Option<Vec<u8>> {
    if endpoint != "chat" {
        return None;
    }
    let mut v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let is_stream = v.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
    if !is_stream {
        return None;
    }
    if v.get("stream_options").is_some() {
        return None;
    }
    v["stream_options"] = serde_json::json!({"include_usage": true});
    serde_json::to_vec(&v).ok()
}

/// Transform a passthrough body in a single parse: inject
/// `stream_options` for streaming chat requests (usage extraction) and
/// rewrite the top-level `model` field when the route carries a model
/// override. Returns `None` when the body needs no change, so the original
/// bytes are forwarded untouched.
///
/// The model rewrite is what makes model overrides work on the
/// chat→chat passthrough path (local MLX/Ollama runtimes) — the convert
/// path applies overrides during protocol conversion, passthrough needs
/// this shallow rewrite.
fn transform_passthrough_body(
    endpoint: &str,
    body: &[u8],
    model_override: Option<&str>,
) -> Option<Vec<u8>> {
    if endpoint != "chat" && model_override.is_none() {
        return None;
    }
    let mut v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let mut changed = false;

    if endpoint == "chat" {
        let is_stream = v.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
        if is_stream && v.get("stream_options").is_none() {
            v["stream_options"] = serde_json::json!({"include_usage": true});
            changed = true;
        }
    }

    if let Some(m) = model_override {
        // Only rewrite when the body already carries a model field: a
        // model-less body (unusual for chat APIs) is forwarded as-is.
        if v.get("model").is_some() && v.get("model").and_then(|x| x.as_str()) != Some(m) {
            v["model"] = serde_json::Value::String(m.to_string());
            changed = true;
        }
    }

    if !changed {
        return None;
    }
    serde_json::to_vec(&v).ok()
}

/// Build the outgoing reqwest request (without sending) for a given upstream.
fn build_upstream_request(
    client: &reqwest::Client,
    method: &str,
    base_url: &str,
    endpoint: &str,
    headers: &HeaderMap,
    body: &[u8],
    auth_key: Option<&str>,
) -> reqwest::RequestBuilder {
    let base = crate::adapters::with_v1_if_bare_host(base_url);
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

    req
}

/// Send the upstream request and return the raw reqwest response so the caller
/// can decide whether to consume the body as bytes (non-stream) or stream it.
async fn try_forward(
    client: &reqwest::Client,
    method: &str,
    base_url: &str,
    endpoint: &str,
    headers: &HeaderMap,
    body: &[u8],
    auth_key: Option<&str>,
) -> std::result::Result<reqwest::Response, String> {
    let req = build_upstream_request(client, method, base_url, endpoint, headers, body, auth_key);
    req.send().await.map_err(|e| e.to_string())
}

/// Whether the upstream response is a streaming SSE response, based on its
/// content-type header containing `text/event-stream`.
fn is_stream_response(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false)
}

/// Text of the last user message, for intent classification.
///
/// chat/messages take the last `role=="user"` entry of `messages`;
/// responses takes the last user item of `input`. Content may be a plain
/// string or an array of typed blocks. Huge prompts are truncated to keep
/// classification cheap.
fn extract_prompt_text(body: &serde_json::Value, endpoint: &str) -> String {
    let messages = match endpoint {
        "responses" => body.get("input").and_then(|i| i.as_array()),
        // chat + messages share the `messages[].content` shape.
        _ => body.get("messages").and_then(|m| m.as_array()),
    };
    let last_user = messages
        .and_then(|msgs| {
            msgs.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        })
        .and_then(|m| m.get("content"));
    match last_user {
        Some(c) => {
            let text = crate::sessions::content_text(c);
            // Cap at 4KB: keyword classification only needs the ask itself,
            // and multi-MB contexts (agentic transcripts) must not pay for it.
            text.chars().take(4096).collect()
        }
        None => String::new(),
    }
}

/// Log a request to the database and fold its outcome into the session
/// (token accumulation + handover attribution). Best-effort: errors are
/// swallowed (the proxy must not fail a successful forward because logging
/// failed).
fn log_request(
    svc: &ProxyService,
    session_id: Option<&str>,
    endpoint: &str,
    model: &Option<String>,
    provider_id: &str,
    tool: &str,
    status: i64,
    usage: Usage,
    duration_ms: i64,
    error: Option<String>,
) {
    let _ = svc.core.db().insert_request_log(&RequestLog {
        ts: now_rfc3339(),
        endpoint: endpoint.to_string(),
        model: model.clone(),
        provider_id: provider_id.to_string(),
        tool: tool.to_string(),
        status,
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        duration_ms,
        error,
        session_id: session_id.map(|s| s.to_string()),
    });
    crate::sessions::record_outcome(
        svc.core.db(),
        session_id,
        tool,
        provider_id,
        model.as_deref(),
        usage.prompt_tokens,
        usage.completion_tokens,
        status != 0 && status < 400,
    );
}

// ── Forward pipeline stages ────────────────────────────────────────────────

/// Resolved route config + provider map (result of `resolve_routes`).
struct ResolvedRoutes<'a> {
    routes: Vec<String>,
    model_override: Option<String>,
    target_protocol: String,
    provider_map: std::collections::HashMap<String, &'a crate::models::Provider>,
}

/// Prepared request bodies for both paths (result of `prepare_bodies`).
struct PreparedBodies {
    converted_body: Option<Vec<u8>>,
    passthrough_body: Option<Vec<u8>>,
    convert_path: bool,
}

/// Resolve routes and the provider map. Returns a 503 response if no routes.
fn resolve_routes<'a>(
    svc: &'a ProxyService,
    providers: &'a [crate::models::Provider],
) -> Result<ResolvedRoutes<'a>, Box<Response>> {
    let (routes, model_override, target_protocol) = match svc.core.db().get_routes() {
        Ok(Some(r)) if !r.0.is_empty() => r,
        _ => {
            return Err(Box::new(
                Response::builder()
                    .status(StatusCode::SERVICE_UNAVAILABLE)
                    .body("no routes configured; use `xfade proxy use <name>`".into())
                    .unwrap(),
            ));
        }
    };
    let provider_map: std::collections::HashMap<String, &crate::models::Provider> =
        providers.iter().map(|p| (p.id.clone(), p)).collect();
    Ok(ResolvedRoutes {
        routes,
        model_override,
        target_protocol,
        provider_map,
    })
}

/// Prepare the request body for conversion or passthrough paths.
/// Returns a `PreparedBodies` on success, or an error response.
/// The returned `Vec<u8>` values are owned to avoid lifetime issues with temporary buffers.
fn prepare_bodies(
    endpoint: &str,
    target_protocol: &str,
    body: &[u8],
    model_override: Option<&str>,
) -> Result<PreparedBodies, Box<Response>> {
    let convert_path =
        (endpoint == "messages" || endpoint == "responses") && target_protocol == "chat";

    // Build converted request body (Anthropic/Responses → OpenAI chat).
    let converted_body: Option<Vec<u8>> = if convert_path {
        let converted = match endpoint {
            "responses" => super::convert::request_responses_to_chat(body, model_override),
            _ => super::convert::request_anthropic_to_openai(body, model_override),
        };
        match converted {
            Ok(b) => {
                // Inject stream_options if needed.
                let injected = maybe_inject_stream_options("chat", &b);
                Some(injected.unwrap_or_else(|| b.to_vec()))
            }
            Err(e) => {
                return Err(Box::new(
                    Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(format!("convert request: {e}").into())
                        .unwrap(),
                ));
            }
        }
    } else {
        None
    };

    // Passthrough body: stream_options injection + shallow model rewrite
    // (single parse). Skipped on the convert path, where only the
    // converted body is ever sent.
    let passthrough_body: Option<Vec<u8>> = if convert_path {
        None
    } else {
        transform_passthrough_body(endpoint, body, model_override)
    };

    Ok(PreparedBodies {
        converted_body,
        passthrough_body,
        convert_path,
    })
}

/// Handle a successful upstream response. Returns the response to send to the client.
/// Callers (the failover loop) guarantee a success/redirection status.
async fn handle_success(
    svc: &ProxyService,
    session_id: Option<&str>,
    route_id: &str,
    tool: &str,
    resp: reqwest::Response,
    endpoint: &str,
    model: Option<String>,
    start: Instant,
    convert_path: bool,
    req_model: String,
) -> Response {
    let streaming = is_stream_response(&resp);

    svc.record_success(route_id);

    if streaming {
        handle_success_streaming(
            svc,
            session_id,
            route_id,
            tool,
            resp,
            endpoint,
            model,
            start,
            convert_path,
            req_model,
        )
    } else {
        handle_success_non_streaming(
            svc,
            session_id,
            route_id,
            tool,
            resp,
            endpoint,
            model,
            start,
            convert_path,
            req_model,
        )
        .await
    }
}

/// Handle successful streaming response.
fn handle_success_streaming(
    svc: &ProxyService,
    session_id: Option<&str>,
    route_id: &str,
    tool: &str,
    resp: reqwest::Response,
    endpoint: &str,
    model: Option<String>,
    start: Instant,
    convert_path: bool,
    req_model: String,
) -> Response {
    let status = resp.status();
    let content_type = if convert_path {
        "text/event-stream".to_string()
    } else {
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_string()
    };

    let provider_id_owned = route_id.to_string();
    let tool_owned = tool.to_string();
    let endpoint_owned = endpoint.to_string();
    let model_owned = model.clone();
    let session_owned = session_id.map(|s| s.to_string());
    let db = svc.core.db().clone();

    let body = if convert_path {
        // Pass upstream errors through: the converters surface them as
        // protocol-level error events (Anthropic `error` / `response.failed`)
        // instead of masking a truncated stream as success.
        let upstream = resp
            .bytes_stream()
            .map(|r| r.map_err(|e| std::io::Error::other(e.to_string())));
        let converted: std::pin::Pin<Box<dyn futures::Stream<Item = Bytes> + Send>> =
            if endpoint == "responses" {
                Box::pin(super::convert::stream_chat_to_responses(
                    upstream, req_model,
                ))
            } else {
                Box::pin(super::convert::stream_openai_to_anthropic(
                    upstream, req_model,
                ))
            };
        make_converted_tapped_streaming_body(
            converted,
            db,
            endpoint_owned,
            model_owned,
            provider_id_owned,
            tool_owned,
            session_owned,
            status,
            start,
        )
    } else {
        make_tapped_streaming_body(
            resp.bytes_stream(),
            db,
            endpoint_owned,
            model_owned,
            provider_id_owned,
            tool_owned,
            session_owned,
            status,
            start,
        )
    };

    let mut resp_builder = Response::builder().status(status);
    if let Some(hv) = resp_builder.headers_mut() {
        hv.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_str(&content_type).unwrap(),
        );
    }
    resp_builder.body(body).unwrap()
}

/// Whether the body plausibly is JSON (first non-whitespace byte is `{` or `[`).
fn body_looks_like_json(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| *b == b'{' || *b == b'[')
}

/// Handle successful non-streaming response.
async fn handle_success_non_streaming(
    svc: &ProxyService,
    session_id: Option<&str>,
    route_id: &str,
    tool: &str,
    resp: reqwest::Response,
    endpoint: &str,
    model: Option<String>,
    start: Instant,
    convert_path: bool,
    req_model: String,
) -> Response {
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            let duration_ms = start.elapsed().as_millis() as i64;
            log_request(
                svc,
                session_id,
                endpoint,
                &model,
                route_id,
                tool,
                0,
                Usage::default(),
                duration_ms,
                Some(format!("read body: {e}")),
            );
            svc.record_failure(route_id);
            return Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(e.to_string().into())
                .unwrap();
        }
    };

    let duration_ms = start.elapsed().as_millis() as i64;

    // A 2xx response that is neither SSE nor JSON means the upstream URL is
    // wrong (typically a gateway web page answering 200 HTML because the
    // provider base_url misses the /v1 path). Fail loudly instead of relaying
    // the page to the client, which surfaces as cryptic errors like Pi's
    // "Stream ended without finish_reason".
    if !content_type.contains("json") && !body_looks_like_json(&bytes) {
        let msg = format!(
            "xfade: upstream provider '{route_id}' returned a non-JSON response \
             (content-type: '{content_type}'); its base_url is probably wrong — \
             OpenAI/Anthropic-compatible upstreams usually need a /v1 path suffix"
        );
        log_request(
            svc,
            session_id,
            endpoint,
            &model,
            route_id,
            tool,
            StatusCode::BAD_GATEWAY.as_u16() as i64,
            Usage::default(),
            duration_ms,
            Some(msg.clone()),
        );
        return Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(msg.into())
            .unwrap();
    }

    if convert_path {
        let converted = if endpoint == "responses" {
            super::convert::response_chat_to_responses(&bytes, &req_model)
        } else {
            super::convert::response_openai_to_anthropic(&bytes, &req_model)
        };
        match converted {
            Ok(anthropic_bytes) => {
                let usage = Usage::from_json(&anthropic_bytes);
                log_request(
                    svc,
                    session_id,
                    endpoint,
                    &model,
                    route_id,
                    tool,
                    status.as_u16() as i64,
                    usage,
                    duration_ms,
                    None,
                );
                return Response::builder()
                    .status(status)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(anthropic_bytes))
                    .unwrap();
            }
            Err(e) => {
                log_request(
                    svc,
                    session_id,
                    endpoint,
                    &model,
                    route_id,
                    tool,
                    status.as_u16() as i64,
                    Usage::default(),
                    duration_ms,
                    Some(format!("convert response: {e}")),
                );
                return Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(format!("convert response: {e}").into())
                    .unwrap();
            }
        }
    }

    let usage = Usage::from_json(&bytes);
    log_request(
        svc,
        session_id,
        endpoint,
        &model,
        route_id,
        tool,
        status.as_u16() as i64,
        usage,
        duration_ms,
        None,
    );
    Response::builder()
        .status(status)
        .body(Body::from(bytes))
        .unwrap()
}

/// Handle an upstream error response (5xx, 429, or connection failure).
/// Returns the response to send (either pass-through for 4xx, or signal
/// for failover continuation).
async fn handle_upstream_error(
    svc: &ProxyService,
    session_id: Option<&str>,
    route_id: &str,
    tool: &str,
    resp: reqwest::Response,
    endpoint: &str,
    model: &Option<String>,
    start: Instant,
) -> Response {
    let status = resp.status();

    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        let bytes = resp.bytes().await.unwrap_or_default();
        let duration_ms = start.elapsed().as_millis() as i64;
        let usage = Usage::from_json(&bytes);
        log_request(
            svc,
            session_id,
            endpoint,
            model,
            route_id,
            tool,
            status.as_u16() as i64,
            usage,
            duration_ms,
            None,
        );
        svc.record_failure(route_id);
        // Signal caller to continue failover via the error variant.
        return Response::builder()
            .status(status)
            .body(String::from_utf8_lossy(&bytes).to_string().into())
            .unwrap();
    }

    // Other 4xx: passthrough, no failover, no circuit breaker.
    let bytes = resp.bytes().await.unwrap_or_default();
    let duration_ms = start.elapsed().as_millis() as i64;
    let usage = Usage::from_json(&bytes);
    log_request(
        svc,
        session_id,
        endpoint,
        model,
        route_id,
        tool,
        status.as_u16() as i64,
        usage,
        duration_ms,
        None,
    );
    Response::builder()
        .status(status)
        .body(Body::from(bytes))
        .unwrap()
}

/// Fold a pipeline-level failure (no routes, bad conversion, all routes
/// exhausted) into the session so it doesn't stay "running" forever.
fn session_failed(svc: &ProxyService, session_id: Option<&str>) {
    if let Some(sid) = session_id {
        crate::sessions::record_outcome(svc.core.db(), Some(sid), "", "", None, 0, 0, false);
    }
}

/// Execute the failover loop: try each route in order, skipping open circuits.
/// Returns the final response (success or last error).
async fn failover_loop(
    svc: &ProxyService,
    session_id: Option<&str>,
    method: &str,
    entries: &[crate::router::decision::RouteEntry],
    provider_map: &std::collections::HashMap<String, &crate::models::Provider>,
    endpoint: &str,
    target_protocol: &str,
    model: Option<String>,
    headers: &HeaderMap,
    original_body: &[u8],
    req_model: &str,
    start: Instant,
) -> Response {
    let mut last_err: Option<(StatusCode, String)> = None;
    // Per-route body preparation is memoized by model override: adjacent
    // entries in a tier usually share the same override (static routing is
    // all-None), so failover retries must not re-run the full conversion.
    let mut bodies_cache: std::collections::HashMap<Option<String>, PreparedBodies> =
        std::collections::HashMap::new();

    for entry in entries {
        let route_id = entry.route_id.as_str();
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

        // Per-route body preparation: each entry may carry its own model
        // override (Smart Fader per-tier models). The convert path is fixed
        // by (endpoint, target_protocol) — only the override differs — so
        // bodies are cached per override value.
        let bodies = match bodies_cache.get(&entry.model_override) {
            Some(b) => b,
            None => {
                let prepared = match prepare_bodies(
                    endpoint,
                    target_protocol,
                    original_body,
                    entry.model_override.as_deref(),
                ) {
                    Ok(r) => r,
                    Err(resp) => {
                        session_failed(svc, session_id);
                        return *resp;
                    }
                };
                bodies_cache
                    .entry(entry.model_override.clone())
                    .or_insert(prepared)
            }
        };
        let PreparedBodies {
            converted_body: conv_body,
            passthrough_body: passthru_body,
            convert_path,
        } = bodies;
        let convert_path = *convert_path;

        let out_body: &[u8] = if convert_path {
            conv_body.as_deref().unwrap_or(original_body)
        } else {
            passthru_body.as_deref().unwrap_or(original_body)
        };
        let upstream_endpoint = if convert_path { "chat" } else { endpoint };

        match try_forward(
            &svc.client,
            method,
            &base_url,
            upstream_endpoint,
            headers,
            out_body,
            auth_key.as_deref(),
        )
        .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() || status.is_redirection() {
                    return handle_success(
                        svc,
                        session_id,
                        route_id,
                        provider.tool.as_str(),
                        resp,
                        endpoint,
                        model,
                        start,
                        convert_path,
                        req_model.to_string(),
                    )
                    .await;
                }
                if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    let _resp = handle_upstream_error(
                        svc,
                        session_id,
                        route_id,
                        provider.tool.as_str(),
                        resp,
                        endpoint,
                        &model,
                        start,
                    )
                    .await;
                    last_err = Some((status, String::new()));
                    continue;
                }
                // Other 4xx: passthrough, stop failover.
                return handle_upstream_error(
                    svc,
                    session_id,
                    route_id,
                    provider.tool.as_str(),
                    resp,
                    endpoint,
                    &model,
                    start,
                )
                .await;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                log_request(
                    svc,
                    session_id,
                    endpoint,
                    &model,
                    route_id,
                    provider.tool.as_str(),
                    0,
                    Usage::default(),
                    duration_ms,
                    Some(e.clone()),
                );
                svc.record_failure(route_id);
                last_err = Some((StatusCode::BAD_GATEWAY, e));
                continue;
            }
        }
    }

    session_failed(svc, session_id);

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

// ── Main forward entry point ───────────────────────────────────────────────

async fn forward_with_failover(
    svc: &ProxyService,
    method: &str,
    endpoint: &str,
    headers: &HeaderMap,
    body: &[u8],
    parsed: Option<&serde_json::Value>,
    model: Option<String>,
    session_id: Option<&str>,
) -> Response {
    let start = Instant::now();

    // Stage 1: resolve routes and providers.
    let providers = svc.core.list(None).unwrap_or_default();
    let ResolvedRoutes {
        routes,
        model_override,
        target_protocol,
        provider_map,
    } = match resolve_routes(svc, &providers) {
        Ok(r) => r,
        Err(resp) => {
            session_failed(svc, session_id);
            return *resp;
        }
    };

    // Stage 2: plan the route entries — Stage 0 for the Smart Fader:
    // extract the last user message and let the active router classify the
    // intent and order the tiers (static order while the Smart Fader is
    // off). Providers listed in the route config but no longer registered
    // are dropped here, same as the old in-loop skip.
    let route_providers: Vec<&crate::models::Provider> = routes
        .iter()
        .filter_map(|id| provider_map.get(id).copied())
        .collect();
    let router = svc.active_router();
    // Prompt extraction (parse + 4KB copy of the last user turn) only pays
    // off when the router actually classifies; the static path skips it.
    let prompt_text = if router.is_smart() {
        parsed.map_or(String::new(), |v| extract_prompt_text(v, endpoint))
    } else {
        String::new()
    };
    let classify_ctx = crate::router::classifier::ClassifyCtx {
        endpoint,
        model: model.as_deref(),
        prompt: &prompt_text,
    };
    let plan = router.plan(
        &classify_ctx,
        &route_providers,
        model_override.as_deref(),
        &|id: &str| svc.circuit_open(id),
        &|id: &str| svc.health_signal(id),
    );
    if plan.entries.is_empty() {
        session_failed(svc, session_id);
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body("no usable provider".into())
            .unwrap();
    }

    let req_model = model.clone().unwrap_or_default();

    // Stage 3: execute failover loop across the planned entries.
    failover_loop(
        svc,
        session_id,
        method,
        &plan.entries,
        &provider_map,
        endpoint,
        &target_protocol,
        model,
        headers,
        body,
        &req_model,
        start,
    )
    .await
}

// ── Streaming body helpers ─────────────────────────────────────────────────

/// Returns true if `line` is an OpenAI SSE `data:` frame whose JSON payload has
/// an empty `choices` array (the new-api trailing usage-only frame).
fn is_empty_choices_line(line: &[u8]) -> bool {
    let text = std::str::from_utf8(line).unwrap_or("");
    let text = text.trim();
    let Some(payload) = text.strip_prefix("data:") else {
        return false;
    };
    let payload = payload.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return false;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    matches!(v.get("choices"), Some(serde_json::Value::Array(a)) if a.is_empty())
}

/// Filter empty-`choices` frames out of an OpenAI SSE byte stream.
/// new-api gateways (e.g. BM TokenHub) append a trailing `data: {"choices":[],...}`
/// frame that breaks streaming clients (litellm "Empty response", Pi "finish_reason").
/// Non-OpenAI SSE (Anthropic events) passes through untouched, since its `data:`
/// payloads never carry an empty `choices` array.
fn filter_empty_choices<S>(upstream: S) -> impl futures::Stream<Item = std::io::Result<Bytes>>
where
    S: futures::Stream<Item = std::io::Result<Bytes>> + Send + 'static,
{
    use std::collections::VecDeque;

    struct State {
        upstream: std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>>,
        pending: Vec<u8>,
        out: VecDeque<Bytes>,
        finished: bool,
    }

    let init = State {
        upstream: Box::pin(upstream),
        pending: Vec::new(),
        out: VecDeque::new(),
        finished: false,
    };

    futures::stream::unfold(init, move |mut st| async move {
        loop {
            if let Some(b) = st.out.pop_front() {
                return Some((Ok(b), st));
            }
            if st.finished {
                return None;
            }
            match st.upstream.as_mut().next().await {
                Some(Ok(chunk)) => {
                    st.pending.extend_from_slice(&chunk);
                    let mut filtered = Vec::new();
                    while let Some(pos) = st.pending.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = st.pending.drain(..=pos).collect();
                        if !is_empty_choices_line(&line) {
                            filtered.extend_from_slice(&line);
                        }
                    }
                    if !filtered.is_empty() {
                        st.out.push_back(Bytes::from(filtered));
                    }
                }
                Some(Err(e)) => {
                    st.finished = true;
                    return Some((Err(e), st));
                }
                None => {
                    st.finished = true;
                    if !st.pending.is_empty() {
                        let rest = std::mem::take(&mut st.pending);
                        if !is_empty_choices_line(&rest) {
                            return Some((Ok(Bytes::from(rest)), st));
                        }
                    }
                    return None;
                }
            }
        }
    })
}

/// Append `chunk` to the tail ring buffer (~`SSE_TAIL_CAP` bytes).
fn push_tail(tail: &Mutex<Vec<u8>>, chunk: &[u8]) {
    let mut t = tail.lock().unwrap();
    t.extend_from_slice(chunk);
    if t.len() > SSE_TAIL_CAP {
        let excess = t.len() - SSE_TAIL_CAP;
        t.drain(..excess);
    }
}

/// Build a streaming axum `Body` from the upstream chunk stream that:
///   - forwards each chunk to the client unchanged, and
///   - maintains a tail ring buffer (~`SSE_TAIL_CAP` bytes) of the most recent
///     bytes, and once the stream ends, extracts usage from that tail and
///     writes a request log row.
///
/// The logging happens as a side effect of the stream being driven to
/// completion by axum/the client.
fn make_tapped_streaming_body<S>(
    upstream: S,
    db: crate::store::db::Database,
    endpoint: String,
    model: Option<String>,
    provider_id: String,
    tool: String,
    session_id: Option<String>,
    status: StatusCode,
    start: Instant,
) -> Body
where
    S: futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let tail: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::with_capacity(SSE_TAIL_CAP)));
    let tail_for_stream = tail.clone();

    let mapped = upstream.then(move |chunk_result| {
        let tail = tail_for_stream.clone();
        async move {
            if let Ok(chunk) = &chunk_result {
                push_tail(&tail, chunk);
            }
            chunk_result.map_err(|e| std::io::Error::other(e.to_string()))
        }
    });

    // Strip new-api's trailing empty-`choices` frame from what we send to the
    // client. The tail buffer above already captured the raw bytes, so usage
    // extraction still sees the empty frame's usage fields.
    let filtered = filter_empty_choices(mapped);

    Body::from_stream(TailLogStream {
        inner: Box::pin(filtered),
        tail,
        endpoint,
        model,
        provider_id,
        tool,
        session_id,
        db,
        status,
        start,
        done_logged: false,
    })
}

/// Same as `make_tapped_streaming_body`, but for the conversion path: the input
/// is a stream of already-converted SSE frames (`Usage::from_sse_tail` handles
/// both Anthropic and Responses events), so no empty-`choices` filtering.
fn make_converted_tapped_streaming_body<S>(
    converted: S,
    db: crate::store::db::Database,
    endpoint: String,
    model: Option<String>,
    provider_id: String,
    tool: String,
    session_id: Option<String>,
    status: StatusCode,
    start: Instant,
) -> Body
where
    S: futures::Stream<Item = Bytes> + Send + 'static,
{
    let tail: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::with_capacity(SSE_TAIL_CAP)));
    let tail_for_stream = tail.clone();

    let mapped = converted.then(move |chunk| {
        let tail = tail_for_stream.clone();
        async move {
            push_tail(&tail, &chunk);
            Ok::<Bytes, std::io::Error>(chunk)
        }
    });

    Body::from_stream(TailLogStream {
        inner: Box::pin(mapped),
        tail,
        endpoint,
        model,
        provider_id,
        tool,
        session_id,
        db,
        status,
        start,
        done_logged: false,
    })
}

// ── Stream wrapper ─────────────────────────────────────────────────────────

/// Forwards an inner SSE stream unchanged; on stream end (or the first error
/// item) writes exactly one request-log row with usage extracted from the tail.
struct TailLogStream {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>>,
    tail: Arc<Mutex<Vec<u8>>>,
    endpoint: String,
    model: Option<String>,
    provider_id: String,
    tool: String,
    session_id: Option<String>,
    db: crate::store::db::Database,
    status: StatusCode,
    start: Instant,
    done_logged: bool,
}

impl TailLogStream {
    fn log(&self, error: Option<String>) {
        let tail_bytes = self.tail.lock().unwrap().clone();
        let usage = Usage::from_sse_tail(&tail_bytes);
        let duration_ms = self.start.elapsed().as_millis() as i64;
        let status_ok = error.is_none();
        let _ = self.db.insert_request_log(&RequestLog {
            ts: now_rfc3339(),
            endpoint: self.endpoint.clone(),
            model: self.model.clone(),
            provider_id: self.provider_id.clone(),
            tool: self.tool.clone(),
            status: self.status.as_u16() as i64,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            duration_ms,
            error,
            session_id: self.session_id.clone(),
        });
        crate::sessions::record_outcome(
            &self.db,
            self.session_id.as_deref(),
            &self.tool,
            &self.provider_id,
            self.model.as_deref(),
            usage.prompt_tokens,
            usage.completion_tokens,
            status_ok,
        );
    }
}

impl futures::Stream for TailLogStream {
    type Item = std::io::Result<Bytes>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(Some(item)) => {
                if item.is_err() && !this.done_logged {
                    this.done_logged = true;
                    this.log(Some("upstream stream interrupted".to_string()));
                }
                std::task::Poll::Ready(Some(item))
            }
            std::task::Poll::Ready(None) => {
                if !this.done_logged {
                    this.done_logged = true;
                    this.log(None);
                }
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

// ── Axum handlers ──────────────────────────────────────────────────────────

pub async fn handle(
    State(svc): State<Arc<ProxyService>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let endpoint = endpoint_from_path(uri.path());
    // Parse the body once; model extraction and the session engine share it.
    let parsed: Option<serde_json::Value> = serde_json::from_slice(&body).ok();
    let model = parsed
        .as_ref()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()))
        .map(|s| s.to_string());
    // Conversation identity: hash the opener (system + first user turn) and
    // reuse the session when the fingerprint matches one started recently.
    let session_id = parsed.as_ref().and_then(|v| {
        let opener = crate::sessions::opener_text(v, endpoint);
        if opener.is_empty() {
            return None;
        }
        let fp = crate::sessions::fingerprint(&opener, endpoint);
        crate::sessions::resolve_session(svc.core.db(), &fp, &opener)
    });
    let mut resp = forward_with_failover(
        &svc,
        "POST",
        endpoint,
        &headers,
        &body,
        parsed.as_ref(),
        model,
        session_id.as_deref(),
    )
    .await;
    // Surface the session id so clients/tools can correlate requests.
    if let Some(sid) = &session_id {
        if let Ok(value) = axum::http::HeaderValue::from_str(sid) {
            resp.headers_mut().insert("x-xfade-session-id", value);
        }
    }
    resp
}

pub async fn handle_get(State(svc): State<Arc<ProxyService>>, headers: HeaderMap) -> Response {
    let endpoint = "models";
    forward_with_failover(&svc, "GET", endpoint, &headers, &[], None, None, None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{stream, StreamExt};

    #[test]
    fn prepare_bodies_convert_path_skips_passthrough_transform() {
        // messages→chat convert path: the passthrough body is never sent
        // (out_body picks the converted body), so prepare_bodies must not
        // spend a parse+serialize cycle producing it.
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "claude-x",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap();
        let prepared = prepare_bodies("messages", "chat", &body, Some("gpt-y"))
            .unwrap_or_else(|_| panic!("prepare failed"));
        assert!(prepared.convert_path);
        assert!(prepared.converted_body.is_some());
        assert!(prepared.passthrough_body.is_none());
    }

    #[test]
    fn prepare_bodies_passthrough_path_still_transforms() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "a",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap();
        let prepared = prepare_bodies("chat", "chat", &body, Some("b"))
            .unwrap_or_else(|_| panic!("prepare failed"));
        assert!(!prepared.convert_path);
        assert!(prepared.passthrough_body.is_some());
    }

    #[test]
    fn extract_prompt_text_chat_takes_last_user_turn() {
        let body: serde_json::Value = serde_json::json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "you are terse"},
                {"role": "user", "content": "first question"},
                {"role": "assistant", "content": "answer"},
                {"role": "user", "content": "why does this fail to compile?"}
            ]
        });
        assert_eq!(
            extract_prompt_text(&body, "chat"),
            "why does this fail to compile?"
        );
    }

    #[test]
    fn extract_prompt_text_chat_collapses_content_blocks() {
        let body: serde_json::Value = serde_json::json!({
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "user", "content": [
                    {"type": "text", "text": "part one"},
                    {"type": "text", "text": "part two"}
                ]}
            ]
        });
        assert_eq!(extract_prompt_text(&body, "chat"), "part one\npart two");
    }

    #[test]
    fn extract_prompt_text_messages_endpoint() {
        let body: serde_json::Value = serde_json::json!({
            "system": "be brief",
            "messages": [
                {"role": "user", "content": "q1"},
                {"role": "assistant", "content": "a1"},
                {"role": "user", "content": "q2"}
            ]
        });
        assert_eq!(extract_prompt_text(&body, "messages"), "q2");
    }

    #[test]
    fn extract_prompt_text_responses_endpoint() {
        let body: serde_json::Value = serde_json::json!({
            "instructions": "be brief",
            "input": [
                {"role": "user", "content": "old question"},
                {"role": "assistant", "content": "old answer"},
                {"role": "user", "content": [
                    {"type": "text", "text": "new question"}
                ]}
            ]
        });
        assert_eq!(extract_prompt_text(&body, "responses"), "new question");
    }

    #[test]
    fn extract_prompt_text_no_user_turn_is_empty() {
        let body: serde_json::Value =
            serde_json::json!({"messages": [{"role": "assistant", "content": "hi"}]});
        assert_eq!(extract_prompt_text(&body, "chat"), "");
        assert_eq!(extract_prompt_text(&serde_json::json!({}), "chat"), "");
    }

    #[test]
    fn extract_prompt_text_truncates_huge_prompts() {
        let huge = "x".repeat(10_000);
        let body: serde_json::Value = serde_json::json!({
            "messages": [{"role": "user", "content": huge}]
        });
        let got = extract_prompt_text(&body, "chat");
        assert_eq!(got.len(), 4096);
    }

    #[test]
    fn is_empty_choices_line_matches_empty_array() {
        assert!(is_empty_choices_line(
            b"data: {\"choices\":[],\"usage\":{}}\n"
        ));
        assert!(is_empty_choices_line(b"data: {\"choices\": []}\n"));
    }

    #[test]
    fn is_empty_choices_line_rejects_other_lines() {
        // non-empty choices
        assert!(!is_empty_choices_line(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n"
        ));
        // [DONE]
        assert!(!is_empty_choices_line(b"data: [DONE]\n"));
        // Anthropic SSE frame (no choices field)
        assert!(!is_empty_choices_line(
            b"data: {\"type\":\"message_delta\"}\n"
        ));
        // event:/empty lines
        assert!(!is_empty_choices_line(b"event: message_start\n"));
        assert!(!is_empty_choices_line(b"\n"));
    }

    #[tokio::test]
    async fn filter_empty_choices_strips_trailing_empty_frame() {
        let input = b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"total_tokens\":1}}\n\n\
data: [DONE]\n\n";

        // Split into small chunks to exercise cross-chunk line buffering.
        let chunks: Vec<std::io::Result<Bytes>> = input
            .chunks(7)
            .map(|c| Ok(Bytes::from(c.to_vec())))
            .collect();
        let filtered = filter_empty_choices(stream::iter(chunks));
        let parts: Vec<Bytes> = filtered.map(|r| r.unwrap()).collect().await;

        let mut out = Vec::new();
        for p in parts {
            out.extend_from_slice(&p);
        }
        let text = String::from_utf8(out).unwrap();

        assert!(
            !text.contains("\"choices\":[]"),
            "empty-choices frame should be stripped: {text}"
        );
        assert!(text.contains("\"content\":\"hi\""));
        assert!(text.contains("\"finish_reason\":\"stop\""));
        assert!(text.contains("[DONE]"));
    }
}
