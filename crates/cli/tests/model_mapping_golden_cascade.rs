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
async fn test_heterogeneous_model_mapping_golden_cascade() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let core = Core::with_paths(
        &home,
        &data,
        Arc::new(xfade_core::store::secrets::MockStore::default()),
    )
    .unwrap();

    // Upstream 1: Anthropic Native Messages (fails with 429 Too Many Requests)
    let srv1_messages = MockServer::spawn(MockAction::StatusJson(
        StatusCode::TOO_MANY_REQUESTS,
        r#"{"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded"}}"#
            .to_string(),
    ))
    .await;

    // Upstream 2: OpenAI Chat (fails with 500 Internal Server Error)
    let srv2_chat = MockServer::spawn(MockAction::StatusJson(
        StatusCode::INTERNAL_SERVER_ERROR,
        r#"{"error":{"message":"chat backend error"}}"#.to_string(),
    ))
    .await;

    // Upstream 3: OpenAI Responses (succeeds with native Responses SSE stream)
    let srv3_responses = MockServer::spawn(MockAction::Sse(
        StatusCode::OK,
        vec![
            "event: response.created\ndata: {\"response\":{\"id\":\"resp_xyz\",\"model\":\"gpt-4o\"}}".into(),
            "event: response.output_item.added\ndata: {\"output_item\":{\"type\":\"message\",\"role\":\"assistant\"}}".into(),
            "event: response.content_part.added\ndata: {\"part\":{\"type\":\"output_text\",\"text\":\"\"}}".into(),
            "event: response.output_text.delta\ndata: {\"delta\":\"Heterogeneous model mapping cascade succeeded!\"}".into(),
            "event: response.content_part.done\ndata: {}".into(),
            "event: response.output_item.done\ndata: {}".into(),
            "event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"total_tokens\":35}}}".into(),
        ],
    ))
    .await;

    // 1. Register heterogeneous providers
    let p1 = Provider::new(
        "p_anthropic",
        ToolKind::ClaudeCode,
        Some(srv1_messages.url.clone()),
    )
    .with_protocol(ProviderProtocol::Messages);
    let p2 = Provider::new("p_chat", ToolKind::ClaudeCode, Some(srv2_chat.url.clone()))
        .with_protocol(ProviderProtocol::Chat);
    let p3 = Provider::new(
        "p_responses",
        ToolKind::ClaudeCode,
        Some(srv3_responses.url.clone()),
    )
    .with_protocol(ProviderProtocol::Responses);

    core.db().upsert(&p1).unwrap();
    core.db().upsert(&p2).unwrap();
    core.db().upsert(&p3).unwrap();

    // 2. Set route order: p_anthropic -> p_chat -> p_responses
    core.db()
        .set_routes(
            &["p_anthropic".into(), "p_chat".into(), "p_responses".into()],
            None,
            "chat",
        )
        .unwrap();

    // 3. Configure explicit model mapping for alias "sonnet"
    core.db()
        .set_model_mappings_batch(
            "sonnet",
            &[
                ("p_anthropic".into(), "claude-3-7-sonnet-20250219".into()),
                ("p_chat".into(), "deepseek-chat".into()),
                ("p_responses".into(), "gpt-4o".into()),
                ("*".into(), "fallback-sonnet-model".into()),
            ],
        )
        .unwrap();

    // 4. Also configure alias "haiku" with wildcard
    core.db()
        .set_model_mappings_batch(
            "haiku",
            &[
                ("p_anthropic".into(), "claude-3-5-haiku-20241022".into()),
                ("*".into(), "wildcard-haiku-target".into()),
            ],
        )
        .unwrap();

    let proxy_url = spawn_proxy(core.clone()).await;
    let client = reqwest::Client::new();

    // 5. Client dispatches request for alias "sonnet" to Anthropic-compatible /v1/messages
    let client_payload = serde_json::json!({
        "model": "sonnet",
        "stream": true,
        "max_tokens": 1024,
        "messages": [
            {"role": "user", "content": "Run cascade test"}
        ]
    });

    let resp = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("content-type", "application/json")
        .header("x-api-key", "test-client-key")
        .header("anthropic-version", "2023-06-01")
        .json(&client_payload)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let resp_body = resp.text().await.unwrap();

    // Verify Upstream 1 (p_anthropic Messages):
    // Received path /v1/messages, body with model "claude-3-7-sonnet-20250219"
    let reqs1 = srv1_messages.requests.lock().unwrap().clone();
    assert_eq!(reqs1.len(), 1, "srv1 must have received exactly 1 request");
    assert!(reqs1[0].path.contains("messages"));
    let body1: serde_json::Value = serde_json::from_str(&reqs1[0].body).unwrap();
    assert_eq!(
        body1["model"], "claude-3-7-sonnet-20250219",
        "p_anthropic must have received its mapped model ID"
    );

    // Verify Upstream 2 (p_chat Chat):
    // Transcoded to /v1/chat/completions, body with model "deepseek-chat"
    let reqs2 = srv2_chat.requests.lock().unwrap().clone();
    assert_eq!(reqs2.len(), 1, "srv2 must have received exactly 1 request");
    assert!(reqs2[0].path.contains("chat/completions"));
    let body2: serde_json::Value = serde_json::from_str(&reqs2[0].body).unwrap();
    assert_eq!(
        body2["model"], "deepseek-chat",
        "p_chat must have received its mapped model ID"
    );

    // Verify Upstream 3 (p_responses Responses):
    // Transcoded to /v1/responses, body with model "gpt-4o"
    let reqs3 = srv3_responses.requests.lock().unwrap().clone();
    assert_eq!(reqs3.len(), 1, "srv3 must have received exactly 1 request");
    assert!(reqs3[0].path.contains("responses"));
    let body3: serde_json::Value = serde_json::from_str(&reqs3[0].body).unwrap();
    assert_eq!(
        body3["model"], "gpt-4o",
        "p_responses must have received its mapped model ID"
    );

    // Verify client received successful stream transcoded to Anthropic format
    assert!(
        resp_body.contains("Heterogeneous model mapping cascade succeeded!"),
        "client should receive transcoded SSE text"
    );

    // 6. Test wildcard fallback: request with model "haiku"
    // p_chat has no explicit mapping under "haiku", so it must receive "*" mapping "wildcard-haiku-target"
    let haiku_payload = serde_json::json!({
        "model": "haiku",
        "messages": [
            {"role": "user", "content": "Test wildcard mapping"}
        ]
    });

    let resp_haiku = client
        .post(format!("{proxy_url}/v1/messages"))
        .header("content-type", "application/json")
        .header("x-api-key", "test-client-key")
        .header("anthropic-version", "2023-06-01")
        .json(&haiku_payload)
        .send()
        .await
        .unwrap();

    assert_eq!(resp_haiku.status(), StatusCode::OK);

    // Check second request received by srv2 (p_chat)
    let reqs2_after = srv2_chat.requests.lock().unwrap().clone();
    assert_eq!(reqs2_after.len(), 2);
    let body2_haiku: serde_json::Value = serde_json::from_str(&reqs2_after[1].body).unwrap();
    assert_eq!(
        body2_haiku["model"], "wildcard-haiku-target",
        "p_chat must receive wildcard '*' mapped model when unmapped"
    );
}

#[test]
fn test_cli_model_map_commands_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let data = dir.path().join("data");

    let bin = env!("CARGO_BIN_EXE_xfade");

    let run_cmd = |args: &[&str]| {
        std::process::Command::new(bin)
            .args(args)
            .env("HOME", &home)
            .env("XFADE_HOME", &home)
            .env("XFADE_DATA_DIR", &data)
            .output()
            .unwrap()
    };

    // 1. Initial list is empty
    let out = run_cmd(&["model-map", "ls"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no model mappings configured"));

    // 2. Add provider first so validation passes
    let out_add = run_cmd(&["add", "anthropic", "--key", "sk-test", "--tool", "claude"]);
    assert!(out_add.status.success());

    // 3. Set model mapping
    let out_set = run_cmd(&[
        "model-map",
        "set",
        "sonnet",
        "anthropic=claude-3-7-sonnet-20250219",
        "*=generic-sonnet",
    ]);
    assert!(out_set.status.success());
    let set_stdout = String::from_utf8_lossy(&out_set.stdout);
    assert!(set_stdout.contains("Updated model mapping for alias 'sonnet'"));

    // 4. List now shows sonnet table
    let out_ls = run_cmd(&["mm", "ls"]);
    assert!(out_ls.status.success());
    let ls_stdout = String::from_utf8_lossy(&out_ls.stdout);
    assert!(ls_stdout.contains("sonnet"));
    assert!(ls_stdout.contains("anthropic"));
    assert!(ls_stdout.contains("claude-3-7-sonnet-20250219"));

    // 5. Status shows model mappings
    let out_status = run_cmd(&["status"]);
    if !out_status.status.success() {
        eprintln!(
            "status failed: stdout: {}, stderr: {}",
            String::from_utf8_lossy(&out_status.stdout),
            String::from_utf8_lossy(&out_status.stderr)
        );
    }
    assert!(out_status.status.success());
    let status_stdout = String::from_utf8_lossy(&out_status.stdout);
    assert!(status_stdout.contains("model mappings:"));
    assert!(status_stdout.contains("sonnet =>"));

    // 6. Remove provider from alias
    let out_rm_p = run_cmd(&["model-map", "rm", "sonnet", "anthropic"]);
    assert!(out_rm_p.status.success());

    // 7. Clear all
    let out_clear = run_cmd(&["model-map", "clear"]);
    assert!(out_clear.status.success());
    let clear_stdout = String::from_utf8_lossy(&out_clear.stdout);
    assert!(clear_stdout.contains("Cleared all model mappings"));
}
