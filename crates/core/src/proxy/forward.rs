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

use super::auth::ClientIdentity;
use super::circuit_breaker::CircuitBreaker;
use super::robustness;
use super::usage::Usage;
use super::ProxyService;
use crate::models::{Provider, ProviderProtocol, ToolKind};
use crate::store::db::RequestLog;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::Response,
};
use futures::StreamExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Inbound headers never forwarded upstream. Besides hop-by-hop headers this
/// includes every credential-carrying header a client might send: the route's
/// own key is injected afterwards, so a client credential (or a browser
/// cookie) must never reach a provider, least of all one reached via failover.
const STRIP_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "x-auth-token",
    "cookie",
    "set-cookie",
    "connection",
    "transfer-encoding",
    "anthropic-version",
    "anthropic-beta",
    // Local-only hint for handover context; must not leak a filesystem path.
    "x-xfade-project-root",
];

/// Tail ring buffer size kept while streaming to extract usage at the end (~8KB).
const SSE_TAIL_CAP: usize = 8 * 1024;
const SSE_HEAD_CAP: usize = 8 * 1024;

/// Per-upstream timeout for the aggregated `/v1/models` probe. The shared
/// client has a 120s total timeout, far too long for a model-list poll.
/// 15s: large cold catalogs (e.g. a 500+ model router with a short cache TTL)
/// can take several seconds to answer; 5s silently dropped whole routes.
const MODELS_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

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
        if endpoint == "messages" || endpoint == "/v1/messages" {
            req = req.header("x-api-key", key);
            req = req.header("anthropic-version", "2023-06-01");
        }
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

/// Extract the domain/host from a URL string.
fn extract_domain(url_str: &str) -> String {
    if let Ok(url) = reqwest::Url::parse(url_str) {
        url.host_str().unwrap_or("unknown").to_string()
    } else {
        url_str
            .split("://")
            .nth(1)
            .unwrap_or(url_str)
            .split('/')
            .next()
            .unwrap_or("unknown")
            .split(':')
            .next()
            .unwrap_or("unknown")
            .to_string()
    }
}

/// Compute a privacy-safe key fingerprint for audit logging (never logs raw secret).
fn key_fingerprint(provider: &crate::models::Provider, key_opt: Option<&str>) -> String {
    match key_opt {
        Some(k) if !k.is_empty() => {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(k.as_bytes());
            let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
            format!("ref:{}#sha256:{}", provider.id, hex)
        }
        _ => format!("ref:{}#none", provider.id),
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
    log_request_full(
        svc,
        session_id,
        endpoint,
        model,
        provider_id,
        tool,
        status,
        usage,
        duration_ms,
        error,
        None,
        None,
    );
}

fn log_request_full(
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
    explicit_domain: Option<&str>,
    failover_path: Option<&str>,
) {
    let cost_usd_micros = crate::proxy::pricing::estimate_cost_with_db(
        Some(svc.core.db()),
        model.as_deref(),
        usage.prompt_tokens,
        usage.completion_tokens,
        usage.cached_prompt_tokens,
        usage.cache_write_tokens,
    );
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
        error: error.clone(),
        session_id: session_id.map(|s| s.to_string()),
        cost_usd_micros,
    });
    crate::sessions::record_outcome(
        svc.core.db(),
        session_id,
        tool,
        provider_id,
        model.as_deref(),
        usage.prompt_tokens,
        usage.completion_tokens,
        usage.cached_prompt_tokens,
        usage.cache_write_tokens,
        status != 0 && status < 400,
    );

    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let (upstream_domain, key_ref) = if let Some(p) = tool
        .parse::<ToolKind>()
        .ok()
        .and_then(|t| svc.core.db().get(t, provider_id).ok().flatten())
    {
        let domain = explicit_domain
            .map(|d| d.to_string())
            .unwrap_or_else(|| extract_domain(p.base_url.as_deref().unwrap_or("")));
        let key_opt = svc.core.secrets().get(&p.key_ref).ok();
        (domain, key_fingerprint(&p, key_opt.as_deref()))
    } else {
        let domain = explicit_domain.map(|d| d.to_string()).unwrap_or_else(|| {
            if let Some(err) = &error {
                if let Some(rest) = err.strip_prefix("outbound domain '") {
                    rest.split('\'').next().unwrap_or("unknown").to_string()
                } else {
                    "unknown".to_string()
                }
            } else {
                "unknown".to_string()
            }
        });
        (domain, format!("ref:{provider_id}#unknown"))
    };

    let _ = svc.core.db().insert_audit_log(&crate::store::db::AuditLog {
        id: 0,
        ts: now_ts,
        client_tool: tool.to_string(),
        client_token_id: format!("token:{tool}"),
        provider_id: provider_id.to_string(),
        upstream_domain,
        upstream_model: model.clone().unwrap_or_default(),
        key_ref,
        status_code: status as u16,
        duration_ms: duration_ms as u64,
        prompt_tokens: usage.prompt_tokens as u64,
        completion_tokens: usage.completion_tokens as u64,
        failover_path: failover_path.map(|s| s.to_string()),
    });
}

// ── Forward pipeline stages ────────────────────────────────────────────────

/// Resolved route config + provider map (result of `resolve_routes`).
struct ResolvedRoutes<'a> {
    routes: Vec<String>,
    model_override: Option<String>,
    target_protocol: String,
    provider_map: std::collections::HashMap<String, &'a crate::models::Provider>,
    /// Per-provider model overrides (`--route-model id=model`); empty when
    /// only the global `--model` fallback is configured.
    route_models: std::collections::BTreeMap<String, String>,
    /// Whether qualifying 4xx responses should trigger failover.
    failover_4xx: bool,
}

#[cfg(test)]
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
    client_tool: Option<ToolKind>,
) -> Result<ResolvedRoutes<'a>, Box<Response>> {
    let tool_routes = match client_tool {
        Some(tool) => svc.core.db().get_tool_routes(tool.as_str()).ok().flatten(),
        None => None,
    };
    let (routes, model_override, target_protocol) = match tool_routes {
        Some(r) if !r.0.is_empty() => r,
        _ => match svc.core.db().get_routes() {
            Ok(Some(r)) if !r.0.is_empty() => r,
            _ => {
                return Err(Box::new(
                    Response::builder()
                        .status(StatusCode::SERVICE_UNAVAILABLE)
                        .body("no routes configured; use `xfade proxy use <name>`".into())
                        .unwrap(),
                ));
            }
        },
    };
    let provider_map: std::collections::HashMap<String, &crate::models::Provider> =
        providers.iter().map(|p| (p.id.clone(), p)).collect();
    // These two are independent config toggles read per request (same
    // pattern as `smart_enabled()` via `active_router()`), so CLI changes
    // take effect on a running daemon immediately.
    let route_models = svc.core.db().get_route_models().unwrap_or_default();
    let failover_4xx = svc.core.db().get_failover_4xx().unwrap_or(false);
    Ok(ResolvedRoutes {
        routes,
        model_override,
        target_protocol,
        provider_map,
        route_models,
        failover_4xx,
    })
}

#[cfg(test)]
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

/// Map client inbound endpoint string to its corresponding ProviderProtocol.
pub fn endpoint_to_inbound_protocol(endpoint: &str) -> ProviderProtocol {
    match endpoint {
        "messages" | "/v1/messages" => ProviderProtocol::Messages,
        "responses" | "/v1/responses" => ProviderProtocol::Responses,
        _ => ProviderProtocol::Chat,
    }
}

/// Prepare the request body and determine target endpoint for a specific provider.
/// Resolves differences between client inbound protocol and provider outbound protocol.
pub fn prepare_body_for_provider(
    endpoint: &str,
    provider: &Provider,
    original_body: &[u8],
    model_override: Option<&str>,
) -> Result<(&'static str, Vec<u8>), Box<Response>> {
    if endpoint == "models" {
        return Ok(("models", original_body.to_vec()));
    }

    let inbound_proto = endpoint_to_inbound_protocol(endpoint);
    let outbound_proto = provider.protocol;

    let target_endpoint = match outbound_proto {
        ProviderProtocol::Chat => "chat",
        ProviderProtocol::Messages => "messages",
        ProviderProtocol::Responses => "responses",
    };

    if inbound_proto == outbound_proto {
        let transformed = transform_passthrough_body(endpoint, original_body, model_override)
            .unwrap_or_else(|| original_body.to_vec());
        return Ok((target_endpoint, transformed));
    }

    // Heterogeneous transcoding: Canonical Chat IR hub-and-spoke
    let converted: crate::error::Result<Vec<u8>> = match (inbound_proto, outbound_proto) {
        (ProviderProtocol::Messages, ProviderProtocol::Chat) => {
            super::convert::request_anthropic_to_openai(original_body, model_override)
                .map(|b| maybe_inject_stream_options("chat", &b).unwrap_or_else(|| b.to_vec()))
        }
        (ProviderProtocol::Messages, ProviderProtocol::Responses) => {
            super::convert::request_anthropic_to_openai(original_body, None)
                .and_then(|chat_bytes| {
                    super::convert::request_chat_to_responses(&chat_bytes, model_override)
                })
                .map(|b| b.to_vec())
        }
        (ProviderProtocol::Responses, ProviderProtocol::Chat) => {
            super::convert::request_responses_to_chat(original_body, model_override)
                .map(|b| maybe_inject_stream_options("chat", &b).unwrap_or_else(|| b.to_vec()))
        }
        (ProviderProtocol::Responses, ProviderProtocol::Messages) => {
            super::convert::request_responses_to_chat(original_body, None)
                .and_then(|chat_bytes| {
                    super::convert::request_chat_to_anthropic(&chat_bytes, model_override)
                })
                .map(|b| b.to_vec())
        }
        (ProviderProtocol::Chat, ProviderProtocol::Messages) => {
            super::convert::request_chat_to_anthropic(original_body, model_override)
                .map(|b| b.to_vec())
        }
        (ProviderProtocol::Chat, ProviderProtocol::Responses) => {
            super::convert::request_chat_to_responses(original_body, model_override)
                .map(|b| b.to_vec())
        }
        _ => unreachable!(),
    };

    match converted {
        Ok(body) => Ok((target_endpoint, body)),
        Err(e) => Err(Box::new(
            Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(format!("convert request: {e}").into())
                .unwrap(),
        )),
    }
}

fn make_robust_stream<S>(
    stream: S,
    idle_timeout: Duration,
    inbound_proto: ProviderProtocol,
    req_id: String,
    has_tool_call: bool,
    provider_id: String,
    circuit_breaker: Arc<super::CircuitBreaker>,
    db: crate::store::db::Database,
    interrupted_flag: Arc<std::sync::atomic::AtomicBool>,
) -> impl futures::Stream<Item = std::io::Result<Bytes>> + Send + 'static
where
    S: futures::Stream<Item = std::io::Result<Bytes>> + Send + 'static,
{
    struct State {
        stream: std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>>,
        idle_timeout: Duration,
        inbound_proto: ProviderProtocol,
        req_id: String,
        has_tool_call: bool,
        provider_id: String,
        circuit_breaker: Arc<super::CircuitBreaker>,
        db: crate::store::db::Database,
        interrupted_flag: Arc<std::sync::atomic::AtomicBool>,
        emitted_interrupted: bool,
    }

    let init = State {
        stream: Box::pin(stream),
        idle_timeout,
        inbound_proto,
        req_id,
        has_tool_call,
        provider_id,
        circuit_breaker,
        db,
        interrupted_flag,
        emitted_interrupted: false,
    };

    futures::stream::unfold(init, |mut st| async move {
        if st.emitted_interrupted {
            return None;
        }

        match tokio::time::timeout(st.idle_timeout, st.stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                let text = String::from_utf8_lossy(&chunk);
                if !st.has_tool_call
                    && (text.contains("tool_calls")
                        || text.contains("tool_use")
                        || text.contains("function_call"))
                {
                    st.has_tool_call = true;
                }
                Some((Ok(chunk), st))
            }
            Ok(Some(Err(_err))) => {
                st.emitted_interrupted = true;
                st.interrupted_flag
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                ProxyService::record_failure_static(&st.circuit_breaker, &st.db, &st.provider_id);
                let interrupted = robustness::synthesize_stream_interrupted(
                    st.inbound_proto,
                    &st.req_id,
                    st.has_tool_call,
                );
                Some((Ok(interrupted), st))
            }
            Ok(None) => None,
            Err(_timeout_elapsed) => {
                st.emitted_interrupted = true;
                st.interrupted_flag
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                ProxyService::record_failure_static(&st.circuit_breaker, &st.db, &st.provider_id);
                let interrupted = robustness::synthesize_stream_interrupted(
                    st.inbound_proto,
                    &st.req_id,
                    st.has_tool_call,
                );
                Some((Ok(interrupted), st))
            }
        }
    })
}

/// Handle committed streaming response with buffering flusher and post-commit idle/interruption wrapper.
fn handle_success_streaming_committed<S>(
    svc: &ProxyService,
    session_id: Option<&str>,
    route_id: &str,
    tool: &str,
    status: StatusCode,
    resp_headers: &reqwest::header::HeaderMap,
    buffered_chunks: Vec<Bytes>,
    raw_stream: S,
    has_tool_call: bool,
    endpoint: &str,
    model: Option<String>,
    start: Instant,
    inbound_proto: ProviderProtocol,
    outbound_proto: ProviderProtocol,
    req_model: String,
    failover_path: Option<&str>,
    req_id: String,
) -> Response
where
    S: futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let is_convert = inbound_proto != outbound_proto;
    let content_type = if is_convert {
        "text/event-stream".to_string()
    } else {
        resp_headers
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
    let cb = svc.circuit_breaker.clone();

    let (upstream_domain, key_ref) = if let Some(p) = tool
        .parse::<ToolKind>()
        .ok()
        .and_then(|t| svc.core.db().get(t, route_id).ok().flatten())
    {
        let domain = extract_domain(p.base_url.as_deref().unwrap_or(""));
        let key_opt = svc.core.secrets().get(&p.key_ref).ok();
        (domain, key_fingerprint(&p, key_opt.as_deref()))
    } else {
        ("unknown".to_string(), format!("ref:{route_id}#unknown"))
    };
    let failover_path_owned = failover_path.map(|s| s.to_string());

    let head: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::with_capacity(SSE_HEAD_CAP)));
    let tail: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::with_capacity(SSE_TAIL_CAP)));
    let head_for_stream = head.clone();
    let tail_for_stream = tail.clone();

    let raw_chained = futures::stream::iter(buffered_chunks.into_iter().map(Ok))
        .chain(raw_stream.map(|r| r.map_err(|e| std::io::Error::other(e.to_string()))));

    let upstream = raw_chained.then(move |chunk_result| {
        let head = head_for_stream.clone();
        let tail = tail_for_stream.clone();
        async move {
            if let Ok(chunk) = &chunk_result {
                push_head_and_tail(&head, &tail, chunk);
            }
            chunk_result
        }
    });

    let proto_stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>,
    > = if is_convert {
        // Step 1: Outbound -> Canonical Chat IR stream
        let chat_stream: std::pin::Pin<
            Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>,
        > = match outbound_proto {
            ProviderProtocol::Chat => Box::pin(upstream),
            ProviderProtocol::Messages => Box::pin(super::convert::stream_anthropic_to_chat(
                upstream,
                req_model.clone(),
            )),
            ProviderProtocol::Responses => Box::pin(super::convert::stream_responses_to_chat(
                upstream,
                req_model.clone(),
            )),
        };

        // Step 2: Canonical Chat IR stream -> Inbound stream
        match inbound_proto {
            ProviderProtocol::Chat => Box::pin(chat_stream),
            ProviderProtocol::Messages => Box::pin(super::convert::stream_openai_to_anthropic(
                chat_stream,
                req_model,
            )),
            ProviderProtocol::Responses => Box::pin(super::convert::stream_chat_to_responses(
                chat_stream,
                req_model,
            )),
        }
    } else {
        Box::pin(filter_empty_choices(upstream))
    };

    let interrupted_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let robust = make_robust_stream(
        proto_stream,
        svc.timeouts.idle_timeout,
        inbound_proto,
        req_id,
        has_tool_call,
        provider_id_owned.clone(),
        cb.clone(),
        db.clone(),
        interrupted_flag.clone(),
    );

    let body = make_tapped_streaming_body_with_flag(
        robust,
        head,
        tail,
        db,
        endpoint_owned,
        model_owned,
        provider_id_owned,
        tool_owned,
        session_owned,
        status,
        start,
        key_ref,
        upstream_domain,
        failover_path_owned,
        interrupted_flag,
        cb,
    );

    let mut resp_builder = Response::builder().status(status);
    if let Some(hv) = resp_builder.headers_mut() {
        hv.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_str(&content_type).unwrap(),
        );
        if let Some(path_json) = failover_path {
            hv.insert(
                axum::http::header::HeaderName::from_static("x-xfade-failover-path"),
                axum::http::HeaderValue::from_str(path_json).unwrap(),
            );
        }
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

/// Handle successful non-streaming response from verified pre-commit bytes.
async fn handle_success_non_streaming(
    svc: &ProxyService,
    session_id: Option<&str>,
    route_id: &str,
    tool: &str,
    status: StatusCode,
    bytes: Bytes,
    endpoint: &str,
    model: Option<String>,
    start: Instant,
    inbound_proto: ProviderProtocol,
    outbound_proto: ProviderProtocol,
    req_model: String,
    failover_path: Option<&str>,
) -> Response {
    let duration_ms = start.elapsed().as_millis() as i64;

    if inbound_proto != outbound_proto {
        // Step 1: Outbound -> Canonical Chat IR
        let chat_res: crate::error::Result<Bytes> = match outbound_proto {
            ProviderProtocol::Chat => Ok(bytes),
            ProviderProtocol::Messages => {
                super::convert::response_anthropic_to_chat(&bytes, &req_model)
            }
            ProviderProtocol::Responses => {
                super::convert::response_responses_to_chat(&bytes, &req_model)
            }
        };

        // Step 2: Canonical Chat IR -> Inbound
        let final_res: crate::error::Result<Bytes> = match chat_res {
            Ok(chat_bytes) => match inbound_proto {
                ProviderProtocol::Chat => Ok(chat_bytes),
                ProviderProtocol::Messages => {
                    super::convert::response_openai_to_anthropic(&chat_bytes, &req_model)
                }
                ProviderProtocol::Responses => {
                    super::convert::response_chat_to_responses(&chat_bytes, &req_model)
                }
            },
            Err(e) => Err(e),
        };

        match final_res {
            Ok(converted_bytes) => {
                let usage = Usage::from_json(&converted_bytes);
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
                    .body(Body::from(converted_bytes))
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
    log_request_full(
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
        None,
        failover_path,
    );
    let mut builder = Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, "application/json");
    if let Some(path_json) = failover_path {
        builder = builder.header(
            axum::http::header::HeaderName::from_static("x-xfade-failover-path"),
            axum::http::HeaderValue::from_str(path_json).unwrap(),
        );
    }
    builder.body(Body::from(bytes)).unwrap()
}

/// Whether a 4xx response is a *transient* upstream problem worth failing
/// over on — deliberately narrow, so a bad request from the client (400
/// validation, 404, 422…) is still returned immediately instead of being
/// retried against every fallback with the same broken payload.
///
/// Qualifying:
/// - 401 / 403: a key/permission problem that may be specific to one
///   upstream (e.g. "No active credentials for provider").
/// - 400 only when it looks like a model-rejection from the gateway
///   (`model_not_allowed` / "model … not in/not allowed/not found"), which
///   is exactly the heterogeneous-upstream case `--route-model` addresses.
fn qualifies_for_4xx_failover(status: StatusCode, body: &[u8]) -> bool {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => true,
        StatusCode::BAD_REQUEST => {
            let text = String::from_utf8_lossy(body).to_ascii_lowercase();
            text.contains("model_not_allowed")
                || (text.contains("model")
                    && (text.contains("not allowed")
                        || text.contains("not in")
                        || text.contains("not found")))
        }
        _ => false,
    }
}

/// Handle an upstream error response. Returns `(response, should_continue)`:
/// `should_continue` is true when the failover loop should try the next
/// route (429/5xx always; a qualifying 4xx only when `failover_4xx` is on).
/// A continuing route is also recorded as a circuit-breaker failure; a
/// passthrough 4xx is not.
async fn handle_upstream_error(
    svc: &ProxyService,
    session_id: Option<&str>,
    route_id: &str,
    tool: &str,
    resp: reqwest::Response,
    endpoint: &str,
    model: &Option<String>,
    start: Instant,
    failover_4xx: bool,
) -> (Response, bool) {
    let status = resp.status();
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

    let should_continue = status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
        || (failover_4xx && qualifies_for_4xx_failover(status, &bytes));

    if should_continue {
        svc.record_failure(route_id);
    }

    let response = Response::builder()
        .status(status)
        .body(Body::from(bytes))
        .unwrap();
    (response, should_continue)
}

/// Fold a pipeline-level failure (no routes, bad conversion, all routes
/// exhausted) into the session so it doesn't stay "running" forever.
fn session_failed(svc: &ProxyService, session_id: Option<&str>) {
    if let Some(sid) = session_id {
        crate::sessions::record_outcome(svc.core.db(), Some(sid), "", "", None, 0, 0, 0, 0, false);
    }
}

type BodiesCache =
    std::collections::HashMap<(ProviderProtocol, Option<String>), (&'static str, Vec<u8>)>;

/// RAII guard for a circuit-breaker probe reservation.
///
/// `CircuitBreaker::allow_request` is a *stateful* admission gate: in `Open`
/// (cooldown elapsed) or `HalfOpen` it claims the single `probe_in_flight`
/// slot. If the request is then skipped (missing provider, missing base URL,
/// client disconnect) without an explicit `record_success` / `record_failure`
/// / `release_probe`, the slot leaks and the route is permanently rejected
/// with `503 no usable provider`. The guard returns the slot on every early
/// `continue` / `return` / future-drop path; call `disarm()` right before the
/// explicit release points that already free the slot.
struct ProbeGuard<'a> {
    cb: &'a CircuitBreaker,
    provider_id: &'a str,
    armed: bool,
}

impl<'a> ProbeGuard<'a> {
    fn new(cb: &'a CircuitBreaker, provider_id: &'a str) -> Self {
        Self {
            cb,
            provider_id,
            armed: true,
        }
    }

    /// The caller has already released the probe slot (via `record_success`,
    /// `record_failure`, or `release_probe`); don't release it again on drop.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.cb.release_probe(self.provider_id);
        }
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
    _target_protocol: &str,
    model: Option<String>,
    headers: &HeaderMap,
    original_body: &[u8],
    req_model: &str,
    start: Instant,
    failover_4xx: bool,
) -> Response {
    let inbound_proto = endpoint_to_inbound_protocol(endpoint);
    let mut last_err: Option<(StatusCode, String, Option<axum::http::HeaderValue>)> = None;
    let mut failover_trajectory: Vec<serde_json::Value> = Vec::new();
    // Per-route body preparation is memoized by (protocol, model_override): adjacent
    // entries sharing protocol and override do not re-run full conversion.
    let mut bodies_cache: BodiesCache = std::collections::HashMap::new();

    for entry in entries {
        let route_id = entry.route_id.as_str();
        if !svc.circuit_breaker.allow_request(route_id) {
            continue;
        }

        // Hold the probe reservation (if any) until an explicit outcome records
        // or the request is skipped/dropped, so a HalfOpen probe slot can never
        // leak and permanently reject the route with `503 no usable provider`.
        let mut probe_guard = ProbeGuard::new(svc.circuit_breaker.as_ref(), route_id);

        let provider = match provider_map.get(route_id) {
            Some(p) => *p,
            None => continue,
        };

        let effective_provider;
        let provider = if provider.protocol == ProviderProtocol::Chat {
            if _target_protocol == "messages" || _target_protocol == "anthropic" {
                effective_provider = (*provider)
                    .clone()
                    .with_protocol(ProviderProtocol::Messages);
                &effective_provider
            } else if _target_protocol == "responses" || _target_protocol == "codex" {
                effective_provider = (*provider)
                    .clone()
                    .with_protocol(ProviderProtocol::Responses);
                &effective_provider
            } else {
                provider
            }
        } else {
            provider
        };

        let base_url = match &provider.base_url {
            Some(u) => u.clone(),
            None => continue,
        };

        if let Ok(allowed) = svc.core.is_domain_allowed(&base_url) {
            if !allowed {
                let duration_ms = start.elapsed().as_millis() as i64;
                let target_domain = extract_domain(&base_url);
                let err_msg = format!("outbound domain '{target_domain}' blocked by egress policy");
                failover_trajectory.push(serde_json::json!({
                    "provider": route_id,
                    "status": 403,
                    "error": "xfade_egress_blocked"
                }));
                log_request_full(
                    svc,
                    session_id,
                    endpoint,
                    &model,
                    route_id,
                    provider.tool.as_str(),
                    403,
                    Usage::default(),
                    duration_ms,
                    Some(err_msg.clone()),
                    Some(&target_domain),
                    None,
                );
                probe_guard.disarm();
                svc.record_failure(route_id);
                last_err = Some((
                    StatusCode::FORBIDDEN,
                    format!(
                        "{{\"error\":\"xfade_egress_blocked\",\"domain\":\"{target_domain}\"}}"
                    ),
                    None,
                ));
                continue;
            }
        }

        let auth_key = if provider.is_official() {
            None
        } else {
            svc.core.secrets().get(&provider.key_ref).ok()
        };

        // Per-route body preparation: each entry may carry its own protocol and model
        // override (Smart Fader per-tier models). Bodies are cached per (protocol, override).
        let cache_key = (provider.protocol, entry.model_override.clone());
        let (upstream_endpoint, sending_body_ref) = match bodies_cache.get(&cache_key) {
            Some((ep, b)) => (*ep, b.as_slice()),
            None => {
                let prepared = match prepare_body_for_provider(
                    endpoint,
                    provider,
                    original_body,
                    entry.model_override.as_deref(),
                ) {
                    Ok(r) => r,
                    Err(resp) => {
                        session_failed(svc, session_id);
                        return *resp;
                    }
                };
                let (cached_ep, cached_b) = bodies_cache.entry(cache_key).or_insert(prepared);
                (*cached_ep, cached_b.as_slice())
            }
        };

        // When target endpoint is Anthropic (/v1/messages) or target protocol is Anthropic,
        // run cache_injector::inject_cache_control on the payload before sending if it's JSON.
        let is_anthropic_target = upstream_endpoint == "messages"
            || upstream_endpoint == "/v1/messages"
            || provider.protocol == ProviderProtocol::Messages;

        let mut injected_body_vec: Option<Vec<u8>> = None;
        if is_anthropic_target {
            if let Ok(mut json_val) = serde_json::from_slice::<serde_json::Value>(sending_body_ref)
            {
                let report = super::cache_injector::inject_cache_control(&mut json_val, 4);
                if report.injected_breakpoints > 0 {
                    if let Ok(bytes) = serde_json::to_vec(&json_val) {
                        injected_body_vec = Some(bytes);
                    }
                }
            }
        }
        let sending_body: &[u8] = injected_body_vec.as_deref().unwrap_or(sending_body_ref);

        let ttft_deadline = tokio::time::Instant::now() + svc.timeouts.ttft_timeout;
        let time_for_headers = ttft_deadline.saturating_duration_since(tokio::time::Instant::now());

        let mut resp = match tokio::time::timeout(
            time_for_headers,
            try_forward(
                &svc.client,
                method,
                &base_url,
                upstream_endpoint,
                headers,
                sending_body,
                auth_key.as_deref(),
            ),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                failover_trajectory.push(serde_json::json!({
                    "provider": route_id,
                    "status": 502,
                    "error": &e
                }));
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
                probe_guard.disarm();
                svc.record_failure(route_id);
                last_err = Some((StatusCode::BAD_GATEWAY, e, None));
                continue;
            }
            Err(_timeout) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                failover_trajectory.push(serde_json::json!({
                    "provider": route_id,
                    "status": 504,
                    "error": "ttft_timeout"
                }));
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
                    Some("ttft_timeout".to_string()),
                );
                probe_guard.disarm();
                svc.record_failure(route_id);
                last_err = Some((
                    StatusCode::GATEWAY_TIMEOUT,
                    "ttft_timeout".to_string(),
                    None,
                ));
                continue;
            }
        };

        // Intra-upstream retry on 429/503 if Retry-After <= 2s (and bounded by remaining TTFT deadline)
        if resp.status() == StatusCode::TOO_MANY_REQUESTS
            || resp.status() == StatusCode::SERVICE_UNAVAILABLE
        {
            if let Some(delay) = robustness::parse_retry_after(resp.headers()) {
                let now = tokio::time::Instant::now();
                if delay <= Duration::from_secs(2) && now + delay < ttft_deadline {
                    tokio::time::sleep(delay).await;
                    let remaining =
                        ttft_deadline.saturating_duration_since(tokio::time::Instant::now());
                    if let Ok(Ok(retry_resp)) = tokio::time::timeout(
                        remaining,
                        try_forward(
                            &svc.client,
                            method,
                            &base_url,
                            upstream_endpoint,
                            headers,
                            sending_body,
                            auth_key.as_deref(),
                        ),
                    )
                    .await
                    {
                        resp = retry_resp;
                    }
                }
            }
        }

        // If response is 400 Bad Request, attempt self-healing parameter rectification
        if resp.status() == StatusCode::BAD_REQUEST {
            let err_bytes = resp.bytes().await.unwrap_or_default();
            let err_body = String::from_utf8_lossy(&err_bytes);
            let mut rectified_bytes = None;
            if let Ok(mut body_json) = serde_json::from_slice::<serde_json::Value>(sending_body) {
                if super::thinking_rectifier::rectify_thinking_error(&mut body_json, 400, &err_body)
                {
                    if let Ok(new_bytes) = serde_json::to_vec(&body_json) {
                        rectified_bytes = Some(new_bytes);
                    }
                }
            }

            if let Some(new_body) = rectified_bytes {
                // Immediately retry on current provider!
                match try_forward(
                    &svc.client,
                    method,
                    &base_url,
                    upstream_endpoint,
                    headers,
                    &new_body,
                    auth_key.as_deref(),
                )
                .await
                {
                    Ok(retry_resp) => {
                        resp = retry_resp;
                    }
                    Err(e) => {
                        let duration_ms = start.elapsed().as_millis() as i64;
                        failover_trajectory.push(serde_json::json!({
                            "provider": route_id,
                            "status": 502,
                            "error": &e
                        }));
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
                        probe_guard.disarm();
                        svc.record_failure(route_id);
                        last_err = Some((StatusCode::BAD_GATEWAY, e, None));
                        continue;
                    }
                }
            } else {
                // Not rectified: handle this 400 error.
                let duration_ms = start.elapsed().as_millis() as i64;
                let usage = Usage::from_json(&err_bytes);
                log_request(
                    svc,
                    session_id,
                    endpoint,
                    &model,
                    route_id,
                    provider.tool.as_str(),
                    400,
                    usage,
                    duration_ms,
                    None,
                );
                let should_continue =
                    failover_4xx && qualifies_for_4xx_failover(StatusCode::BAD_REQUEST, &err_bytes);
                if should_continue {
                    failover_trajectory.push(serde_json::json!({
                        "provider": route_id,
                        "status": 400,
                        "error": "model_rejection"
                    }));
                    probe_guard.disarm();
                    svc.record_failure(route_id);
                    let transcoded = super::convert::transcode_error_body(
                        &err_bytes,
                        provider.protocol,
                        inbound_proto,
                    );
                    let text = String::from_utf8_lossy(&transcoded).to_string();
                    last_err = Some((StatusCode::BAD_REQUEST, text, None));
                    continue;
                }
                // Release in-flight probe reservation so HalfOpen is not permanently blocked
                probe_guard.disarm();
                svc.circuit_breaker.release_probe(route_id);
                let out_bytes = if inbound_proto != provider.protocol {
                    super::convert::transcode_error_body(
                        &err_bytes,
                        provider.protocol,
                        inbound_proto,
                    )
                } else {
                    err_bytes.to_vec()
                };
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(out_bytes))
                    .unwrap();
            }
        }

        let status = resp.status();
        if status.is_success() || status.is_redirection() {
            let is_stream = is_stream_response(&resp);
            let resp_headers = resp.headers().clone();
            if !is_stream {
                let content_type = resp_headers
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_lowercase();
                // Non-streaming pre-commit gate: read body and inspect with total timeout
                let bytes =
                    match tokio::time::timeout(svc.timeouts.total_non_stream_timeout, resp.bytes())
                        .await
                    {
                        Ok(Ok(b)) => b,
                        Ok(Err(e)) => {
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
                                Some(format!("read body: {e}")),
                            );
                            probe_guard.disarm();
                            svc.record_failure(route_id);
                            failover_trajectory.push(serde_json::json!({
                                "provider": route_id,
                                "status": 502,
                                "error": format!("read body: {e}"),
                            }));
                            last_err = Some((StatusCode::BAD_GATEWAY, e.to_string(), None));
                            continue;
                        }
                        Err(_timeout) => {
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
                                Some("read body timeout".to_string()),
                            );
                            probe_guard.disarm();
                            svc.record_failure(route_id);
                            failover_trajectory.push(serde_json::json!({
                                "provider": route_id,
                                "status": 504,
                                "error": "read body timeout",
                            }));
                            last_err = Some((
                                StatusCode::GATEWAY_TIMEOUT,
                                "read body timeout".to_string(),
                                None,
                            ));
                            continue;
                        }
                    };

                let is_valid_json = serde_json::from_slice::<serde_json::Value>(&bytes).is_ok();
                if (!content_type.contains("json") && !body_looks_like_json(&bytes))
                    || !is_valid_json
                {
                    let msg = format!(
                        "xfade: upstream provider '{route_id}' returned a non-JSON response \
                         (content-type: '{content_type}'); its base_url is probably wrong — \
                         OpenAI/Anthropic-compatible upstreams usually need a /v1 path suffix"
                    );
                    let duration_ms = start.elapsed().as_millis() as i64;
                    log_request(
                        svc,
                        session_id,
                        endpoint,
                        &model,
                        route_id,
                        provider.tool.as_str(),
                        StatusCode::BAD_GATEWAY.as_u16() as i64,
                        Usage::default(),
                        duration_ms,
                        Some(msg.clone()),
                    );
                    probe_guard.disarm();
                    svc.record_failure(route_id);
                    failover_trajectory.push(serde_json::json!({
                        "provider": route_id,
                        "status": 502,
                        "error": "non_json_response",
                    }));
                    last_err = Some((StatusCode::BAD_GATEWAY, msg, None));
                    continue;
                }

                if let Some(err_msg) = robustness::SemanticDetector::is_error_payload(&bytes) {
                    let duration_ms = start.elapsed().as_millis() as i64;
                    log_request(
                        svc,
                        session_id,
                        endpoint,
                        &model,
                        route_id,
                        provider.tool.as_str(),
                        StatusCode::BAD_GATEWAY.as_u16() as i64,
                        Usage::default(),
                        duration_ms,
                        Some(format!("upstream 200 error payload: {err_msg}")),
                    );
                    probe_guard.disarm();
                    svc.record_failure(route_id);
                    failover_trajectory.push(serde_json::json!({
                        "provider": route_id,
                        "status": 200,
                        "error": format!("error_payload: {err_msg}"),
                    }));
                    let is_rate_limit = err_msg.to_lowercase().contains("rate")
                        || err_msg.to_lowercase().contains("quota")
                        || err_msg.to_lowercase().contains("insufficient");
                    let mapped_status = if is_rate_limit {
                        StatusCode::TOO_MANY_REQUESTS
                    } else {
                        StatusCode::BAD_GATEWAY
                    };
                    last_err = Some((
                        mapped_status,
                        format!(
                            "{{\"error\":{{\"code\":\"upstream_error\",\"message\":{}}}}}",
                            serde_json::to_string(&err_msg)
                                .unwrap_or_else(|_| "\"upstream error\"".to_string())
                        ),
                        None,
                    ));
                    continue;
                }

                // Verified non-streaming response: commit!
                probe_guard.disarm();
                svc.record_success(route_id);
                failover_trajectory.push(serde_json::json!({
                    "provider": route_id,
                    "status": status.as_u16(),
                }));
                let failover_path_json = if failover_trajectory.len() > 1 {
                    serde_json::to_string(&failover_trajectory).ok()
                } else {
                    None
                };
                let effective_model = entry.model_override.clone().or_else(|| model.clone());
                return handle_success_non_streaming(
                    svc,
                    session_id,
                    route_id,
                    provider.tool.as_str(),
                    status,
                    bytes,
                    endpoint,
                    effective_model,
                    start,
                    inbound_proto,
                    provider.protocol,
                    req_model.to_string(),
                    failover_path_json.as_deref(),
                )
                .await;
            } else {
                // Streaming pre-commit gate: buffer initial chunks up to TTFT timeout
                const MAX_PRE_COMMIT_BUFFER_BYTES: usize = 2 * 1024 * 1024; // 2MB
                let mut raw_stream = resp.bytes_stream();
                let mut buffered_chunks: Vec<Bytes> = Vec::new();
                let mut buffered_bytes: usize = 0;
                let mut accumulated_buf: Vec<u8> = Vec::new();
                let mut has_tool_call = false;
                let mut is_committed = false;
                let mut stream_err: Option<String> = None;

                while !is_committed {
                    let now = tokio::time::Instant::now();
                    if now >= ttft_deadline {
                        stream_err = Some("ttft_timeout".to_string());
                        break;
                    }
                    let remaining = ttft_deadline - now;
                    match tokio::time::timeout(remaining, raw_stream.next()).await {
                        Ok(Some(Ok(chunk))) => {
                            buffered_bytes += chunk.len();
                            if buffered_bytes > MAX_PRE_COMMIT_BUFFER_BYTES {
                                stream_err = Some("pre_commit_buffer_overflow".to_string());
                                break;
                            }
                            accumulated_buf.extend_from_slice(&chunk);
                            let decision = robustness::SemanticDetector::inspect_chunk(
                                &accumulated_buf,
                                provider.protocol,
                            );
                            match decision {
                                robustness::CommitDecision::Error(e) => {
                                    stream_err = Some(format!("upstream_stream_error: {e}"));
                                    break;
                                }
                                robustness::CommitDecision::Commit { has_tool_call: tc } => {
                                    has_tool_call = tc;
                                    is_committed = true;
                                    buffered_chunks.push(chunk);
                                }
                                robustness::CommitDecision::Buffering => {
                                    buffered_chunks.push(chunk);
                                }
                            }
                        }
                        Ok(Some(Err(e))) => {
                            stream_err = Some(format!("upstream_read_error: {e}"));
                            break;
                        }
                        Ok(None) => {
                            stream_err = Some("stream_ended_before_commit".to_string());
                            break;
                        }
                        Err(_timeout) => {
                            stream_err = Some("ttft_timeout".to_string());
                            break;
                        }
                    }
                }

                if !is_committed {
                    let duration_ms = start.elapsed().as_millis() as i64;
                    let err_str = stream_err.unwrap_or_else(|| "pre_commit_failure".to_string());
                    log_request(
                        svc,
                        session_id,
                        endpoint,
                        &model,
                        route_id,
                        provider.tool.as_str(),
                        StatusCode::BAD_GATEWAY.as_u16() as i64,
                        Usage::default(),
                        duration_ms,
                        Some(err_str.clone()),
                    );
                    probe_guard.disarm();
                    svc.record_failure(route_id);
                    failover_trajectory.push(serde_json::json!({
                        "provider": route_id,
                        "status": 502,
                        "error": &err_str,
                    }));
                    last_err = Some((
                        StatusCode::BAD_GATEWAY,
                        format!("{{\"error\":\"{err_str}\"}}"),
                        None,
                    ));
                    continue;
                }

                // We reached Commit Point!
                // Success will be recorded when the stream completes cleanly without interruption.
                failover_trajectory.push(serde_json::json!({
                    "provider": route_id,
                    "status": status.as_u16(),
                }));
                let failover_path_json = if failover_trajectory.len() > 1 {
                    serde_json::to_string(&failover_trajectory).ok()
                } else {
                    None
                };

                let req_id = headers
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("req_{:016x}", rand::random::<u64>()));

                let effective_model = entry.model_override.clone().or_else(|| model.clone());
                return handle_success_streaming_committed(
                    svc,
                    session_id,
                    route_id,
                    provider.tool.as_str(),
                    status,
                    &resp_headers,
                    buffered_chunks,
                    raw_stream,
                    has_tool_call,
                    endpoint,
                    effective_model,
                    start,
                    inbound_proto,
                    provider.protocol,
                    req_model.to_string(),
                    failover_path_json.as_deref(),
                    req_id,
                );
            }
        }
        let retry_after = resp.headers().get(axum::http::header::RETRY_AFTER).cloned();
        let (err_resp, should_continue) = handle_upstream_error(
            svc,
            session_id,
            route_id,
            provider.tool.as_str(),
            resp,
            endpoint,
            &model,
            start,
            failover_4xx,
        )
        .await;
        // `handle_upstream_error` records the failure (and releases the probe)
        // on the continue path; the non-qualifying 4xx branch below releases it
        // too. Either way the guard must not release again on drop.
        probe_guard.disarm();
        if should_continue {
            failover_trajectory.push(serde_json::json!({
                "provider": route_id,
                "status": status.as_u16(),
            }));
            // Preserve the upstream error body (e.g. 429 budget_exceeded)
            // transcoded to client inbound protocol so the client understands the error.
            let raw_bytes = match axum::body::to_bytes(err_resp.into_body(), 1024 * 1024).await {
                Ok(b) => b,
                Err(_) => Bytes::new(),
            };
            let transcoded =
                super::convert::transcode_error_body(&raw_bytes, provider.protocol, inbound_proto);
            let text = String::from_utf8_lossy(&transcoded).to_string();
            last_err = Some((status, text, retry_after));
            continue;
        }
        // Non-qualifying 4xx: passthrough, stop failover.
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            svc.record_failure(route_id);
        } else {
            svc.circuit_breaker.release_probe(route_id);
        }
        if inbound_proto != provider.protocol {
            let (parts, body) = err_resp.into_parts();
            let raw_bytes = axum::body::to_bytes(body, 1024 * 1024)
                .await
                .unwrap_or_default();
            let transcoded =
                super::convert::transcode_error_body(&raw_bytes, provider.protocol, inbound_proto);
            let mut builder = Response::builder().status(parts.status);
            if let Some(ct) = parts.headers.get(axum::http::header::CONTENT_TYPE) {
                builder = builder.header(axum::http::header::CONTENT_TYPE, ct);
            } else {
                builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
            }
            return builder.body(Body::from(transcoded)).unwrap();
        }
        return err_resp;
    }

    session_failed(svc, session_id);

    match last_err {
        Some((status, text, retry_after)) => {
            let mut builder = Response::builder().status(status);
            if let Some(v) = retry_after {
                builder = builder.header(axum::http::header::RETRY_AFTER, v);
            }
            if text.starts_with('{') {
                builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
            }
            builder.body(text.into()).unwrap()
        }
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
    client_tool: Option<ToolKind>,
) -> Response {
    let start = Instant::now();

    // Stage 1: resolve routes and providers.
    let providers = svc.core.list(None).unwrap_or_default();
    let ResolvedRoutes {
        routes,
        model_override,
        target_protocol,
        provider_map,
        route_models,
        failover_4xx,
    } = match resolve_routes(svc, &providers, client_tool) {
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
    let mut plan = router.plan(
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

    // Stage 2b: resolve each route's effective model override. The router
    // plans with the single global override; here:
    // 1. Explicit model mappings (e.g. `sonnet` -> anthropic=claude-3-7-sonnet, deepseek=deepseek-chat)
    // 2. Per-provider override (`--route-model id=model`)
    // 3. Global override (`--model`)
    // 4. Passthrough (None)
    // `failover_loop` already caches prepared bodies keyed by each
    // entry's model_override, so heterogeneous upstreams each get their own
    // correctly rewritten request body.
    let client_mappings = if let Some(sel) = model.as_deref() {
        svc.core
            .db()
            .get_mappings_for_client_model(sel)
            .ok()
            .filter(|m| !m.is_empty())
    } else {
        None
    };
    for entry in &mut plan.entries {
        entry.model_override = crate::router::decision::resolve_effective_model(
            &entry.route_id,
            model.as_deref(),
            client_mappings.as_ref(),
            &route_models,
            model_override.as_deref(),
        );
    }

    // Stage 2c: model→route pinning (aggregated `/v1/models`). Off by default.
    // If the request matched an explicit client model mapping alias, Stage 2c is strictly
    // bypassed so all planned failover routes are preserved with their respective mapped models.
    if client_mappings.is_none() && svc.core.db().models_merge_enabled().unwrap_or(false) {
        if let Some(sel) = model.as_deref() {
            let mr = svc.core.db().model_route_for(sel).unwrap_or_default();
            if let Some(mr) = mr {
                if let Some(pos) = plan.entries.iter().position(|e| e.route_id == mr.route_id) {
                    let mut pinned = plan.entries.remove(pos);
                    pinned.model_override = Some(mr.upstream_model);
                    plan.entries.insert(0, pinned);
                }
            }
        }
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
        failover_4xx,
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

/// Append `chunk` to both head buffer (first ~8KB) and tail ring buffer (last ~8KB).
fn push_head_and_tail(head: &Mutex<Vec<u8>>, tail: &Mutex<Vec<u8>>, chunk: &[u8]) {
    {
        let mut h = head.lock().unwrap();
        if h.len() < SSE_HEAD_CAP {
            let space = SSE_HEAD_CAP - h.len();
            let take = chunk.len().min(space);
            h.extend_from_slice(&chunk[..take]);
        }
    }
    {
        let mut t = tail.lock().unwrap();
        t.extend_from_slice(chunk);
        if t.len() > SSE_TAIL_CAP {
            let excess = t.len() - SSE_TAIL_CAP;
            t.drain(..excess);
        }
    }
}

/// Build a streaming axum `Body` from the upstream chunk stream that:
///   - forwards each chunk to the client unchanged, and
///   - maintains head and tail buffers of the stream, and once the stream ends,
///     extracts usage from those buffers and writes a request log row.
///
/// The logging happens as a side effect of the stream being driven to
/// completion by axum/the client.
fn make_tapped_streaming_body_with_flag<S>(
    stream: S,
    head: Arc<Mutex<Vec<u8>>>,
    tail: Arc<Mutex<Vec<u8>>>,
    db: crate::store::db::Database,
    endpoint: String,
    model: Option<String>,
    provider_id: String,
    tool: String,
    session_id: Option<String>,
    status: StatusCode,
    start: Instant,
    key_ref: String,
    upstream_domain: String,
    failover_path: Option<String>,
    interrupted_flag: Arc<std::sync::atomic::AtomicBool>,
    circuit_breaker: Arc<super::CircuitBreaker>,
) -> Body
where
    S: futures::Stream<Item = std::io::Result<Bytes>> + Send + 'static,
{
    Body::from_stream(TailLogStream {
        inner: Box::pin(stream),
        head,
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
        key_ref,
        upstream_domain,
        failover_path,
        interrupted_flag,
        circuit_breaker,
    })
}

// ── Stream wrapper ─────────────────────────────────────────────────────────

/// Forwards an inner SSE stream unchanged; on stream end (or the first error
/// item) writes exactly one request-log row with usage extracted from head and tail.
struct TailLogStream {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>>,
    head: Arc<Mutex<Vec<u8>>>,
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
    key_ref: String,
    upstream_domain: String,
    failover_path: Option<String>,
    interrupted_flag: Arc<std::sync::atomic::AtomicBool>,
    circuit_breaker: Arc<super::CircuitBreaker>,
}

impl TailLogStream {
    fn log(&self, error: Option<String>) {
        let head_bytes = self.head.lock().unwrap().clone();
        let tail_bytes = self.tail.lock().unwrap().clone();
        let usage = Usage::from_sse_buffers(&head_bytes, &tail_bytes);
        let duration_ms = self.start.elapsed().as_millis() as i64;
        let status_ok = error.is_none();
        let cost_usd_micros = crate::proxy::pricing::estimate_cost_with_db(
            Some(&self.db),
            self.model.as_deref(),
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.cached_prompt_tokens,
            usage.cache_write_tokens,
        );
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
            cost_usd_micros,
        });
        crate::sessions::record_outcome(
            &self.db,
            self.session_id.as_deref(),
            &self.tool,
            &self.provider_id,
            self.model.as_deref(),
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.cached_prompt_tokens,
            usage.cache_write_tokens,
            status_ok,
        );

        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let _ = self.db.insert_audit_log(&crate::store::db::AuditLog {
            id: 0,
            ts: now_ts,
            client_tool: self.tool.clone(),
            client_token_id: format!("token:{}", self.tool),
            provider_id: self.provider_id.clone(),
            upstream_domain: self.upstream_domain.clone(),
            upstream_model: self.model.clone().unwrap_or_default(),
            key_ref: self.key_ref.clone(),
            status_code: self.status.as_u16(),
            duration_ms: duration_ms as u64,
            prompt_tokens: usage.prompt_tokens as u64,
            completion_tokens: usage.completion_tokens as u64,
            failover_path: self.failover_path.clone(),
        });
    }
}

impl Drop for TailLogStream {
    fn drop(&mut self) {
        // The client disconnected (or the response was dropped) before the
        // stream completed: record the request and close the session so the
        // outcome isn't silently lost. The circuit breaker is intentionally
        // left untouched — the upstream already committed at the pre-commit
        // gate, so a client-side abort must not penalize it.
        if !self.done_logged {
            self.done_logged = true;
            self.log(Some("stream dropped before completion".to_string()));
        }
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
                    let is_interrupted = this
                        .interrupted_flag
                        .load(std::sync::atomic::Ordering::SeqCst);
                    let err = if is_interrupted {
                        Some("upstream stream interrupted".to_string())
                    } else {
                        this.circuit_breaker.record_success_streamed(&this.provider_id);
                        None
                    };
                    this.log(err);
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
    identity: Option<axum::extract::Extension<ClientIdentity>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client_tool = identity.map(|axum::extract::Extension(ClientIdentity(t))| t);
    let endpoint = endpoint_from_path(uri.path());
    let mut body = body;
    // Parse the body once; model extraction and the session engine share it.
    let mut parsed: Option<serde_json::Value> = serde_json::from_slice(&body).ok();
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

    // Opt-in L3: inject pending handover as user-level data (never system).
    let mut injected_handover: Option<i64> = None;
    let auto_inject = svc
        .core
        .db()
        .handover_auto_inject_enabled()
        .unwrap_or(false);
    if auto_inject {
        let cutoff = crate::sessions::cutoff_rfc3339(crate::sessions::SESSION_STICKY_WINDOW_SECS);
        if let Ok(Some(handover)) = svc
            .core
            .db()
            .find_pending_handover(session_id.as_deref(), &cutoff)
        {
            if let Some(ref mut parsed_val) = parsed {
                let header_root = headers
                    .get("x-xfade-project-root")
                    .and_then(|v| v.to_str().ok());
                let root =
                    crate::sessions::handover::resolve_project_root(svc.core.db(), header_root);
                let mut brief = crate::sessions::handover::collect_handover_for(
                    svc.core.db(),
                    &root,
                    crate::sessions::handover::DEFAULT_MAX_DIFF_CHARS,
                    session_id.as_deref(),
                );
                if brief.model.is_none() {
                    brief.model = handover.from_model.clone();
                }
                brief.session_id = session_id.clone();
                let xml = brief
                    .to_injection_xml(handover.from_model.as_deref(), handover.to_model.as_deref());
                if crate::sessions::handover::inject_handover_into_payload(
                    parsed_val, endpoint, &xml,
                ) {
                    if let Ok(new_bytes) = serde_json::to_vec(parsed_val) {
                        body = Bytes::from(new_bytes);
                    }
                    // Marked only after the upstream accepts the request, so
                    // a failed attempt does not burn the handover.
                    injected_handover = Some(handover.id);
                }
            }
        }
    }
    let mut resp = forward_with_failover(
        &svc,
        "POST",
        endpoint,
        &headers,
        &body,
        parsed.as_ref(),
        model,
        session_id.as_deref(),
        client_tool,
    )
    .await;
    if let Some(id) = injected_handover {
        if resp.status().is_success() {
            let _ = svc.core.db().mark_handover_injected(
                id,
                session_id.as_deref().unwrap_or(""),
                &crate::sessions::now_rfc3339(),
            );
        }
    }
    // Surface the session id so clients/tools can correlate requests.
    if let Some(sid) = &session_id {
        if let Ok(value) = axum::http::HeaderValue::from_str(sid) {
            resp.headers_mut().insert("x-xfade-session-id", value);
        }
    }
    resp
}

/// One upstream's contribution to the aggregated model catalog.
struct ModelProbe {
    route_id: String,
    models: Vec<serde_json::Value>,
}

/// Fetch and merge `/v1/models` from every configured route. Raw ids are kept
/// when unambiguous; ids exposed by more than one route get a `<route_id>/`
/// prefix so a client-selected id can be mapped back to exactly one provider.
/// Circuit state is deliberately not mutated by these discovery probes.
async fn aggregate_models(
    svc: &ProxyService,
    headers: &HeaderMap,
    client_tool: Option<ToolKind>,
) -> Response {
    let providers = svc.core.list(None).unwrap_or_default();
    let ResolvedRoutes {
        routes,
        provider_map,
        ..
    } = match resolve_routes(svc, &providers, client_tool) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };

    let mut probes = Vec::new();
    for route_id in &routes {
        if svc.circuit_open(route_id) {
            continue;
        }
        let Some(provider) = provider_map.get(route_id).copied() else {
            continue;
        };
        let Some(base_url) = provider.base_url.clone() else {
            continue;
        };
        if let Ok(allowed) = svc.core.is_domain_allowed(&base_url) {
            if !allowed {
                continue;
            }
        }
        let auth_key = if provider.is_official() {
            None
        } else {
            svc.core.secrets().get(&provider.key_ref).ok()
        };
        let client = svc.client.clone();
        let hdrs = headers.clone();
        let rid = route_id.clone();
        probes.push(async move {
            // Reuse the normal upstream request builder: correct `/v1/models`
            // URL, stripped client auth, injected upstream key, GET no body.
            let outcome = tokio::time::timeout(
                MODELS_PROBE_TIMEOUT,
                try_forward(
                    &client,
                    "GET",
                    &base_url,
                    "models",
                    &hdrs,
                    &[],
                    auth_key.as_deref(),
                ),
            )
            .await;
            let models = match outcome {
                Ok(Ok(resp)) if resp.status().is_success() => {
                    let bytes = resp.bytes().await.unwrap_or_default();
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                        .ok()
                        .and_then(|v| {
                            v["data"].as_array().map(|a| {
                                a.iter()
                                    .filter(|m| m.get("id").and_then(|i| i.as_str()).is_some())
                                    .cloned()
                                    .collect()
                            })
                        })
                        .unwrap_or_default()
                }
                _ => Vec::new(),
            };
            ModelProbe {
                route_id: rid,
                models,
            }
        });
    }

    let results: Vec<ModelProbe> = futures::future::join_all(probes).await;

    // Count how many distinct routes expose each raw id (a route exposing the
    // same id twice still counts once).
    let mut route_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for probe in &results {
        let mut seen = std::collections::HashSet::new();
        for m in &probe.models {
            if let Some(id) = m.get("id").and_then(|i| i.as_str()) {
                if seen.insert(id.to_string()) {
                    *route_counts.entry(id.to_string()).or_insert(0) += 1;
                }
            }
        }
    }

    let mut merged: Vec<serde_json::Value> = Vec::new();
    let mut map = crate::store::db::ModelRoutes::new();
    for probe in &results {
        for m in &probe.models {
            let Some(raw) = m.get("id").and_then(|i| i.as_str()) else {
                continue;
            };
            let ambiguous = route_counts.get(raw).copied().unwrap_or(1) > 1;
            let (exposed, entry) = if ambiguous {
                let exposed = format!("{}/{}", probe.route_id, raw);
                let mut e = m.clone();
                e["id"] = serde_json::Value::String(exposed.clone());
                (exposed, e)
            } else {
                (raw.to_string(), m.clone())
            };
            merged.push(entry);
            map.insert(
                exposed,
                crate::store::db::ModelRoute {
                    route_id: probe.route_id.clone(),
                    upstream_model: raw.to_string(),
                },
            );
        }
    }

    if merged.is_empty() {
        // No usable catalog: degrade to the pre-feature first-success forward
        // and keep the last-known map so existing pinning still works.
        return forward_with_failover(
            svc,
            "GET",
            "models",
            headers,
            &[],
            None,
            None,
            None,
            client_tool,
        )
        .await;
    }

    // In models_merge mode: query all configured client model aliases and inject them
    // into the returned catalog list (with owned_by: "xfade").
    // NOTE: Aliases are purposely NOT added to `map` (and stripped from `map` if an upstream
    // happened to advertise a matching model ID), so that client requests using an alias are
    // never locked to a single provider in Stage 2c.
    if let Ok(mappings) = svc.core.db().get_model_mappings() {
        let now_secs = crate::proxy::circuit_breaker::now_secs();
        for alias in mappings.keys() {
            map.remove(alias);
            if let Some(pos) = merged
                .iter()
                .position(|m| m.get("id").and_then(|i| i.as_str()) == Some(alias))
            {
                merged.remove(pos);
            }
            merged.push(serde_json::json!({
                "id": alias,
                "object": "model",
                "created": now_secs,
                "owned_by": "xfade",
                "permission": []
            }));
        }
    }

    let _ = svc.core.db().set_model_routes(&map);

    let body = serde_json::json!({ "object": "list", "data": merged });
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
        .unwrap()
}

pub async fn handle_get(
    State(svc): State<Arc<ProxyService>>,
    identity: Option<axum::extract::Extension<ClientIdentity>>,
    headers: HeaderMap,
) -> Response {
    let client_tool = identity.map(|axum::extract::Extension(ClientIdentity(t))| t);
    // Read per request so `xfade proxy models-merge on/off` reaches a running
    // daemon immediately. Off keeps the exact pre-feature first-success path.
    if svc.core.db().models_merge_enabled().unwrap_or(false) {
        return aggregate_models(&svc, &headers, client_tool).await;
    }
    forward_with_failover(
        &svc,
        "GET",
        "models",
        &headers,
        &[],
        None,
        None,
        None,
        client_tool,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{stream, StreamExt};

    #[test]
    fn upstream_request_drops_all_inbound_credentials() {
        let mut headers = HeaderMap::new();
        for (k, v) in [
            ("authorization", "Bearer client-secret"),
            ("proxy-authorization", "Basic abc"),
            ("x-api-key", "client-anthropic-key"),
            ("api-key", "client-azure-key"),
            ("x-goog-api-key", "client-google-key"),
            ("x-auth-token", "client-token"),
            ("cookie", "session=browser"),
            ("x-custom", "keep-me"),
        ] {
            headers.insert(
                axum::http::HeaderName::from_static(k),
                axum::http::HeaderValue::from_static(v),
            );
        }
        let client = reqwest::Client::new();
        let req = build_upstream_request(
            &client,
            "POST",
            "http://127.0.0.1:1",
            "chat",
            &headers,
            b"{}",
            Some("route-key"),
        )
        .build()
        .unwrap();
        let h = req.headers();
        assert_eq!(h.get("authorization").unwrap(), "Bearer route-key");
        for k in [
            "proxy-authorization",
            "x-api-key",
            "api-key",
            "x-goog-api-key",
            "x-auth-token",
            "cookie",
        ] {
            assert!(h.get(k).is_none(), "{k} leaked upstream");
        }
        assert_eq!(h.get("x-custom").unwrap(), "keep-me");
    }

    #[test]
    fn qualifies_for_4xx_failover_auth_statuses() {
        assert!(qualifies_for_4xx_failover(StatusCode::UNAUTHORIZED, b"{}"));
        assert!(qualifies_for_4xx_failover(StatusCode::FORBIDDEN, b"{}"));
    }

    #[test]
    fn qualifies_for_4xx_failover_400_model_rejection_only() {
        // freebuff-style explicit code.
        assert!(qualifies_for_4xx_failover(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"code":"model_not_allowed","message":"not allowed"}}"#
        ));
        // "not in this proxy's model list" phrasing.
        assert!(qualifies_for_4xx_failover(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"Model 'x' is not in this proxy's model list"}}"#
        ));
        // "model not found" phrasing.
        assert!(qualifies_for_4xx_failover(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"model not found"}}"#
        ));
        // Generic 400 validation errors must NOT fail over.
        assert!(!qualifies_for_4xx_failover(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"invalid temperature"}}"#
        ));
        assert!(!qualifies_for_4xx_failover(
            StatusCode::BAD_REQUEST,
            b"bad request"
        ));
    }

    #[test]
    fn qualifies_for_4xx_failover_rejects_other_4xx() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::UNPROCESSABLE_ENTITY,
            StatusCode::TOO_MANY_REQUESTS, // handled by the always-failover branch
        ] {
            assert!(!qualifies_for_4xx_failover(status, b"model not found"));
        }
    }

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

    #[tokio::test]
    async fn egress_blocked_route_returns_403_and_logs_audit() {
        use crate::{store::secrets::MockStore, Core, Provider};
        let temp = tempfile::tempdir().unwrap();
        let core = Core::with_paths(
            temp.path(),
            temp.path(),
            std::sync::Arc::new(MockStore::default()),
        )
        .unwrap();

        let p = Provider::new(
            "evil",
            ToolKind::ClaudeCode,
            Some("https://evil.internal.corp/v1".into()),
        );

        let svc = ProxyService::new(core);
        let entries = vec![crate::router::decision::RouteEntry {
            route_id: "evil".into(),
            model_override: None,
            tier: crate::router::decision::Tier::Cloud,
        }];
        let mut provider_map = std::collections::HashMap::new();
        provider_map.insert("evil".to_string(), &p);
        let headers = HeaderMap::new();

        let resp = failover_loop(
            &svc,
            None,
            "POST",
            &entries,
            &provider_map,
            "chat",
            "chat",
            None,
            &headers,
            b"{}",
            "test-model",
            Instant::now(),
            false,
        )
        .await;

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("xfade_egress_blocked"));
        assert!(body_str.contains("evil.internal.corp"));

        // Verify audit log has the egress rejection
        let audit = svc.core.recent_audit_logs(1, None).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].status_code, 403);
        assert_eq!(audit[0].upstream_domain, "evil.internal.corp");
    }

    #[tokio::test]
    async fn failover_records_trajectory_in_audit_log() {
        use crate::{store::secrets::MockStore, Core, Provider};
        let temp = tempfile::tempdir().unwrap();
        let core = Core::with_paths(
            temp.path(),
            temp.path(),
            std::sync::Arc::new(MockStore::default()),
        )
        .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 27\r\n\r\n{\"choices\":[],\"usage\":null}";
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
        });

        let p1 = Provider::new(
            "evil",
            ToolKind::ClaudeCode,
            Some("https://evil.internal.corp/v1".into()),
        );
        let p2 = Provider::new(
            "local",
            ToolKind::ClaudeCode,
            Some(format!("http://{addr}/v1")),
        );
        core.db().upsert(&p2).unwrap();

        let svc = ProxyService::new(core);
        let entries = vec![
            crate::router::decision::RouteEntry {
                route_id: "evil".into(),
                model_override: None,
                tier: crate::router::decision::Tier::Cloud,
            },
            crate::router::decision::RouteEntry {
                route_id: "local".into(),
                model_override: None,
                tier: crate::router::decision::Tier::Local,
            },
        ];
        let mut provider_map = std::collections::HashMap::new();
        provider_map.insert("evil".to_string(), &p1);
        provider_map.insert("local".to_string(), &p2);
        let headers = HeaderMap::new();

        let resp = failover_loop(
            &svc,
            None,
            "POST",
            &entries,
            &provider_map,
            "chat",
            "chat",
            None,
            &headers,
            b"{}",
            "test-model",
            Instant::now(),
            false,
        )
        .await;

        assert_eq!(resp.status(), StatusCode::OK);

        let audit = svc.core.recent_audit_logs(10, None).unwrap();
        let success_log = audit.iter().find(|l| l.provider_id == "local").unwrap();
        assert_eq!(success_log.status_code, 200);
        assert!(success_log.failover_path.is_some());
        let path_str = success_log.failover_path.as_ref().unwrap();
        assert!(path_str.contains("evil"));
        assert!(path_str.contains("403"));
        assert!(path_str.contains("local"));
        assert!(path_str.contains("200"));
    }

    #[test]
    fn test_prepare_body_for_heterogeneous_providers() {
        let client_endpoint = "messages"; // Claude client
        let body = br#"{"messages":[{"role":"user","content":"hi"}]}"#;

        // Upstream 1: Anthropic Native -> Passthrough
        let p_native = crate::models::Provider::new("p1", ToolKind::ClaudeCode, None)
            .with_protocol(crate::models::ProviderProtocol::Messages);
        let (ep1, body1) =
            prepare_body_for_provider(client_endpoint, &p_native, body, None).unwrap();
        assert_eq!(ep1, "messages");
        let val1: serde_json::Value = serde_json::from_slice(&body1).unwrap();
        assert!(val1.get("messages").is_some());

        // Upstream 2: OpenAI Chat -> Convert to Chat
        let p_chat = crate::models::Provider::new(
            "p2",
            ToolKind::ClaudeCode,
            Some("https://api.openai.com".into()),
        )
        .with_protocol(crate::models::ProviderProtocol::Chat);
        let (ep2, body2) = prepare_body_for_provider(client_endpoint, &p_chat, body, None).unwrap();
        assert_eq!(ep2, "chat");
        let val2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
        assert!(val2.get("messages").is_some());

        // Upstream 3: OpenAI Responses -> Convert to Responses
        let p_resp = crate::models::Provider::new(
            "p3",
            ToolKind::Codex,
            Some("https://api.openai.com".into()),
        )
        .with_protocol(crate::models::ProviderProtocol::Responses);
        let (ep3, body3) = prepare_body_for_provider(client_endpoint, &p_resp, body, None).unwrap();
        assert_eq!(ep3, "responses");
        let val3: serde_json::Value = serde_json::from_slice(&body3).unwrap();
        assert!(val3.get("input").is_some());
    }

    #[test]
    fn test_prepare_body_for_models_endpoint_preserves_models() {
        let p = crate::models::Provider::new("p1", ToolKind::ClaudeCode, None)
            .with_protocol(crate::models::ProviderProtocol::Messages);
        let (ep, _) = prepare_body_for_provider("models", &p, b"", None).unwrap();
        assert_eq!(ep, "models");
    }
}
