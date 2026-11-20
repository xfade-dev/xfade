//! Sync engine for fetching live model pricing catalogs from OpenRouter or custom URLs.

use crate::error::{CoreError, Result};
use crate::store::db::{Database, DynamicPriceRow};

pub const DEFAULT_OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// Summary statistics returned upon catalog synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStats {
    pub total_fetched: usize,
    pub models_synced: usize,
    pub source_url: String,
}

fn parse_rate_to_micros(val: Option<&serde_json::Value>) -> i64 {
    let Some(v) = val else { return 0 };
    let rate_f64 = match v {
        serde_json::Value::Number(num) => num.as_f64().unwrap_or(0.0),
        serde_json::Value::String(s) => s.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    };
    if !rate_f64.is_finite() || rate_f64 <= 0.0 {
        return 0;
    }
    // Rate is in USD per token.
    // 1M tokens in USD = rate_f64 * 1,000,000.
    // In micro-dollars ($1.00 = 1,000,000 micros), 1M tokens = rate_f64 * 10^12.
    (rate_f64 * 1_000_000_000_000.0).round() as i64
}

fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Parse raw OpenRouter `/api/v1/models` JSON payload into `DynamicPriceRow` entries.
pub fn parse_openrouter_models_payload(
    bytes: &[u8],
    updated_at: &str,
) -> Result<Vec<DynamicPriceRow>> {
    let root: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| CoreError::Proxy(format!("failed to parse model pricing JSON: {e}")))?;

    let items = if let Some(arr) = root.get("data").and_then(|d| d.as_array()) {
        arr
    } else if let Some(arr) = root.as_array() {
        arr
    } else {
        return Err(CoreError::Proxy(
            "pricing JSON missing expected 'data' array".to_string(),
        ));
    };

    let mut rows = Vec::new();
    for item in items {
        let id = match item.get("id").and_then(|v| v.as_str()) {
            Some(id) if !id.trim().is_empty() => id.trim().to_lowercase(),
            _ => continue,
        };

        let pricing = item.get("pricing");
        let input_micros = parse_rate_to_micros(pricing.and_then(|p| p.get("prompt")));
        let output_micros = parse_rate_to_micros(pricing.and_then(|p| p.get("completion")));
        let cache_read_micros =
            parse_rate_to_micros(pricing.and_then(|p| p.get("input_cache_read")));
        let cache_write_micros =
            parse_rate_to_micros(pricing.and_then(|p| p.get("input_cache_write")));

        // Full namespaced id (e.g. "anthropic/claude-sonnet-5.5")
        rows.push(DynamicPriceRow {
            model_pattern: id.clone(),
            input_micros_per_m: input_micros,
            output_micros_per_m: output_micros,
            cache_read_micros_per_m: cache_read_micros,
            cache_write_micros_per_m: cache_write_micros,
            source: "synced".to_string(),
            updated_at: updated_at.to_string(),
        });

        // Bare name without provider namespace (e.g. "claude-sonnet-5.5")
        if let Some((_, bare)) = id.rsplit_once('/') {
            let bare = bare.trim();
            if !bare.is_empty() && bare != id {
                rows.push(DynamicPriceRow {
                    model_pattern: bare.to_string(),
                    input_micros_per_m: input_micros,
                    output_micros_per_m: output_micros,
                    cache_read_micros_per_m: cache_read_micros,
                    cache_write_micros_per_m: cache_write_micros,
                    source: "synced".to_string(),
                    updated_at: updated_at.to_string(),
                });
            }
        }
    }

    Ok(rows)
}

/// Fetch live model pricing from OpenRouter (or a custom URL) and sync into `Database`.
pub async fn sync_pricing_catalog(db: &Database, url: Option<&str>) -> Result<SyncStats> {
    let target_url = url.unwrap_or(DEFAULT_OPENROUTER_MODELS_URL);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| CoreError::Proxy(format!("failed to initialize HTTP client: {e}")))?;

    let res = client
        .get(target_url)
        .header("User-Agent", "xfade-sync/1.0")
        .send()
        .await
        .map_err(|e| {
            CoreError::Proxy(format!(
                "failed to fetch pricing catalog from {target_url}: {e}"
            ))
        })?;

    let status = res.status();
    if !status.is_success() {
        return Err(CoreError::Proxy(format!(
            "pricing sync request to {target_url} returned HTTP {status}"
        )));
    }

    if let Some(content_length) = res.content_length() {
        const MAX_BODY_BYTES: u64 = 25 * 1024 * 1024; // 25 MB
        if content_length > MAX_BODY_BYTES {
            return Err(CoreError::Proxy(format!(
                "pricing response from {target_url} exceeds 25MB limit ({content_length} bytes)"
            )));
        }
    }

    let bytes = res.bytes().await.map_err(|e| {
        CoreError::Proxy(format!(
            "failed to read response bytes from {target_url}: {e}"
        ))
    })?;

    let now = now_rfc3339();
    let rows = parse_openrouter_models_payload(&bytes, &now)?;
    let total_fetched = rows.len();

    let count = db.batch_upsert_model_pricing(&rows)?;

    Ok(SyncStats {
        total_fetched,
        models_synced: count,
        source_url: target_url.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::Database;
    use axum::{routing::get, Json, Router};

    #[test]
    fn test_parse_openrouter_payload() {
        let json = r#"{
            "data": [
                {
                    "id": "anthropic/claude-sonnet-5.5",
                    "pricing": {
                        "prompt": "0.000002",
                        "completion": "0.00001",
                        "input_cache_read": "0.0000001",
                        "input_cache_write": "0.0000025"
                    }
                },
                {
                    "id": "deepseek/deepseek-v4.1-flash",
                    "pricing": {
                        "prompt": 0.000000036,
                        "completion": 0.0000005,
                        "input_cache_read": 0.00000001
                    }
                }
            ]
        }"#;

        let rows =
            parse_openrouter_models_payload(json.as_bytes(), "2026-11-20T20:00:00Z").unwrap();
        // 2 models * 2 (full + bare) = 4 rows
        assert_eq!(rows.len(), 4);

        let sonnet_full = rows
            .iter()
            .find(|r| r.model_pattern == "anthropic/claude-sonnet-5.5")
            .unwrap();
        assert_eq!(sonnet_full.input_micros_per_m, 2_000_000);
        assert_eq!(sonnet_full.output_micros_per_m, 10_000_000);
        assert_eq!(sonnet_full.cache_read_micros_per_m, 100_000);
        assert_eq!(sonnet_full.cache_write_micros_per_m, 2_500_000);

        let sonnet_bare = rows
            .iter()
            .find(|r| r.model_pattern == "claude-sonnet-5.5")
            .unwrap();
        assert_eq!(sonnet_bare.input_micros_per_m, 2_000_000);

        let deepseek = rows
            .iter()
            .find(|r| r.model_pattern == "deepseek/deepseek-v4.1-flash")
            .unwrap();
        assert_eq!(deepseek.input_micros_per_m, 36_000);
        assert_eq!(deepseek.output_micros_per_m, 500_000);
        assert_eq!(deepseek.cache_read_micros_per_m, 10_000);
        assert_eq!(deepseek.cache_write_micros_per_m, 0);
    }

    #[tokio::test]
    async fn test_sync_pricing_catalog_e2e_with_mock_server() {
        let payload = serde_json::json!({
            "data": [
                {
                    "id": "openai/gpt-6.2-sol",
                    "pricing": {
                        "prompt": "0.000002",
                        "completion": "0.00001"
                    }
                }
            ]
        });

        let app = Router::new().route("/models", get(move || async move { Json(payload) }));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mock_url = format!("http://{addr}/models");
        let db = Database::open_memory().unwrap();

        let stats = sync_pricing_catalog(&db, Some(&mock_url)).await.unwrap();
        assert_eq!(stats.models_synced, 2); // 1 full + 1 bare
        assert_eq!(stats.source_url, mock_url);

        let found_bare = db
            .find_dynamic_model_price("gpt-6.2-sol", None)
            .unwrap()
            .expect("should find bare name");
        assert_eq!(found_bare.input_micros_per_m, 2_000_000);
        assert_eq!(found_bare.output_micros_per_m, 10_000_000);
        assert_eq!(found_bare.source, "synced");
    }
}
