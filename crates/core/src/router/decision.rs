//! Route planning and tiered routing decisions (Smart Fader).

use crate::models::Provider;
use crate::router::classifier::{ClassifyCtx, Intent, IntentClassifier, KeywordClassifier};

/// Provider tier: local model (cheap/fast) vs cloud (stronger), plus the
/// fallback bucket label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Local,
    Cloud,
    Fallback,
}

/// Recent health of one provider, used to order routes within a tier
/// bucket: lower error rate first, then lower latency. `None` (no data)
/// keeps the configured position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HealthSignal {
    /// Errors / requests over the stats window, 0.0..=1.0.
    pub error_rate: f64,
    /// Average request latency in ms over the stats window.
    pub avg_ms: i64,
}

/// One executable route: which provider, with which model override, in which
/// bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteEntry {
    pub route_id: String,
    pub model_override: Option<String>,
    pub tier: Tier,
}

/// The router's decision for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePlan {
    /// Entries in execution order (primary bucket first, then fallback).
    pub entries: Vec<RouteEntry>,
    pub intent: Intent,
    /// Human-readable explanation (observability: logs / future TUI).
    pub reason: String,
}

/// Pluggable route planner.
///
/// `routes` is the configured route set in configured order (as from
/// `xfade proxy use`); `open` reports whether a provider's circuit breaker
/// is currently open (open routes are skipped).
pub trait Router: Send + Sync {
    fn plan(
        &self,
        ctx: &ClassifyCtx<'_>,
        routes: &[Provider],
        model_override: Option<&str>,
        open: &dyn Fn(&str) -> bool,
        health: &dyn Fn(&str) -> Option<HealthSignal>,
    ) -> RoutePlan;
}

/// Static router (Smart Fader off): preserve the configured order exactly.
pub struct DefaultRouter;

impl Router for DefaultRouter {
    fn plan(
        &self,
        _ctx: &ClassifyCtx<'_>,
        routes: &[Provider],
        model_override: Option<&str>,
        open: &dyn Fn(&str) -> bool,
        _health: &dyn Fn(&str) -> Option<HealthSignal>,
    ) -> RoutePlan {
        let entries = routes
            .iter()
            .filter(|p| !open(&p.id))
            .map(|p| RouteEntry {
                route_id: p.id.clone(),
                model_override: model_override.map(String::from),
                tier: provider_tier(p),
            })
            .collect();
        RoutePlan {
            entries,
            intent: Intent::Unknown,
            reason: "static route order".into(),
        }
    }
}

/// Determine a provider's tier: explicit `extra.tier` wins; otherwise a
/// loopback base_url is local, everything else cloud.
pub fn provider_tier(p: &Provider) -> Tier {
    if let Some(t) = p.extra.get("tier").and_then(|v| v.as_str()) {
        return match t {
            "local" => Tier::Local,
            _ => Tier::Cloud,
        };
    }
    match p.base_url.as_deref() {
        // Loopback heuristic: bare-host local runtimes (Ollama, LM Studio,
        // MLX servers). These markers can only appear in the authority
        // component of an http(s) URL, never later in the path.
        Some(u) if u.contains("127.0.0.1") || u.contains("localhost") || u.contains("[::1]") => {
            Tier::Local
        }
        _ => Tier::Cloud,
    }
}

/// Light intents run local-first; heavy intents run cloud-first; unknown
/// keeps the configured order.
fn light(intent: Intent) -> bool {
    matches!(
        intent,
        Intent::ExplainError | Intent::WriteTest | Intent::StaticCheck | Intent::SmallRefactor
    )
}

/// Pure tier-bucket planning for a known intent.
pub fn plan_for_intent(
    intent: Intent,
    routes: &[Provider],
    model_override: Option<&str>,
    open: &dyn Fn(&str) -> bool,
    health: &dyn Fn(&str) -> Option<HealthSignal>,
) -> RoutePlan {
    let usable: Vec<&Provider> = routes.iter().filter(|p| !open(&p.id)).collect();
    let entry_for = |p: &Provider| RouteEntry {
        route_id: p.id.clone(),
        model_override: model_override.map(String::from),
        tier: provider_tier(p),
    };
    // Within a bucket: healthiest first (error rate, then latency). A
    // missing signal keeps the configured position relative to signalled
    // providers — stable sort, no data = no demotion beyond that.
    let sort_bucket = |bucket: &mut Vec<RouteEntry>| {
        bucket.sort_by(|a, b| {
            let ha = health(&a.route_id);
            let hb = health(&b.route_id);
            match (ha, hb) {
                (Some(x), Some(y)) => x
                    .error_rate
                    .total_cmp(&y.error_rate)
                    .then(x.avg_ms.cmp(&y.avg_ms)),
                // No signal sorts after any signalled provider, equal among
                // themselves (stable → configured order preserved).
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });
    };

    let (entries, reason) = if intent == Intent::Unknown {
        // Unknown intent: keep the configured order exactly (only
        // circuit-open routes are dropped).
        (
            usable.iter().map(|p| entry_for(p)).collect(),
            "unknown intent: static route order".to_string(),
        )
    } else {
        let mut local = Vec::new();
        let mut cloud = Vec::new();
        for p in &usable {
            let e = entry_for(p);
            if e.tier == Tier::Local {
                local.push(e);
            } else {
                cloud.push(e);
            }
        }
        sort_bucket(&mut local);
        sort_bucket(&mut cloud);
        if light(intent) {
            let mut entries = local.clone();
            entries.extend(cloud.clone());
            (
                entries,
                format!(
                    "{intent:?}: local-first with cloud fallback ({} local, {} cloud)",
                    local.len(),
                    cloud.len()
                ),
            )
        } else {
            let mut entries = cloud.clone();
            entries.extend(local.clone());
            (
                entries,
                format!(
                    "{intent:?}: cloud-first with local fallback ({} cloud, {} local)",
                    cloud.len(),
                    local.len()
                ),
            )
        }
    };

    RoutePlan {
        entries,
        intent,
        reason,
    }
}

/// Classify the request (keyword rules) and plan the tier buckets for it.
pub fn plan_routes(
    ctx: &ClassifyCtx<'_>,
    routes: &[Provider],
    model_override: Option<&str>,
    open: &dyn Fn(&str) -> bool,
    health: &dyn Fn(&str) -> Option<HealthSignal>,
) -> RoutePlan {
    let intent = KeywordClassifier.classify(ctx);
    plan_for_intent(intent, routes, model_override, open, health)
}

/// Convenience: the phase-one smart router (keyword classifier + tier rules).
pub struct SmartRouter {
    pub classifier: Box<dyn IntentClassifier>,
}

impl Default for SmartRouter {
    fn default() -> Self {
        Self {
            classifier: Box::new(KeywordClassifier),
        }
    }
}

impl Router for SmartRouter {
    fn plan(
        &self,
        ctx: &ClassifyCtx<'_>,
        routes: &[Provider],
        model_override: Option<&str>,
        open: &dyn Fn(&str) -> bool,
        health: &dyn Fn(&str) -> Option<HealthSignal>,
    ) -> RoutePlan {
        let intent = self.classifier.classify(ctx);
        plan_for_intent(intent, routes, model_override, open, health)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(id: &str, base_url: Option<&str>, extra: serde_json::Value) -> Provider {
        let mut p = Provider::new(id, crate::models::ToolKind::OpenCode, base_url.map(String::from));
        p.extra = extra;
        p
    }

    fn cloud(id: &str) -> Provider {
        provider(id, Some("https://api.example.com/v1"), json!({}))
    }

    fn local(id: &str) -> Provider {
        provider(id, Some("http://127.0.0.1:8080/v1"), json!({}))
    }

    fn ctx(prompt: &str) -> ClassifyCtx<'_> {
        ClassifyCtx {
            endpoint: "chat",
            model: None,
            prompt,
        }
    }

    fn ids(plan: &RoutePlan) -> Vec<String> {
        plan.entries.iter().map(|e| e.route_id.clone()).collect()
    }

    fn none_open(_: &str) -> bool {
        false
    }

    fn no_health(_: &str) -> Option<HealthSignal> {
        None
    }

    const LIGHT: &str = "why does this fail to compile?";
    const HEAVY: &str = "restructure the whole proxy module into layers";

    #[test]
    fn health_orders_bucket_healthy_first() {
        // Same (local) tier: l-sick has a high error rate, l-healthy is
        // clean. Configured order is sick first; health must flip it.
        let routes = vec![local("l-sick"), local("l-healthy"), cloud("c")];
        let health = |id: &str| -> Option<HealthSignal> {
            match id {
                "l-sick" => Some(HealthSignal { error_rate: 0.5, avg_ms: 800 }),
                "l-healthy" => Some(HealthSignal { error_rate: 0.0, avg_ms: 100 }),
                _ => None,
            }
        };
        let plan = plan_routes(&ctx(LIGHT), &routes, None, &none_open, &health);
        assert_eq!(ids(&plan), vec!["l-healthy", "l-sick", "c"]);
    }

    #[test]
    fn health_ties_break_on_latency() {
        let routes = vec![local("l-slow"), local("l-fast")];
        let health = |id: &str| -> Option<HealthSignal> {
            match id {
                "l-slow" => Some(HealthSignal { error_rate: 0.0, avg_ms: 900 }),
                "l-fast" => Some(HealthSignal { error_rate: 0.0, avg_ms: 120 }),
                _ => None,
            }
        };
        let plan = plan_routes(&ctx(LIGHT), &routes, None, &none_open, &health);
        assert_eq!(ids(&plan), vec!["l-fast", "l-slow"]);
    }

    #[test]
    fn health_no_signal_keeps_configured_order() {
        // No stats for anyone: bucketing still applies, within-bucket order
        // stays as configured.
        let routes = vec![local("l2"), local("l1"), cloud("c")];
        let plan = plan_routes(&ctx(LIGHT), &routes, None, &none_open, &no_health);
        assert_eq!(ids(&plan), vec!["l2", "l1", "c"]);
    }

    #[test]
    fn health_sorts_each_bucket_independently() {
        // Both buckets get health-ordered; light intent keeps local-first.
        let routes = vec![local("l-sick"), local("l-ok"), cloud("c-sick"), cloud("c-ok")];
        let health = |id: &str| -> Option<HealthSignal> {
            match id {
                "l-sick" | "c-sick" => Some(HealthSignal { error_rate: 0.9, avg_ms: 500 }),
                _ => Some(HealthSignal { error_rate: 0.0, avg_ms: 50 }),
            }
        };
        let plan = plan_routes(&ctx(LIGHT), &routes, None, &none_open, &health);
        assert_eq!(ids(&plan), vec!["l-ok", "l-sick", "c-ok", "c-sick"]);
    }

    #[test]
    fn unknown_intent_ignores_health_ordering() {
        // Unknown intent = static order contract; health is not consulted
        // for reordering (only circuit-open routes are dropped).
        let routes = vec![local("l-sick"), local("l-healthy")];
        let health = |id: &str| -> Option<HealthSignal> {
            match id {
                "l-sick" => Some(HealthSignal { error_rate: 0.9, avg_ms: 500 }),
                _ => Some(HealthSignal { error_rate: 0.0, avg_ms: 50 }),
            }
        };
        let plan = plan_routes(&ctx("hello"), &routes, None, &none_open, &health);
        assert_eq!(ids(&plan), vec!["l-sick", "l-healthy"]);
    }

    #[test]
    fn health_missing_signal_sorts_after_signalled() {
        // A provider with no stats (freshly added) sorts after providers
        // with known health, but before nothing else changes: it is not
        // mistaken for "perfectly healthy" (which would starve proven
        // routes) nor for "unhealthy" (which would bury it below the
        // fallback bucket).
        let routes = vec![local("l-new"), local("l-known-bad"), local("l-known-ok")];
        let health = |id: &str| -> Option<HealthSignal> {
            match id {
                "l-known-bad" => Some(HealthSignal { error_rate: 0.4, avg_ms: 300 }),
                "l-known-ok" => Some(HealthSignal { error_rate: 0.0, avg_ms: 90 }),
                _ => None,
            }
        };
        let plan = plan_routes(&ctx(LIGHT), &routes, None, &none_open, &health);
        assert_eq!(ids(&plan), vec!["l-known-ok", "l-known-bad", "l-new"]);
    }

    #[test]
    fn tier_reads_extra_before_heuristic() {
        let explicit = provider("mlx", Some("https://api.example.com/v1"), json!({"tier": "local"}));
        assert_eq!(provider_tier(&explicit), Tier::Local);

        let loopback = provider("ollama", Some("http://127.0.0.1:11434/v1"), json!({}));
        assert_eq!(provider_tier(&loopback), Tier::Local);

        let localhost = provider("lm", Some("http://localhost:1234/v1"), json!({}));
        assert_eq!(provider_tier(&localhost), Tier::Local);

        let remote = provider("or", Some("https://openrouter.ai/api/v1"), json!({}));
        assert_eq!(provider_tier(&remote), Tier::Cloud);

        let official = provider("official", None, json!({}));
        assert_eq!(provider_tier(&official), Tier::Cloud);
    }

    #[test]
    fn default_router_preserves_configured_order() {
        let routes = vec![cloud("a"), local("b"), cloud("c")];
        let plan = DefaultRouter.plan(&ctx(LIGHT), &routes, None, &none_open, &no_health);
        assert_eq!(ids(&plan), vec!["a", "b", "c"]);
    }

    #[test]
    fn default_router_carries_model_override_and_skips_open_circuits() {
        let routes = vec![cloud("a"), cloud("b")];
        let open = |id: &str| id == "a";
        let plan = DefaultRouter.plan(&ctx("hi"), &routes, Some("m-x"),&open, &no_health);
        assert_eq!(ids(&plan), vec!["b"]);
        assert_eq!(plan.entries[0].model_override.as_deref(), Some("m-x"));
    }

    #[test]
    fn light_intent_prefers_local_then_cloud() {
        let routes = vec![cloud("a"), local("b"), cloud("c"), local("d")];
        let plan = plan_routes(&ctx(LIGHT), &routes, None, &none_open, &no_health);
        assert_eq!(ids(&plan), vec!["b", "d", "a", "c"]);
        assert_eq!(plan.intent, Intent::ExplainError);
    }

    #[test]
    fn heavy_intent_prefers_cloud_then_local() {
        let routes = vec![local("b"), cloud("a"), local("d"), cloud("c")];
        let plan = plan_routes(&ctx(HEAVY), &routes, None, &none_open, &no_health);
        assert_eq!(ids(&plan), vec!["a", "c", "b", "d"]);
        assert_eq!(plan.intent, Intent::BigRefactor);
    }

    #[test]
    fn unknown_intent_keeps_static_order() {
        let routes = vec![cloud("a"), local("b")];
        let plan = plan_routes(&ctx("hello there"), &routes, None, &none_open, &no_health);
        assert_eq!(ids(&plan), vec!["a", "b"]);
        assert_eq!(plan.intent, Intent::Unknown);
    }

    #[test]
    fn single_bucket_keeps_configured_order() {
        // All-cloud routes with a light intent: no reordering opportunity.
        let routes = vec![cloud("c2"), cloud("c1")];
        let plan = plan_routes(&ctx(LIGHT), &routes, None, &none_open, &no_health);
        assert_eq!(ids(&plan), vec!["c2", "c1"]);
    }

    #[test]
    fn open_circuits_are_skipped_before_bucketing() {
        let routes = vec![local("l1"), local("l2"), cloud("c1")];
        let open = |id: &str| id == "l1";
        let plan = plan_routes(&ctx(LIGHT), &routes, None,&open, &no_health);
        assert_eq!(ids(&plan), vec!["l2", "c1"]);
    }

    #[test]
    fn plan_records_reason() {
        let routes = vec![local("l1")];
        let plan = plan_routes(&ctx(LIGHT), &routes, None, &none_open, &no_health);
        assert!(!plan.reason.is_empty());
    }

    #[test]
    fn smart_router_classifies_and_buckets() {
        let routes = vec![cloud("a"), local("b")];
        let router = SmartRouter::default();
        let plan = router.plan(&ctx(LIGHT), &routes, None, &none_open, &no_health);
        assert_eq!(ids(&plan), vec!["b", "a"]);
        assert_eq!(plan.intent, Intent::ExplainError);
    }
}
