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

fn extract_model(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("model")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
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

/// Log a request to the database. Best-effort: errors are swallowed (proxy
/// must not fail a successful forward because logging failed).
fn log_request(
    svc: &ProxyService,
    endpoint: &str,
    model: &Option<String>,
    provider_id: &str,
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
        status,
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        duration_ms,
        error,
    });
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
    let convert_path = endpoint == "messages" && target_protocol == "chat";

    // Build converted request body (Anthropic→OpenAI).
    let converted_body: Option<Vec<u8>> = if convert_path {
        match super::convert::request_anthropic_to_openai(body, model_override) {
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

    // Passthrough body with optional stream_options injection.
    let passthrough_body: Option<Vec<u8>> = maybe_inject_stream_options(endpoint, body);

    Ok(PreparedBodies {
        converted_body,
        passthrough_body,
        convert_path,
    })
}

/// Handle a successful upstream response. Returns the response to send to the client.
async fn handle_success(
    svc: &ProxyService,
    route_id: &str,
    resp: reqwest::Response,
    endpoint: &str,
    model: Option<String>,
    start: Instant,
    convert_path: bool,
    req_model: String,
) -> Response {
    let status = resp.status();
    let streaming = is_stream_response(&resp);

    if !status.is_success() && !status.is_redirection() {
        // Delegate to error handler (should not normally be called with non-success,
        // but the caller already checked status).
        return handle_upstream_error(svc, route_id, resp, endpoint, &model, start).await;
    }

    svc.record_success(route_id);

    if streaming {
        handle_success_streaming(svc, route_id, resp, endpoint, model, start, convert_path, req_model)
    } else {
        handle_success_non_streaming(svc, route_id, resp, endpoint, model, start, convert_path, req_model).await
    }
}

/// Handle successful streaming response.
fn handle_success_streaming(
    svc: &ProxyService,
    route_id: &str,
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
    let endpoint_owned = endpoint.to_string();
    let model_owned = model.clone();
    let db = svc.core.db().clone();

    let body = if convert_path {
        let upstream = resp.bytes_stream().filter_map(|r| async move { r.ok() });
        let converted = super::convert::stream_openai_to_anthropic(upstream, req_model);
        make_converted_tapped_streaming_body(
            converted,
            db,
            endpoint_owned,
            model_owned,
            provider_id_owned,
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

/// Handle successful non-streaming response.
async fn handle_success_non_streaming(
    svc: &ProxyService,
    route_id: &str,
    resp: reqwest::Response,
    endpoint: &str,
    model: Option<String>,
    start: Instant,
    convert_path: bool,
    req_model: String,
) -> Response {
    let status = resp.status();
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            let duration_ms = start.elapsed().as_millis() as i64;
            log_request(svc, endpoint, &model, route_id, 0, Usage::default(), duration_ms, Some(format!("read body: {e}")));
            svc.record_failure(route_id);
            return Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(e.to_string().into())
                .unwrap();
        }
    };

    let duration_ms = start.elapsed().as_millis() as i64;

    if convert_path {
        match super::convert::response_openai_to_anthropic(&bytes, &req_model) {
            Ok(anthropic_bytes) => {
                let usage = Usage::from_json(&anthropic_bytes);
                log_request(svc, endpoint, &model, route_id, status.as_u16() as i64, usage, duration_ms, None);
                return Response::builder()
                    .status(status)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(anthropic_bytes))
                    .unwrap();
            }
            Err(e) => {
                log_request(svc, endpoint, &model, route_id, status.as_u16() as i64, Usage::default(), duration_ms, Some(format!("convert response: {e}")));
                return Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(format!("convert response: {e}").into())
                    .unwrap();
            }
        }
    }

    let usage = Usage::from_json(&bytes);
    log_request(svc, endpoint, &model, route_id, status.as_u16() as i64, usage, duration_ms, None);
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
    route_id: &str,
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
        log_request(svc, endpoint, model, route_id, status.as_u16() as i64, usage, duration_ms, None);
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
    log_request(svc, endpoint, model, route_id, status.as_u16() as i64, usage, duration_ms, None);
    Response::builder()
        .status(status)
        .body(Body::from(bytes))
        .unwrap()
}

/// Execute the failover loop: try each route in order, skipping open circuits.
/// Returns the final response (success or last error).
async fn failover_loop(
    svc: &ProxyService,
    method: &str,
    routes: &[String],
    provider_map: &std::collections::HashMap<String, &crate::models::Provider>,
    endpoint: &str,
    model: Option<String>,
    headers: &HeaderMap,
    conv_body: Option<Vec<u8>>,
    passthru_body: Option<Vec<u8>>,
    original_body: &[u8],
    convert_path: bool,
    req_model: &str,
    start: Instant,
) -> Response {
    let mut last_err: Option<(StatusCode, String)> = None;

    for route_id in routes {
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

        let out_body: &[u8] = if convert_path {
            conv_body.as_deref().unwrap_or(original_body)
        } else {
            passthru_body.as_deref().unwrap_or(original_body)
        };
        let upstream_endpoint = if convert_path { "chat" } else { endpoint };

        match try_forward(&svc.client, method, &base_url, upstream_endpoint, headers, out_body, auth_key.as_deref()).await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() || status.is_redirection() {
                    return handle_success(svc, route_id, resp, endpoint, model, start, convert_path, req_model.to_string()).await;
                }
                if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    let _resp = handle_upstream_error(svc, route_id, resp, endpoint, &model, start).await;
                    last_err = Some((status, String::new()));
                    continue;
                }
                // Other 4xx: passthrough, stop failover.
                return handle_upstream_error(svc, route_id, resp, endpoint, &model, start).await;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                log_request(svc, endpoint, &model, route_id, 0, Usage::default(), duration_ms, Some(e.clone()));
                svc.record_failure(route_id);
                last_err = Some((StatusCode::BAD_GATEWAY, e));
                continue;
            }
        }
    }

    match last_err {
        Some((status, text)) => Response::builder().status(status).body(text.into()).unwrap(),
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
    model: Option<String>,
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
        Err(resp) => return *resp,
    };

    // Stage 2: prepare request bodies (conversion + stream_options injection).
    let PreparedBodies {
        converted_body: conv_body,
        passthrough_body: passthru_body,
        convert_path,
    } = match prepare_bodies(endpoint, &target_protocol, body, model_override.as_deref()) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };

    let req_model = model.clone().unwrap_or_default();

    // Stage 3: execute failover loop across routes.
    failover_loop(
        svc,
        method,
        &routes,
        &provider_map,
        endpoint,
        model,
        headers,
        conv_body,
        passthru_body,
        body,
        convert_path,
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
fn filter_empty_choices<S>(
    upstream: S,
) -> impl futures::Stream<Item = std::io::Result<Bytes>>
where
    S: futures::Stream<Item = std::io::Result<Bytes>> + Send + 'static,
{
    use std::collections::VecDeque;

    struct State {
        upstream:
            std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>>,
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
    status: StatusCode,
    start: Instant,
) -> Body
where
    S: futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let tail: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::with_capacity(SSE_TAIL_CAP)));
    let tail_for_stream = tail.clone();
    let errored: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let errored_for_stream = errored.clone();

    let mapped = upstream.then(move |chunk_result| {
        let tail = tail_for_stream.clone();
        let errored = errored_for_stream.clone();
        async move {
            match &chunk_result {
                Ok(chunk) => {
                    let mut t = tail.lock().unwrap();
                    t.extend_from_slice(chunk);
                    if t.len() > SSE_TAIL_CAP {
                        let excess = t.len() - SSE_TAIL_CAP;
                        t.drain(..excess);
                    }
                }
                Err(_) => {
                    *errored.lock().unwrap() = true;
                }
            }
            chunk_result.map_err(|e| std::io::Error::other(e.to_string()))
        }
    });

    // Strip new-api's trailing empty-`choices` frame from what we send to the
    // client. The tail buffer above already captured the raw bytes, so usage
    // extraction still sees the empty frame's usage fields.
    let filtered = filter_empty_choices(mapped);

    let finalized = TailStream {
        inner: Box::pin(filtered),
        tail,
        errored,
        endpoint,
        model,
        provider_id,
        db,
        status,
        start,
        done_logged: false,
    };

    Body::from_stream(finalized)
}

/// Build a streaming axum `Body` for the conversion path: the input is a stream
/// of already-converted Anthropic SSE `Bytes` frames. Each frame is forwarded
/// to the client unchanged, and a tail ring buffer is maintained so that when
/// the stream ends, usage is extracted from the converted Anthropic SSE tail
/// (`Usage::from_sse_tail` handles Anthropic events) and a request log row is
/// written.
fn make_converted_tapped_streaming_body<S>(
    converted: S,
    db: crate::store::db::Database,
    endpoint: String,
    model: Option<String>,
    provider_id: String,
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
            let mut t = tail.lock().unwrap();
            t.extend_from_slice(&chunk);
            if t.len() > SSE_TAIL_CAP {
                let excess = t.len() - SSE_TAIL_CAP;
                t.drain(..excess);
            }
            Ok::<Bytes, std::io::Error>(chunk)
        }
    });

    let finalized = ConvertedTailStream {
        inner: Box::pin(mapped),
        tail,
        endpoint,
        model,
        provider_id,
        db,
        status,
        start,
        done_logged: false,
    };

    Body::from_stream(finalized)
}

// ── Stream wrappers ────────────────────────────────────────────────────────

struct ConvertedTailStream {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>>,
    tail: Arc<Mutex<Vec<u8>>>,
    endpoint: String,
    model: Option<String>,
    provider_id: String,
    db: crate::store::db::Database,
    status: StatusCode,
    start: Instant,
    done_logged: bool,
}

impl futures::Stream for ConvertedTailStream {
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
                    let tail_bytes = this.tail.lock().unwrap().clone();
                    let usage = Usage::from_sse_tail(&this.endpoint, &tail_bytes);
                    let duration_ms = this.start.elapsed().as_millis() as i64;
                    let _ = this.db.insert_request_log(&RequestLog {
                        ts: now_rfc3339(),
                        endpoint: this.endpoint.clone(),
                        model: this.model.clone(),
                        provider_id: this.provider_id.clone(),
                        status: this.status.as_u16() as i64,
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                        duration_ms,
                        error: Some("upstream stream interrupted".to_string()),
                    });
                }
                std::task::Poll::Ready(Some(item))
            }
            std::task::Poll::Ready(None) => {
                if !this.done_logged {
                    this.done_logged = true;
                    let tail_bytes = this.tail.lock().unwrap().clone();
                    let usage = Usage::from_sse_tail(&this.endpoint, &tail_bytes);
                    let duration_ms = this.start.elapsed().as_millis() as i64;
                    let _ = this.db.insert_request_log(&RequestLog {
                        ts: now_rfc3339(),
                        endpoint: this.endpoint.clone(),
                        model: this.model.clone(),
                        provider_id: this.provider_id.clone(),
                        status: this.status.as_u16() as i64,
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                        duration_ms,
                        error: None,
                    });
                }
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

struct TailStream {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>>,
    tail: Arc<Mutex<Vec<u8>>>,
    errored: Arc<Mutex<bool>>,
    endpoint: String,
    model: Option<String>,
    provider_id: String,
    db: crate::store::db::Database,
    status: StatusCode,
    start: Instant,
    done_logged: bool,
}

impl futures::Stream for TailStream {
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
                    *this.errored.lock().unwrap() = true;
                    let tail_bytes = this.tail.lock().unwrap().clone();
                    let usage = Usage::from_sse_tail(&this.endpoint, &tail_bytes);
                    let duration_ms = this.start.elapsed().as_millis() as i64;
                    let _ = this.db.insert_request_log(&RequestLog {
                        ts: now_rfc3339(),
                        endpoint: this.endpoint.clone(),
                        model: this.model.clone(),
                        provider_id: this.provider_id.clone(),
                        status: this.status.as_u16() as i64,
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                        duration_ms,
                        error: Some("upstream stream interrupted".to_string()),
                    });
                }
                std::task::Poll::Ready(Some(item))
            }
            std::task::Poll::Ready(None) => {
                if !this.done_logged {
                    this.done_logged = true;
                    let tail_bytes = this.tail.lock().unwrap().clone();
                    let errored = *this.errored.lock().unwrap();
                    let usage = Usage::from_sse_tail(&this.endpoint, &tail_bytes);
                    let duration_ms = this.start.elapsed().as_millis() as i64;
                    let error = if errored {
                        Some("upstream stream interrupted".to_string())
                    } else {
                        None
                    };
                    let _ = this.db.insert_request_log(&RequestLog {
                        ts: now_rfc3339(),
                        endpoint: this.endpoint.clone(),
                        model: this.model.clone(),
                        provider_id: this.provider_id.clone(),
                        status: this.status.as_u16() as i64,
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                        duration_ms,
                        error,
                    });
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
    let model = extract_model(&body);
    forward_with_failover(&svc, "POST", endpoint, &headers, &body, model).await
}

pub async fn handle_get(State(svc): State<Arc<ProxyService>>, headers: HeaderMap) -> Response {
    let endpoint = "models";
    forward_with_failover(&svc, "GET", endpoint, &headers, &[], None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{stream, StreamExt};

    #[test]
    fn is_empty_choices_line_matches_empty_array() {
        assert!(is_empty_choices_line(b"data: {\"choices\":[],\"usage\":{}}\n"));
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
        assert!(!is_empty_choices_line(b"data: {\"type\":\"message_delta\"}\n"));
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
