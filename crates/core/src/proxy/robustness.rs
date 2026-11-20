//! Robustness, timeout management, retry-after backoff, and stream interruption handling.

use axum::http::HeaderMap;
use std::time::Duration;

/// Tiered timeout configuration for the proxy forwarding pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutConfig {
    /// Connect timeout for establishing TCP/TLS connections to upstreams.
    pub connect_timeout: Duration,
    /// Time To First Token (TTFT) timeout: duration to wait before receiving the first semantic event.
    pub ttft_timeout: Duration,
    /// Chunk-to-chunk idle timeout during an active response stream.
    pub idle_timeout: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            ttft_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(15),
        }
    }
}

/// Parse `Retry-After` (seconds or HTTP-date) or `retry-after-ms` from response headers.
pub fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    // 1. Check `retry-after` header
    if let Some(val) = headers.get("retry-after").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        // Try parsing integer seconds
        if let Ok(secs) = trimmed.parse::<u64>() {
            return Some(Duration::from_secs(secs));
        }
        // Try parsing HTTP-date (RFC 2822 / RFC 7231 / RFC 1123)
        if let Ok(target_time) =
            time::OffsetDateTime::parse(trimmed, &time::format_description::well_known::Rfc2822)
        {
            let now = time::OffsetDateTime::now_utc();
            if target_time > now {
                let diff = target_time - now;
                return Some(Duration::from_secs(diff.whole_seconds().max(0) as u64));
            }
            return Some(Duration::ZERO);
        }
    }

    // 2. Check `retry-after-ms` header
    if let Some(val) = headers.get("retry-after-ms").and_then(|v| v.to_str().ok()) {
        if let Ok(ms) = val.trim().parse::<u64>() {
            return Some(Duration::from_millis(ms));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use std::time::Duration;

    #[test]
    fn test_timeout_config_defaults() {
        let cfg = TimeoutConfig::default();
        assert_eq!(cfg.connect_timeout, Duration::from_secs(10));
        assert_eq!(cfg.ttft_timeout, Duration::from_secs(30));
        assert_eq!(cfg.idle_timeout, Duration::from_secs(15));
    }

    #[test]
    fn test_parse_retry_after_seconds_and_millis() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", "2".parse().unwrap());
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(2)));

        let mut h_ms = HeaderMap::new();
        h_ms.insert("retry-after-ms", "1500".parse().unwrap());
        assert_eq!(parse_retry_after(&h_ms), Some(Duration::from_millis(1500)));

        let h_empty = HeaderMap::new();
        assert_eq!(parse_retry_after(&h_empty), None);

        let mut h_invalid = HeaderMap::new();
        h_invalid.insert("retry-after", "not-a-number".parse().unwrap());
        assert_eq!(parse_retry_after(&h_invalid), None);
    }
}
