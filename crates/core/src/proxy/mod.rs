pub mod cache_injector;
pub mod circuit_breaker;
pub mod convert;
pub mod forward;
pub mod pricing;
pub mod pricing_sync;
pub mod robustness;
#[cfg(test)]
pub mod testsupport;
pub mod thinking_rectifier;
pub mod usage;

mod auth;
pub mod status;

pub use auth::ClientIdentity;
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
pub use pricing_sync::{sync_pricing_catalog, SyncStats, DEFAULT_OPENROUTER_MODELS_URL};

use crate::error::Result;
use crate::service::Core;
use crate::store::tokens::TokenManager;
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
    pub token_manager: Arc<TokenManager>,
    pub client: reqwest::Client,
    pub circuit_breaker: Arc<CircuitBreaker>,
    pub timeouts: robustness::TimeoutConfig,
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
        let circuit_breaker = Arc::new(CircuitBreaker::default());
        // Restore persisted circuit state from database on startup.
        if let Ok(circuits) = core.db().load_circuits() {
            let now = now_secs();
            for (id, c) in circuits {
                if c.is_open(now) {
                    for _ in 0..c.fails.max(FAIL_THRESHOLD) {
                        circuit_breaker.record_failure(&id);
                    }
                } else {
                    let _ = core.db().delete_circuit(&id);
                }
            }
        }
        let timeouts = robustness::TimeoutConfig::default();
        let client = reqwest::Client::builder()
            .connect_timeout(timeouts.connect_timeout)
            .pool_max_idle_per_host(4)
            .build()
            .expect("reqwest Client::builder should not fail");
        let token_manager = Arc::new(TokenManager::new(Arc::new(core.db().clone())));
        Self {
            core,
            token_manager,
            client,
            circuit_breaker,
            timeouts,
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
        // An empty token means "none", never "accept an empty credential".
        self.auth_token = token.filter(|t| !t.is_empty());
        self
    }

    pub fn with_circuit_breaker(mut self, cb: Arc<CircuitBreaker>) -> Self {
        self.circuit_breaker = cb;
        self
    }

    pub fn with_timeouts(mut self, timeouts: robustness::TimeoutConfig) -> Self {
        self.timeouts = timeouts;
        self
    }

    pub fn circuit_open(&self, provider_id: &str) -> bool {
        match self.circuit_breaker.state(provider_id) {
            CircuitState::Closed | CircuitState::HalfOpen => false,
            CircuitState::Open => {
                if let Some(remaining) = self.circuit_breaker.remaining_cooldown_secs(provider_id) {
                    remaining > 0
                } else {
                    false
                }
            }
        }
    }

    pub fn record_success(&self, provider_id: &str) {
        self.circuit_breaker.record_success(provider_id);
        if self.circuit_breaker.state(provider_id) == CircuitState::Closed {
            let _ = self.core.db().delete_circuit(provider_id);
        }
    }

    pub fn record_failure(&self, provider_id: &str) {
        Self::record_failure_static(&self.circuit_breaker, self.core.db(), provider_id);
    }

    pub fn record_failure_static(
        circuit_breaker: &CircuitBreaker,
        db: &crate::store::db::Database,
        provider_id: &str,
    ) {
        circuit_breaker.record_failure(provider_id);
        let fails = circuit_breaker.failure_count(provider_id);
        let cooldown_remaining = circuit_breaker
            .remaining_cooldown_secs(provider_id)
            .unwrap_or(0);
        let cooldown_until_secs = if cooldown_remaining > 0 {
            now_secs() + cooldown_remaining as i64
        } else {
            0
        };
        let c = Circuit {
            fails,
            cooldown_until_secs,
        };
        let _ = db.save_circuit(provider_id, &c);
    }

    /// Start the proxy; shut down gracefully when the `shutdown` future completes. host/port come from self fields.
    pub async fn serve_with_shutdown(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        if !auth::is_loopback_host(&self.host) && self.auth_token.is_none() {
            return Err(crate::error::CoreError::Proxy(format!(
                "refusing to listen on non-loopback address '{}' without an auth token; \
                 pass --auth-token-file <path> (or bind 127.0.0.1)",
                self.host
            )));
        }
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
            .route(
                "/health",
                get(|| async {
                    axum::Json(serde_json::json!({
                        "status": "ok",
                        "app": "xfade"
                    }))
                }),
            )
            .merge(protected)
            .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                auth::host_origin_guard,
            ))
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

    async fn raw_get(url: &str, path: &str, headers: &[(&str, &str)]) -> u16 {
        let mut req = reqwest::Client::new().get(format!("{url}{path}"));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.send().await.unwrap().status().as_u16()
    }

    #[tokio::test]
    async fn loopback_listener_rejects_foreign_host_and_origin() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let url = spawn_service(ProxyService::new(core)).await;
        // DNS-rebinding style: attacker hostname resolving to 127.0.0.1.
        assert_eq!(
            raw_get(&url, "/__xfade/status", &[("Host", "evil.example:24860")]).await,
            403
        );
        // Cross-site browser request.
        assert_eq!(
            raw_get(
                &url,
                "/__xfade/status",
                &[("Origin", "https://evil.example")]
            )
            .await,
            403
        );
        assert_eq!(
            raw_get(&url, "/__xfade/status", &[("Origin", "null")]).await,
            403
        );
        // /health is covered too.
        assert_eq!(
            raw_get(&url, "/health", &[("Host", "evil.example")]).await,
            403
        );
        // Legitimate local clients still work.
        assert_eq!(raw_get(&url, "/__xfade/status", &[]).await, 200);
        assert_eq!(
            raw_get(
                &url,
                "/__xfade/status",
                &[("Origin", "http://localhost:3000")]
            )
            .await,
            200
        );
        assert_eq!(
            raw_get(&url, "/__xfade/status", &[("Host", "localhost:24860")]).await,
            200
        );
    }

    #[tokio::test]
    async fn token_accepts_bearer_or_x_api_key_and_rejects_others() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let url =
            spawn_service(ProxyService::new(core).with_auth_token(Some("tok-123".into()))).await;
        assert_eq!(raw_get(&url, "/__xfade/status", &[]).await, 401);
        assert_eq!(
            raw_get(&url, "/__xfade/status", &[("Authorization", "Bearer nope")]).await,
            401
        );
        assert_eq!(
            raw_get(&url, "/__xfade/status", &[("x-api-key", "nope")]).await,
            401
        );
        assert_eq!(
            raw_get(
                &url,
                "/__xfade/status",
                &[("Authorization", "Bearer tok-123")]
            )
            .await,
            200
        );
        assert_eq!(
            raw_get(&url, "/__xfade/status", &[("x-api-key", "tok-123")]).await,
            200
        );
        // /health stays open for liveness probes.
        assert_eq!(raw_get(&url, "/health", &[]).await, 200);
    }

    #[tokio::test]
    async fn empty_token_is_treated_as_unset() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let svc = ProxyService::new(core).with_auth_token(Some(String::new()));
        assert!(svc.auth_token.is_none());
    }

    #[tokio::test]
    async fn non_loopback_bind_requires_token() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let err = ProxyService::new(core)
            .with_host_port("0.0.0.0", 0)
            .serve_with_shutdown(async {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("non-loopback"), "{err}");
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
        // Default off: even a qualifying 4xx is passed through unchanged.
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
    async fn per_route_models_reach_each_upstream() {
        // Heterogeneous upstreams: `--route-model` gives each provider its
        // own valid model. The primary 500 forces failover so both bodies
        // are observable and must carry different `model` values.
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(
            &dir,
            &[("freebuff", &up.url(), "k1"), ("omni", &up2.url(), "k2")],
        );
        let mut models: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        models.insert("freebuff".into(), "z-ai/glm-5.3-flash".into());
        models.insert("omni".into(), "auto/best-fast".into());
        core.db()
            .set_routes_with_models(&["freebuff".into(), "omni".into()], None, "chat", &models)
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(500, "boom");
        up2.respond_with(
            200,
            r#"{"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        );
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"client-model","messages":[{"role":"user","content":"hi"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            up.last_request()
                .await
                .body
                .contains(r#""model":"z-ai/glm-5.3-flash""#),
            "primary did not receive its per-route model"
        );
        assert!(
            up2.last_request()
                .await
                .body
                .contains(r#""model":"auto/best-fast""#),
            "fallback did not receive its per-route model"
        );
    }

    #[tokio::test]
    async fn route_model_overrides_global_fallback() {
        // A provider with no explicit entry falls back to the global --model.
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1"), ("bak", &up2.url(), "k2")]);
        let mut models: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        models.insert("yy".into(), "specific-model".into());
        core.db()
            .set_routes_with_models(
                &["yy".into(), "bak".into()],
                Some("global-fallback"),
                "chat",
                &models,
            )
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(500, "boom");
        up2.respond_with(200, "{}");
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"client-model","messages":[]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert!(up
            .last_request()
            .await
            .body
            .contains(r#""model":"specific-model""#));
        assert!(up2
            .last_request()
            .await
            .body
            .contains(r#""model":"global-fallback""#));
    }

    #[tokio::test]
    async fn failover_4xx_on_fails_over_on_403() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1"), ("bak", &up2.url(), "k2")]);
        core.db()
            .set_routes(&["yy".into(), "bak".into()], None, "chat")
            .unwrap();
        core.db().set_failover_4xx(true).unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(403, r#"{"error":"denied"}"#);
        up2.respond_with(200, r#"{"usage":{}}"#);
        let (status, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
        assert_eq!(status, 200);
        assert_eq!(up.call_count().await, 1);
        assert_eq!(up2.call_count().await, 1);
    }

    #[tokio::test]
    async fn failover_4xx_on_fails_over_on_model_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1"), ("bak", &up2.url(), "k2")]);
        core.db()
            .set_routes(&["yy".into(), "bak".into()], None, "chat")
            .unwrap();
        core.db().set_failover_4xx(true).unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(
            400,
            r#"{"error":{"code":"model_not_allowed","message":"not in model list"}}"#,
        );
        up2.respond_with(200, r#"{"usage":{}}"#);
        let (status, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
        assert_eq!(status, 200);
        assert_eq!(up2.call_count().await, 1);
    }

    #[tokio::test]
    async fn failover_4xx_on_ignores_non_qualifying_4xx() {
        // Even with the toggle on, a generic 400/404 is a client error and
        // must not be retried against every fallback.
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1"), ("bak", &up2.url(), "k2")]);
        core.db()
            .set_routes(&["yy".into(), "bak".into()], None, "chat")
            .unwrap();
        core.db().set_failover_4xx(true).unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(404, "not found");
        let (status, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
        assert_eq!(status, 404);
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
        let core = test_core(
            &dir,
            &[("p_resp", &up.url(), "k1"), ("p_msg", &up.url(), "k2")],
        );
        let p_resp = core
            .db()
            .get(crate::models::ToolKind::Codex, "p_resp")
            .unwrap()
            .unwrap()
            .with_protocol(crate::models::ProviderProtocol::Responses);
        core.db().upsert(&p_resp).unwrap();
        let p_msg = core
            .db()
            .get(crate::models::ToolKind::Codex, "p_msg")
            .unwrap()
            .unwrap()
            .with_protocol(crate::models::ProviderProtocol::Messages);
        core.db().upsert(&p_msg).unwrap();

        // Route pointing to Responses provider -> /v1/responses reaches /v1/responses
        core.db()
            .set_routes(&["p_resp".into()], None, "responses")
            .unwrap();
        let url = spawn_service(ProxyService::new(core.clone())).await;

        up.respond_with(200, "{}");
        let (s1, _) = http_post_json(&url, "/v1/responses", "{}", "Bearer x").await;
        assert_eq!(s1, 200);
        assert_eq!(up.last_request().await.path, "/v1/responses");

        // Route pointing to Messages provider -> /v1/messages reaches /v1/messages
        core.db()
            .set_routes(&["p_msg".into()], None, "messages")
            .unwrap();
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
        core.db().set_handover_auto_inject_enabled(true).unwrap();
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

        // The mid-session handover got backfilled onto this session and injected.
        let hs = db.list_handovers(&sid).unwrap();
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].from_model.as_deref(), Some("old-m"));
        assert_eq!(hs[0].to_model.as_deref(), Some("new-m"));
        assert!(hs[0].injected, "handover should be marked as injected");

        // The upstream received user-level handover context on Turn 2; system untouched.
        let turn2_req = up.last_request().await;
        let turn2: serde_json::Value = serde_json::from_str(&turn2_req.body).unwrap();
        assert_eq!(turn2["messages"][0]["content"], "s");
        assert!(
            turn2["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap()
                .contains("<xfade_handover_context>"),
            "Turn 2 last user message should carry injected handover context"
        );
        assert!(
            turn2_req.body.contains("Model Transition: old-m -> new-m"),
            "Turn 2 should include model transition info"
        );

        // Turn 3: No new model switch → no re-injection.
        up.respond_sequence(vec![(
            200,
            r#"{"id":"r3","choices":[{"index":0,"message":{"role":"assistant","content":"ok3"},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":4}}"#,
        )]);
        let (status, _headers3, _body) = http_post_json_with_headers(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"system","content":"s"},{"role":"user","content":"hello"},{"role":"assistant","content":"ok"},{"role":"user","content":"and now?"},{"role":"assistant","content":"ok2"},{"role":"user","content":"turn 3"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        let turn3_req = up.last_request().await;
        assert!(
            !turn3_req.body.contains("<xfade_handover_context>"),
            "Turn 3 body should NOT re-inject handover context"
        );
    }

    #[tokio::test]
    async fn test_proxy_records_cost_in_request_logs_and_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        let db = core.db().clone();
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core)).await;

        // Turn 1: 10_000 prompt tokens (2_000 cached), 1_000 completion tokens
        // For claude-3-7-sonnet:
        // uncached prompt: 8,000 * $3.00/M = $0.0240 = 24,000 micros
        // cached prompt:   2,000 * $0.30/M = $0.0006 =    600 micros
        // completion:      1,000 * $15.00/M = $0.0150 = 15,000 micros
        // Total Turn 1 = 39,600 micros ($0.0396)
        up.respond_sequence(vec![(
            200,
            r#"{"id":"r1","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10000,"completion_tokens":1000,"prompt_tokens_details":{"cached_tokens":2000}}}"#,
        )]);

        let (status, headers, _body) = http_post_json_with_headers(
            &url,
            "/v1/chat/completions",
            r#"{"model":"claude-3-7-sonnet","messages":[{"role":"user","content":"hello"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        let sid = headers
            .iter()
            .find(|(k, _)| k == "x-xfade-session-id")
            .expect("session id header")
            .1
            .clone();

        let logs = db.recent_request_logs(1).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].cost_usd_micros, 39_600);

        let sessions = db.list_sessions(1).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, sid);
        assert_eq!(sessions[0].cost_usd_micros, 39_600);

        // Streaming Turn 2: same session, gpt-4o, SSE stream with usage tail
        // 1,000 prompt tokens (no cache), 500 completion tokens
        // For gpt-4o:
        // prompt:     1,000 * $2.50/M = $0.0025 = 2,500 micros
        // completion:   500 * $10.00/M = $0.0050 = 5,000 micros
        // Turn 2 = 7,500 micros ($0.0075)
        // Accumulated Session cost = 39,600 + 7,500 = 47,100 micros ($0.0471)
        up.clear_responses();
        up.respond_sse(vec![
            r#"data: {"choices":[{"delta":{"content":"streaming text"}}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":500}}"#,
            r#"data: [DONE]"#,
        ]);

        let (status, _headers, _body) = http_post_json_with_headers(
            &url,
            "/v1/chat/completions",
            r#"{"model":"gpt-4o","stream":true,"messages":[{"role":"user","content":"hello"},{"role":"assistant","content":"ok"},{"role":"user","content":"streaming please"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);

        // Give stream logger moment to flush
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let logs = db.recent_request_logs(2).unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].cost_usd_micros, 7_500);

        let sessions = db.list_sessions(1).unwrap();
        assert_eq!(sessions[0].cost_usd_micros, 47_100);
    }

    #[tokio::test]
    async fn test_proxy_streaming_large_response_preserves_head_usage() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        let db = core.db().clone();
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        let url = spawn_service(ProxyService::new(core)).await;

        // Simulate Anthropic streaming:
        // 1. message_start with 50 input_tokens, 20,000 cache_read_input_tokens
        // 2. Many output text chunks totaling > 15 KB to force eviction from an 8 KB tail buffer
        // 3. message_delta with cumulative output_tokens = 500
        let mut sse_lines = Vec::new();
        sse_lines.push("event: message_start".to_string());
        sse_lines.push(r#"data: {"type":"message_start","message":{"usage":{"input_tokens":50,"cache_read_input_tokens":20000}}}"#.to_string());

        // 100 chunks of 150 bytes = 15,000 bytes > 8KB SSE_TAIL_CAP
        for _ in 0..100 {
            sse_lines.push(format!(
                r#"data: {{"choices":[{{"delta":{{"content":"{}"}}}}]}}"#,
                "a".repeat(150)
            ));
        }
        sse_lines.push("event: message_delta".to_string());
        sse_lines
            .push(r#"data: {"type":"message_delta","usage":{"output_tokens":500}}"#.to_string());
        sse_lines.push("data: [DONE]".to_string());

        let lines_ref: Vec<&str> = sse_lines.iter().map(|s| s.as_str()).collect();
        up.respond_sse(lines_ref);

        let (status, _headers, _body) = http_post_json_with_headers(
            &url,
            "/v1/chat/completions",
            r#"{"model":"claude-3-7-sonnet","stream":true,"messages":[{"role":"user","content":"write long text"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);

        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let logs = db.recent_request_logs(1).unwrap();
        assert_eq!(logs.len(), 1);
        // Prompt tokens should not be lost despite tail buffer overflow!
        // Total prompt: 50 uncached + 20,000 cached = 20,050
        assert_eq!(logs[0].prompt_tokens, 20_050);
        assert_eq!(logs[0].completion_tokens, 500);
        // Cost:
        // uncached: 50 * $3.00/M = 150 micros
        // cached read: 20,000 * $0.30/M = 6,000 micros
        // completion: 500 * $15.00/M = 7,500 micros
        // Total = 150 + 6,000 + 7,500 = 13,650 micros
        assert_eq!(logs[0].cost_usd_micros, 13_650);
    }

    #[tokio::test]
    async fn test_proxy_calculates_cost_with_model_override_and_alias() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        let db = core.db().clone();
        // Route model override sets model to "claude-3-7-sonnet"
        core.db()
            .set_routes_with_models(
                &["yy".into()],
                Some("claude-3-7-sonnet"),
                "chat",
                &Default::default(),
            )
            .unwrap();
        let url = spawn_service(ProxyService::new(core)).await;

        up.respond_sequence(vec![(
            200,
            r#"{"id":"r1","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1000,"completion_tokens":200}}"#,
        )]);

        // Client request specifies generic alias "sonnet"
        let (status, _headers, _body) = http_post_json_with_headers(
            &url,
            "/v1/chat/completions",
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);

        let logs = db.recent_request_logs(1).unwrap();
        assert_eq!(logs.len(), 1);
        // Cost should be calculated on claude-3-7-sonnet: 1000 * 3 + 200 * 15 = 3000 + 3000 = 6000 micros
        assert_eq!(logs[0].cost_usd_micros, 6000);
    }

    #[tokio::test]
    async fn handover_auto_injection_off_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        let db = core.db().clone();
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        assert!(!db.handover_auto_inject_enabled().unwrap());
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

        let (status, _headers, _body) = http_post_json_with_headers(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"system","content":"s"},{"role":"user","content":"hello"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);

        crate::sessions::record_handover(&db, "", Some("old-m"), Some("new-m")).unwrap();

        let (status, _headers2, _body) = http_post_json_with_headers(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"system","content":"s"},{"role":"user","content":"hello"},{"role":"assistant","content":"ok"},{"role":"user","content":"and now?"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        let turn2_req = up.last_request().await;
        assert!(
            !turn2_req.body.contains("<xfade_handover_context>"),
            "default-off must not rewrite upstream body"
        );
    }

    #[tokio::test]
    async fn handover_not_consumed_when_upstream_fails() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        let db = core.db().clone();
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        core.db().set_handover_auto_inject_enabled(true).unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        let ok = r#"{"id":"r","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
        up.respond_sequence(vec![(200, ok), (500, r#"{"error":"boom"}"#), (200, ok)]);

        let (status, headers, _) = http_post_json_with_headers(
            &url,
            "/v1/chat/completions",
            r#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        let sid = headers
            .iter()
            .find(|(k, _)| k == "x-xfade-session-id")
            .unwrap()
            .1
            .clone();
        crate::sessions::record_handover(&db, "", Some("a"), Some("b")).unwrap();

        let turn2 = r#"{"model":"m","messages":[{"role":"user","content":"hello"},{"role":"assistant","content":"ok"},{"role":"user","content":"next"}]}"#;
        let (status, _, _) =
            http_post_json_with_headers(&url, "/v1/chat/completions", turn2, "Bearer x").await;
        assert_ne!(status, 200, "upstream 500 must surface");
        assert!(
            !db.list_handovers(&sid).unwrap().iter().any(|h| h.injected),
            "failed request must not consume the handover"
        );
        let cutoff = crate::sessions::cutoff_rfc3339(crate::sessions::SESSION_STICKY_WINDOW_SECS);
        assert!(db
            .find_pending_handover(Some(&sid), &cutoff)
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn handover_auto_injection_anthropic_messages_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        let db = core.db().clone();
        core.db()
            .set_routes(&["yy".into()], None, "messages")
            .unwrap();
        core.db().set_handover_auto_inject_enabled(true).unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_sequence(vec![
            (
                200,
                r#"{"id":"m1","type":"message","role":"assistant","content":[{"type":"text","text":"hello"}],"model":"claude-3-7-sonnet","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2}}"#,
            ),
            (
                200,
                r#"{"id":"m2","type":"message","role":"assistant","content":[{"type":"text","text":"sure"}],"model":"claude-3-7-sonnet","stop_reason":"end_turn","usage":{"input_tokens":25,"output_tokens":4}}"#,
            ),
        ]);

        // Turn 1
        let (status, headers, _body) = http_post_json_with_headers(
            &url,
            "/v1/messages",
            r#"{"model":"claude-3-7-sonnet","system":"You are Claude.","messages":[{"role":"user","content":"hi"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        let sid = headers
            .iter()
            .find(|(k, _)| k == "x-xfade-session-id")
            .expect("session id on turn 1")
            .1
            .clone();

        // Model switch during session
        crate::sessions::record_handover(
            &db,
            "claude",
            Some("claude-3-5-sonnet"),
            Some("claude-3-7-sonnet"),
        )
        .unwrap();

        // Turn 2
        let (status, _headers2, _body) = http_post_json_with_headers(
            &url,
            "/v1/messages",
            r#"{"model":"claude-3-7-sonnet","system":"You are Claude.","messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"},{"role":"user","content":"help me write code"}]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);

        // Verify upstream received user-level handover context; system untouched.
        let req2 = up.last_request().await;
        let v: serde_json::Value = serde_json::from_str(&req2.body).unwrap();
        let sys_str = match &v["system"] {
            serde_json::Value::String(s) => s.as_str(),
            serde_json::Value::Array(a) => a[0]["text"].as_str().unwrap(),
            _ => panic!("unexpected system format"),
        };
        assert_eq!(sys_str, "You are Claude.");
        let last_user = v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .unwrap();
        let content = match &last_user["content"] {
            serde_json::Value::String(s) => s.as_str(),
            serde_json::Value::Array(a) => a[0]["text"].as_str().unwrap(),
            _ => panic!("unexpected content format"),
        };
        assert!(content.contains("<xfade_handover_context>"));
        assert!(content.contains("Model Transition: claude-3-5-sonnet -> claude-3-7-sonnet"));
        assert!(content.ends_with("help me write code"));

        // Verify handover record in DB is marked injected
        let hs = db.list_handovers(&sid).unwrap();
        assert_eq!(hs.len(), 1);
        assert!(hs[0].injected);
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

    // ── Aggregated /v1/models (`models-merge`) ────────────────────────────

    fn models_catalog(ids: &[&str]) -> String {
        let data: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| serde_json::json!({ "id": id, "object": "model" }))
            .collect();
        serde_json::json!({ "object": "list", "data": data }).to_string()
    }

    fn two_upstreams(dir: &tempfile::TempDir, up: &MockUpstream, up2: &MockUpstream) -> Core {
        test_core(
            dir,
            &[("freebuff", &up.url(), "k1"), ("omni", &up2.url(), "k2")],
        )
    }

    fn two_routes() -> Vec<String> {
        vec!["freebuff".to_string(), "omni".to_string()]
    }

    /// Spawn two upstreams and a service wired to both routes with the given
    /// models-merge toggle. Returns the upstreams, a clone of the shared
    /// `Database` (for asserting the persisted map), and the base URL.
    async fn two_route_service(
        dir: &tempfile::TempDir,
        merge: bool,
    ) -> (
        MockUpstream,
        MockUpstream,
        crate::store::db::Database,
        String,
    ) {
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = two_upstreams(dir, &up, &up2);
        let db = core.db().clone();
        core.db()
            .set_routes_with_models(&two_routes(), None, "chat", &Default::default())
            .unwrap();
        core.db().set_models_merge_enabled(merge).unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        (up, up2, db, url)
    }

    fn catalog_ids(v: &serde_json::Value) -> Vec<&str> {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn models_merge_off_is_first_success_passthrough() {
        // With the toggle off, /v1/models must behave exactly as before: the
        // first successful upstream's catalog verbatim, second one never queried.
        let dir = tempfile::tempdir().unwrap();
        let (up, up2, _db, url) = two_route_service(&dir, false).await;
        let expected = models_catalog(&["m-a"]);
        up.respond_with(200, &expected);
        up2.respond_with(200, &models_catalog(&["m-b"]));

        let (status, body) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);
        assert_eq!(
            body, expected,
            "OFF must pass the first catalog through verbatim"
        );
        assert_eq!(up2.call_count().await, 0);
    }

    #[tokio::test]
    async fn models_merge_on_aggregates_all_route_catalogs() {
        let dir = tempfile::tempdir().unwrap();
        let (up, up2, _db, url) = two_route_service(&dir, true).await;
        up.respond_with(200, &models_catalog(&["m-a", "m-c"]));
        up2.respond_with(200, &models_catalog(&["m-b"]));

        let (status, headers, body) = http_get_with_headers(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);
        assert!(headers
            .iter()
            .any(|(k, v)| k == "content-type" && v.contains("application/json")));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["object"], "list");
        let ids = catalog_ids(&v);
        assert_eq!(ids.len(), 3);
        for expected in ["m-a", "m-c", "m-b"] {
            assert!(ids.contains(&expected), "missing {expected} in {ids:?}");
        }
        assert_eq!(up.call_count().await, 1);
        assert_eq!(up2.call_count().await, 1);
    }

    #[tokio::test]
    async fn models_merge_on_namespaces_colliding_ids() {
        let dir = tempfile::tempdir().unwrap();
        let (up, up2, _db, url) = two_route_service(&dir, true).await;
        up.respond_with(200, &models_catalog(&["gpt-x", "only-a"]));
        up2.respond_with(200, &models_catalog(&["gpt-x", "only-b"]));

        let (status, body) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let ids = catalog_ids(&v);
        assert!(
            !ids.contains(&"gpt-x"),
            "ambiguous raw id must not be exposed"
        );
        for expected in ["freebuff/gpt-x", "omni/gpt-x", "only-a", "only-b"] {
            assert!(ids.contains(&expected), "missing {expected} in {ids:?}");
        }
    }

    #[tokio::test]
    async fn models_merge_on_persists_model_to_route_map() {
        let dir = tempfile::tempdir().unwrap();
        let (up, up2, db, url) = two_route_service(&dir, true).await;
        up.respond_with(200, &models_catalog(&["only-a", "shared"]));
        up2.respond_with(200, &models_catalog(&["only-b", "shared"]));

        let (status, _) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);
        let map = db.get_model_routes().unwrap();
        assert_eq!(map["only-a"].route_id, "freebuff");
        assert_eq!(map["only-a"].upstream_model, "only-a");
        assert_eq!(map["only-b"].route_id, "omni");
        assert_eq!(map["freebuff/shared"].route_id, "freebuff");
        assert_eq!(map["freebuff/shared"].upstream_model, "shared");
        assert_eq!(map["omni/shared"].route_id, "omni");
        assert_eq!(map["omni/shared"].upstream_model, "shared");
        assert!(
            !map.contains_key("shared"),
            "ambiguous raw id is not mapped"
        );
    }

    #[tokio::test]
    async fn post_pinned_model_routes_to_owner_with_real_id() {
        let dir = tempfile::tempdir().unwrap();
        let (up, up2, _db, url) = two_route_service(&dir, true).await;
        up.respond_with(200, &models_catalog(&["a-model"]));
        up2.respond_with(200, &models_catalog(&["auto/best-fast"]));

        let (status, _) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"auto/best-fast","messages":[]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        // The model is owned by the *second* route, so it must be pinned first.
        assert_eq!(up2.call_count().await, 2, "omni should see GET + POST");
        assert_eq!(up.call_count().await, 1, "freebuff should only see the GET");
        assert!(up2
            .last_request()
            .await
            .body
            .contains(r#""model":"auto/best-fast""#));
    }

    #[tokio::test]
    async fn post_namespaced_model_forwards_real_upstream_id() {
        let dir = tempfile::tempdir().unwrap();
        let (up, up2, _db, url) = two_route_service(&dir, true).await;
        up.respond_with(200, &models_catalog(&["gpt-x"]));
        up2.respond_with(200, &models_catalog(&["gpt-x"]));

        let (status, _) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"omni/gpt-x","messages":[]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(up2.call_count().await, 2);
        assert_eq!(up.call_count().await, 1);
        let seen = up2.last_request().await;
        assert!(
            seen.body.contains(r#""model":"gpt-x""#),
            "namespaced id must be stripped to the real upstream id: {}",
            seen.body
        );
    }

    #[tokio::test]
    async fn post_unknown_model_keeps_existing_override_behavior() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = two_upstreams(&dir, &up, &up2);
        let mut models = std::collections::BTreeMap::new();
        models.insert("freebuff".to_string(), "z-ai/glm-5.3-flash".to_string());
        models.insert("omni".to_string(), "auto/best-fast".to_string());
        core.db()
            .set_routes_with_models(&two_routes(), None, "chat", &models)
            .unwrap();
        core.db().set_models_merge_enabled(true).unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(200, &models_catalog(&["a-model"]));
        up2.respond_with(200, &models_catalog(&["b-model"]));

        let (status, _) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);
        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"legacy-unknown","messages":[]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        // No pin: the primary keeps its --route-model override.
        assert!(up
            .last_request()
            .await
            .body
            .contains(r#""model":"z-ai/glm-5.3-flash""#));
        assert_eq!(
            up2.call_count().await,
            1,
            "only the catalog GET reached omni"
        );
    }

    #[tokio::test]
    async fn models_merge_off_post_ignores_map() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = two_upstreams(&dir, &up, &up2);
        core.db().set_routes(&two_routes(), None, "chat").unwrap();
        // Manually populate a map while the toggle stays off.
        let mut map = crate::store::db::ModelRoutes::new();
        map.insert(
            "auto/best-fast".to_string(),
            crate::store::db::ModelRoute {
                route_id: "omni".to_string(),
                upstream_model: "auto/best-fast".to_string(),
            },
        );
        core.db().set_model_routes(&map).unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        up.respond_with(200, "{}");
        up2.respond_with(200, "{}");

        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"auto/best-fast","messages":[]}"#,
            "Bearer x",
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(up.call_count().await, 1, "primary should receive the POST");
        assert_eq!(up2.call_count().await, 0, "no pin when the toggle is off");
        assert!(up
            .last_request()
            .await
            .body
            .contains(r#""model":"auto/best-fast""#));
    }

    #[tokio::test]
    async fn models_merge_on_skips_circuit_open_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = two_upstreams(&dir, &up, &up2);
        core.db()
            .set_routes_with_models(&two_routes(), None, "chat", &Default::default())
            .unwrap();
        core.db().set_models_merge_enabled(true).unwrap();
        let svc = ProxyService::new(core);
        for _ in 0..super::FAIL_THRESHOLD {
            svc.record_failure("freebuff");
        }
        let url = spawn_service(svc).await;
        up.respond_with(200, &models_catalog(&["a-model"]));
        up2.respond_with(200, &models_catalog(&["b-model"]));

        let (status, body) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);
        assert!(body.contains("b-model"));
        assert!(!body.contains("a-model"));
        assert_eq!(up.call_count().await, 0);
    }

    #[tokio::test]
    async fn models_merge_on_one_upstream_down_degrades() {
        let dir = tempfile::tempdir().unwrap();
        let (up, up2, _db, url) = two_route_service(&dir, true).await;
        up.respond_with(500, "boom");
        up2.respond_with(200, &models_catalog(&["b-model"]));

        let (status, body) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(catalog_ids(&v), vec!["b-model"]);
    }

    #[tokio::test]
    async fn models_merge_on_zero_success_degrades_to_first_success() {
        let dir = tempfile::tempdir().unwrap();
        let (up, up2, db, url) = two_route_service(&dir, true).await;
        // Seed a last-known map; a zero-success probe must NOT clear it.
        let mut seed = crate::store::db::ModelRoutes::new();
        seed.insert(
            "m-old".to_string(),
            crate::store::db::ModelRoute {
                route_id: "freebuff".to_string(),
                upstream_model: "m-old".to_string(),
            },
        );
        db.set_model_routes(&seed).unwrap();
        up.respond_with(500, "boom");
        up2.respond_with(500, "boom");

        let (status, _) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_ne!(
            status, 200,
            "zero-success must not return an empty 200 catalog"
        );
        assert_eq!(
            db.get_model_routes().unwrap(),
            seed,
            "last-known map must survive the zero-success degrade"
        );
    }

    #[tokio::test]
    async fn models_probe_does_not_mutate_circuit_state() {
        let dir = tempfile::tempdir().unwrap();
        let (up, up2, _db, url) = two_route_service(&dir, true).await;
        up.respond_with(500, "boom");
        up2.respond_with(200, &models_catalog(&["b-model"]));

        let (status, _) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);

        // A failed discovery probe must not feed the chat circuit breaker.
        let v: serde_json::Value = reqwest::get(format!("{url}/__xfade/status"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let freebuff = v["circuits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["provider_id"] == "freebuff");
        assert!(
            freebuff.is_none() || freebuff.unwrap()["fails"] == 0,
            "failed models probe must not record a circuit failure: {v}"
        );
    }

    #[tokio::test]
    async fn status_endpoint_surfaces_models_merge_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db().set_routes(&["yy".into()], None, "chat").unwrap();
        core.db().set_models_merge_enabled(true).unwrap();
        let url = spawn_service(ProxyService::new(core)).await;
        let v: serde_json::Value = reqwest::get(format!("{url}/__xfade/status"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["models_merge"], true);
    }

    #[tokio::test]
    async fn test_proxy_auto_injects_cache_control_and_survives_circuit_trip() {
        let dir = tempfile::tempdir().unwrap();
        let up_failing = MockUpstream::spawn().await;
        let up_backup = MockUpstream::spawn().await;

        let core = test_core(
            &dir,
            &[
                ("failing", &up_failing.url(), "k1"),
                ("backup", &up_backup.url(), "k2"),
            ],
        );
        core.db()
            .set_routes(&["failing".into(), "backup".into()], None, "messages")
            .unwrap();

        let svc = ProxyService::new(core);
        let url = spawn_service(svc).await;

        // up_failing returns 500s to trip circuit breaker
        up_failing.respond_with(500, r#"{"error":"overloaded"}"#);
        up_backup.respond_with(
            200,
            r#"{"id":"resp-1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        );

        // Send request through xfade proxy to Anthropic endpoint (/v1/messages)
        let body =
            r#"{"model":"claude-3-7-sonnet","messages":[{"role":"user","content":"hello"}]}"#;
        let (status, text) = http_post_json(&url, "/v1/messages", body, "Bearer test").await;

        assert_eq!(
            status, 200,
            "proxy must seamlessly fail over without client 500: {text}"
        );
        assert!(text.contains("resp-1"));

        // Verify auto-injected cache_control was received by the backup provider
        let backup_req = up_backup.last_request().await;
        assert!(
            backup_req.body.contains("cache_control"),
            "target Anthropic messages request must have auto-injected cache_control, but got: {}",
            backup_req.body
        );

        // Verify failing provider was called and circuit breaker tracked failure
        assert_eq!(up_failing.call_count().await, 1);
        assert_eq!(up_backup.call_count().await, 1);

        // Send 2 more requests so failing provider reaches failure_threshold (3)
        for _ in 0..2 {
            let (s, _) = http_post_json(&url, "/v1/messages", body, "Bearer test").await;
            assert_eq!(s, 200);
        }
        assert_eq!(up_failing.call_count().await, 3);
        assert_eq!(up_backup.call_count().await, 3);

        // 4th request: circuit is OPEN on failing provider, so it must be skipped without an HTTP call!
        let (s4, t4) = http_post_json(&url, "/v1/messages", body, "Bearer test").await;
        assert_eq!(s4, 200);
        assert!(t4.contains("resp-1"));
        assert_eq!(
            up_failing.call_count().await,
            3,
            "circuit-open provider must be skipped without network call"
        );
        assert_eq!(up_backup.call_count().await, 4);
    }

    #[tokio::test]
    async fn test_proxy_rectifies_thinking_error_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;

        let core = test_core(&dir, &[("primary", &up.url(), "k1")]);
        core.db()
            .set_routes(&["primary".into()], None, "messages")
            .unwrap();

        let svc = ProxyService::new(core);
        let url = spawn_service(svc).await;

        // First attempt returns 400 with max_tokens budget error
        up.respond_with(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 100 must be greater than budget_tokens: 1024"}}"#,
        );
        // Retry attempt on the same upstream succeeds
        up.respond_with(
            200,
            r#"{"id":"resp-rectified","type":"message","role":"assistant","content":[{"type":"text","text":"healed"}]}"#,
        );

        let body = r#"{"model":"claude-3-7-sonnet","max_tokens":100,"thinking":{"type":"enabled","budget_tokens":1024},"messages":[{"role":"user","content":"test"}]}"#;
        let (status, text) = http_post_json(&url, "/v1/messages", body, "Bearer test").await;

        assert_eq!(
            status, 200,
            "thinking error should be rectified and succeed: {text}"
        );
        assert!(text.contains("resp-rectified"));

        // Verify upstream received 2 requests and the second request had rectified max_tokens
        assert_eq!(up.call_count().await, 2);
        let last_req = up.last_request().await;
        assert!(
            last_req.body.contains("\"max_tokens\":2048"),
            "rectified request should have bumped max_tokens above budget, got: {}",
            last_req.body
        );
    }

    #[tokio::test]
    async fn test_probe_non_qualifying_4xx_releases_reservation_and_does_not_hang_half_open() {
        let dir = tempfile::tempdir().unwrap();
        let up_primary = MockUpstream::spawn().await;
        let up_backup = MockUpstream::spawn().await;

        let core = test_core(
            &dir,
            &[
                ("pri", &up_primary.url(), "k1"),
                ("bak", &up_backup.url(), "k2"),
            ],
        );
        core.db()
            .set_routes(&["pri".into(), "bak".into()], None, "chat")
            .unwrap();

        let cb = Arc::new(CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 1,
            timeout_seconds: 0, // Instant cooldown for probe testing
            error_rate_threshold: 0.5,
            min_requests: 1,
        }));

        let svc = ProxyService::new(core).with_circuit_breaker(cb.clone());
        let url = spawn_service(svc).await;

        // 1. Primary returns 500 -> trips to Open
        up_primary.respond_sequence(vec![
            (500, "overloaded"),
            (400, r#"{"error":"invalid syntax"}"#),
            (404, "not found"),
        ]);
        up_backup.respond_with(200, r#"{"choices":[]}"#);

        let (s1, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
        assert_eq!(s1, 200);
        assert_eq!(cb.state("pri"), CircuitState::Open);

        // 2. Next request probes in HalfOpen: primary returns unrectified 400
        let (s2, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
        assert_eq!(s2, 400);

        // Verify probe slot was released and did not hang HalfOpen:
        assert_eq!(cb.state("pri"), CircuitState::HalfOpen);
        assert!(
            cb.allow_request("pri"),
            "probe in flight must be released on unrectified 400"
        );
        cb.release_probe("pri"); // release probe slot claimed in assertion

        // 3. Next probe returns 404:
        let (s3, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
        assert_eq!(s3, 404);
        assert!(
            cb.allow_request("pri"),
            "probe in flight must be released on 404"
        );
    }

    #[tokio::test]
    async fn test_half_open_probe_slot_released_on_missing_base_url() {
        use crate::models::{Provider, ToolKind};

        let dir = tempfile::tempdir().unwrap();

        // A single route entry whose provider has no base_url (official): the
        // failover loop skips it immediately after admitting the probe, which
        // used to leak the HalfOpen probe slot and permanently reject the route
        // with `503 no usable provider`.
        let core = test_core(&dir, &[]);
        core.add_provider(Provider::new("p", ToolKind::Pi, None), None)
            .unwrap();
        core.db()
            .set_tool_routes("pi", &["p".into()], None, "chat")
            .unwrap();

        let cb = Arc::new(CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 3,
            success_threshold: 2,
            timeout_seconds: 0, // instant cooldown for probe testing
            error_rate_threshold: 0.5,
            min_requests: 1,
        }));
        cb.record_failure("p");
        cb.record_failure("p");
        cb.record_failure("p");
        assert_eq!(cb.state("p"), CircuitState::Open);

        let svc = ProxyService::new(core).with_circuit_breaker(cb.clone());
        let token_pi = svc.token_manager.get_or_create(ToolKind::Pi).unwrap();
        let url = spawn_service(svc).await;

        let (status, body) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"messages":[{"role":"user","content":"hi"}]}"#,
            &format!("Bearer {token_pi}"),
        )
        .await;
        assert_eq!(status, 503);
        assert_eq!(body, "no usable provider");

        // The probe slot must have been released (not leaked): a subsequent
        // request can admit a probe instead of being rejected forever.
        assert!(
            cb.allow_request("p"),
            "HalfOpen probe slot must be released after a missing-base-url skip"
        );
    }

    #[tokio::test]
    async fn test_restoring_expired_cooldown_circuit_starts_in_closed_state() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[("expired_provider", "http://127.0.0.1:9999", "k1")]);

        // Save a circuit whose cooldown expired 100 seconds ago
        let past = now_secs() - 100;
        let circuit = Circuit {
            fails: 3,
            cooldown_until_secs: past,
        };
        core.db()
            .save_circuit("expired_provider", &circuit)
            .unwrap();

        // Start ProxyService with this core DB
        let svc = ProxyService::new(core.clone());

        // Verify provider is in Closed state, not Open!
        assert_eq!(
            svc.circuit_breaker.state("expired_provider"),
            CircuitState::Closed
        );
        assert!(svc.circuit_breaker.allow_request("expired_provider"));
        assert!(!svc.circuit_open("expired_provider"));

        // Verify the expired circuit was cleaned from the database
        let loaded = core.db().load_circuits().unwrap();
        assert!(!loaded.contains_key("expired_provider"));
    }

    #[tokio::test]
    async fn test_multi_tool_route_isolation_via_client_tokens() {
        use crate::models::ToolKind;

        let dir = tempfile::tempdir().unwrap();
        let up_claude = MockUpstream::spawn().await;
        let up_codex = MockUpstream::spawn().await;

        let core = test_core(
            &dir,
            &[
                ("prov-claude", &up_claude.url(), "key-claude"),
                ("prov-codex", &up_codex.url(), "key-codex"),
            ],
        );

        // Set tool routes: Claude Code -> prov-claude, Codex -> prov-codex
        core.db()
            .set_tool_routes("claude", &["prov-claude".into()], None, "chat")
            .unwrap();
        core.db()
            .set_tool_routes("codex", &["prov-codex".into()], None, "chat")
            .unwrap();

        let svc = ProxyService::new(core);
        let token_claude = svc
            .token_manager
            .get_or_create(ToolKind::ClaudeCode)
            .unwrap();
        let token_codex = svc.token_manager.get_or_create(ToolKind::Codex).unwrap();

        let url = spawn_service(svc).await;

        up_claude.respond_with(200, r#"{"from":"claude"}"#);
        up_codex.respond_with(200, r#"{"from":"codex"}"#);

        // Request with Claude's token goes to up_claude
        let (s1, b1) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"messages":[{"role":"user","content":"hi"}]}"#,
            &format!("Bearer {token_claude}"),
        )
        .await;
        assert_eq!(s1, 200);
        assert!(
            b1.contains("claude"),
            "expected response from claude, got {b1}"
        );

        // Request with Codex's token goes to up_codex
        let (s2, b2) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"messages":[{"role":"user","content":"hi"}]}"#,
            &format!("Bearer {token_codex}"),
        )
        .await;
        assert_eq!(s2, 200);
        assert!(
            b2.contains("codex"),
            "expected response from codex, got {b2}"
        );
    }

    #[tokio::test]
    async fn test_proxy_explicit_model_mapping_cascades_across_routes() {
        let dir = tempfile::tempdir().unwrap();
        let up1 = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("p1", &up1.url(), "k1"), ("p2", &up2.url(), "k2")]);
        core.db()
            .set_routes(&["p1".into(), "p2".into()], None, "chat")
            .unwrap();
        core.db()
            .set_model_mapping("sonnet", "p1", "claude-3-7-sonnet")
            .unwrap();
        core.db()
            .set_model_mapping("sonnet", "p2", "deepseek-chat")
            .unwrap();

        let url = spawn_service(ProxyService::new(core)).await;
        up1.respond_with(500, "boom");
        up2.respond_with(
            200,
            r#"{"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        );

        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#,
            "Bearer x",
        )
        .await;

        assert_eq!(status, 200);
        assert!(
            up1.last_request()
                .await
                .body
                .contains(r#""model":"claude-3-7-sonnet""#),
            "primary p1 did not receive its mapped model"
        );
        assert!(
            up2.last_request()
                .await
                .body
                .contains(r#""model":"deepseek-chat""#),
            "fallback p2 did not receive its mapped model"
        );
    }

    #[tokio::test]
    async fn test_proxy_explicit_model_mapping_bypasses_stage_2c_pinning() {
        let dir = tempfile::tempdir().unwrap();
        let up1 = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("p1", &up1.url(), "k1"), ("p2", &up2.url(), "k2")]);
        core.db()
            .set_routes(&["p1".into(), "p2".into()], None, "chat")
            .unwrap();
        core.db().set_models_merge_enabled(true).unwrap();

        // Seed model_routes to simulate "sonnet" was pinned to p2
        let mut seed = crate::store::db::ModelRoutes::new();
        seed.insert(
            "sonnet".to_string(),
            crate::store::db::ModelRoute {
                route_id: "p2".to_string(),
                upstream_model: "pinned-p2-model".to_string(),
            },
        );
        core.db().set_model_routes(&seed).unwrap();

        // Configure explicit model mapping for sonnet
        core.db()
            .set_model_mapping("sonnet", "p1", "claude-3-7-sonnet")
            .unwrap();
        core.db()
            .set_model_mapping("sonnet", "p2", "deepseek-chat")
            .unwrap();

        let url = spawn_service(ProxyService::new(core)).await;
        up1.respond_with(
            200,
            r#"{"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        );

        let (status, _) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"model":"sonnet","messages":[{"role":"user","content":"hi"}]}"#,
            "Bearer x",
        )
        .await;

        assert_eq!(status, 200);
        // p1 must be called (NOT pinned to p2!)
        assert_eq!(
            up1.call_count().await,
            1,
            "p1 should have been called first"
        );
        assert_eq!(up2.call_count().await, 0, "p2 should not have been called");
        assert!(
            up1.last_request()
                .await
                .body
                .contains(r#""model":"claude-3-7-sonnet""#),
            "p1 should have received its mapped model"
        );
    }

    #[tokio::test]
    async fn test_aggregate_models_includes_aliases_without_polluting_model_routes() {
        let dir = tempfile::tempdir().unwrap();
        let up1 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("p1", &up1.url(), "k1")]);
        core.db().set_routes(&["p1".into()], None, "chat").unwrap();
        core.db().set_models_merge_enabled(true).unwrap();
        core.db()
            .set_model_mapping("sonnet", "p1", "claude-3-7-sonnet")
            .unwrap();

        up1.respond_with(200, &models_catalog(&["model-from-up1", "sonnet"]));

        let url = spawn_service(ProxyService::new(core.clone())).await;
        let (status, body) = http_get(&url, "/v1/models", "Bearer x").await;
        assert_eq!(status, 200);

        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let data = v["data"].as_array().expect("data array");

        // Verify sonnet alias is present, exactly once, and owned_by xfade
        let sonnet_matches: Vec<_> = data.iter().filter(|m| m["id"] == "sonnet").collect();
        assert_eq!(
            sonnet_matches.len(),
            1,
            "sonnet should be deduplicated in /v1/models: {v}"
        );
        assert_eq!(sonnet_matches[0]["owned_by"], "xfade");

        // Verify alias is NOT cached in model_routes
        let cached = core.db().model_route_for("sonnet").unwrap();
        assert!(
            cached.is_none(),
            "aliases must not be saved to model_routes cache"
        );
    }

    #[tokio::test]
    async fn test_proxy_pre_commit_200_error_body_fails_over() {
        let dir = tempfile::tempdir().unwrap();
        let up1 = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("p1", &up1.url(), "k1"), ("p2", &up2.url(), "k2")]);
        core.db()
            .set_routes(&["p1".into(), "p2".into()], None, "chat")
            .unwrap();

        let url = spawn_service(ProxyService::new(core)).await;
        // p1 returns 200 OK with error body
        up1.respond_with(
            200,
            r#"{"error":{"message":"upstream overloaded","type":"server_error"}}"#,
        );
        // p2 returns 200 OK with valid response
        up2.respond_with(
            200,
            r#"{"choices":[{"message":{"content":"healthy from p2"}}]}"#,
        );

        let (status, body) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"messages":[{"role":"user","content":"hi"}]}"#,
            "Bearer x",
        )
        .await;

        assert_eq!(status, 200);
        assert!(
            body.contains("healthy from p2"),
            "expected failover to p2, got: {body}"
        );
    }

    #[tokio::test]
    async fn test_proxy_retry_after_under_2s_retries_same_route() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("p1", &up.url(), "k1")]);
        core.db().set_routes(&["p1".into()], None, "chat").unwrap();

        let url = spawn_service(ProxyService::new(core)).await;
        // First attempt returns 429 with Retry-After: 0
        up.respond_with_headers(429, &[("retry-after", "0")], r#"{"error":"rate_limited"}"#);
        // Second attempt on the same provider returns 200 OK
        up.respond_with(
            200,
            r#"{"choices":[{"message":{"content":"success after retry"}}]}"#,
        );

        let (status, body) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"messages":[{"role":"user","content":"hi"}]}"#,
            "Bearer x",
        )
        .await;

        assert_eq!(status, 200);
        assert!(
            body.contains("success after retry"),
            "expected retry on same route to succeed, got: {body}"
        );
    }

    #[tokio::test]
    async fn test_proxy_streaming_post_commit_interruption_synthesizes_error() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("p1", &up.url(), "k1")]);
        core.db().set_routes(&["p1".into()], None, "chat").unwrap();

        let svc = ProxyService::new(core);
        let cb = svc.circuit_breaker.clone();
        let url = spawn_service(svc).await;

        up.respond_sse_error(vec![r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#]);

        let (status, body) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
            "Bearer x",
        )
        .await;

        assert_eq!(status, 200);
        assert!(
            body.contains("Hello"),
            "client must receive committed tokens"
        );
        assert!(
            body.contains("xfade_stream_interrupted"),
            "expected synthesized xfade_stream_interrupted frame, got: {body}"
        );
        assert_eq!(
            cb.failure_count("p1"),
            1,
            "failure must be recorded on circuit breaker"
        );
    }

    #[tokio::test]
    async fn test_proxy_streaming_pre_commit_error_frame_fails_over() {
        let dir = tempfile::tempdir().unwrap();
        let up1 = MockUpstream::spawn().await;
        let up2 = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("p1", &up1.url(), "k1"), ("p2", &up2.url(), "k2")]);
        core.db()
            .set_routes(&["p1".into(), "p2".into()], None, "chat")
            .unwrap();

        let svc = ProxyService::new(core);
        let cb = svc.circuit_breaker.clone();
        let url = spawn_service(svc).await;

        // p1 returns 200 OK SSE, but first line is an error payload
        up1.respond_sse(vec![
            r#"data: {"error":{"message":"overloaded","type":"server_error"}}"#,
        ]);
        // p2 returns 200 OK SSE with valid text
        up2.respond_sse(vec![
            r#"data: {"choices":[{"delta":{"content":"hello from p2"}}]}"#,
            r#"data: [DONE]"#,
        ]);

        let (status, body) = http_post_json(
            &url,
            "/v1/chat/completions",
            r#"{"stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
            "Bearer x",
        )
        .await;

        assert_eq!(status, 200);
        assert!(
            body.contains("hello from p2"),
            "expected failover to p2, got: {body}"
        );
        assert_eq!(
            cb.failure_count("p1"),
            1,
            "pre-commit failure must trip circuit on p1"
        );
    }
}
