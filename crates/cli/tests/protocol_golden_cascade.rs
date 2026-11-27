use axum::{
    body::Body, extract::Request, http::StatusCode, response::Response, routing::any, Router,
};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use xfade_core::models::{Provider, ProviderProtocol, ToolKind};
use xfade_core::proxy::ProxyService;
use xfade_core::service::Core;

#[derive(Debug, Clone)]
struct RecordedRequest {
    path: String,
    body: String,
}

#[derive(Clone)]
enum MockAction {
    StatusJson(StatusCode, String),
    Sse(StatusCode, Vec<String>),
    Sequence(
        Vec<(StatusCode, String)>,
        Arc<std::sync::atomic::AtomicUsize>,
    ),
}

struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl MockServer {
    async fn spawn(action: MockAction) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let req_clone = requests.clone();

        let app = Router::new().fallback(any(move |req: Request| {
            let req_clone = req_clone.clone();
            let action = action.clone();
            async move {
                let (parts, body) = req.into_parts();
                let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                let recorded = RecordedRequest {
                    path: parts.uri.path().to_string(),
                    body: String::from_utf8_lossy(&bytes).to_string(),
                };
                req_clone.lock().unwrap().push(recorded);

                match action {
                    MockAction::StatusJson(status, resp_body) => Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Body::from(resp_body))
                        .unwrap(),
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
                    MockAction::Sequence(steps, idx) => {
                        let i = idx.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let (status, resp_body) = steps
                            .get(i)
                            .or_else(|| steps.last())
                            .cloned()
                            .unwrap_or((StatusCode::OK, "{}".to_string()));
                        Response::builder()
                            .status(status)
                            .header("content-type", "application/json")
                            .body(Body::from(resp_body))
                            .unwrap()
                    }
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

async fn spawn_proxy(core: Core) -> String {
    let svc = ProxyService::new(core);
    let app = svc.build_router();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

#[tokio::test]
async fn test_heterogeneous_failover_anthropic_to_responses_streaming() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let core = Core::with_paths(
        &home,
        &data,
        Arc::new(xfade_core::store::secrets::MockStore::default()),
    )
    .unwrap();

    // Upstream 1: Anthropic Native provider (fails with 500 Internal Server Error)
    let srv_anthropic = MockServer::spawn(MockAction::StatusJson(
        StatusCode::INTERNAL_SERVER_ERROR,
        r#"{"type":"error","error":{"type":"api_error","message":"anthropic backend down"}}"#
            .to_string(),
    ))
    .await;

    // Upstream 2: OpenAI Responses provider (succeeds with native Responses SSE stream)
    let srv_responses = MockServer::spawn(MockAction::Sse(
        StatusCode::OK,
        vec![
            "event: response.created\ndata: {\"response\":{\"id\":\"resp_999\",\"model\":\"gpt-4o\"}}".into(),
            "event: response.output_item.added\ndata: {\"output_item\":{\"type\":\"message\",\"role\":\"assistant\"}}".into(),
            "event: response.content_part.added\ndata: {\"part\":{\"type\":\"output_text\",\"text\":\"\"}}".into(),
            "event: response.output_text.delta\ndata: {\"delta\":\"Hello from OpenAI Responses upstream!\"}".into(),
            "event: response.content_part.done\ndata: {}".into(),
            "event: response.output_item.done\ndata: {}".into(),
            "event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"total_tokens\":42}}}".into(),
        ],
    ))
    .await;

    // Register providers with their respective native protocols
    let p1 = Provider::new(
        "primary-anthropic",
        ToolKind::ClaudeCode,
        Some(srv_anthropic.url.clone()),
    )
    .with_protocol(ProviderProtocol::Messages);
    let p2 = Provider::new(
        "backup-responses",
        ToolKind::Codex,
        Some(srv_responses.url.clone()),
    )
    .with_protocol(ProviderProtocol::Responses);

    core.db().upsert(&p1).unwrap();
    core.db().upsert(&p2).unwrap();

    // Configure cascade route: Primary -> Backup
    core.db()
        .set_routes(
            &["primary-anthropic".into(), "backup-responses".into()],
            None,
            "chat",
        )
        .unwrap();

    let proxy_url = spawn_proxy(core).await;

    // Client sends an Anthropic Messages streaming request to /v1/messages
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("content-type", "application/json")
        .body(r#"{"model":"claude-3-7-sonnet","messages":[{"role":"user","content":"hello cascade"}],"stream":true}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/event-stream"));

    let body_text = resp.text().await.unwrap();

    // Verify client received valid Anthropic SSE event frames
    assert!(body_text.contains("event: message_start"));
    assert!(body_text.contains("event: content_block_delta"));
    assert!(body_text.contains("Hello from OpenAI Responses upstream!"));
    assert!(body_text.contains("event: message_stop"));

    // Verify upstream 1 received request on /v1/messages and failed
    let reqs_1 = srv_anthropic.requests.lock().unwrap();
    assert_eq!(reqs_1.len(), 1);
    assert_eq!(reqs_1[0].path, "/v1/messages");

    // Verify upstream 2 received transcoded request on /v1/responses
    let reqs_2 = srv_responses.requests.lock().unwrap();
    assert_eq!(reqs_2.len(), 1);
    assert_eq!(reqs_2[0].path, "/v1/responses");
    let parsed_body: serde_json::Value = serde_json::from_str(&reqs_2[0].body).unwrap();
    assert!(parsed_body.get("input").is_some());
}

#[tokio::test]
async fn test_heterogeneous_responses_client_to_anthropic_upstream() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let core = Core::with_paths(
        &home,
        &data,
        Arc::new(xfade_core::store::secrets::MockStore::default()),
    )
    .unwrap();

    // Upstream: Anthropic provider returning native Anthropic message
    let srv_anthropic = MockServer::spawn(MockAction::StatusJson(
        StatusCode::OK,
        r#"{
            "id": "msg_anthropic_123",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Anthropic answers Responses client"}],
            "model": "claude-3-7-sonnet",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 15, "output_tokens": 8}
        }"#
        .to_string(),
    ))
    .await;

    let p = Provider::new(
        "anthropic-upstream",
        ToolKind::ClaudeCode,
        Some(srv_anthropic.url.clone()),
    )
    .with_protocol(ProviderProtocol::Messages);
    core.db().upsert(&p).unwrap();
    core.db()
        .set_routes(&["anthropic-upstream".into()], None, "chat")
        .unwrap();

    let proxy_url = spawn_proxy(core).await;

    // Client sends an OpenAI Responses request to /v1/responses
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_url}/v1/responses"))
        .header("content-type", "application/json")
        .body(r#"{"model":"gpt-4o","input":[{"role":"user","content":"hi from codex"}]}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body_text = resp.text().await.unwrap();

    // Verify response was transcoded into OpenAI Responses format
    let val: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(val["status"], "completed");
    assert!(body_text.contains("Anthropic answers Responses client"));

    // Verify upstream received request on /v1/messages formatted as Anthropic request
    let reqs = srv_anthropic.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/v1/messages");
    let parsed_body: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
    assert!(parsed_body.get("messages").is_some());
}

#[test]
fn test_cli_heterogeneous_protocol_management_and_status() {
    use assert_cmd::Command;
    use predicates::prelude::*;

    let home = tempfile::tempdir().unwrap();

    let xfade = |args: &[&str]| {
        let mut cmd = Command::cargo_bin("xfade").unwrap();
        cmd.env("HOME", home.path())
            .env("XFADE_DATA_DIR", home.path().join(".xfade-data"))
            .env("XFADE_SECRETS", "file")
            .args(args)
            .assert()
    };

    // 1. Add providers with distinct explicit protocols
    xfade(&[
        "add",
        "p-msg",
        "--tool",
        "claude",
        "--base-url",
        "https://api.anthropic.com/v1",
        "--protocol",
        "messages",
        "--key",
        "k1",
    ])
    .success();

    xfade(&[
        "add",
        "p-resp",
        "--tool",
        "codex",
        "--base-url",
        "https://api.openai.com/v1",
        "--protocol",
        "responses",
        "--key",
        "k2",
    ])
    .success();

    xfade(&[
        "add",
        "p-chat",
        "--tool",
        "opencode",
        "--base-url",
        "https://api.deepseek.com/v1",
        "--protocol",
        "chat",
        "--key",
        "k3",
    ])
    .success();

    // 2. Verify `xfade ls` table displays the protocol for each provider
    xfade(&["ls"])
        .success()
        .stdout(predicate::str::contains("messages"))
        .stdout(predicate::str::contains("responses"))
        .stdout(predicate::str::contains("chat"));

    // 3. Configure heterogeneous cascade route: p-msg -> p-resp -> p-chat
    xfade(&["proxy", "use", "p-msg", "p-resp", "p-chat"]).success();

    // 4. Verify `xfade status` reflects the full cascade
    xfade(&["status"])
        .success()
        .stdout(predicate::str::contains(
            "routes: p-msg -> p-resp -> p-chat",
        ));

    // 5. Switch p-chat protocol to messages via `xfade edit --protocol`
    xfade(&[
        "edit",
        "p-chat",
        "--tool",
        "opencode",
        "--protocol",
        "messages",
    ])
    .success();
    xfade(&["ls", "--tool", "opencode"])
        .success()
        .stdout(predicate::str::contains("p-chat"))
        .stdout(predicate::str::contains("messages"));
}

#[tokio::test]
async fn test_agy_gemini_inbound_transcodes_to_chat_upstream() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let core = Core::with_paths(
        &home,
        &data,
        Arc::new(xfade_core::store::secrets::MockStore::default()),
    )
    .unwrap();

    let up_chat = MockServer::spawn(MockAction::Sse(
        StatusCode::OK,
        vec![
            r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"Hello from Chat"}}]}"#.to_string(),
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":4}}"#.to_string(),
            "data: [DONE]".to_string(),
        ],
    ))
    .await;

    let p = Provider::new("oai", ToolKind::Agy, Some(up_chat.url.clone()))
        .with_protocol(ProviderProtocol::Chat);
    core.add_provider(p, Some("sk-upstream")).unwrap();
    core.db()
        .set_routes(&["oai".to_string()], Some("gpt-5"), "chat")
        .unwrap();

    let local_token = xfade_core::store::tokens::TokenManager::new(Arc::new(core.db().clone()))
        .get_or_create(ToolKind::Agy)
        .unwrap();
    let proxy_url = spawn_proxy(core).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{proxy_url}/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        ))
        .header("x-goog-api-key", &local_token)
        .json(&serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Hello from Chat"),
        "expected converted Gemini SSE text, got: {body}"
    );
    assert!(
        body.contains("\"finishReason\":\"STOP\""),
        "expected Gemini finishReason, got: {body}"
    );

    let reqs = up_chat.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/v1/chat/completions");
    let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
    assert_eq!(sent["model"], "gpt-5");
    assert_eq!(sent["stream"], true);
}

#[tokio::test]
async fn test_messages_proactive_strips_null_tool_extras() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let core = Core::with_paths(
        &home,
        &data,
        Arc::new(xfade_core::store::secrets::MockStore::default()),
    )
    .unwrap();

    let up = MockServer::spawn(MockAction::StatusJson(
        StatusCode::OK,
        r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":2}}"#.to_string(),
    ))
    .await;

    let p = Provider::new("anth", ToolKind::ClaudeCode, Some(up.url.clone()))
        .with_protocol(ProviderProtocol::Messages);
    core.add_provider(p, Some("sk-anth")).unwrap();
    core.db()
        .set_routes(&["anth".to_string()], Some("claude-sonnet-4-6"), "messages")
        .unwrap();

    let local_token = xfade_core::store::tokens::TokenManager::new(Arc::new(core.db().clone()))
        .get_or_create(ToolKind::ClaudeCode)
        .unwrap();
    let proxy_url = spawn_proxy(core).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("x-api-key", &local_token)
        .json(&serde_json::json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 128,
            "messages": [
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "bash",
                        "input": { "cmd": "pwd" },
                        "toolset_name": null,
                        "caller": null,
                        "non_null_caller": "computer"
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_1",
                        "content": "/tmp",
                        "extra_null": null
                    }]
                }
            ]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let reqs = up.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
    let tool_use = &sent["messages"][0]["content"][0];
    assert!(tool_use.get("toolset_name").is_none());
    assert!(tool_use.get("caller").is_none());
    assert_eq!(tool_use["non_null_caller"], "computer");
    assert!(sent["messages"][1]["content"][0]
        .get("extra_null")
        .is_none());
}

#[tokio::test]
async fn test_messages_400_extra_fields_retries_once_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let core = Core::with_paths(
        &home,
        &data,
        Arc::new(xfade_core::store::secrets::MockStore::default()),
    )
    .unwrap();

    let up = MockServer::spawn(MockAction::Sequence(
        vec![
            (
                StatusCode::BAD_REQUEST,
                r#"{"error":{"message":"litellm.BadRequestError: #/messages/0/content/0/tool_use.custom_extra: Extra inputs are not permitted"}}"#.to_string(),
            ),
            (
                StatusCode::OK,
                r#"{"id":"msg_2","type":"message","role":"assistant","content":[{"type":"text","text":"retried ok"}],"stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":2}}"#.to_string(),
            ),
        ],
        Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    ))
    .await;

    let p = Provider::new("anth", ToolKind::ClaudeCode, Some(up.url.clone()))
        .with_protocol(ProviderProtocol::Messages);
    core.add_provider(p, Some("sk-anth")).unwrap();
    core.db()
        .set_routes(&["anth".to_string()], Some("claude-sonnet-4-6"), "messages")
        .unwrap();

    let local_token = xfade_core::store::tokens::TokenManager::new(Arc::new(core.db().clone()))
        .get_or_create(ToolKind::ClaudeCode)
        .unwrap();
    let proxy_url = spawn_proxy(core).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("x-api-key", &local_token)
        .json(&serde_json::json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 128,
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "bash",
                    "input": { "cmd": "pwd" },
                    "custom_extra": "v1"
                }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(body.contains("retried ok"));

    let reqs = up.requests.lock().unwrap();
    assert_eq!(reqs.len(), 2, "expected strictly one retry on 400");
    let first: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
    let second: serde_json::Value = serde_json::from_str(&reqs[1].body).unwrap();
    assert_eq!(first["messages"][0]["content"][0]["custom_extra"], "v1");
    assert!(second["messages"][0]["content"][0]
        .get("custom_extra")
        .is_none());
}
