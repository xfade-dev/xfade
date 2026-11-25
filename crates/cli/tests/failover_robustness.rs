use axum::{
    body::Body, extract::Request, http::StatusCode, response::Response, routing::any, Router,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use xfade_core::models::{Provider, ProviderProtocol, ToolKind};
use xfade_core::proxy::robustness::TimeoutConfig;
use xfade_core::proxy::{CircuitBreaker, ProxyService};
use xfade_core::service::Core;
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct RecordedRequest {
    path: String,
    body: String,
}

#[derive(Clone)]
enum MockAction {
    StatusJson(StatusCode, String),
    StatusJsonWithHeaders(StatusCode, Vec<(String, String)>, String),
    Sse(StatusCode, Vec<String>),
    SseDelayed(StatusCode, Duration, Vec<String>),
    SseErrorMidStream(StatusCode, Vec<String>),
    Sequence(Vec<MockAction>),
}

struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl MockServer {
    async fn spawn(action: MockAction) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(Mutex::new(action));
        let req_clone = requests.clone();

        let app = Router::new().fallback(any(move |req: Request| {
            let req_clone = req_clone.clone();
            let state_clone = state.clone();
            async move {
                let (parts, body) = req.into_parts();
                let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                let recorded = RecordedRequest {
                    path: parts.uri.path().to_string(),
                    body: String::from_utf8_lossy(&bytes).to_string(),
                };
                req_clone.lock().unwrap().push(recorded);

                let current_action = {
                    let mut lock = state_clone.lock().unwrap();
                    match &mut *lock {
                        MockAction::Sequence(seq) => {
                            if seq.is_empty() {
                                MockAction::StatusJson(StatusCode::OK, "{}".to_string())
                            } else if seq.len() == 1 {
                                seq[0].clone()
                            } else {
                                seq.remove(0)
                            }
                        }
                        other => other.clone(),
                    }
                };

                match current_action {
                    MockAction::StatusJson(status, resp_body) => Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Body::from(resp_body))
                        .unwrap(),
                    MockAction::StatusJsonWithHeaders(status, headers, resp_body) => {
                        let mut b = Response::builder().status(status);
                        for (k, v) in headers {
                            b = b.header(k, v);
                        }
                        b.body(Body::from(resp_body)).unwrap()
                    }
                    MockAction::Sse(status, lines) => {
                        let mut payload = Vec::new();
                        for line in lines {
                            payload.extend_from_slice(line.as_bytes());
                            payload.extend_from_slice(b"\n\n");
                        }
                        Response::builder()
                            .status(status)
                            .header("content-type", "text/event-stream")
                            .body(Body::from(payload))
                            .unwrap()
                    }
                    MockAction::SseDelayed(status, delay, lines) => {
                        let s = futures::stream::unfold(
                            (lines, 0usize, delay),
                            |(lines, idx, delay)| async move {
                                if idx == 0 {
                                    tokio::time::sleep(delay).await;
                                }
                                if idx < lines.len() {
                                    let mut payload = lines[idx].as_bytes().to_vec();
                                    payload.extend_from_slice(b"\n\n");
                                    Some((
                                        Ok::<axum::body::Bytes, std::io::Error>(
                                            axum::body::Bytes::from(payload),
                                        ),
                                        (lines, idx + 1, Duration::ZERO),
                                    ))
                                } else {
                                    None
                                }
                            },
                        );
                        Response::builder()
                            .status(status)
                            .header("content-type", "text/event-stream")
                            .body(Body::from_stream(s))
                            .unwrap()
                    }
                    MockAction::SseErrorMidStream(status, lines) => {
                        let s = futures::stream::unfold(
                            (lines, 0usize, false),
                            |(lines, idx, yielded_err)| async move {
                                if idx < lines.len() {
                                    let mut payload = lines[idx].as_bytes().to_vec();
                                    payload.extend_from_slice(b"\n\n");
                                    Some((
                                        Ok::<axum::body::Bytes, std::io::Error>(
                                            axum::body::Bytes::from(payload),
                                        ),
                                        (lines, idx + 1, false),
                                    ))
                                } else if !yielded_err {
                                    tokio::time::sleep(Duration::from_millis(20)).await;
                                    Some((
                                        Err(std::io::Error::new(
                                            std::io::ErrorKind::ConnectionAborted,
                                            "upstream connection dropped mid-stream",
                                        )),
                                        (lines, idx, true),
                                    ))
                                } else {
                                    None
                                }
                            },
                        );
                        Response::builder()
                            .status(status)
                            .header("content-type", "text/event-stream")
                            .body(Body::from_stream(s))
                            .unwrap()
                    }
                    MockAction::Sequence(_) => unreachable!(),
                }
            }
        }));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self { url, requests }
    }
}

async fn spawn_proxy_with_svc(svc: ProxyService) -> (String, Arc<CircuitBreaker>) {
    let cb = svc.circuit_breaker.clone();
    let app = svc.build_router();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, cb)
}

fn setup_test_core(dir: &tempfile::TempDir, providers: &[(&str, &str, ProviderProtocol)]) -> Core {
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let secrets = Arc::new(xfade_core::store::secrets::MockStore::default());
    let core = Core::with_paths(&home, &data, secrets.clone()).unwrap();

    let mut route_ids = Vec::new();
    for (id, url, proto) in providers {
        core.secrets()
            .set(&format!("ref:{id}"), "sk-mock-key")
            .unwrap();
        let p = Provider::new(*id, ToolKind::Aider, Some(url.to_string())).with_protocol(*proto);
        core.db().upsert(&p).unwrap();
        route_ids.push(id.to_string());
    }
    core.db().set_routes(&route_ids, None, "aider").unwrap();
    core
}

// ── Scenario 1: Pre-Commit Upstream Error Body (HTTP 200 with error payload) ──

#[tokio::test]
async fn test_scenario_1_pre_commit_upstream_200_error_body_fails_over() {
    let dir = tempfile::tempdir().unwrap();

    // Upstream 1: Returns HTTP 200 OK with JSON error payload
    let up1 = MockServer::spawn(MockAction::StatusJson(
        StatusCode::OK,
        r#"{"error":{"message":"Internal server capacity error","type":"server_error","code":"server_overloaded"}}"#.to_string(),
    ))
    .await;

    // Upstream 2: Returns HTTP 200 OK with valid choices response
    let up2 = MockServer::spawn(MockAction::StatusJson(
        StatusCode::OK,
        r#"{"choices":[{"message":{"content":"Healthy response from candidate 2"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#.to_string(),
    ))
    .await;

    let core = setup_test_core(
        &dir,
        &[
            ("p1", &up1.url, ProviderProtocol::Chat),
            ("p2", &up2.url, ProviderProtocol::Chat),
        ],
    );

    let (url, cb) = spawn_proxy_with_svc(ProxyService::new(core)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}/v1/chat/completions"))
        .header("authorization", "Bearer test-key")
        .header("x-xfade-tool", "aider")
        .json(&serde_json::json!({
            "model": "mock-model",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let failover_header = resp
        .headers()
        .get("x-xfade-failover-path")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = resp.text().await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Healthy response from candidate 2"),
        "expected candidate 2 body, got: {body}"
    );
    assert!(
        !body.contains("Internal server capacity error"),
        "error body from candidate 1 must not leak to client"
    );
    assert!(
        failover_header.is_some(),
        "failover header must track trajectory"
    );
    assert_eq!(
        cb.failure_count(&xfade_core::proxy::circuit_key(ToolKind::Aider, "p1")),
        1,
        "candidate 1 failure must be recorded on circuit breaker"
    );
}

// ── Scenario 2: Pre-Commit TTFT Timeout Fails Over ───────────────────────────

#[tokio::test]
async fn test_scenario_2_pre_commit_ttft_timeout_fails_over() {
    let dir = tempfile::tempdir().unwrap();

    // Upstream 1: Sends headers but stalls (sleeps 400ms before sending anything)
    let up1 = MockServer::spawn(MockAction::SseDelayed(
        StatusCode::OK,
        Duration::from_millis(400),
        vec![r#"data: {"choices":[{"delta":{"content":"late"}}]}"#.to_string()],
    ))
    .await;

    // Upstream 2: Returns SSE stream immediately
    let up2 = MockServer::spawn(MockAction::Sse(
        StatusCode::OK,
        vec![
            r#"data: {"choices":[{"delta":{"content":"Fast tokens from candidate 2"}}]}"#
                .to_string(),
            "data: [DONE]".to_string(),
        ],
    ))
    .await;

    let core = setup_test_core(
        &dir,
        &[
            ("p1", &up1.url, ProviderProtocol::Chat),
            ("p2", &up2.url, ProviderProtocol::Chat),
        ],
    );

    // Set tight TTFT timeout (150ms) to trigger pre-commit failover
    let timeouts = TimeoutConfig {
        connect_timeout: Duration::from_secs(5),
        ttft_timeout: Duration::from_millis(150),
        idle_timeout: Duration::from_secs(5),
        total_non_stream_timeout: Duration::from_secs(5),
    };
    let (url, cb) = spawn_proxy_with_svc(ProxyService::new(core).with_timeouts(timeouts)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}/v1/chat/completions"))
        .header("authorization", "Bearer test-key")
        .header("x-xfade-tool", "aider")
        .json(&serde_json::json!({
            "model": "mock-model",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();

    assert!(
        body.contains("Fast tokens from candidate 2"),
        "expected fast response from candidate 2, got: {body}"
    );
    assert_eq!(
        cb.failure_count(&xfade_core::proxy::circuit_key(ToolKind::Aider, "p1")),
        1,
        "TTFT timeout must record failure on candidate 1"
    );
}

// ── Scenario 3: Post-Commit Mid-Stream Connection Drop ───────────────────────

#[tokio::test]
async fn test_scenario_3_post_commit_mid_stream_connection_drop_synthesizes_error() {
    let dir = tempfile::tempdir().unwrap();

    // Upstream 1: Emits valid text delta, then drops TCP connection mid-stream
    let up1 = MockServer::spawn(MockAction::SseErrorMidStream(
        StatusCode::OK,
        vec![r#"data: {"choices":[{"delta":{"content":"Committed text tokens..."}}]}"#.to_string()],
    ))
    .await;

    // Upstream 2: Fallback route - must NEVER be called once committed!
    let up2 = MockServer::spawn(MockAction::Sse(
        StatusCode::OK,
        vec![
            r#"data: {"choices":[{"delta":{"content":"Should NEVER be replayed!"}}]}"#.to_string(),
            "data: [DONE]".to_string(),
        ],
    ))
    .await;

    let core = setup_test_core(
        &dir,
        &[
            ("p1", &up1.url, ProviderProtocol::Chat),
            ("p2", &up2.url, ProviderProtocol::Chat),
        ],
    );

    let (url, cb) = spawn_proxy_with_svc(ProxyService::new(core)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}/v1/chat/completions"))
        .header("authorization", "Bearer test-key")
        .header("x-xfade-tool", "aider")
        .header("x-request-id", "req-test-scenario-3")
        .json(&serde_json::json!({
            "model": "mock-model",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();

    // 1. Text before interruption was received
    assert!(
        body.contains("Committed text tokens..."),
        "client must receive committed text, got: {body}"
    );
    // 2. Synthesized protocol-native interruption event
    assert!(
        body.contains(r#""code":"xfade_stream_interrupted""#),
        "body must contain xfade_stream_interrupted code: {body}"
    );
    assert!(
        body.contains(r#""retryable":true"#),
        "text-only interruption must be retryable: true: {body}"
    );
    assert!(
        body.contains(r#""request_id":"req-test-scenario-3""#),
        "interruption event must preserve request_id: {body}"
    );
    assert!(
        body.ends_with("data: [DONE]\n\n"),
        "stream must terminate cleanly with [DONE]: {body}"
    );
    // 3. Fallback route was NEVER called
    assert_eq!(
        up2.requests.lock().unwrap().len(),
        0,
        "post-commit interruption must NEVER replay to candidate 2"
    );
    // 4. Candidate 1 marked failed on circuit breaker
    assert_eq!(
        cb.failure_count(&xfade_core::proxy::circuit_key(ToolKind::Aider, "p1")),
        1,
        "interrupted stream must record failure on circuit breaker"
    );
}

// ── Scenario 4: Post-Commit Tool Call Interruption Non-Retryable ──────────────

#[tokio::test]
async fn test_scenario_4_post_commit_tool_call_interruption_non_retryable() {
    let dir = tempfile::tempdir().unwrap();

    // Upstream 1: Emits tool call start delta, then drops TCP connection
    let up1 = MockServer::spawn(MockAction::SseErrorMidStream(
        StatusCode::OK,
        vec![
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"write_file","arguments":""}}]}}]}"#.to_string(),
        ],
    ))
    .await;

    // Upstream 2: Fallback route - must NEVER be called
    let up2 = MockServer::spawn(MockAction::Sse(
        StatusCode::OK,
        vec![
            r#"data: {"choices":[{"delta":{"content":"Should NEVER be replayed!"}}]}"#.to_string(),
            "data: [DONE]".to_string(),
        ],
    ))
    .await;

    let core = setup_test_core(
        &dir,
        &[
            ("p1", &up1.url, ProviderProtocol::Chat),
            ("p2", &up2.url, ProviderProtocol::Chat),
        ],
    );

    let (url, cb) = spawn_proxy_with_svc(ProxyService::new(core)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}/v1/chat/completions"))
        .header("authorization", "Bearer test-key")
        .header("x-xfade-tool", "aider")
        .header("x-request-id", "req-test-tool-call-4")
        .json(&serde_json::json!({
            "model": "mock-model",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();

    // 1. Tool call delta was received
    assert!(
        body.contains("tool_calls"),
        "client must receive tool_calls delta: {body}"
    );
    // 2. Synthesized protocol-native interruption event with retryable: false
    assert!(
        body.contains(r#""code":"xfade_stream_interrupted""#),
        "body must contain xfade_stream_interrupted code: {body}"
    );
    assert!(
        body.contains(r#""retryable":false"#),
        "tool-call interruption must be retryable: false to prevent duplicate side effects: {body}"
    );
    assert!(
        body.contains(r#""request_id":"req-test-tool-call-4""#),
        "interruption event must preserve request_id: {body}"
    );
    // 3. Fallback route was NEVER called
    assert_eq!(
        up2.requests.lock().unwrap().len(),
        0,
        "tool-call interruption must NEVER replay to candidate 2"
    );
    // 4. Candidate 1 marked failed on circuit breaker
    assert_eq!(
        cb.failure_count(&xfade_core::proxy::circuit_key(ToolKind::Aider, "p1")),
        1,
        "interrupted tool-call stream must record failure on circuit breaker"
    );
}

// ── Scenario 5: Intra-Upstream Retry with Retry-After ─────────────────────────

#[tokio::test]
async fn test_scenario_5_intra_upstream_retry_with_retry_after() {
    let dir = tempfile::tempdir().unwrap();

    // Upstream 1: Returns 429 on first call with Retry-After: 0, then 200 on retry
    let up1 = MockServer::spawn(MockAction::Sequence(vec![
        MockAction::StatusJsonWithHeaders(
            StatusCode::TOO_MANY_REQUESTS,
            vec![("retry-after".to_string(), "0".to_string())],
            r#"{"error":{"message":"rate limited, please back off"}}"#.to_string(),
        ),
        MockAction::StatusJson(
            StatusCode::OK,
            r#"{"choices":[{"message":{"content":"Success on intra-upstream retry"}}],"usage":{"prompt_tokens":12,"completion_tokens":6}}"#.to_string(),
        ),
    ]))
    .await;

    // Upstream 2: Fallback route - should NOT be called because p1 retry succeeds
    let up2 = MockServer::spawn(MockAction::StatusJson(
        StatusCode::OK,
        r#"{"choices":[{"message":{"content":"From candidate 2"}}]}"#.to_string(),
    ))
    .await;

    let core = setup_test_core(
        &dir,
        &[
            ("p1", &up1.url, ProviderProtocol::Chat),
            ("p2", &up2.url, ProviderProtocol::Chat),
        ],
    );

    let (url, cb) = spawn_proxy_with_svc(ProxyService::new(core)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}/v1/chat/completions"))
        .header("authorization", "Bearer test-key")
        .header("x-xfade-tool", "aider")
        .json(&serde_json::json!({
            "model": "mock-model",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();

    assert!(
        body.contains("Success on intra-upstream retry"),
        "expected success on retry from candidate 1, got: {body}"
    );
    // Candidate 1 was called twice (initial attempt + retry)
    assert_eq!(
        up1.requests.lock().unwrap().len(),
        2,
        "candidate 1 must be called twice"
    );
    // Candidate 2 was not called
    assert_eq!(
        up2.requests.lock().unwrap().len(),
        0,
        "candidate 2 must not be called when intra-upstream retry succeeds"
    );
    // Candidate 1 circuit breaker did not trip
    assert_eq!(
        cb.failure_count(&xfade_core::proxy::circuit_key(ToolKind::Aider, "p1")),
        0,
        "successful retry must not record circuit breaker failure"
    );
}

// ── Scenario 6: Heterogeneous Protocol Post-Commit Interruption ─────────────

#[tokio::test]
async fn test_scenario_6_heterogeneous_protocol_post_commit_interruption_synthesizes_error() {
    let dir = tempfile::tempdir().unwrap();

    // Upstream 1: Chat protocol emits text delta then drops TCP connection mid-stream
    let up1 = MockServer::spawn(MockAction::SseErrorMidStream(
        StatusCode::OK,
        vec![
            r#"data: {"choices":[{"delta":{"role":"assistant","content":"Heterogeneous tokens..."}}]}"#.to_string(),
        ],
    ))
    .await;

    // Upstream 2: Fallback route - must NEVER be called once committed!
    let up2 = MockServer::spawn(MockAction::Sse(
        StatusCode::OK,
        vec![
            r#"data: {"choices":[{"delta":{"content":"Should NEVER replay"}}]}"#.to_string(),
            "data: [DONE]".to_string(),
        ],
    ))
    .await;

    let core = setup_test_core(
        &dir,
        &[
            ("p1", &up1.url, ProviderProtocol::Chat),
            ("p2", &up2.url, ProviderProtocol::Chat),
        ],
    );

    let (url, cb) = spawn_proxy_with_svc(ProxyService::new(core)).await;

    // Inbound client uses Anthropic Messages protocol
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}/v1/messages"))
        .header("authorization", "Bearer test-key")
        .header("x-xfade-tool", "claude_code")
        .header("x-request-id", "req-test-scenario-6")
        .header("anthropic-version", "2023-06-01")
        .json(&serde_json::json!({
            "model": "mock-model",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100,
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();

    // 1. Converted text delta was received by Anthropic client
    assert!(
        body.contains("Heterogeneous tokens..."),
        "client must receive converted text, got: {body}"
    );
    // 2. Synthesized protocol-native Anthropic error event
    assert!(
        body.contains("event: error"),
        "Anthropic client must receive event: error on mid-stream interruption: {body}"
    );
    assert!(
        body.contains(r#""type":"xfade_stream_interrupted""#),
        "interruption payload must have type xfade_stream_interrupted: {body}"
    );
    assert!(
        body.contains(r#""retryable":true"#),
        "text-only interruption must be retryable: true: {body}"
    );
    // 3. Fallback was NEVER called
    assert_eq!(
        up2.requests.lock().unwrap().len(),
        0,
        "post-commit interruption must NEVER replay to candidate 2"
    );
    // 4. Candidate 1 circuit breaker marked failed
    assert_eq!(
        cb.failure_count(&xfade_core::proxy::circuit_key(ToolKind::Aider, "p1")),
        1,
        "interrupted upstream must record circuit breaker failure"
    );
}
