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
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(|s| s.to_string()))
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

    // Compute the request body to send upstream, injecting stream_options if needed.
    // Only the injection case rewrites the body; otherwise we forward the original bytes.
    let injected = maybe_inject_stream_options(endpoint, body);
    let out_body: &[u8] = injected.as_deref().unwrap_or(body);

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
            out_body,
            auth_key.as_deref(),
        )
        .await
        {
            Ok(resp) => {
                let status = resp.status();
                let streaming = is_stream_response(&resp);

                if status.is_success() || status.is_redirection() {
                    svc.record_success(route_id);

                    if streaming {
                        let content_type = resp
                            .headers()
                            .get(reqwest::header::CONTENT_TYPE)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("text/event-stream")
                            .to_string();

                        let provider_id_owned = route_id.clone();
                        let endpoint_owned = endpoint.to_string();
                        let model_owned = model.clone();
                        let db = svc.core.db().clone();

                        let body = make_tapped_streaming_body(
                            resp.bytes_stream(),
                            db,
                            endpoint_owned,
                            model_owned,
                            provider_id_owned,
                            status,
                            start,
                        );

                        let mut resp_builder = Response::builder().status(status);
                        if let Some(hv) = resp_builder.headers_mut() {
                            hv.insert(
                                axum::http::header::CONTENT_TYPE,
                                axum::http::HeaderValue::from_str(&content_type).unwrap(),
                            );
                        }
                        return resp_builder.body(body).unwrap();
                    }

                    // Non-stream success: read full bytes.
                    let bytes = match resp.bytes().await {
                        Ok(b) => b,
                        Err(e) => {
                            let duration_ms = start.elapsed().as_millis() as i64;
                            log_request(
                                svc,
                                endpoint,
                                &model,
                                route_id,
                                0,
                                Usage::default(),
                                duration_ms,
                                Some(format!("read body: {e}")),
                            );
                            svc.record_failure(route_id);
                            last_err = Some((StatusCode::BAD_GATEWAY, e.to_string()));
                            continue;
                        }
                    };
                    let duration_ms = start.elapsed().as_millis() as i64;
                    let usage = Usage::from_json(&bytes);
                    log_request(
                        svc,
                        endpoint,
                        &model,
                        route_id,
                        status.as_u16() as i64,
                        usage,
                        duration_ms,
                        None,
                    );
                    return Response::builder()
                        .status(status)
                        .body(Body::from(bytes))
                        .unwrap();
                }

                if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    // Read body text for error reporting and best-effort usage.
                    let bytes = resp.bytes().await.unwrap_or_default();
                    let duration_ms = start.elapsed().as_millis() as i64;
                    let usage = Usage::from_json(&bytes);
                    log_request(
                        svc,
                        endpoint,
                        &model,
                        route_id,
                        status.as_u16() as i64,
                        usage,
                        duration_ms,
                        None,
                    );
                    svc.record_failure(route_id);
                    last_err = Some((status, String::from_utf8_lossy(&bytes).to_string()));
                    continue;
                }

                // Other 4xx: passthrough, no failover, no circuit breaker.
                let bytes = resp.bytes().await.unwrap_or_default();
                let duration_ms = start.elapsed().as_millis() as i64;
                let usage = Usage::from_json(&bytes);
                log_request(
                    svc,
                    endpoint,
                    &model,
                    route_id,
                    status.as_u16() as i64,
                    usage,
                    duration_ms,
                    None,
                );
                return Response::builder()
                    .status(status)
                    .body(Body::from(bytes))
                    .unwrap();
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                log_request(
                    svc,
                    endpoint,
                    &model,
                    route_id,
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
                    // Mark for the TailStream's completion logger.
                    *errored.lock().unwrap() = true;
                }
            }
            chunk_result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
        }
    });

    let finalized = TailStream {
        inner: Box::pin(mapped),
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

/// A stream wrapper that forwards chunks from `inner` and, when `inner` is
/// exhausted, extracts usage from the accumulated `tail` and writes a log row.
struct TailStream {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>>,
    tail: Arc<Mutex<Vec<u8>>>,
    /// Set to true when the inner stream ever yields an `Err` chunk.
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
                // If the inner stream yielded an error, record it now (the
                // stream may be aborted by axum before we ever see Ready(None)).
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
