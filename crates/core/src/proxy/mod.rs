pub mod convert;
pub mod forward;
#[cfg(test)]
pub mod testsupport;
pub mod usage;

mod auth;

use crate::error::Result;
use crate::service::Core;
use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post},
    Router,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const FAIL_THRESHOLD: u32 = 3;
pub const COOLDOWN_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub struct Circuit {
    pub fails: u32,
    pub cooldown_until: Option<Instant>,
}

pub struct ProxyService {
    pub core: Core,
    pub client: reqwest::Client,
    pub circuits: Arc<Mutex<HashMap<String, Circuit>>>,
    pub auth_token: Option<String>,
    pub host: String,
    pub port: u16,
}

impl ProxyService {
    pub fn new(core: Core) -> Self {
        Self {
            core,
            client: reqwest::Client::new(),
            circuits: Arc::new(Mutex::new(HashMap::new())),
            auth_token: None,
            host: "127.0.0.1".to_string(),
            port: 0,
        }
    }

    pub fn with_host_port(mut self, host: impl Into<String>, port: u16) -> Self {
        self.host = host.into();
        self.port = port;
        self
    }

    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.auth_token = token;
        self
    }

    pub fn core(&self) -> &Core {
        &self.core
    }

    pub fn circuit_open(&self, provider_id: &str) -> bool {
        let circuits = self.circuits.lock().unwrap();
        if let Some(c) = circuits.get(provider_id) {
            if let Some(until) = c.cooldown_until {
                if until > Instant::now() {
                    return true;
                }
            }
        }
        false
    }

    pub fn record_success(&self, provider_id: &str) {
        let mut circuits = self.circuits.lock().unwrap();
        circuits.remove(provider_id);
    }

    pub fn record_failure(&self, provider_id: &str) {
        let mut circuits = self.circuits.lock().unwrap();
        let c = circuits.entry(provider_id.to_string()).or_insert(Circuit {
            fails: 0,
            cooldown_until: None,
        });
        c.fails += 1;
        if c.fails >= FAIL_THRESHOLD {
            c.cooldown_until = Some(Instant::now() + std::time::Duration::from_secs(COOLDOWN_SECS));
        }
    }

    pub fn circuit_status(&self, provider_id: &str) -> Option<Circuit> {
        self.circuits.lock().unwrap().get(provider_id).cloned()
    }

    /// 启动代理；`shutdown` future 完成时优雅关停。host/port 取自自身字段。
    pub async fn serve_with_shutdown(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let addr = format!("{}:{}", self.host, self.port);
        let app = self.build_router();
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| crate::error::CoreError::Proxy(format!("bind {addr}: {e}")))?;
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|e| crate::error::CoreError::Proxy(format!("serve: {e}")))?;
        Ok(())
    }

    /// CLI 用：阻塞运行直到中断（永不触发优雅关停信号）。
    pub async fn serve(self) -> Result<()> {
        self.serve_with_shutdown(std::future::pending::<()>()).await
    }

    pub fn build_router(self) -> Router {
        let state = Arc::new(self);
        let protected = Router::<Arc<ProxyService>>::new()
            .route("/v1/chat/completions", post(forward::handle))
            .route("/v1/responses", post(forward::handle))
            .route("/v1/messages", post(forward::handle))
            .route("/v1/models", get(forward::handle_get))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                auth::auth_guard,
            ));
        Router::<Arc<ProxyService>>::new()
            .route("/health", get(|| async { "ok" }))
            .merge(protected)
            .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
            .with_state(state)
    }
}

#[cfg(test)]
mod tests {
    use super::testsupport::*;
    use super::*;

    #[tokio::test]
    async fn forwards_chat_with_replaced_auth() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "sk-real")]);
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(
            200,
            r#"{"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        );
        let (status, body) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"gpt-x","messages":[]}"#,
            "Bearer sk-whatever",
        )
        .await;
        assert_eq!(status, 200);
        assert!(body.contains("prompt_tokens"));
        let seen = up.last_request().await;
        assert_eq!(seen.authorization, "Bearer sk-real");
        assert!(seen.body.contains("\"model\":\"gpt-x\""));
    }

    #[tokio::test]
    async fn no_route_returns_503() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let url = spawn_service(ProxyService::new(core)).await;
        let (status, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
        assert_eq!(status, 503);
    }

    #[tokio::test]
    async fn upstream_4xx_no_failover() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1"), ("bak", &up2.url(), "k2")]);
        core.db()
            .set_routes(&["yy".into(), "bak".into()], None, "chat")
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(403, r#"{"error":"denied"}"#);
        let (status, body) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
        assert_eq!(status, 403);
        assert!(body.contains("denied"));
        assert_eq!(up2.call_count().await, 0);
    }

    #[tokio::test]
    async fn auth_token_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core).with_auth_token(Some("t1".into()))).await;
        let (s1, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer wrong").await;
        assert_eq!(s1, 401);
        up.respond_with(200, "{}");
        let (s2, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer t1").await;
        assert_eq!(s2, 200);
    }

    #[tokio::test]
    async fn non_stream_usage_logged() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let db = core.db().clone();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(
            200,
            r#"{"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        );
        let _ = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m1","messages":[]}"#,
            "Bearer x",
        )
        .await;
        let stats = db
            .stats_since(
                "2020-01-01T00:00:00Z",
                crate::store::db::StatsGroupBy::Provider,
            )
            .unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].prompt_tokens, 10);
        assert_eq!(stats[0].completion_tokens, 5);
    }

    #[tokio::test]
    async fn responses_and_messages_reach_correct_upstream_path() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        // target=messages so /v1/messages is passthrough (no conversion);
        // /v1/responses is always passthrough regardless of target.
        core.db()
            .set_routes(&["yy".into()], None, "messages")
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;

        up.respond_with(200, "{}");
        let (s1, _) = http_post_json(&url, "/v1/responses", "{}", "Bearer x").await;
        assert_eq!(s1, 200);
        assert_eq!(up.last_request().await.path, "/responses");

        up.respond_with(200, "{}");
        let (s2, _) = http_post_json(&url, "/v1/messages", "{}", "Bearer x").await;
        assert_eq!(s2, 200);
        assert_eq!(up.last_request().await.path, "/messages");
    }

    #[tokio::test]
    async fn stream_chat_passthrough_and_usage() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        let db = core.db().clone();
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_sse(vec![
            r#"data: {"choices":[{"delta":{"content":"o"}}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":1}}"#,
            "data: [DONE]",
        ]);
        let (status, body) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[],"stream":true}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert!(body.contains("\"content\":\"o\""));
        assert!(body.contains("[DONE]"));
        let stats = db
            .stats_since(
                "2020-01-01T00:00:00Z",
                crate::store::db::StatsGroupBy::Provider,
            )
            .unwrap();
        assert_eq!(stats[0].prompt_tokens, 7);
        assert_eq!(stats[0].completion_tokens, 1);
        assert!(up.last_request().await.body.contains("include_usage"));
    }

    #[tokio::test]
    async fn failover_on_429_then_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1"), ("bak", &up2.url(), "k2")]);
        let db = core.db().clone();
        core.db()
            .set_routes(&["yy".into(), "bak".into()], None, "chat")
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(429, "rate limited");
        up2.respond_with(
            200,
            r#"{"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        );
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(up.call_count().await, 1);
        assert_eq!(up2.call_count().await, 1);
        let stats = db
            .stats_since(
                "2020-01-01T00:00:00Z",
                crate::store::db::StatsGroupBy::Provider,
            )
            .unwrap();
        assert_eq!(stats.iter().find(|s| s.group == "yy").unwrap().errors, 1);
        assert_eq!(stats.iter().find(|s| s.group == "bak").unwrap().requests, 1);
    }

    #[tokio::test]
    async fn circuit_opens_after_threshold_and_skips() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1"), ("bak", &up2.url(), "k2")]);
        core.db()
            .set_routes(&["yy".into(), "bak".into()], None, "chat")
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(500, "boom");
        up2.respond_with(200, "{}");
        for _ in 0..3 {
            let _ = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
        }
        let calls_after_3 = up.call_count().await;
        let _ = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
        assert_eq!(up.call_count().await, calls_after_3);
        assert!(up2.call_count().await >= 4);
    }

    #[tokio::test]
    async fn serve_with_shutdown_stops_and_releases_port() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        // 取一个空闲端口
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let core2 = core.clone();
        let handle = tokio::spawn(async move {
            ProxyService::new(core2)
                .with_host_port("127.0.0.1", port)
                .serve_with_shutdown(async move {
                    let _ = rx.await;
                })
                .await
        });
        // 等监听就绪
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let resp = reqwest::get(format!("http://127.0.0.1:{port}/health"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        tx.send(()).unwrap();
        handle.await.unwrap().unwrap();

        // 端口已释放：可重新绑定
        let rebind = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await;
        assert!(
            rebind.is_ok(),
            "port should be released after graceful shutdown"
        );
    }

    // ----- C4: anthropic→openai conversion end-to-end tests -----

    #[tokio::test]
    async fn serve_with_shutdown_uses_configured_host_port() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            ProxyService::new(core)
                .with_host_port("127.0.0.1", port)
                .serve_with_shutdown(async move {
                    let _ = rx.await;
                })
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let resp = reqwest::get(format!("http://127.0.0.1:{port}/health"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }

    // ----- C4: anthropic→openai conversion end-to-end tests -----

    #[tokio::test]
    async fn e2e_anthropic_to_openai_text() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db()
            .set_routes(&["yy".into()], Some("gpt-5.6-luna"), "chat")
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        // mock returns OpenAI-format response
        up.respond_with(
            200,
            r#"{"id":"r1","model":"gpt-x","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#,
        );
        // client sends Anthropic-format request
        let (status, body) = http_post_json(
            &url,
            "/v1/messages",
            r#"{"model":"claude-sonnet-4-6","max_tokens":100,"messages":[{"role":"user","content":"say ok"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"][0]["text"], "ok");
        assert_eq!(v["stop_reason"], "end_turn");
        // upstream received OpenAI-format + model_override
        let seen = up.last_request().await;
        assert_eq!(seen.path, "/chat/completions");
        let seen_v: serde_json::Value = serde_json::from_str(&seen.body).unwrap();
        assert_eq!(seen_v["model"], "gpt-5.6-luna");
        assert!(seen_v["messages"].is_array());
    }

    #[tokio::test]
    async fn e2e_anthropic_to_openai_streaming_tool_use() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db()
            .set_routes(&["yy".into()], Some("gpt-5.6-luna"), "chat")
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_sse(vec![
            r#"data: {"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"t1","type":"function","function":{"name":"ls","arguments":"{\"path\":\""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"/\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":4}}"#,
            "data: [DONE]",
        ]);
        let (status, body) = http_post_json(
            &url,
            "/v1/messages",
            r#"{"model":"m","max_tokens":100,"stream":true,"messages":[{"role":"user","content":"list files"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert!(body.contains("message_start"));
        assert!(body.contains(r#""type":"tool_use""#));
        assert!(body.contains(r#""id":"t1""#));
        assert!(body.contains(r#""name":"ls""#));
        assert!(body.contains(r#""input_json_delta""#));
        assert!(body.contains(r#""partial_json":"{\"path\":\"/\"}""#));
        assert!(body.contains("content_block_stop"));
        assert!(body.contains("message_stop"));
    }

    #[tokio::test]
    async fn target_messages_passthrough_regression() {
        // target=messages still goes through v0.2 passthrough, no conversion
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db()
            .set_routes(&["yy".into()], None, "messages")
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(
            200,
            r#"{"type":"message","content":[{"type":"text","text":"ok"}]}"#,
        );
        let (status, body) = http_post_json(
            &url,
            "/v1/messages",
            r#"{"model":"m","max_tokens":1,"messages":[{"role":"user","content":"x"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert!(body.contains(r#""type":"text","text":"ok""#));
        // upstream received the original Anthropic request (unconverted)
        let seen = up.last_request().await;
        assert_eq!(seen.path, "/messages");
        assert!(seen.body.contains(r#""role":"user""#));
    }

    #[tokio::test]
    async fn interrupted_stream_marked_in_logs() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        let db = core.db().clone();
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        // Emit two SSE lines then error mid-stream.
        up.respond_sse_error(vec![
            r#"data: {"choices":[{"delta":{"content":"o"}}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":1}}"#,
        ]);
        // Tolerate the mid-stream body error (lossy helper).
        let _ = http_post_json_lossy(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[],"stream":true}"#,
            "Bearer x",
        )
        .await;
        // Give the deferred stream-completion logger a moment to flush.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let logs = db.recent_request_logs(5).unwrap();
        assert!(!logs.is_empty(), "expected a request log row");
        let last = &logs[0];
        assert_eq!(last.endpoint, "chat");
        assert_eq!(last.provider_id, "yy");
        assert_eq!(last.status, 200);
        assert!(
            last.error.as_deref().unwrap_or("").contains("interrupted"),
            "expected error field set to 'upstream stream interrupted', got {:?}",
            last.error
        );
    }
}
