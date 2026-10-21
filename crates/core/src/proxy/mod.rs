pub mod convert;
pub mod forward;
#[cfg(test)]
pub mod testsupport;
pub mod usage;

mod auth;
pub mod status;

use crate::error::Result;
use crate::service::Core;
use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post},
    Router,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;

pub const FAIL_THRESHOLD: u32 = 3;
pub const COOLDOWN_SECS: u64 = 60;

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Circuit {
    pub fails: u32,
    /// Unix timestamp (seconds) when the cooldown period ends. 0 means no cooldown.
    pub cooldown_until_secs: i64,
}

impl Circuit {
    pub fn is_open(&self, now_secs: i64) -> bool {
        self.cooldown_until_secs > 0 && now_secs < self.cooldown_until_secs
    }
}

pub struct ProxyService {
    pub core: Core,
    pub client: reqwest::Client,
    pub circuits: Arc<Mutex<HashMap<String, Circuit>>>,
    pub auth_token: Option<String>,
    pub host: String,
    pub port: u16,
    /// Static-order planner (Smart Fader off).
    pub default_router: Arc<dyn crate::router::Router>,
    /// Intent-aware tiered planner (Smart Fader on).
    pub smart_router: Arc<dyn crate::router::Router>,
    /// TTL snapshot of per-provider health (error rate / latency) so the
    /// Smart Fader doesn't run a GROUP BY on every request.
    health_cache: Mutex<HealthSnapshot>,
}

/// Window for health aggregation and how long a snapshot stays fresh.
const HEALTH_WINDOW_SECS: i64 = 3600;
const HEALTH_TTL: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
struct HealthSnapshot {
    refreshed_at: Option<std::time::Instant>,
    signals: HashMap<String, crate::router::HealthSignal>,
}

impl HealthSnapshot {
    fn get(&self, id: &str) -> Option<crate::router::HealthSignal> {
        self.signals.get(id).copied()
    }

    fn is_fresh(&self) -> bool {
        self.refreshed_at
            .is_some_and(|at| at.elapsed() < HEALTH_TTL)
    }
}

impl ProxyService {
    pub fn new(core: Core) -> Self {
        // Restore persisted circuit state from database on startup.
        let circuits = core.db().load_circuits().unwrap_or_default();
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .pool_max_idle_per_host(4)
            .build()
            .expect("reqwest Client::builder should not fail");
        Self {
            core,
            client,
            circuits: Arc::new(Mutex::new(circuits)),
            auth_token: None,
            host: "127.0.0.1".to_string(),
            port: 0,
            default_router: Arc::new(crate::router::DefaultRouter),
            smart_router: Arc::new(crate::router::SmartRouter::default()),
            health_cache: Mutex::new(HealthSnapshot::default()),
        }
    }

    /// Pick the active router: re-read from the DB on every request so
    /// `xfade proxy smart on/off` reaches a running daemon immediately.
    /// Read failure falls back to the static router (fail-safe).
    fn active_router(&self) -> &dyn crate::router::Router {
        if self.core.db().smart_enabled().unwrap_or(false) {
            &*self.smart_router
        } else {
            &*self.default_router
        }
    }

    /// Provider health for within-bucket ordering. Served from a 30s TTL
    /// snapshot so per-request cost is a map lookup, not a GROUP BY.
    fn health_signal(&self, id: &str) -> Option<crate::router::HealthSignal> {
        {
            let snap = self.health_cache.lock().unwrap();
            if snap.is_fresh() {
                return snap.get(id);
            }
        }
        // Expired (or first call): refresh from request_logs. Best-effort —
        // on stats failure keep the stale snapshot rather than failing the
        // request path.
        let since = OffsetDateTime::now_utc()
            .checked_sub(time::Duration::seconds(HEALTH_WINDOW_SECS))
            .and_then(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
        if let Ok(rows) =
            self.core
                .db()
                .stats_since(&since, crate::store::db::StatsGroupBy::Provider, None)
        {
            let mut snap = self.health_cache.lock().unwrap();
            snap.signals = rows
                .into_iter()
                .filter(|r| r.requests > 0)
                .map(|r| {
                    (
                        r.group,
                        crate::router::HealthSignal {
                            error_rate: r.errors as f64 / r.requests as f64,
                            avg_ms: r.avg_duration_ms,
                        },
                    )
                })
                .collect();
            snap.refreshed_at = Some(std::time::Instant::now());
        }
        self.health_cache.lock().unwrap().get(id)
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

    pub fn circuit_open(&self, provider_id: &str) -> bool {
        let circuits = self.circuits.lock().unwrap();
        if let Some(c) = circuits.get(provider_id) {
            return c.is_open(now_secs());
        }
        false
    }

    pub fn record_success(&self, provider_id: &str) {
        let mut circuits = self.circuits.lock().unwrap();
        circuits.remove(provider_id);
        // Persist: remove from DB.
        let _ = self.core.db().delete_circuit(provider_id);
    }

    pub fn record_failure(&self, provider_id: &str) {
        let mut circuits = self.circuits.lock().unwrap();
        let now = now_secs();
        let c = circuits.entry(provider_id.to_string()).or_insert(Circuit {
            fails: 0,
            cooldown_until_secs: 0,
        });
        c.fails += 1;
        if c.fails >= FAIL_THRESHOLD {
            c.cooldown_until_secs = now + COOLDOWN_SECS as i64;
        }
        // Persist updated circuit state.
        let _ = self.core.db().save_circuit(provider_id, c);
    }

    /// Start the proxy; shut down gracefully when the `shutdown` future completes. host/port come from self fields.
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

    /// For the CLI: block until interrupted (never triggers a graceful shutdown signal).
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
            .route("/__xfade/status", get(status::handle))
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
    async fn passthrough_model_override_rewrites_model_field() {
        // chat → chat passthrough with a model_override must rewrite the
        // `model` field of the forwarded body. Local runtimes (MLX/Ollama)
        // are chat passthrough targets — dropping the override there would
        // silently route to the wrong local model.
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db()
            .set_routes(&["yy".into()], Some("qwen3-coder"), "chat")
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(
            200,
            r#"{"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        );
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"gpt-x","messages":[{"role":"user","content":"hi"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        let seen = up.last_request().await;
        assert!(
            seen.body.contains("\"model\":\"qwen3-coder\""),
            "model field not rewritten: {}",
            seen.body
        );
        assert!(!seen.body.contains("gpt-x"));
    }

    #[tokio::test]
    async fn passthrough_without_override_keeps_model_field() {
        // No model_override: the client's model must reach the upstream
        // untouched.
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(200, "{}");
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"gpt-x","messages":[]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        let seen = up.last_request().await;
        assert!(seen.body.contains("\"model\":\"gpt-x\""));
    }

    #[tokio::test]
    async fn smart_fader_routes_light_intent_local_first() {
        // Smart Fader on: an error-triage prompt must go to the local
        // (loopback) upstream even though the cloud provider is configured
        // first. Both mocks bind 127.0.0.1; tier separation comes from an
        // explicit `extra.tier` on the cloud provider.
        let dir = tempfile::tempdir().unwrap();
        let local = MockUpstream::spawn().await;
        let cloud = MockUpstream::spawn().await;
        let core = test_core(&dir, &[]);

        // Loopback URL → tier local via the base_url heuristic.
        let lp = crate::models::Provider::new(
            "local",
            crate::models::ToolKind::Codex,
            Some(local.url()),
        );
        core.add_provider(lp, Some("kl")).unwrap();

        let mut cp = crate::models::Provider::new(
            "cloud",
            crate::models::ToolKind::Codex,
            Some(cloud.url()),
        );
        cp.extra = serde_json::json!({"tier": "cloud"}); // explicit cloud
        core.add_provider(cp, Some("kc")).unwrap();

        core.db()
            .set_routes(&["cloud".into(), "local".into()], None, "chat")
            .unwrap();
        core.db().set_smart_enabled(true).unwrap();

        let url = spawn_service(ProxyService::new(core)).await;
        local.respond_with(200, "{}");
        cloud.respond_with(200, "{}");
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"user","content":"why does this fail to compile?"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(local.call_count().await, 1);
        assert_eq!(cloud.call_count().await, 0);
    }

    #[tokio::test]
    async fn smart_fader_off_keeps_configured_order() {
        // Smart Fader off (default): same setup, but the light-intent prompt
        // must hit the cloud provider first — the configured order wins.
        let dir = tempfile::tempdir().unwrap();
        let local = MockUpstream::spawn().await;
        let cloud = MockUpstream::spawn().await;
        let core = test_core(&dir, &[]);

        let lp = crate::models::Provider::new(
            "local",
            crate::models::ToolKind::Codex,
            Some(local.url()),
        );
        core.add_provider(lp, Some("kl")).unwrap();

        let mut cp = crate::models::Provider::new(
            "cloud",
            crate::models::ToolKind::Codex,
            Some(cloud.url()),
        );
        cp.extra = serde_json::json!({"tier": "cloud"});
        core.add_provider(cp, Some("kc")).unwrap();

        core.db()
            .set_routes(&["cloud".into(), "local".into()], None, "chat")
            .unwrap();

        let url = spawn_service(ProxyService::new(core)).await;
        local.respond_with(200, "{}");
        cloud.respond_with(200, "{}");
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"user","content":"why does this fail to compile?"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(cloud.call_count().await, 1);
        assert_eq!(local.call_count().await, 0);
    }

    #[tokio::test]
    async fn smart_fader_routes_heavy_intent_cloud_first() {
        // Smart Fader on: a big-refactor prompt must go to the cloud
        // provider even though the local provider is configured first.
        let dir = tempfile::tempdir().unwrap();
        let local = MockUpstream::spawn().await;
        let cloud = MockUpstream::spawn().await;
        let core = test_core(&dir, &[]);

        let lp = crate::models::Provider::new(
            "local",
            crate::models::ToolKind::Codex,
            Some(local.url()),
        );
        core.add_provider(lp, Some("kl")).unwrap();

        let mut cp = crate::models::Provider::new(
            "cloud",
            crate::models::ToolKind::Codex,
            Some(cloud.url()),
        );
        cp.extra = serde_json::json!({"tier": "cloud"});
        core.add_provider(cp, Some("kc")).unwrap();

        core.db()
            .set_routes(&["local".into(), "cloud".into()], None, "chat")
            .unwrap();
        core.db().set_smart_enabled(true).unwrap();

        let url = spawn_service(ProxyService::new(core)).await;
        local.respond_with(200, "{}");
        cloud.respond_with(200, "{}");
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"user","content":"restructure the whole proxy module into layers"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(cloud.call_count().await, 1);
        assert_eq!(local.call_count().await, 0);
    }

    #[tokio::test]
    async fn smart_fader_light_intent_local_500_fails_over_to_cloud() {
        // Smart Fader on, light intent: local is primary but returns 500 —
        // the failover loop must continue into the cloud bucket and the
        // request must succeed there.
        let dir = tempfile::tempdir().unwrap();
        let local = MockUpstream::spawn().await;
        let cloud = MockUpstream::spawn().await;
        let core = test_core(&dir, &[]);

        let lp = crate::models::Provider::new(
            "local",
            crate::models::ToolKind::Codex,
            Some(local.url()),
        );
        core.add_provider(lp, Some("kl")).unwrap();

        let mut cp = crate::models::Provider::new(
            "cloud",
            crate::models::ToolKind::Codex,
            Some(cloud.url()),
        );
        cp.extra = serde_json::json!({"tier": "cloud"});
        core.add_provider(cp, Some("kc")).unwrap();

        core.db()
            .set_routes(&["cloud".into(), "local".into()], None, "chat")
            .unwrap();
        core.db().set_smart_enabled(true).unwrap();

        let url = spawn_service(ProxyService::new(core)).await;
        local.respond_with(500, r#"{"error":"local model overloaded"}"#);
        cloud.respond_with(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
        );
        let (status, body) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"user","content":"why does this fail to compile?"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200, "body: {body}");
        assert_eq!(local.call_count().await, 1);
        assert_eq!(cloud.call_count().await, 1);
    }

    #[tokio::test]
    async fn smart_fader_circuit_open_local_falls_to_cloud_without_call() {
        // Smart Fader on, light intent, local circuit open after repeated
        // failures: the planner skips local entirely (no upstream call),
        // cloud serves the request.
        let dir = tempfile::tempdir().unwrap();
        let local = MockUpstream::spawn().await;
        let cloud = MockUpstream::spawn().await;
        let core = test_core(&dir, &[]);

        let lp = crate::models::Provider::new(
            "local",
            crate::models::ToolKind::Codex,
            Some(local.url()),
        );
        core.add_provider(lp, Some("kl")).unwrap();

        let mut cp = crate::models::Provider::new(
            "cloud",
            crate::models::ToolKind::Codex,
            Some(cloud.url()),
        );
        cp.extra = serde_json::json!({"tier": "cloud"});
        core.add_provider(cp, Some("kc")).unwrap();

        core.db()
            .set_routes(&["cloud".into(), "local".into()], None, "chat")
            .unwrap();
        core.db().set_smart_enabled(true).unwrap();

        let svc = ProxyService::new(core);
        // Blow the circuit: FAIL_THRESHOLD (3) consecutive failures.
        for _ in 0..super::FAIL_THRESHOLD {
            svc.record_failure("local");
        }
        assert!(svc.circuit_open("local"));

        let url = spawn_service(svc).await;
        cloud.respond_with(200, "{}");
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"user","content":"why does this fail to compile?"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(
            local.call_count().await,
            0,
            "circuit-open local must not be called"
        );
        assert_eq!(cloud.call_count().await, 1);
    }

    #[tokio::test]
    async fn smart_fader_toggle_flips_routing_without_restart() {
        // `xfade proxy smart on|off` writes proxy_state.smart_enabled and
        // the daemon re-reads it per request (active_router), so flipping
        // the flag mid-flight must reroute immediately — no restart.
        let dir = tempfile::tempdir().unwrap();
        let local = MockUpstream::spawn().await;
        let cloud = MockUpstream::spawn().await;
        let core = test_core(&dir, &[]);

        let lp = crate::models::Provider::new(
            "local",
            crate::models::ToolKind::Codex,
            Some(local.url()),
        );
        core.add_provider(lp, Some("kl")).unwrap();

        let mut cp = crate::models::Provider::new(
            "cloud",
            crate::models::ToolKind::Codex,
            Some(cloud.url()),
        );
        cp.extra = serde_json::json!({"tier": "cloud"});
        core.add_provider(cp, Some("kc")).unwrap();

        core.db()
            .set_routes(&["cloud".into(), "local".into()], None, "chat")
            .unwrap();
        // smart starts OFF (default).

        let handle = core.clone();
        let url = spawn_service(ProxyService::new(core)).await;
        cloud.respond_with(200, "{}");
        local.respond_with(200, "{}");

        let light = r#"{"model":"m","messages":[{"role":"user","content":"why does this fail to compile?"}]}"#;

        // OFF: configured order wins → cloud serves the light prompt.
        let (status, _) = http_post_json(&url, "/v1/chat/completions", light, "Bearer x").await;
        assert_eq!(status, 200);
        assert_eq!(cloud.call_count().await, 1);
        assert_eq!(local.call_count().await, 0);

        // Flip ON mid-flight: the next light prompt goes local-first.
        handle.db().set_smart_enabled(true).unwrap();
        let (status, _) = http_post_json(&url, "/v1/chat/completions", light, "Bearer x").await;
        assert_eq!(status, 200);
        assert_eq!(
            local.call_count().await,
            1,
            "smart on must reroute without restart"
        );
        assert_eq!(
            cloud.call_count().await,
            1,
            "no extra cloud call after flip"
        );

        // Flip back OFF: configured order wins again.
        handle.db().set_smart_enabled(false).unwrap();
        let (status, _) = http_post_json(&url, "/v1/chat/completions", light, "Bearer x").await;
        assert_eq!(status, 200);
        assert_eq!(cloud.call_count().await, 2);
        assert_eq!(
            local.call_count().await,
            1,
            "smart off must restore static order"
        );
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
                None,
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
        assert_eq!(up.last_request().await.path, "/v1/responses");

        up.respond_with(200, "{}");
        let (s2, _) = http_post_json(&url, "/v1/messages", "{}", "Bearer x").await;
        assert_eq!(s2, 200);
        assert_eq!(up.last_request().await.path, "/v1/messages");
    }

    #[tokio::test]
    async fn html_200_from_upstream_returns_actionable_502() {
        // Regression: a wrong base_url (missing /v1) makes gateways answer
        // 200 HTML (their web UI); relaying that broke Pi with a cryptic
        // "Stream ended without finish_reason".
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(200, "<!doctype html><html><body>web ui</body></html>");
        let (status, body) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[],"stream":true}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 502);
        assert!(body.contains("base_url"), "body: {body}");
        assert!(body.contains("yy"), "body should name the provider: {body}");
    }

    #[tokio::test]
    async fn empty_200_body_returns_502() {
        // Locks the guard's behavior for degenerate upstreams: a 200 with an
        // empty body is treated as a broken upstream, not relayed as success.
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(200, "");
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 502);
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
                None,
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
                None,
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
        // grab a free port
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
        // wait for the listener to be ready
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let resp = reqwest::get(format!("http://127.0.0.1:{port}/health"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        tx.send(()).unwrap();
        handle.await.unwrap().unwrap();

        // port released: can rebind
        let rebind = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await;
        assert!(
            rebind.is_ok(),
            "port should be released after graceful shutdown"
        );
    }

    #[tokio::test]
    async fn status_endpoint_returns_state() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db()
            .set_routes(&["yy".into()], Some("gpt-x"), "chat")
            .unwrap();
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
        let v: serde_json::Value = reqwest::get(format!("http://127.0.0.1:{port}/__xfade/status"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["running"], true);
        assert_eq!(v["port"], port);
        assert_eq!(v["auth_enabled"], false);
        assert_eq!(v["routes"][0], "yy");
        assert_eq!(v["model_override"], "gpt-x");
        assert_eq!(v["target_protocol"], "chat");
        tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn status_endpoint_requires_auth_token() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            ProxyService::new(core)
                .with_host_port("127.0.0.1", port)
                .with_auth_token(Some("t1".into()))
                .serve_with_shutdown(async move {
                    let _ = rx.await;
                })
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let s = reqwest::get(format!("http://127.0.0.1:{port}/__xfade/status"))
            .await
            .unwrap()
            .status();
        assert_eq!(s, 401);
        let s2 = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/__xfade/status"))
            .header("Authorization", "Bearer t1")
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(s2, 200);
        tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
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
    async fn e2e_responses_to_chat_streaming() {
        // Codex-shaped /v1/responses request converted to chat upstream.
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db()
            .set_routes(&["yy".into()], Some("glm-5-2-260617"), "chat")
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_sse(vec![
            r#"data: {"id":"chatcmpl-1","choices":[{"delta":{"content":"hi"}}]}"#,
            r#"data: {"id":"chatcmpl-1","choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}"#,
            "data: [DONE]",
        ]);
        let (status, body) = http_post_json(
            &url,
            "/v1/responses",
            r#"{"model":"glm-5-2-260617","stream":true,"instructions":"be brief","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert!(body.contains("response.created"), "body: {body}");
        assert!(body.contains(r#""delta":"hi""#));
        assert!(body.contains("response.output_item.done"));
        assert!(body.contains("response.completed"));
        assert!(body.contains(r#""input_tokens":2"#));
        // upstream received a chat request with instructions as system message
        let seen = up.last_request().await;
        assert_eq!(seen.path, "/v1/chat/completions");
        let seen_v: serde_json::Value = serde_json::from_str(&seen.body).unwrap();
        assert_eq!(seen_v["model"], "glm-5-2-260617");
        assert_eq!(seen_v["messages"][0]["role"], "system");
        assert_eq!(seen_v["messages"][0]["content"], "be brief");
        assert_eq!(seen_v["messages"][1]["content"], "hello");
    }

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
        assert_eq!(seen.path, "/v1/chat/completions");
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
        assert_eq!(seen.path, "/v1/messages");
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

    // ----- V2 session identity end-to-end -----

    #[tokio::test]
    async fn session_id_stable_across_turns_and_backfills_handover() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        let db = core.db().clone();
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_sequence(vec![
            (
                200,
                r#"{"id":"r1","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
            ),
            (
                200,
                r#"{"id":"r2","choices":[{"index":0,"message":{"role":"assistant","content":"ok2"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":3}}"#,
            ),
        ]);

        // Turn 1 of a conversation.
        let (status, headers, _body) = http_post_json_with_headers(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"system","content":"s"},{"role":"user","content":"hello"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        let sid = headers
            .iter()
            .find(|(k, _)| k == "x-xfade-session-id")
            .expect("session id header on turn 1")
            .1
            .clone();
        assert!(!sid.is_empty());

        // A mid-session model switch (`xfade proxy use --model` records this;
        // session_id starts NULL).
        crate::sessions::record_handover(&db, "", Some("old-m"), Some("new-m")).unwrap();

        // Turn 2: same opener, grown history → same session id.
        let (status, headers2, _body) = http_post_json_with_headers(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"system","content":"s"},{"role":"user","content":"hello"},{"role":"assistant","content":"ok"},{"role":"user","content":"and now?"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        let sid2 = headers2
            .iter()
            .find(|(k, _)| k == "x-xfade-session-id")
            .expect("session id header on turn 2")
            .1
            .clone();
        assert_eq!(
            sid, sid2,
            "same opener within the sticky window keeps the session"
        );

        // Both request-log rows carry the session id.
        let logs = db.recent_request_logs(5).unwrap();
        assert_eq!(logs.len(), 2);
        assert!(logs
            .iter()
            .all(|l| l.session_id.as_deref() == Some(sid.as_str())));

        // The session row folded in both turns.
        let sessions = db.list_sessions(10).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, sid);
        assert_eq!(sessions[0].prompt_tokens, 15);
        assert_eq!(sessions[0].completion_tokens, 5);
        assert_eq!(sessions[0].status, "ok");
        assert_eq!(sessions[0].tool, "codex"); // provider yy is registered under codex
        assert_eq!(sessions[0].provider_id.as_deref(), Some("yy"));
        assert_eq!(sessions[0].first_prompt.as_deref(), Some("s\nhello"));

        // The mid-session handover got backfilled onto this session.
        let hs = db.list_handovers(&sid).unwrap();
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].from_model.as_deref(), Some("old-m"));
        assert_eq!(hs[0].to_model.as_deref(), Some("new-m"));
    }

    #[tokio::test]
    async fn distinct_conversations_get_distinct_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        let db = core.db().clone();
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(
            200,
            r#"{"id":"r","choices":[{"message":{"role":"assistant","content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        );

        for opener in ["first question", "a different question"] {
            let body =
                format!(r#"{{"model":"m","messages":[{{"role":"user","content":"{opener}"}}]}}"#);
            let (status, headers, _) =
                http_post_json_with_headers(&url, "/v1/chat/completions", &body, "Bearer x").await;
            assert_eq!(status, 200);
            assert!(headers
                .iter()
                .any(|(k, v)| k == "x-xfade-session-id" && !v.is_empty()));
        }
        // Two openers → two sessions.
        assert_eq!(db.list_sessions(10).unwrap().len(), 2);
    }
}
